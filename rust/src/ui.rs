/* ncdu-rs - drawing code (ratatui), comparable to src/browser.c
 *
 * Layout: status bar, path bar, item list, totals line.
 */

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::scan::Node;
use crate::{NAME, VERSION};

const BAR_WIDTH: usize = 10;

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_status(f, chunks[0]);
    draw_path(f, app, chunks[1]);
    app.view_height = chunks[2].height as usize;
    draw_list(f, app, chunks[2]);
    draw_footer(f, app, chunks[3]);
}

fn draw_status(f: &mut Frame, area: Rect) {
    let style = Style::default().add_modifier(Modifier::REVERSED);
    let text = format!(
        "{} {} ~ ↑/↓ move  →/Enter enter  ←/u back  s sort  d order  a size  q quit",
        NAME, VERSION
    );
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn draw_path(f: &mut Frame, app: &App, area: Rect) {
    let style = Style::default().add_modifier(Modifier::REVERSED);
    let mut line = format!("--- {} ", app.cur_path());
    let width = area.width as usize;
    while line.chars().count() < width {
        line.push('-');
    }
    f.render_widget(Paragraph::new(line).style(style), area);
}

fn draw_list(f: &mut Frame, app: &mut App, area: Rect) {
    let total = app.cur_total();
    let show_as = app.show_as;
    let items: Vec<ListItem> = if app.sorted.is_empty() {
        vec![ListItem::new("(no entries)")]
    } else {
        app.sorted
            .iter()
            .map(|&idx| ListItem::new(row_text(app.node(idx), total, show_as)))
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

/// One list line: size, graph, percentage and name ("/" marks a directory).
fn row_text(n: &Node, total: u64, show_as: bool) -> String {
    let size = if show_as { n.asize } else { n.size };
    let pct = if total > 0 {
        size as f64 * 100.0 / total as f64
    } else {
        0.0
    };
    let name = if n.error {
        format!("{} [error]", n.name)
    } else {
        n.name.clone()
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
