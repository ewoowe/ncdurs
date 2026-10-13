/* ncdurs - drawing code (ratatui), mirrors src/browser.c of ncdu 1.22
 *
 * Layout and strings follow the C version:
 *
 *   row 0            "ncdu 1.22 ~ Use the arrow keys to navigate, press ? for help"
 *                    (the "?" is UIC_KEY_HD; "[imported]" hugs the right edge)
 *   row 1            '-' rule in UIC_DEFAULT, with "--- <path> " (UIC_DIR) once
 *                    inside a subdirectory
 *   rows 2 .. h-2    listing, one row per entry:
 *                      [flag][size 10][graph N][ ' '][/ or ' '][name]
 *   row h-1          "*Total disk usage: ...   Apparent size: ...   Items: ..."
 *                    or " No items to display." when there is nothing to show
 *
 * Colours come from the active theme (see theme.rs); the same slots the C
 * version uses are applied to the same runs of text: numbers are UIC_NUM*,
 * names of directories UIC_DIR*, the bar UIC_GRAPH*, the flag column UIC_FLAG*,
 * and everything else on a row is UIC_DEFAULT / UIC_SEL.
 */

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::scan::{Node, Scanner};
use crate::theme::Colors;
use crate::{NAME, VERSION};

/* --------------------- helpers mirroring the C code --------------------- */

/// Width of the graph column (src/browser.c:148).
fn bar_size(cols: usize) -> usize {
    (cols / 7).max(10)
}

/// `--si`: the C version keeps this in a global (src/util.c:76) and switches
/// between decimal (kB/MB/...) and binary (KiB/MiB/...) unit prefixes.
pub fn set_si(on: bool) {
    SI.store(on, std::sync::atomic::Ordering::Relaxed);
}

static SI: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// formatsize() (src/util.c:75). It uses float, not double - keeping the same
/// precision avoids last-digit differences.
fn formatsize(bytes: u64) -> (f32, &'static str) {
    let mut r = bytes as f32;
    if SI.load(std::sync::atomic::Ordering::Relaxed) {
        return if r < 1000.0 {
            (r, " B")
        } else if r < 1e6 {
            (r / 1e3, "kB")
        } else if r < 1e9 {
            (r / 1e6, "MB")
        } else if r < 1e12 {
            (r / 1e9, "GB")
        } else if r < 1e15 {
            (r / 1e12, "TB")
        } else if r < 1e18 {
            (r / 1e15, "PB")
        } else {
            (r / 1e18, "EB")
        };
    }
    let unit = if r < 1000.0 {
        "  B"
    } else if r < 1023e3 {
        r /= 1024.0;
        "KiB"
    } else if r < 1023e6 {
        r /= 1048576.0;
        "MiB"
    } else if r < 1023e9 {
        r /= 1073741824.0;
        "GiB"
    } else if r < 1023e12 {
        r /= 1099511627776.0;
        "TiB"
    } else if r < 1023e15 {
        r /= 1125899906842624.0;
        "PiB"
    } else {
        r /= 1152921504606846976.0;
        "EiB"
    };
    (r, unit)
}

/// printsize() (src/util.c:99) split into its two colour runs: the number is
/// UIC_NUM* and the space plus unit fall back to the base colour.
fn printsize_spans(bytes: u64, num: Style, base: Style) -> Vec<Span<'static>> {
    let (r, unit) = formatsize(bytes);
    vec![
        Span::styled(format!("{r:>5.1}"), num),
        Span::styled(format!(" {unit}"), base),
    ]
}

/// cropstr() (src/util.c:54): keeps both ends and puts "..." in the middle,
/// resulting in exactly `width` characters. The C version cuts bytes; we cut
/// characters so multi-byte names stay valid.
fn cropstr(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < width || width < 4 {
        return s.to_string();
    }
    let head = width / 2 - 3;
    let tail = width / 2;
    let mut out = String::with_capacity(s.len());
    out.extend(chars[..head].iter());
    out.push_str("...");
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// browse_draw_flag() (src/browser.c:120). Only the flags we can actually
/// know about are produced; `<`, `>`, `^`, `F` need scan options and `H`
/// needs hard link bookkeeping (see the README comparison table).
fn flag_char(n: &Node) -> char {
    if n.error {
        '!'
    } else if n.excluded {
        '<'
    } else if n.othfs {
        '>'
    } else if n.notreg {
        '@'
    } else if n.is_dir && n.children.is_empty() {
        'e'
    } else {
        ' '
    }
}

/// browse_draw_graph() (src/browser.c:145), default "hash" style.
fn graph_bar(size: u64, total: u64, width: usize) -> String {
    if total == 0 || size == 0 {
        return " ".repeat(width);
    }
    let mut max = total as i64;
    let mut num = size as i64;
    if max < width as i64 {
        max *= width as i64;
        num *= width as i64;
    } else if max > 1i64 << 56 {
        max <<= 5;
        num <<= 5;
    }
    let perblock = (max / width as i64).max(1);
    let mut bar = String::with_capacity(width);
    for _ in 0..width {
        let frac = ((num * 8) / perblock).clamp(0, 8);
        bar.push(if frac == 8 { '#' } else { ' ' });
        num -= perblock;
    }
    bar
}

/* ---------------------------- browse screen ----------------------------- */

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = frame_layout(f);
    let cols = f.area().width as usize;

    // The C version sets the window background with bkgd() before drawing.
    let full = f.area();
    f.buffer_mut().set_style(full, app.colors.default);

    draw_status(f, app, chunks[0], cols);
    draw_path(f, app, chunks[1], cols);
    app.view_height = chunks[2].height as usize;
    draw_list(f, app, chunks[2], cols);
    draw_footer(f, app, chunks[3]);

    // Both windows overlay the listing, like in the C version (delete.c draws
    // on top of the browser, the message window on top of that).
    if app.confirm.is_some() {
        draw_delete_confirm(f, app, &app.colors);
    }
    if let Some(msg) = app.message.clone() {
        draw_message(f, &msg, &app.colors);
    }
    if app.info {
        draw_info(f, app, &app.colors);
    }
    if let Some(page) = app.help_page {
        draw_help(f, page, app.help_scroll, &app.colors);
    }
}

/// delete_draw_confirm() (src/delete.c:46): a 6x60 window with the question and
/// three options, the highlighted one reversed.
fn draw_delete_confirm(f: &mut Frame, app: &App, colors: &Colors) {
    let Some(idx) = app.confirm else { return };
    let node = app.node(idx);
    let name = app.tree.name(idx);

    let inner = window(f, 6, 60, "Confirm delete", colors);
    win_line(
        f,
        inner,
        1,
        2,
        Line::from(Span::styled(
            format!(
                "Are you sure you want to delete \"{}\"{}",
                cropstr(name, 21),
                if node.is_dir { ' ' } else { '?' }
            ),
            colors.default,
        )),
    );
    if node.is_dir && !node.children.is_empty() {
        win_line(
            f,
            inner,
            2,
            18,
            Line::from(Span::styled("and all of its contents?", colors.default)),
        );
    }

    // (4,15) "yes", (4,24) "no", (4,31) "don't ask me again"
    for (i, (col, text)) in [(15u16, "yes"), (24, "no"), (31, "don't ask me again")]
        .into_iter()
        .enumerate()
    {
        let style = if app.delete_choice == i {
            colors.sel
        } else {
            colors.default
        };
        win_line(
            f,
            inner,
            4,
            col,
            Line::from(Span::styled(text, style)),
        );
    }
}

/// Top line (src/browser.c:292-301).
fn draw_status(f: &mut Frame, app: &App, area: Rect, cols: usize) {
    let c = &app.colors;
    let hint = "Use the arrow keys to navigate, press ";
    let left = format!("{NAME} {VERSION} ~ {hint}? for help");
    let right = if app.imported { "[imported]" } else { "" };
    // The C version puts the marker flush against the right edge of the line.
    let pad = cols.saturating_sub(left.chars().count() + right.chars().count());
    let spans = vec![
        Span::styled(format!("{NAME} {VERSION} ~ {hint}"), c.hd),
        Span::styled("?", c.key_hd),
        Span::styled(" for help", c.hd),
        Span::styled(" ".repeat(pad), c.hd),
        Span::styled(right, c.hd),
    ];
    // Styling the block as well keeps the header colour running to the right
    // edge even when the text does not reach it.
    f.render_widget(Paragraph::new(Line::from(spans)).style(c.hd), area);
}

/// Second line (src/browser.c:304-310): a '-' rule in UIC_DEFAULT with the
/// current directory (UIC_DIR) starting at column 4. The C version draws it
/// whenever a listing is shown: `dirlist_par` is the *open* directory, not its
/// parent, so it is set even when we are at the scan root.
fn draw_path(f: &mut Frame, app: &App, area: Rect, cols: usize) {
    let c = &app.colors;
    let path = cropstr(&app.cur_path(), cols.saturating_sub(8));
    let len = path.chars().count();
    let spans = vec![
        Span::styled("--- ", c.default),
        Span::styled(path, c.dir),
        Span::styled(" ", c.default),
        Span::styled("-".repeat(cols.saturating_sub(5 + len)), c.default),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The listing (src/browser.c:343-348 draws winrows-3 rows from row 2).
fn draw_list(f: &mut Frame, app: &mut App, area: Rect, cols: usize) {
    // The C version scales the graph against the largest entry of the listing
    // (dirlist_maxs / dirlist_maxa), not against the directory total - which is
    // why the biggest row always comes out completely full.
    let total = app
        .sorted
        .iter()
        .map(|&i| {
            let n = app.node(i);
            if app.show_as { n.asize } else { n.size }
        })
        .max()
        .unwrap_or(0);
    let width = bar_size(cols);
    let colors = app.colors;

    let mut items: Vec<ListItem> = Vec::with_capacity(app.row_count());
    if app.has_parent_row() {
        items.push(ListItem::new(parent_row_line(
            app.selected == 0,
            cols,
            width,
            &colors,
        )));
    }
    for (i, &idx) in app.sorted.iter().enumerate() {
        let row = i + usize::from(app.has_parent_row());
        items.push(ListItem::new(row_line(
            app.node(idx),
            app.tree.name(idx),
            total,
            width,
            cols,
            row == app.selected,
            app.show_as,
            &colors,
        )));
    }

    // No highlight style: the selected row carries its own UIC_*_SEL colours.
    let list = List::new(items);
    if app.row_count() == 0 {
        app.list_state.select(None);
    } else {
        app.list_state.select(Some(app.selected));
    }
    f.render_stateful_widget(list, area, &mut app.list_state);
}

/// The ".." row (the C version's `dirlist_parent`): browse_draw_item() skips
/// both the size and the graph for it, leaving just the name.
fn parent_row_line(selected: bool, cols: usize, width: usize, c: &Colors) -> Line<'static> {
    let base = if selected { c.sel } else { c.default };
    let dir = if selected { c.dir_sel } else { c.dir };

    // flag(2) + size(10) + graph(width+3) + prefix(1); the C version shows the
    // parent entry as "/.." because it carries FF_DIR.
    let pad = 2 + 10 + width + 3 + 1;
    let mut spans = vec![
        Span::styled(" ".repeat(pad), base),
        Span::styled("/..", dir),
    ];
    let used = pad + 3;
    if used < cols {
        spans.push(Span::styled(" ".repeat(cols - used), base));
    }
    Line::from(spans)
}

/// browse_draw_item() (src/browser.c:250-281).
#[allow(clippy::too_many_arguments)]
fn row_line(
    n: &Node,
    name: &str,
    total: u64,
    width: usize,
    cols: usize,
    selected: bool,
    show_as: bool,
    c: &Colors,
) -> Line<'static> {
    let base = if selected { c.sel } else { c.default };
    let num = if selected { c.num_sel } else { c.num };
    let flag = if selected { c.flag_sel } else { c.flag };
    let bar = if selected { c.graph_sel } else { c.graph };
    let dir = if selected { c.dir_sel } else { c.dir };

    let size = if show_as { n.asize } else { n.size };
    // flag(2) + size(10) + graph(width+3) + prefix(1)
    let used = 2 + 10 + (width + 3) + 1;
    let room = cols.saturating_sub(used);
    let name_style = if n.is_dir { dir } else { base };

    let mut spans = vec![
        Span::styled(flag_char(n).to_string(), flag),
        Span::styled(" ", base),
    ];
    spans.extend(printsize_spans(size, num, base));
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled("[", base));
    spans.push(Span::styled(graph_bar(size, total, width), bar));
    spans.push(Span::styled("]", base));
    spans.push(Span::styled(" ", base));
    spans.push(Span::styled(
        format!("{}{}", if n.is_dir { "/" } else { " " }, cropstr(name, room)),
        name_style,
    ));

    // The C version clears the whole row first (mvhline), so pad to the full
    // width - this is what gives the selected row its full-length background.
    let used_cols: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if used_cols < cols {
        spans.push(Span::styled(" ".repeat(cols - used_cols), base));
    }

    Line::from(spans)
}

/// Bottom line (src/browser.c:312-333).
fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let c = &app.colors;

    // A transient message temporarily replaces the totals; the delete
    // confirmation and the "Message" window are drawn on top instead.
    if let Some((msg, is_err)) = &app.notice {
        let style = if *is_err {
            c.hd.fg(ratatui::style::Color::Red)
        } else {
            c.hd
        };
        f.render_widget(Paragraph::new(msg.as_str()).style(style), area);
        return;
    }

    if app.sorted.is_empty() {
        f.render_widget(
            Paragraph::new(" No items to display.").style(c.hd),
            area,
        );
        return;
    }

    let node = app.cur_node();
    // "Total disk usage: " is bold while the disk usage is the active metric.
    let label = if app.show_as {
        c.hd
    } else {
        c.hd.add_modifier(Modifier::BOLD)
    };
    let mut spans = vec![
        Span::styled(if app.show_as { " " } else { "*" }, label),
        Span::styled("Total disk usage: ", label),
    ];
    spans.extend(printsize_spans(node.size, c.num_hd, c.hd));
    spans.push(Span::styled("  ", c.hd));
    spans.push(Span::styled(if app.show_as { "*" } else { " " }, c.hd));
    spans.push(Span::styled("Apparent size: ", c.hd));
    spans.extend(printsize_spans(node.asize, c.num_hd, c.hd));
    spans.push(Span::styled("   Items: ", c.hd));
    spans.push(Span::styled(node.items.to_string(), c.num_hd));

    // The C version clears the whole line with UIC_HD first (mvhline), so the
    // header colour runs to the right edge; a plain span list would stop right
    // after the last number.
    f.render_widget(Paragraph::new(Line::from(spans)).style(c.hd), area);
}

/* --------------------------- centered windows --------------------------- */

/// nccreate() (src/util.c:200): clears a centered rectangle of the given size,
/// draws a border around it and writes the title at column 4 of that border.
/// Returns the area inside the border.
fn window(f: &mut Frame, height: u16, width: u16, title: &str, colors: &Colors) -> Rect {
    let area = f.area();
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 2;
    let win = Rect::new(x, y, w, h);

    f.render_widget(Clear, win);
    let block = Block::bordered()
        .title(Line::from(format!("   {title}")))
        .style(colors.default)
        .title_style(colors.box_title);
    let inner = block.inner(win);
    f.render_widget(block, win);
    inner
}

/// ncprint() (src/util.c:234): writes one line in a window. Row and column are
/// window-relative and include the border: (0,0) is the top-left corner of the
/// frame, (2,2) is the second line inside it.
fn win_line(f: &mut Frame, inner: Rect, row: u16, col: u16, line: Line<'static>) {
    let ox = inner.x.saturating_sub(1);
    let oy = inner.y.saturating_sub(1);
    // Only as wide as the text, so a label and a value can share one row.
    let text: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    let width = (text as u16).min((inner.width + 2).saturating_sub(col));
    if width == 0 || row > inner.height + 1 {
        return;
    }
    let area = Rect {
        x: ox + col,
        y: oy + row,
        width,
        height: 1,
    };
    f.render_widget(Paragraph::new(line), area);
}

/* ----------------------------- scan screen ------------------------------ */

/// draw_progress() (src/dir_common.c:111): a centered 10-line window, the
/// "Scanning..."/"Loading..." title, the running totals and the abort hint.
pub fn draw_scan(f: &mut Frame, scanner: &Scanner, colors: &Colors) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let p = scanner.progress();
    let area = f.area();
    f.buffer_mut().set_style(area, colors.default);

    let title = "Scanning...";
    let width = area.width.saturating_sub(5);
    let inner = window(f, 10, width, title, colors);

    // (2,2) "Total items: " + %-9d, then (2,24) "size: " + printsize.
    let mut total = vec![
        Span::styled("Total items: ", colors.default),
        Span::styled(format!("{:<9}", p.entries), colors.num),
    ];
    if p.bytes > 0 {
        total.push(Span::styled("size: ", colors.default));
        total.extend(printsize_spans(p.bytes, colors.num, colors.default));
    }
    win_line(f, inner, 2, 2, Line::from(total));

    // (3,2) "Current item: <path>"
    let room = (width as usize).saturating_sub(18);
    win_line(
        f,
        inner,
        3,
        2,
        Line::from(Span::styled(
            format!("Current item: {}", cropstr(&p.current, room)),
            colors.default,
        )),
    );

    // (5,2) "Warning:" + (6,3) hint, when directories could not be read.
    if p.errors > 0 {
        win_line(
            f,
            inner,
            5,
            2,
            Line::from(vec![
                Span::styled("Warning:", colors.default.add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("error scanning {} directories", p.errors),
                    colors.default,
                ),
            ]),
        );
        win_line(
            f,
            inner,
            6,
            3,
            Line::from(Span::styled(
                "some directory sizes may not be correct",
                colors.default,
            )),
        );
    }

    // (8,3) the animated "Scanning..." and (8,width-18) "Press q to abort".
    static ANPOS: AtomicUsize = AtomicUsize::new(0);
    let text = title.as_bytes();
    let pos = ANPOS.fetch_add(1, Ordering::Relaxed) % (text.len() * 2);
    let mut ani = vec![b' '; text.len()];
    if pos < text.len() {
        ani[..=pos].copy_from_slice(&text[..=pos]);
    } else {
        let back = pos - text.len();
        for i in back..text.len() {
            ani[i] = text[i];
        }
    }
    win_line(
        f,
        inner,
        8,
        3,
        Line::from(Span::styled(
            String::from_utf8_lossy(&ani).into_owned(),
            colors.default,
        )),
    );
    win_line(
        f,
        inner,
        8,
        (width as u16).saturating_sub(18),
        Line::from(vec![
            Span::styled("Press ", colors.default),
            Span::styled("q", colors.key),
            Span::styled(" to abort", colors.default),
        ]),
    );
}

/// The C version's centered message window (src/browser.c:350-355).
pub fn draw_message(f: &mut Frame, text: &str, colors: &Colors) {
    let inner = window(f, 6, 60, "Message", colors);
    win_line(
        f,
        inner,
        2,
        2,
        Line::from(Span::styled(text.to_string(), colors.default)),
    );
    win_line(
        f,
        inner,
        4,
        33,
        Line::from(Span::styled(
            "Press any key to continue",
            colors.default,
        )),
    );
}

/* ------------------------------- shared --------------------------------- */

/* --------------------------- item info window --------------------------- */

/// fullsize() (src/util.c:109): decimal number with thousand separators.
fn fullsize(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.char_indices() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    out
}

/// browse_draw_info() (src/browser.c:39-117): the 11x60 "Item info" window.
/// Values are written first and labels afterwards so that the two shares of a
/// row do not overwrite each other.
pub fn draw_info(f: &mut Frame, app: &App, colors: &Colors) {
    let Some(idx) = app.row_node(app.selected) else {
        return;
    };
    let node = app.node(idx);
    let name = app.tree.name(idx);
    let bold = colors.default.add_modifier(Modifier::BOLD);

    let inner = window(f, 11, 60, "Item info", colors);

    win_line(
        f,
        inner,
        2,
        9,
        Line::from(Span::styled(cropstr(name, 49), colors.default)),
    );
    win_line(
        f,
        inner,
        2,
        3,
        Line::from(Span::styled("Name:", bold)),
    );

    let path = app.tree.path_of(app.cur);
    win_line(
        f,
        inner,
        3,
        9,
        Line::from(Span::styled(cropstr(&path, 49), colors.default)),
    );
    win_line(
        f,
        inner,
        3,
        3,
        Line::from(Span::styled("Path:", bold)),
    );

    let kind = if node.is_dir {
        "Directory"
    } else if node.notreg {
        "Other"
    } else {
        "File"
    };
    win_line(
        f,
        inner,
        4,
        9,
        Line::from(Span::styled(kind, colors.default)),
    );
    win_line(
        f,
        inner,
        4,
        3,
        Line::from(Span::styled("Type:", bold)),
    );

    for (row, bytes, label) in [
        (6u16, node.size, "   Disk usage:"),
        (7, node.asize, "Apparent size:"),
    ] {
        let mut spans = printsize_spans(bytes, colors.num, colors.default);
        spans.push(Span::styled(" (", colors.default));
        spans.push(Span::styled(fullsize(bytes), colors.num));
        spans.push(Span::styled(" B)", colors.default));
        win_line(f, inner, row, 18, Line::from(spans));
        win_line(f, inner, row, 3, Line::from(Span::styled(label, bold)));
    }

    win_line(
        f,
        inner,
        9,
        31,
        Line::from(vec![
            Span::styled("Press ", colors.default),
            Span::styled("i", colors.key),
            Span::styled(" to hide this window", colors.default),
        ]),
    );
}

/* ------------------------------ help window ----------------------------- */

/// Page 1 of the help window (src/help.c:36-57).
const HELP_KEYS: [(&str, &str); 19] = [
    ("up, k", "Move cursor up"),
    ("down, j", "Move cursor down"),
    ("right/enter", "Open selected directory"),
    ("left, <, h", "Open parent directory"),
    ("n", "Sort by name (ascending/descending)"),
    ("s", "Sort by size (ascending/descending)"),
    ("C", "Sort by items (ascending/descending)"),
    ("M", "Sort by mtime (-e flag)"),
    ("d", "Delete selected file or directory"),
    ("t", "Toggle dirs before files when sorting"),
    ("g", "Show percentage and/or graph"),
    ("a", "Toggle between apparent size and disk usage"),
    ("c", "Toggle display of child item counts"),
    ("m", "Toggle display of latest mtime (-e flag)"),
    ("e", "Show/hide hidden or excluded files"),
    ("i", "Show information about selected item"),
    ("r", "Recalculate the current directory"),
    ("b", "Spawn shell in current directory"),
    ("q", "Quit ncdu"),
];

/// Page 2 of the help window (src/help.c:60-70).
const HELP_FLAGS: [(char, &str); 9] = [
    ('!', "An error occurred while reading this directory"),
    ('.', "An error occurred while reading a subdirectory"),
    ('<', "File or directory is excluded from the statistics"),
    ('e', "Empty directory"),
    ('>', "Directory was on another filesystem"),
    ('@', "This is not a file nor a dir (symlink, socket, ...)"),
    ('^', "Excluded Linux pseudo-filesystem"),
    ('H', "Same file was already counted (hard link)"),
    ('F', "Excluded firmlink"),
];

/// Table sizes, so the key handler in app.rs can clamp its scrolling.
pub const HELP_KEYS_LEN: usize = HELP_KEYS.len();
pub const HELP_FLAGS_LEN: usize = HELP_FLAGS.len();

/// How many entries fit on a help page (src/help.c:88 uses 10 for the keys
/// page and 7 for the flags page).
fn help_visible(page: usize) -> usize {
    if page == 1 {
        10
    } else {
        7
    }
}

/// The help window (src/help.c:78-161): 15x60 with three tabs, the list of the
/// active page and a "Press q to close" hint.
pub fn draw_help(f: &mut Frame, page: usize, scroll: usize, colors: &Colors) {
    let inner = window(f, 15, 60, "ncdu help", colors);

    // Tabs on the top border: "1:Keys", "2:Format", "3:About" (help.c:85-87).
    for (i, (col, label)) in [(30u16, "Keys"), (39, "Format"), (50, "About")]
        .into_iter()
        .enumerate()
    {
        let sel = page == i + 1;
        let num = if sel { colors.key_hd } else { colors.key };
        let rest = if sel { colors.hd } else { colors.default };
        win_line(
            f,
            inner,
            0,
            col,
            Line::from(vec![
                Span::styled((i + 1).to_string(), num),
                Span::styled(":", rest),
                Span::styled(label, rest),
            ]),
        );
    }

    match page {
        1 => {
            let visible = help_visible(1);
            for (i, (key, desc)) in HELP_KEYS.iter().skip(scroll).take(visible).enumerate() {
                win_line(
                    f,
                    inner,
                    2 + i as u16,
                    1,
                    Line::from(vec![
                        Span::styled(format!("{key:>13}"), colors.key),
                        Span::styled(format!("  {desc}"), colors.default),
                    ]),
                );
            }
            if scroll + visible < HELP_KEYS.len() {
                win_line(
                    f,
                    inner,
                    12,
                    25,
                    Line::from(Span::styled("-- more --", colors.default)),
                );
            }
        }
        2 => {
            win_line(
                f,
                inner,
                2,
                3,
                Line::from(Span::styled(
                    "X  [size] [graph] [file or directory]",
                    colors.default.add_modifier(Modifier::BOLD),
                )),
            );
            win_line(
                f,
                inner,
                3,
                4,
                Line::from(Span::styled(
                    "The X is only present in the following cases:",
                    colors.default,
                )),
            );
            let visible = help_visible(2);
            for (i, (flag, desc)) in HELP_FLAGS.iter().skip(scroll).take(visible).enumerate() {
                win_line(
                    f,
                    inner,
                    4 + i as u16,
                    4,
                    Line::from(vec![
                        Span::styled(flag.to_string(), colors.flag),
                        Span::styled(format!("  {desc}"), colors.default),
                    ]),
                );
            }
            if scroll + visible < HELP_FLAGS.len() {
                win_line(
                    f,
                    inner,
                    12,
                    25,
                    Line::from(Span::styled("-- more --", colors.default)),
                );
            }
        }
        _ => {
            for (i, text) in [
                "ncdurs - an ncdu-compatible disk usage browser",
                "",
                "Written from scratch in Rust, mirroring the",
                "behaviour and the interface of ncdu 1.22.",
                "",
                "https://dev.yorhel.nl/ncdu/",
            ]
            .iter()
            .enumerate()
            {
                win_line(
                    f,
                    inner,
                    3 + i as u16,
                    6,
                    Line::from(Span::styled(text.to_string(), colors.default)),
                );
            }
        }
    }

    win_line(
        f,
        inner,
        13,
        42,
        Line::from(vec![
            Span::styled("Press ", colors.default),
            Span::styled("q", colors.key),
            Span::styled(" to close", colors.default),
        ]),
    );
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::scan::Tree;
    use crate::theme::Theme;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// A root with two files: the directory total is larger than any single
    /// entry, which is what distinguishes the graph scale.
    fn demo_app() -> App {
        let mut tree = Tree::new();
        let root = tree.push_node(None, "/demo", true);
        let big = tree.push_node(Some(root), "big", false);
        let small = tree.push_node(Some(root), "small", false);
        tree.nodes[big].size = 8192;
        tree.nodes[big].asize = 8192;
        tree.nodes[small].size = 1024;
        tree.nodes[small].asize = 1024;
        // The directory's own block counts towards its total, like a scan.
        tree.nodes[root].size = 4096 + 8192 + 1024;
        tree.nodes[root].asize = 4096 + 8192 + 1024;
        tree.nodes[root].items = 2;
        App::new(tree, false, false, Theme::Dark)
    }

    fn row_text_at(terminal: &Terminal<TestBackend>, row: u16, cols: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..cols)
            .map(|x| buffer.cell((x, row)).unwrap().symbol().to_string())
            .collect()
    }

    /// The C version clears the header and the footer with mvhline before
    /// writing any text, so their background reaches the right edge. Writing
    /// only the spans would leave the tail of the line uncoloured.
    #[test]
    fn header_and_footer_fill_their_lines() {
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let mut app = demo_app();
        terminal.draw(|f| draw(f, &mut app)).unwrap();

        let background = app.colors.hd.bg;
        let buffer = terminal.backend().buffer();
        for x in 0..80 {
            for y in [0, 9] {
                let cell = buffer.cell((x, y)).unwrap();
                assert_eq!(
                    cell.style().bg,
                    background,
                    "row {y} column {x} should carry the header background"
                );
            }
        }
    }

    /// The graph is scaled against the largest entry of the listing, so that
    /// row comes out full. Scaling against the directory total (which also
    /// counts the directory block and the other entry) would leave it short.
    #[test]
    fn the_biggest_entry_fills_the_graph() {
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let mut app = demo_app();
        terminal.draw(|f| draw(f, &mut app)).unwrap();

        // Rows start at 2; "big" sorts first (size descending).
        let line = row_text_at(&terminal, 2, 80);
        let start = line.find('[').expect("a graph column") + 1;
        let end = line[start..].find(']').expect("a closing bracket") + start;
        let bar = &line[start..end];
        assert!(
            !bar.is_empty() && bar.chars().all(|c| c == '#'),
            "the largest entry should fill the bar completely: {line:?}"
        );
    }
}
