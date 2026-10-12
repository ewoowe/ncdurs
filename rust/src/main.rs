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
use std::fs;
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ratatui::backend::CrosstermBackend;
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
  s                  cycle sort column (size -> name -> items)
  d                  reverse the sort order
  a                  toggle apparent size / disk usage
  q, Esc             quit

Notes:
  The size of a directory includes all of its subdirectories.
  Hard links are counted only once, symlinks are not followed.
",
        name = NAME,
        version = VERSION
    )
}
