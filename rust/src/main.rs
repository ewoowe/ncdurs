/* ncdurs - a cross-platform ncdu reimplementation in Rust
 *
 * Flow: parse arguments -> check the path -> enter the terminal -> scan with a
 * progress screen (abortable with q) -> browse the result -> restore terminal.
 * See src/scan.rs (walk), src/app.rs (browse state) and src/ui.rs (drawing).
 */

mod app;
mod platform;
mod scan;
mod ui;

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Stdout};
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
use scan::Scanner;

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

    for arg in env::args().skip(1) {
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

    let path = path.unwrap_or_else(|| PathBuf::from("."));

    // Validate before entering the terminal, so errors stay readable.
    let md = fs::symlink_metadata(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
    if !md.is_dir() {
        return Err(format!("{}: not a directory", path.display()).into());
    }

    let mut terminal = setup_terminal()?;
    let outcome = run_tui(&mut terminal, &path, show_as);
    restore_terminal(&mut terminal)?;

    // Anything the user should see is printed after the terminal is restored.
    if let Some(note) = outcome? {
        eprintln!("{}", note);
    }
    Ok(())
}

/// Scans with a progress screen, then browses the result. Returns a message to
/// print once the terminal has been restored.
fn run_tui(
    terminal: &mut Tui,
    path: &Path,
    show_as: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
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
        return Ok(Some("Scan aborted".to_string()));
    }
    if let Some(err) = scanner.failure() {
        let msg = format!("scan failed: {err}");
        scanner.abort();
        return Err(msg.into());
    }

    let (tree, stats) = scanner.into_result();
    let note = (stats.errors > 0)
        .then(|| format!("Warning: {} entries could not be read", stats.errors));

    let mut app = App::new(tree, show_as);
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        // Delete / refresh run synchronously; redraw once with the hint so the
        // user sees what is going on before the terminal blocks.
        if let Some(job) = app.job.take() {
            app.notice = Some(match &job {
                app::Job::Delete(_) => ("deleting...".to_string(), false),
                app::Job::Refresh => ("refreshing...".to_string(), false),
                app::Job::Shell => ("starting shell...".to_string(), false),
            });
            terminal.draw(|frame| ui::draw(frame, &mut app))?;
            match job {
                app::Job::Delete(idx) => app.run_delete(idx),
                app::Job::Refresh => app.run_refresh(),
                app::Job::Shell => {
                    let note = run_shell(terminal, &app);
                    app.notice = note;
                }
            }
            continue;
        }

        if let Some(key) = poll_key(BROWSE_POLL)? {
            app.on_key(key);
        }
    }

    Ok(note)
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
