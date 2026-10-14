/* ncdurs - a cross-platform ncdu reimplementation in Rust
 *
 * Flow: parse arguments -> check the path -> enter the terminal -> scan with a
 * progress screen (abortable with q) -> browse the result -> restore terminal.
 * See src/scan.rs (walk), src/app.rs (browse state) and src/ui.rs (drawing).
 */

mod app;
mod backend;
mod exclude;
mod export;
mod import;
mod platform;
mod scan;
mod theme;
mod ui;

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use app::{App, DisplayOptions, SortCol};
use scan::{ScanOptions, Scanner, Tree};
use theme::Theme;

pub const NAME: &str = env!("CARGO_PKG_NAME");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

type Tui = Terminal<backend::AnsiBackend<Stdout>>;

/// Entries processed between two checks for user input during the scan.
const SCAN_BATCH: usize = 4096;
/// Minimum time between two scan progress redraws.
const SCAN_UI_INTERVAL: Duration = Duration::from_millis(200);
/// Poll timeout of the browse loop.
const BROWSE_POLL: Duration = Duration::from_millis(200);

fn main() -> ExitCode {
    // The C version enables the locale early on; the thousands separator in
    // the info window depends on it.
    platform::init_locale();
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
    let mut theme = Theme::default();
    let mut excludes = exclude::Excludes::default();
    let mut one_file_system = false;
    let mut exclude_caches = false;
    let mut exclude_kernfs = false;
    let mut extended = false;
    let mut show_hidden = true;
    let mut show_mtime = false;
    let mut show_items = false;
    let mut graph: u8 = 1;
    let mut graph_style: u8 = 0;
    let mut sort_col = SortCol::Size;
    let mut sort_desc = true;
    let mut sort_df = false;
    let mut natsort = true;
    let mut can_delete = true;
    let mut can_shell = true;
    let mut can_refresh = true;
    let mut confirm_quit = false;
    let mut delete_confirm = true;
    // -0/-1/-2: how much feedback the scan gives (2 = full screen, the default
    // when browsing; the C version picks 0/1 by itself when exporting).
    let mut ui_level: u8 = 2;
    // -q: refresh the scan screen every 2 s instead of 10 times a second.
    let mut slow_ui = false;
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
            "--exclude" => {
                let pattern = args.next().ok_or("--exclude requires a pattern")?;
                excludes.add(&pattern);
            }
            "-X" | "--exclude-from" => {
                let file = args.next().ok_or("-X requires a file name")?;
                excludes
                    .add_file(&file)
                    .map_err(|e| format!("{file}: {e}"))?;
            }
            "-x" | "--one-file-system" => one_file_system = true,
            "--cross-file-system" => one_file_system = false,
            "--exclude-caches" => exclude_caches = true,
            "--include-caches" => exclude_caches = false,
            "--exclude-kernfs" => exclude_kernfs = true,
            "--include-kernfs" => exclude_kernfs = false,
            "-e" | "--extended" => extended = true,
            "--no-extended" => extended = false,
            "--show-hidden" => show_hidden = true,
            "--hide-hidden" => show_hidden = false,
            "--show-mtime" => show_mtime = true,
            "--hide-mtime" => show_mtime = false,
            "--show-itemcount" => show_items = true,
            "--hide-itemcount" => show_items = false,
            // The C version keeps the graph in a 0..3 value: bit 0 is the bar,
            // bit 1 the percentage.
            "--show-graph" => graph |= 1,
            "--hide-graph" => graph &= !1,
            "--show-percent" => graph |= 2,
            "--hide-percent" => graph &= !2,
            "--graph-style" => {
                let value = args.next().ok_or("--graph-style requires a style")?;
                graph_style = match value.as_str() {
                    "hash" => 0,
                    "half-block" => 1,
                    // The C version accepts this misspelling too (main.c:231).
                    "eighth-block" | "eigth-block" => 2,
                    _ => {
                        return Err(format!("Unknown --graph-style option: {value}.").into());
                    }
                };
            }
            // Feature switches (src/main.c:194-222).
            "--enable-delete" => can_delete = true,
            "--disable-delete" => can_delete = false,
            "--enable-shell" => can_shell = true,
            "--disable-shell" => can_shell = false,
            "--enable-refresh" => can_refresh = true,
            "--disable-refresh" => can_refresh = false,
            // Read-only mode: -r disables deletion, -rr the shell as well.
            "-r" | "-rr" => {
                can_delete = false;
                if arg == "-rr" {
                    can_shell = false;
                }
            }
            "--confirm-quit" => confirm_quit = true,
            "--no-confirm-quit" => confirm_quit = false,
            "--confirm-delete" => delete_confirm = true,
            "--no-confirm-delete" => delete_confirm = false,
            "--enable-natsort" => natsort = true,
            "--disable-natsort" => natsort = false,
            "--group-directories-first" => sort_df = true,
            "--no-group-directories-first" => sort_df = false,
            "--sort" => {
                let value = args.next().ok_or("--sort requires a column")?;
                parse_sort(&value, &mut sort_col, &mut sort_desc, &mut show_as)?;
            }
            // Interface level and update interval (ncdu.1 "Interface Options").
            "-0" => ui_level = 0,
            "-1" => ui_level = 1,
            "-2" => ui_level = 2,
            "-q" | "--slow-ui-updates" => slow_ui = true,
            "--fast-ui-updates" => slow_ui = false,
            "--si" => ui::set_si(true),
            "--no-si" => ui::set_si(false),
            "--color" => {
                let value = args.next().ok_or("--color requires a scheme")?;
                theme = Theme::parse(&value)
                    .ok_or_else(|| format!("unknown --color option: {value}"))?;
            }
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

    // The display switches are also valid for `-f` (browsing an import file),
    // so they are collected before either mode runs. "Directories first" has
    // no command line switch in the C version either - only the 't' key.
    let display = DisplayOptions {
        show_hidden,
        show_mtime,
        show_items,
        graph,
        graph_style,
        sort_df,
        sort_col,
        sort_desc,
        natsort,
        can_delete,
        can_shell,
        can_refresh,
        confirm_quit,
        delete_confirm,
        ui_level,
        slow_ui,
    };

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
        let outcome = run_browse(&mut terminal, tree, show_as, theme, display);
        restore_terminal(&mut terminal)?;
        if let Some(note) = outcome? {
            eprintln!("{}", note);
        }
        return Ok(());
    }

    // Mode 2: scan a directory, optionally exporting instead of browsing.
    let path = path.unwrap_or_else(|| PathBuf::from("."));
    let options = ScanOptions {
        excludes,
        one_file_system,
        exclude_caches,
        exclude_kernfs,
        extended,
    };

    // Validate before entering the terminal, so errors stay readable.
    let md = fs::symlink_metadata(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
    if !md.is_dir() {
        return Err(format!("{}: not a directory", path.display()).into());
    }

    // Exporting does not need a terminal: when stdout is redirected (a pipe or
    // a file) scan without the progress screen, so it works from scripts.
    if let Some(file) = export.as_deref()
        && !io::stdout().is_terminal()
    {
        let (tree, warning) = scan_plain(&path, &options)?;
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

    let mut terminal = setup_terminal()?;
    let outcome = run_tui(
        &mut terminal,
        &path,
        show_as,
        export.as_deref(),
        theme,
        options,
        display,
    );
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
/// `--sort COLUMN[-asc|-desc]`. The column names follow the C version
/// (src/main.c:341): disk-usage, name, apparent-size, itemcount, mtime.
fn parse_sort(
    value: &str,
    col: &mut SortCol,
    desc: &mut bool,
    show_as: &mut bool,
) -> Result<(), String> {
    let (name, order) = if let Some(rest) = value.strip_suffix("-asc") {
        (rest, Some(false))
    } else if let Some(rest) = value.strip_suffix("-desc") {
        (rest, Some(true))
    } else {
        (value, None)
    };
    match name {
        "disk-usage" => {
            *col = SortCol::Size;
            *show_as = false;
        }
        // The apparent size is the same column, just the other metric - which
        // matches how `a` toggles the C version.
        "apparent-size" => {
            *col = SortCol::Size;
            *show_as = true;
        }
        "name" => *col = SortCol::Name,
        "itemcount" => *col = SortCol::Items,
        "mtime" => *col = SortCol::Mtime,
        _ => return Err(format!("unknown --sort column: {value}")),
    }
    // Names read best ascending, every other column starts with the biggest.
    *desc = !matches!(*col, SortCol::Name);
    if let Some(order) = order {
        *desc = order;
    }
    Ok(())
}

fn scan_plain(
    path: &Path,
    options: &ScanOptions,
) -> Result<(Tree, Option<String>), Box<dyn std::error::Error>> {
    let mut scanner = Scanner::new(path, options.clone())?;
    while !scanner.step(SCAN_BATCH) {}
    if let Some(err) = scanner.failure().map(|e| e.to_string()) {
        scanner.abort();
        return Err(format!("scan failed: {err}").into());
    }
    let (tree, stats) = scanner.into_result();
    let warning =
        (stats.errors > 0).then(|| format!("Warning: {} entries could not be read", stats.errors));
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
    theme: Theme,
    display: DisplayOptions,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let mut app = App::new(tree, show_as, true, theme);
    app.set_display(display);
    browse_loop(terminal, &mut app)?;
    Ok(None)
}

/// Scans with a progress screen, then browses or exports the result.
fn run_tui(
    terminal: &mut Tui,
    path: &Path,
    show_as: bool,
    export: Option<&Path>,
    theme: Theme,
    options: ScanOptions,
    display: DisplayOptions,
) -> Result<Outcome, Box<dyn std::error::Error>> {
    let mut scanner = Scanner::new(path, options.clone())?;
    let mut aborted = false;
    let mut last_draw: Option<Instant> = None;
    // -q / --slow-ui-updates: 2 s instead of the default 10 updates a second.
    let interval = if display.slow_ui {
        Duration::from_secs(2)
    } else {
        SCAN_UI_INTERVAL
    };

    loop {
        let done = scanner.step(SCAN_BATCH);
        let redraw = done || last_draw.is_none_or(|t| t.elapsed() >= interval);

        if redraw {
            // -0: no feedback at all until the scan is done.
            if display.ui_level > 0 {
                terminal.draw(|frame| ui::draw_scan(frame, &scanner, &theme.colors()))?;
            }
            last_draw = Some(Instant::now());
            if let Some(key) = poll_key(Duration::ZERO)?
                && is_abort_key(&key)
            {
                aborted = true;
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
    let warning =
        (stats.errors > 0).then(|| format!("Warning: {} entries could not be read", stats.errors));

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

    let mut app = App::new(tree, show_as, false, theme);
    app.scan_options = options;
    app.set_display(display);
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
    Terminal::new(backend::AnsiBackend::new(out))
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
      --si / --no-si    Use decimal (kB/MB) instead of binary (KiB/MiB) units
      --exclude PATTERN Leave matching files out of the statistics (repeatable)
  -X, --exclude-from FILE  Read exclude patterns from FILE, one per line
  -x, --one-file-system    Do not cross filesystem boundaries
      --exclude-caches     Skip directories that hold a CACHEDIR.TAG
      --exclude-kernfs     Skip Linux pseudo filesystems (proc, sys, cgroup, ...)
  -e, --extended           Collect uid/gid/mode/mtime (enables the mtime
                           column with 'm' and sorting by it with 'M')
      --show-hidden        Show hidden and excluded entries (default, 'e' key)
      --hide-hidden        Hide dotfiles, backup files and excluded entries
      --show-mtime         Show the modification time column (needs -e)
      --hide-mtime         Hide the modification time column (default)
      --show-itemcount     Show the number of contained items (default: off)
      --hide-itemcount     Hide the item count column
      --show-graph         Show the bar graph (default)
      --hide-graph         Hide the bar graph
      --show-percent       Show the percentage (default: off)
      --hide-percent       Hide the percentage
      --graph-style STYLE  hash (default) / half-block / eighth-block
      --sort COLUMN        Sort by disk-usage (default) / name / apparent-size /
                           itemcount / mtime, optionally with -asc or -desc
      --enable-natsort     Natural order when sorting by name (default)
      --disable-natsort    Compare names byte-wise instead
      --group-directories-first / --no-group-directories-first
      --confirm-quit       Ask before quitting
      --confirm-delete     Ask before deleting (default)
      --enable-delete / --disable-delete
      --enable-shell / --disable-shell
      --enable-refresh / --disable-refresh
  -r / -rr                 Read-only: -r disables delete, -rr the shell as well
  -0 / -1 / -2             Feedback while scanning: none / full screen (default)
  -q, --slow-ui-updates    Refresh the scan screen every 2 s
      --color SCHEME    Colour scheme: auto (default), off, dark, dark-bg
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
  i                  show information about the selected entry
  ?                  help window (keys / format / about)
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
