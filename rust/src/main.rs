/* ncdu-rs - a minimal Rust reimplementation of ncdu
 *
 * First version: scan a directory tree, then browse the result in a TUI.
 * See src/scan.rs (scanning), src/app.rs (state/keys) and src/ui.rs (drawing).
 */

mod app;
mod platform;
mod scan;
mod ui;

use std::env;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;

use app::App;

pub const NAME: &str = env!("CARGO_PKG_NAME");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

type Tui = Terminal<CrosstermBackend<Stdout>>;

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
    eprintln!("Scanning {} ...", path.display());
    let (tree, stats) = scan::scan(&path)?;
    if stats.errors > 0 {
        eprintln!("Warning: {} entries could not be read", stats.errors);
    }

    let mut app = App::new(tree, show_as);
    let mut terminal = setup_terminal()?;
    let result = event_loop(&mut terminal, &mut app);
    restore_terminal(&mut terminal)?;
    result
}

fn event_loop(terminal: &mut Tui, app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.on_key(key);
                }
            }
        }
    }
    Ok(())
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
        "{name} {version} - minimal Rust reimplementation of ncdu

Usage: {name} [OPTIONS] [PATH]

Options:
  -a, --apparent-size   Show apparent size instead of disk usage
  -h, --help            Show this help and exit
  -V, --version         Print version

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
