/* ncdurs - drawing code (ratatui), comparable to src/browser.c
 *
 * Two screens share the same four-row frame (status bar, path bar, content,
 * footer): the scan progress screen shown while the tree is built, and the
 * browse screen. The footer shows, in order of priority, the delete
 * confirmation, a transient notice, or the totals.
 */

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::scan::{Node, Progress, Scanner};
use crate::{NAME, VERSION};

const BAR_WIDTH: usize = 10;

const KEYS_HINT: &str =
    "↑/↓ move  →/Enter enter  ←/u back  n/s/C sort  a size  d del  r refresh  b shell  q quit";

fn frame_layout(f: &mut Frame) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area())
}

/* ---------------------------- browse screen ----------------------------- */

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = frame_layout(f);
    if app.imported {
        // The tree was loaded from a file and does not describe the disk.
        draw_status(f, chunks[0], &format!("{KEYS_HINT}  [imported]"));
    } else {
        draw_status(f, chunks[0], KEYS_HINT);
    }
    draw_path(f, chunks[1], &app.cur_path());
    app.view_height = chunks[2].height as usize;
    draw_list(f, app, chunks[2]);
    draw_footer(f, app, chunks[3]);
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let total = app.cur_total();
    let show_as = app.show_as;
    let items: Vec<ListItem> = if app.sorted.is_empty() {
        vec![ListItem::new("(no entries)")]
    } else {
        app.sorted
            .iter()
            .map(|&idx| {
                ListItem::new(row_text(app.node(idx), app.tree.name(idx), total, show_as))
            })
            .collect()
    };

    let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    if app.sorted.is_empty() {
        app.list_state.select(None);
    } else {
        app.list_state.select(Some(app.selected));
    }
    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    // Priority: delete confirmation > transient notice > totals.
    if let Some(idx) = app.confirm {
        let text = format!(
            "Really delete \"{}\"?  y = yes, any other key = cancel",
            app.tree.path_of(idx)
        );
        let style = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(Paragraph::new(text).style(style), area);
        return;
    }
    if let Some((msg, is_err)) = &app.notice {
        let style = if *is_err {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };
        f.render_widget(Paragraph::new(msg.as_str()).style(style), area);
        return;
    }

    let node = app.cur_node();
    let text = format!(
        "Disk usage: {}   Apparent size: {}   Items: {}   Sort: {} {}",
        human_size(node.size),
        human_size(node.asize),
        node.items,
        app.sort_col.label(),
        if app.sort_desc { "desc" } else { "asc" }
    );
    f.render_widget(Paragraph::new(text).alignment(Alignment::Right), area);
}

/* ----------------------------- scan screen ------------------------------ */

pub fn draw_scan(f: &mut Frame, scanner: &Scanner) {
    let chunks = frame_layout(f);
    let progress = scanner.progress();

    draw_status(
        f,
        chunks[0],
        &format!("{NAME} {VERSION} ~ Scanning... press q to abort"),
    );
    draw_path(f, chunks[1], &progress.root);
    draw_progress(f, chunks[2], &progress);
    draw_scan_footer(f, chunks[3], &progress);
}

fn draw_progress(f: &mut Frame, area: Rect, p: &Progress) {
    let lines = vec![
        Line::from(""),
        Line::from(format!("   Total items: {}", p.entries)),
        Line::from(format!("   size: {}", human_size(p.bytes))),
        Line::from(""),
        Line::from(format!("   Current item: {}", p.current)),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_scan_footer(f: &mut Frame, area: Rect, p: &Progress) {
    let text = if p.errors > 0 {
        format!("errors: {}   ", p.errors)
    } else {
        String::new()
    };
    f.render_widget(Paragraph::new(text).alignment(Alignment::Right), area);
}

/* ------------------------------- shared --------------------------------- */

fn draw_status(f: &mut Frame, area: Rect, hint: &str) {
    let style = Style::default().add_modifier(Modifier::REVERSED);
    let text = format!("{NAME} {VERSION} ~ {hint}");
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn draw_path(f: &mut Frame, area: Rect, path: &str) {
    let style = Style::default().add_modifier(Modifier::REVERSED);
    let mut line = format!("--- {path} ");
    let width = area.width as usize;
    while line.chars().count() < width {
        line.push('-');
    }
    f.render_widget(Paragraph::new(line).style(style), area);
}

/// One list line: size, graph, percentage and name ("/" marks a directory).
fn row_text(n: &Node, name: &str, total: u64, show_as: bool) -> String {
    let size = if show_as { n.asize } else { n.size };
    let pct = if total > 0 {
        size as f64 * 100.0 / total as f64
    } else {
        0.0
    };
    let name = if n.error {
        format!("{name} [error]")
    } else {
        name.to_string()
    };
    format!(
        "{:>9} [{}] {:>4}% {}{}",
        human_size(size),
        bar(pct),
        pct.round() as u64,
        if n.is_dir { "/" } else { "" },
        name
    )
}

fn bar(pct: f64) -> String {
    let filled = ((pct / 100.0) * BAR_WIDTH as f64).round() as usize;
    // Any non-empty entry gets at least one mark, like the C version does.
    let filled = if pct > 0.0 {
        filled.clamp(1, BAR_WIDTH)
    } else {
        0
    };
    format!("{}{}", "#".repeat(filled), " ".repeat(BAR_WIDTH - filled))
}

/// Formats a byte count as e.g. "4.0 KiB" or "123 B", using base 2 units.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{} B", bytes);
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{:.1} {}", value, UNITS[unit])
}
