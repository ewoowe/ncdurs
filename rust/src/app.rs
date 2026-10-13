/* ncdurs - browsing state and key handling
 *
 * The counterpart of the C version's src/dirlist.c: it keeps track of the
 * currently opened directory, the sorted view of its children, the cursor and
 * the pending user actions (delete / refresh). Blocking actions are not run
 * here: the main loop executes them so it can redraw a hint first.
 *
 * Key bindings follow the C version where they exist: n / s / C sort by name /
 * size / items (pressing the active one again reverses it), d deletes (with a
 * y/n confirmation), r re-scans the open directory.
 */

use std::cmp::Ordering;
use std::fs;
use std::path::Path;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::scan::{Node, ScanOptions, Scanner, Tree};
use crate::theme::{Colors, Theme};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortCol {
    Size,
    Name,
    Items,
}


/// A blocking action requested by the user; the main loop runs it.
pub enum Job {
    Delete(usize),
    Refresh,
    /// Suspend the TUI and run the user's shell in the open directory.
    Shell,
}

pub struct App {
    pub tree: Tree,
    /// Index of the directory that is currently opened.
    pub cur: usize,
    /// Children of `cur`, sorted according to the current settings.
    pub sorted: Vec<usize>,
    /// Cursor position, as an index into `sorted`.
    pub selected: usize,
    pub sort_col: SortCol,
    pub sort_desc: bool,
    /// Show apparent size instead of disk usage.
    pub show_as: bool,
    pub quit: bool,
    /// The tree was loaded from an export file: it does not describe the
    /// filesystem, so delete, refresh and the shell are disabled (same
    /// behaviour as the C version).
    pub imported: bool,
    /// Resolved colours of the active theme (`--color`).
    pub colors: Colors,
    /// The options the tree was scanned with; `r` (refresh) reuses them so that
    /// exclusions and `-x` stay in effect.
    pub scan_options: ScanOptions,
    /// Height of the list area, updated while drawing.
    pub view_height: usize,
    pub list_state: ListState,
    /// Entry waiting for delete confirmation (y / n).
    pub confirm: Option<usize>,
    /// One-line message for the footer, cleared on the next key press.
    pub notice: Option<(String, bool)>,
    /// A centered "Message" window (src/browser.c:350-355), dismissed by any
    /// key. Used for "feature disabled" hints, like the C version.
    pub message: Option<String>,
    /// Ask before deleting (src/delete.c); the "don't ask me again" option
    /// clears this for the rest of the session.
    pub delete_confirm: bool,
    /// Highlighted option of the delete confirmation: 0 yes, 1 no,
    /// 2 don't ask me again.
    pub delete_choice: usize,
    /// Open help window: Some(page), page 1..=3 (src/help.c).
    pub help_page: Option<usize>,
    pub help_scroll: usize,
    /// Show the "Item info" window (src/browser.c:39) for the selected entry.
    pub info: bool,
    /// Action for the main loop to execute.
    pub job: Option<Job>,
}

impl App {
    pub fn new(tree: Tree, show_as: bool, imported: bool, theme: Theme) -> App {
        let mut app = App {
            tree,
            cur: 0,
            sorted: Vec::new(),
            selected: 0,
            sort_col: SortCol::Size,
            sort_desc: true,
            show_as,
            quit: false,
            imported,
            colors: theme.colors(),
            scan_options: ScanOptions::default(),
            view_height: 10,
            list_state: ListState::default(),
            confirm: None,
            notice: None,
            message: None,
            delete_confirm: true,
            delete_choice: 0,
            help_page: None,
            help_scroll: 0,
            info: false,
            job: None,
        };
        app.resort();
        app
    }

    pub fn cur_node(&self) -> &Node {
        &self.tree.nodes[self.cur]
    }

    pub fn node(&self, idx: usize) -> &Node {
        &self.tree.nodes[idx]
    }

    /// Absolute path of the currently opened directory, using the separator of
    /// the current platform.
    pub fn cur_path(&self) -> String {
        self.tree.path_of(self.cur)
    }

    /// Re-sorts the children of the current directory.
    ///
    /// The comparison chain follows the C version (dirlist.c:89-104): the
    /// active column first, then the remaining keys in a fixed order, using
    /// the natural-order name comparison from deps/strnatcmp.c.
    pub fn resort(&mut self) {
        let mut view = self.tree.nodes[self.cur].children.clone();
        let tree = &self.tree;
        let col = self.sort_col;
        let desc = self.sort_desc;
        let show_as = self.show_as;

        view.sort_by(|&a, &b| {
            let ord = cmp_entries(
                &tree.nodes[a],
                &tree.nodes[b],
                tree.name(a),
                tree.name(b),
                col,
                show_as,
            );
            if desc { ord.reverse() } else { ord }
        });

        self.sorted = view;
        let rows = self.row_count();
        if self.selected >= rows {
            self.selected = rows.saturating_sub(1);
        }
    }

    /// The C version shows a ".." entry (its `dirlist_parent`) as the first row
    /// once we are inside a subdirectory.
    pub fn has_parent_row(&self) -> bool {
        self.cur != 0
    }

    /// Number of selectable rows (the listing plus the optional ".." entry).
    pub fn row_count(&self) -> usize {
        self.sorted.len() + usize::from(self.has_parent_row())
    }

    /// Node behind a row; None is the ".." entry.
    pub fn row_node(&self, row: usize) -> Option<usize> {
        if self.has_parent_row() {
            if row == 0 {
                return None;
            }
            self.sorted.get(row - 1).copied()
        } else {
            self.sorted.get(row).copied()
        }
    }

    fn move_by(&mut self, delta: i64) {
        let rows = self.row_count();
        if rows == 0 {
            return;
        }
        let max = rows as i64 - 1;
        self.selected = (self.selected as i64 + delta).clamp(0, max) as usize;
    }

    fn enter(&mut self) {
        let Some(idx) = self.row_node(self.selected) else {
            // The ".." entry behaves like "open parent directory".
            self.leave();
            return;
        };
        let node = &self.tree.nodes[idx];
        if !node.is_dir || node.error {
            return;
        }
        self.cur = idx;
        self.selected = 0;
        self.resort();
    }

    fn leave(&mut self) {
        let Some(parent) = self.cur_node().parent else { return };
        let leaving = self.cur;
        self.cur = parent;
        self.resort();
        // Keep the cursor on the directory we just left. `selected` is a row
        // number, and the parent may have a ".." row of its own - skipping it
        // is what used to shift the cursor one line up.
        self.selected = self
            .sorted
            .iter()
            .position(|&i| i == leaving)
            .map(|i| i + usize::from(self.has_parent_row()))
            .unwrap_or(0);
    }

    /// Selecting the active column again reverses the sort order.
    fn set_sort(&mut self, col: SortCol) {
        // Keep the cursor on the same entry, like the C version does.
        let keep = self.row_node(self.selected);
        if self.sort_col == col {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_col = col;
            // Names read best ascending, everything else starts with the biggest.
            self.sort_desc = col != SortCol::Name;
        }
        self.resort();
        if let Some(keep) = keep {
            if let Some(pos) = self.sorted.iter().position(|&i| i == keep) {
                self.selected = pos + usize::from(self.has_parent_row());
            }
        }
    }

    fn request_delete(&mut self) {
        let Some(idx) = self.row_node(self.selected) else {
            return; // the ".." entry cannot be deleted
        };
        if self.tree.nodes[idx].parent.is_none() {
            self.notice = Some(("cannot delete the root directory".to_string(), true));
            return;
        }
        if self.delete_confirm {
            self.delete_choice = 0;
            self.confirm = Some(idx);
        } else {
            self.job = Some(Job::Delete(idx));
        }
    }

    /// Deletes the selected entry: filesystem first, tree second, so the view
    /// is never out of sync with the disk.
    pub fn run_delete(&mut self, idx: usize) {
        let path = self.tree.path_of(idx);
        let is_dir = self.tree.nodes[idx].is_dir;
        let result = if is_dir {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match result {
            Ok(()) => {
                self.tree.remove_subtree(idx);
                self.resort();
                self.notice = Some((format!("deleted {path}"), false));
            }
            Err(e) => {
                self.notice = Some((format!("delete failed: {path}: {e}"), true));
            }
        }
    }

    /// Re-scans the open directory and grafts the result into the tree.
    pub fn run_refresh(&mut self) {
        let path = self.cur_path();
        let mut scanner = match Scanner::new(Path::new(&path), self.scan_options.clone()) {
            Ok(s) => s,
            Err(e) => {
                self.notice = Some((format!("refresh failed: {path}: {e}"), true));
                return;
            }
        };
        while !scanner.step(4096) {}
        if let Some(err) = scanner.failure() {
            let msg = err.to_string();
            scanner.abort();
            self.notice = Some((format!("refresh failed: {msg}"), true));
            return;
        }
        let (fresh, _) = scanner.into_result();
        self.tree.graft(self.cur, fresh);
        self.resort();
        self.notice = Some((format!("refreshed {path}"), false));
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        // The help window is modal: 1/2/3 switch pages, the arrows and PgUp /
        // PgDn scroll, anything else closes it (src/help.c:166-203).
        if let Some(page) = self.help_page {
            let len = match page {
                1 => crate::ui::HELP_KEYS_LEN,
                2 => crate::ui::HELP_FLAGS_LEN,
                _ => 0,
            };
            let visible = if page == 1 { 10 } else { 7 };
            let max_scroll = len.saturating_sub(visible);
            match key.code {
                KeyCode::Char('1') => {
                    self.help_page = Some(1);
                    self.help_scroll = 0;
                }
                KeyCode::Char('2') => {
                    self.help_page = Some(2);
                    self.help_scroll = 0;
                }
                KeyCode::Char('3') => {
                    self.help_page = Some(3);
                    self.help_scroll = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Char(' ') => {
                    self.help_scroll = (self.help_scroll + 1).min(max_scroll);
                }
                KeyCode::PageUp | KeyCode::Left | KeyCode::Char('h') => {
                    self.help_scroll = self.help_scroll.saturating_sub(visible);
                }
                KeyCode::PageDown | KeyCode::Right | KeyCode::Char('l') => {
                    self.help_scroll = (self.help_scroll + visible).min(max_scroll);
                }
                _ => self.help_page = None,
            }
            return;
        }
        // A "Message" window swallows any key (src/browser.c:371-375).
        if self.message.is_some() {
            self.message = None;
            return;
        }
        // The delete confirmation takes precedence: y confirms, anything else
        // (including Esc / q) cancels.
        if let Some(idx) = self.confirm {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.confirm = None;
                    self.job = Some(Job::Delete(idx));
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm = None;
                    self.notice = Some(("delete cancelled".to_string(), false));
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    self.delete_choice = self.delete_choice.saturating_sub(1);
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                    self.delete_choice = (self.delete_choice + 1).min(2);
                }
                KeyCode::Enter => {
                    self.confirm = None;
                    match self.delete_choice {
                        0 => self.job = Some(Job::Delete(idx)),
                        1 => self.notice = Some(("delete cancelled".to_string(), false)),
                        _ => {
                            self.delete_confirm = false;
                            self.notice = Some((
                                "deleting without confirmation from now on".to_string(),
                                false,
                            ));
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        self.notice = None;

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('?') => {
                self.help_page = Some(1);
                self.help_scroll = 0;
            }
            KeyCode::Char('i') => self.info = !self.info,
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-(self.view_height.max(1) as i64)),
            KeyCode::PageDown => self.move_by(self.view_height.max(1) as i64),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = self.row_count().saturating_sub(1)
            }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l') => self.enter(),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('u') => self.leave(),
            KeyCode::Char('n') => self.set_sort(SortCol::Name),
            KeyCode::Char('s') => self.set_sort(SortCol::Size),
            KeyCode::Char('C') => self.set_sort(SortCol::Items),
            KeyCode::Char('a') => {
                self.show_as = !self.show_as;
                self.resort();
            }
            KeyCode::Char('d') => {
                if self.imported {
                    self.message = Some("Deletion feature disabled.".to_string());
                } else {
                    self.request_delete();
                }
            }
            KeyCode::Char('r') => {
                if self.imported {
                    self.message = Some("Directory refresh feature disabled.".to_string());
                } else {
                    self.job = Some(Job::Refresh);
                }
            }
            KeyCode::Char('b') => {
                if self.imported {
                    self.message = Some("Shell feature disabled.".to_string());
                } else {
                    self.job = Some(Job::Shell);
                }
            }
            _ => {}
        }
    }
}

/* --------------- sorting, mirroring src/dirlist.c + strnatcmp ----------- */

/// dirlist_cmp() (src/dirlist.c:89-104): the active column first, then the
/// remaining keys in a fixed order.
///
///   SIZE:  size  -> asize -> name -> items
///   NAME:  name  -> size  -> asize -> items
///   ITEMS: items -> size  -> asize -> name
///
/// `show_as` selects the SIZE or the ASIZE column, exactly like the C version
/// where the `a` key switches both the display and the sort column.
fn cmp_entries(a: &Node, b: &Node, an: &str, bn: &str, col: SortCol, show_as: bool) -> Ordering {
    let (size, asize) = if show_as {
        (a.asize.cmp(&b.asize), a.size.cmp(&b.size))
    } else {
        (a.size.cmp(&b.size), a.asize.cmp(&b.asize))
    };
    let items = a.items.cmp(&b.items);
    let name = natsort(an, bn);
    match col {
        SortCol::Size => size.then(asize).then(name).then(items),
        SortCol::Name => name.then(size).then(asize).then(items),
        SortCol::Items => items.then(size).then(asize).then(name),
    }
}

/// strnatcmp() from deps/strnatcmp.c: runs of digits compare by value, so
/// "file2" sorts before "file10". Comparison is byte-wise and case-sensitive,
/// like the C version.
fn natsort(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut ai, mut bi) = (0, 0);
    loop {
        let mut ca = byte_at(a, ai);
        let mut cb = byte_at(b, bi);
        // The C version skips spaces before a run of digits.
        while is_space(ca) {
            ai += 1;
            ca = byte_at(a, ai);
        }
        while is_space(cb) {
            bi += 1;
            cb = byte_at(b, bi);
        }
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let r = if ca == b'0' || cb == b'0' {
                compare_left(&a[ai..], &b[bi..])
            } else {
                compare_right(&a[ai..], &b[bi..])
            };
            if r != Ordering::Equal {
                return r;
            }
        }
        if ca == 0 && cb == 0 {
            return Ordering::Equal;
        }
        if ca != cb {
            return ca.cmp(&cb);
        }
        ai += 1;
        bi += 1;
    }
}

fn byte_at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn is_space(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\r' || c == b'\n'
}

/// compare_left() from deps/strnatcmp.c: numbers with a leading zero are
/// compared as text, digit by digit.
fn compare_left(a: &[u8], b: &[u8]) -> Ordering {
    let mut i = 0;
    loop {
        let (ca, cb) = (byte_at(a, i), byte_at(b, i));
        if !ca.is_ascii_digit() && !cb.is_ascii_digit() {
            return Ordering::Equal;
        }
        if !ca.is_ascii_digit() {
            return Ordering::Less;
        }
        if !cb.is_ascii_digit() {
            return Ordering::Greater;
        }
        if ca != cb {
            return ca.cmp(&cb);
        }
        i += 1;
    }
}

/// compare_right() from deps/strnatcmp.c: the longest run of digits wins,
/// otherwise the greatest value (remembered in `bias`).
fn compare_right(a: &[u8], b: &[u8]) -> Ordering {
    let mut bias = Ordering::Equal;
    let mut i = 0;
    loop {
        let (ca, cb) = (byte_at(a, i), byte_at(b, i));
        if !ca.is_ascii_digit() && !cb.is_ascii_digit() {
            return bias;
        }
        if !ca.is_ascii_digit() {
            return Ordering::Less;
        }
        if !cb.is_ascii_digit() {
            return Ordering::Greater;
        }
        if ca < cb {
            if bias == Ordering::Equal {
                bias = Ordering::Less;
            }
        } else if ca > cb {
            if bias == Ordering::Equal {
                bias = Ordering::Greater;
            }
        } else if ca == 0 && cb == 0 {
            return bias;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    /// root/a/b/f1 - two levels deep, so that "a" has a ".." row of its own.
    fn app_with_nested_dir() -> App {
        let mut tree = Tree::new();
        let root = tree.push_node(None, "/demo", true);
        let a = tree.push_node(Some(root), "a", true);
        let b = tree.push_node(Some(a), "b", true);
        let _f1 = tree.push_node(Some(b), "f1", false);
        App::new(tree, false, false, Theme::Off)
    }

    fn row_of(app: &App, name: &str) -> usize {
        (0..app.row_count())
            .find(|&r| app.row_node(r).is_some_and(|i| app.tree.name(i) == name))
            .unwrap_or_else(|| panic!("{name} is not listed"))
    }

    #[test]
    fn leaving_a_directory_puts_the_cursor_back_on_it() {
        let mut app = app_with_nested_dir();
        app.selected = row_of(&app, "a");
        app.enter();
        assert_eq!(app.tree.name(app.cur), "a");

        app.selected = row_of(&app, "b");
        app.enter();
        assert_eq!(app.tree.name(app.cur), "b");

        // Back in "a": it has a ".." row, so the cursor has to be offset by it.
        app.leave();
        assert_eq!(app.tree.name(app.cur), "a");
        assert_eq!(app.tree.name(app.row_node(app.selected).unwrap()), "b");
    }

    #[test]
    fn sorting_keeps_the_cursor_on_the_same_entry() {
        let mut app = app_with_nested_dir();
        app.selected = row_of(&app, "a");
        app.enter();
        app.selected = row_of(&app, "b");

        app.set_sort(SortCol::Name);
        assert_eq!(app.tree.name(app.row_node(app.selected).unwrap()), "b");
        app.set_sort(SortCol::Size);
        assert_eq!(app.tree.name(app.row_node(app.selected).unwrap()), "b");
    }
}
