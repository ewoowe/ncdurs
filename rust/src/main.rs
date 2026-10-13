/* ncdurs - a cross-platform ncdu reimplementation in Rust
 *
 * Flow: parse arguments -> check the path -> enter the terminal -> scan with a
 * progress screen (abortable with q) -> browse the result -> restore terminal.
 * See src/scan.rs (walk), src/app.rs (browse state) and src/ui.rs (drawing).
 */

mod app;
mod export;
mod import;
mod platform;
mod scan;
mod ui;

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;

use app::App;
use scan::{Scanner, Tree};

pub const NAME: &str = env!("CARGO_PKG_NAME");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Entries processed between two checks for user input during the scan.
const SCAN_BATCH: usize = 4096;
/// Minimum time between two scan progress redraws.
const SCAN_UI_INTERVAL: Duration = Duration::from_millis(200);
/// Poll timeout of the browse loop.
const BROWSE_POLL: Duration = Duration::from_millis(200);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}: {}", NAME, err);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut path: Option<PathBuf> = None;
    let mut show_as = false;
    let mut export: Option<PathBuf> = None;
    let mut import: Option<PathBuf> = None;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{}", usage());
                return Ok(());
            }
            "-V" | "--version" => {
                println!("{} {}", NAME, VERSION);
                return Ok(());
            }
            "-a" | "--apparent-size" => show_as = true,
            "-o" | "--export" => {
                let file = args.next().ok_or("-o requires a file name")?;
                export = Some(PathBuf::from(file));
            }
            "-f" | "--import" => {
                let file = args.next().ok_or("-f requires a file name")?;
                import = Some(PathBuf::from(file));
            }
            _ if arg.starts_with('-') && arg != "-" => {
                return Err(format!("unknown option '{}', try --help", arg).into());
            }
            _ => {
                if path.is_some() {
                    return Err("only one path can be scanned at a time".into());
                }
                path = Some(PathBuf::from(arg));
            }
        }
    }

    // Mode 1: load an export file ("-" = stdin), optionally re-exporting it
    // instead of browsing.
    if let Some(file) = &import {
        if path.is_some() {
            return Err("a path cannot be combined with -f".into());
        }
        let tree = read_export(file)?;
        if let Some(out) = &export {
            write_export(&tree, out)?;
            return Ok(());
        }
        if !io::stdout().is_terminal() {
            return Err("browsing an import needs a terminal".into());
        }
        let mut terminal = setup_terminal()?;
        let outcome = run_browse(&mut terminal, tree, show_as);
        restore_terminal(&mut terminal)?;
        if let Some(note) = outcome? {
            eprintln!("{}", note);
        }
        return Ok(());
    }

    // Mode 2: scan a directory, optionally exporting instead of browsing.
    let path = path.unwrap_or_else(|| PathBuf::from("."));

    // Validate before entering the terminal, so errors stay readable.
    let md = fs::symlink_metadata(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
    if !md.is_dir() {
        return Err(format!("{}: not a directory", path.display()).into());
    }

    // Exporting does not need a terminal: when stdout is redirected (a pipe or
    // a file) scan without the progress screen, so it works from scripts.
    if let Some(file) = export.as_deref() {
        if !io::stdout().is_terminal() {
            let (tree, warning) = scan_plain(&path)?;
            let entries = tree.nodes.len();
            if file == Path::new("-") {
                let mut out = io::stdout().lock();
                export::write(&tree, &mut out).map_err(|e| format!("stdout: {e}"))?;
                out.flush()?;
            } else {
                write_export(&tree, file)?;
                eprintln!("exported {entries} entries to {}", file.display());
            }
            if let Some(warning) = warning {
                eprintln!("{warning}");
            }
            return Ok(());
        }
    }

    let mut terminal = setup_terminal()?;
    let outcome = run_tui(&mut terminal, &path, show_as, export.as_deref());
    restore_terminal(&mut terminal)?;

    // Anything the user should see is printed after the terminal is restored.
    let outcome = outcome?;
    if let Some(data) = outcome.stdout_export {
        let mut out = io::stdout().lock();
        out.write_all(&data)?;
        out.flush()?;
    }
    if let Some(note) = outcome.note {
        eprintln!("{}", note);
    }
    Ok(())
}

/// Scans without any terminal interaction (used when stdout is redirected).
fn scan_plain(path: &Path) -> Result<(Tree, Option<String>), Box<dyn std::error::Error>> {
    let mut scanner = Scanner::new(path)?;
    while !scanner.step(SCAN_BATCH) {}
    if let Some(err) = scanner.failure().map(|e| e.to_string()) {
        scanner.abort();
        return Err(format!("scan failed: {err}").into());
    }
    let (tree, stats) = scanner.into_result();
    let warning = (stats.errors > 0)
        .then(|| format!("Warning: {} entries could not be read", stats.errors));
    Ok((tree, warning))
}

/// Opens an export file ("-" = stdin) and loads it into a tree.
fn read_export(file: &Path) -> Result<Tree, Box<dyn std::error::Error>> {
    let result = if file == Path::new("-") {
        import::read(io::stdin().lock())
    } else {
        let f = fs::File::open(file).map_err(|e| format!("{}: {}", file.display(), e))?;
        import::read(io::BufReader::new(f))
    };
    result.map_err(|e| format!("{}: {}", file.display(), e).into())
}

/// Writes the tree as an ncdu-compatible export file ("-" = stdout).
fn write_export(tree: &Tree, file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if file == Path::new("-") {
        let mut out = io::stdout().lock();
        export::write(tree, &mut out).map_err(|e| format!("stdout: {e}"))?;
    } else {
        let f = fs::File::create(file).map_err(|e| format!("{}: {}", file.display(), e))?;
        let mut out = io::BufWriter::new(f);
        export::write(tree, &mut out).map_err(|e| format!("{}: {}", file.display(), e))?;
    }
    Ok(())
}

/// What a terminal session produced; handled after the terminal is restored.
#[derive(Default)]
struct Outcome {
    /// Message to print on stderr.
    note: Option<String>,
    /// Export data destined for stdout (only used for `-o -`).
    stdout_export: Option<Vec<u8>>,
}

/// Browses an already loaded tree (used with -f).
fn run_browse(
    terminal: &mut Tui,
    tree: Tree,
    show_as: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut app = App::new(tree, show_as, true);
    browse_loop(terminal, &mut app)?;
    Ok(None)
}

/// Scans with a progress screen, then browses or exports the result.
fn run_tui(
    terminal: &mut Tui,
    path: &Path,
    show_as: bool,
    export: Option<&Path>,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let mut scanner = Scanner::new(path)?;
    let mut aborted = false;
    let mut last_draw: Option<Instant> = None;

    loop {
        let done = scanner.step(SCAN_BATCH);
        let redraw = done || last_draw.is_none_or(|t| t.elapsed() >= SCAN_UI_INTERVAL);

        if redraw {
            terminal.draw(|frame| ui::draw_scan(frame, &scanner))?;
            last_draw = Some(Instant::now());
            if let Some(key) = poll_key(Duration::ZERO)? {
                if is_abort_key(&key) {
                    aborted = true;
                }
            }
        }

        if aborted || done || scanner.failure().is_some() {
            break;
        }
    }

    if aborted {
        scanner.abort();
        return Ok(Outcome {
            note: Some("Scan aborted".to_string()),
            stdout_export: None,
        });
    }
    if let Some(err) = scanner.failure() {
        let msg = format!("scan failed: {err}");
        scanner.abort();
        return Err(msg.into());
    }

    let (tree, stats) = scanner.into_result();
    let warning = (stats.errors > 0)
        .then(|| format!("Warning: {} entries could not be read", stats.errors));

    if let Some(file) = export {
        let entries = tree.nodes.len();
        if file == Path::new("-") {
            let mut buf = Vec::new();
            export::write(&tree, &mut buf).map_err(|e| format!("export: {e}"))?;
            return Ok(Outcome {
                note: Some(format!("exported {entries} entries to stdout")),
                stdout_export: Some(buf),
            });
        }
        write_export(&tree, file)?;
        let note = format!("exported {entries} entries to {}", file.display());
        return Ok(Outcome {
            note: Some(match warning {
                Some(w) => format!("{w}; {note}"),
                None => note,
            }),
            stdout_export: None,
        });
    }

    let mut app = App::new(tree, show_as, false);
    browse_loop(terminal, &mut app)?;
    Ok(Outcome {
        note: warning,
        stdout_export: None,
    })
}

/// The browse screen: draws the app and runs the jobs it requests.
fn browse_loop(terminal: &mut Tui, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Delete / refresh / shell run synchronously; redraw once with the hint
        // so the user sees what is going on before the terminal blocks.
        if let Some(job) = app.job.take() {
            app.notice = Some(match &job {
                app::Job::Delete(_) => ("deleting...".to_string(), false),
                app::Job::Refresh => ("refreshing...".to_string(), false),
                app::Job::Shell => ("starting shell...".to_string(), false),
            });
            terminal.draw(|frame| ui::draw(frame, app))?;
            match job {
                app::Job::Delete(idx) => app.run_delete(idx),
                app::Job::Refresh => app.run_refresh(),
                app::Job::Shell => {
                    let note = run_shell(terminal, app);
                    app.notice = note;
                }
            }
            continue;
        }

        if let Some(key) = poll_key(BROWSE_POLL)? {
            app.on_key(key);
        }
    }
    Ok(())
}

/// Waits up to `timeout` for a key press; returns None for other events.
fn poll_key(timeout: Duration) -> io::Result<Option<KeyEvent>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(Some(key)),
        _ => Ok(None),
    }
}

/// Keys that abort the running scan.
fn is_abort_key(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => true,
        KeyCode::Char('c') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

fn setup_terminal() -> io::Result<Tui> {
    // Make sure the terminal is restored even if we panic.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        hook(info);
    }));

    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(out))
}

fn restore_terminal(terminal: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/* ------------------------------ spawn shell ------------------------------ */

#[cfg(unix)]
const DEFAULT_SHELL: &str = "/bin/sh";
#[cfg(windows)]
const DEFAULT_SHELL: &str = "cmd.exe";

/// Command to run for `b`: NCDU_SHELL, then SHELL, then a platform default
/// (same order as the C version).
fn shell_command() -> std::ffi::OsString {
    env::var_os("NCDU_SHELL")
        .or_else(|| env::var_os("SHELL"))
        .unwrap_or_else(|| OsStr::new(DEFAULT_SHELL).to_os_string())
}

/// `NCDU_LEVEL` handling of the C version: unset (or anything that is not a
/// single digit 1..8) becomes "1", a digit 1..8 is incremented, "9" wraps to 1.
fn level_for(current: Option<&str>) -> String {
    if let Some(s) = current {
        let b = s.as_bytes();
        if b.len() == 1 && (b'1'..=b'8').contains(&b[0]) {
            return ((b[0] + 1) as char).to_string();
        }
    }
    "1".to_string()
}

/// Runs `shell` in `dir` through the platform shell, mirroring the C version's
/// system(shell) call - so NCDU_SHELL may contain arguments.
fn spawn_shell(shell: &OsStr, dir: &str, level: &str) -> io::Result<std::process::ExitStatus> {
    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(shell);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(shell);
        c
    };
    cmd.current_dir(dir).env("NCDU_LEVEL", level).status()
}

/// Suspends the TUI, runs the user's shell in the open directory, then resumes
/// the TUI. Returns a footer message on failure (success is silent, like the
/// C version).
fn run_shell(terminal: &mut Tui, app: &App) -> Option<(String, bool)> {
    if app.imported {
        return Some(("unavailable: this tree was imported".to_string(), true));
    }

    let path = app.cur_path();
    if !Path::new(&path).is_dir() {
        return Some((format!("shell: {path} is no longer a directory"), true));
    }

    let shell = shell_command();
    // Note: the variable is only set for the child, so the level is bumped
    // exactly once per ncdu session - same effect as the C version's static flag.
    let level = level_for(env::var("NCDU_LEVEL").ok().as_deref());

    // Hand the terminal over to the shell.
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen, Show);

    let status = spawn_shell(&shell, &path, &level);

    // Take the terminal back; clear() forces a full redraw.
    let _ = enable_raw_mode();
    let _ = execute!(terminal.backend_mut(), EnterAlternateScreen);
    let _ = terminal.clear();

    match status {
        Ok(st) if st.success() => None,
        Ok(st) => Some((format!("shell exited with {st}"), true)),
        Err(e) => Some((format!("failed to run {shell:?}: {e}"), true)),
    }
}

fn usage() -> String {
    format!(
        "{name} {version} - cross-platform ncdu reimplementation in Rust

Usage: {name} [OPTIONS] [PATH]

Options:
  -a, --apparent-size   Show apparent size instead of disk usage
  -o, --export FILE     Scan and write the result as JSON to FILE (use - for stdout)
  -f, --import FILE     Load a FILE created with -o and browse it (use - for stdin)
  -h, --help            Show this help and exit
  -V, --version         Print version

Scanning:
  While the tree is being built the current path, the number of entries and
  the size seen so far are shown; press q (or Esc) to abort the scan.

Keys:
  Up/k, Down/j       move the cursor
  PgUp, PgDn         move one page up/down
  g, G               jump to the first/last entry
  Right/l/Enter      open the selected directory
  Left/u/Backspace   go back to the parent directory
  n, s, C            sort by name / size / items (press again to reverse)
  a                  toggle apparent size / disk usage
  d                  delete the selected entry (asks for confirmation)
  r                  re-scan the currently open directory
  b                  run the shell (NCDU_SHELL / $SHELL) in this directory
  q, Esc             quit

Notes:
  The size of a directory includes all of its subdirectories.
  Hard links are counted only once, symlinks are not followed.
  The export format is interchangeable with ncdu's own JSON, e.g.
  'ncdurs -o - /usr | ncdu -f -'. A tree loaded with -f does not describe the
  filesystem, so refresh, delete and the shell are disabled for it.
",
        name = NAME,
        version = VERSION
    )
}

#[cfg(test)]
mod tests {
    use super::level_for;

    #[test]
    fn ncdu_level_follows_the_c_version() {
        assert_eq!(level_for(None), "1");
        assert_eq!(level_for(Some("")), "1");
        assert_eq!(level_for(Some("1")), "2");
        assert_eq!(level_for(Some("5")), "6");
        assert_eq!(level_for(Some("8")), "9");
        assert_eq!(level_for(Some("9")), "1"); // the C version wraps 9 back to 1
        assert_eq!(level_for(Some("12")), "1");
        assert_eq!(level_for(Some("x")), "1");
    }
}
