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

use std::fs;
use std::path::Path;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::scan::{Node, Scanner, Tree};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortCol {
    Size,
    Name,
    Items,
}

impl SortCol {
    pub fn label(self) -> &'static str {
        match self {
            SortCol::Size => "size",
            SortCol::Name => "name",
            SortCol::Items => "items",
        }
    }


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
    /// Height of the list area, updated while drawing.
    pub view_height: usize,
    pub list_state: ListState,
    /// Entry waiting for delete confirmation (y / n).
    pub confirm: Option<usize>,
    /// One-line message for the footer, cleared on the next key press.
    pub notice: Option<(String, bool)>,
    /// Action for the main loop to execute.
    pub job: Option<Job>,
}

impl App {
    pub fn new(tree: Tree, show_as: bool) -> App {
        let mut app = App {
            tree,
            cur: 0,
            sorted: Vec::new(),
            selected: 0,
            sort_col: SortCol::Size,
            sort_desc: true,
            show_as,
            quit: false,
            view_height: 10,
            list_state: ListState::default(),
            confirm: None,
            notice: None,
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

    /// Size of an item according to the active metric.
    pub fn metric(&self, n: &Node) -> u64 {
        if self.show_as { n.asize } else { n.size }
    }

    /// Total size of the currently opened directory.
    pub fn cur_total(&self) -> u64 {
        self.metric(self.cur_node())
    }

    pub fn selected_node(&self) -> Option<&Node> {
        self.sorted.get(self.selected).map(|&i| self.node(i))
    }

    /// Absolute path of the currently opened directory, using the separator of
    /// the current platform.
    pub fn cur_path(&self) -> String {
        self.tree.path_of(self.cur)
    }

    /// Re-sorts the children of the current directory.
    pub fn resort(&mut self) {
        let mut view = self.tree.nodes[self.cur].children.clone();
        let tree = &self.tree;
        let col = self.sort_col;
        let desc = self.sort_desc;
        let show_as = self.show_as;

        if col == SortCol::Name {
            // Case-insensitive name sort: compute the key once per entry
            // instead of allocating on every comparison.
            let mut keyed: Vec<(String, usize)> = view
                .iter()
                .map(|&i| (tree.name(i).to_lowercase(), i))
                .collect();
            keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| tree.name(a.1).cmp(tree.name(b.1))));
            view = keyed.into_iter().map(|(_, i)| i).collect();
            if desc {
                view.reverse();
            }
        } else {
            view.sort_by(|&a, &b| {
                let (na, nb) = (&tree.nodes[a], &tree.nodes[b]);
                let ord = match col {
                    SortCol::Size => {
                        let (sa, sb) = if show_as {
                            (na.asize, nb.asize)
                        } else {
                            (na.size, nb.size)
                        };
                        sa.cmp(&sb)
                    }
                    SortCol::Items => na.items.cmp(&nb.items),
                    SortCol::Name => unreachable!(),
                };
                // Fall back to the name for a stable, predictable order.
                let ord = ord.then_with(|| tree.name(a).cmp(tree.name(b)));
                if desc { ord.reverse() } else { ord }
            });
        }

        self.sorted = view;
        if self.selected >= self.sorted.len() {
            self.selected = self.sorted.len().saturating_sub(1);
        }
    }

    fn move_by(&mut self, delta: i64) {
        if self.sorted.is_empty() {
            return;
        }
        let max = self.sorted.len() as i64 - 1;
        self.selected = (self.selected as i64 + delta).clamp(0, max) as usize;
    }

    fn enter(&mut self) {
        let Some(node) = self.selected_node() else { return };
        if !node.is_dir || node.error {
            return;
        }
        self.cur = self.sorted[self.selected];
        self.selected = 0;
        self.resort();
    }

    fn leave(&mut self) {
        let Some(parent) = self.cur_node().parent else { return };
        let leaving = self.cur;
        self.cur = parent;
        self.resort();
        // Keep the cursor on the directory we just left.
        self.selected = self
            .sorted
            .iter()
            .position(|&i| i == leaving)
            .unwrap_or(0);
    }

    /// Selecting the active column again reverses the sort order.
    fn set_sort(&mut self, col: SortCol) {
        if self.sort_col == col {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_col = col;
            // Names read best ascending, everything else starts with the biggest.
            self.sort_desc = col != SortCol::Name;
        }
        self.selected = 0;
        self.resort();
    }

    fn request_delete(&mut self) {
        let Some(&idx) = self.sorted.get(self.selected) else {
            return;
        };
        if self.tree.nodes[idx].parent.is_none() {
            self.notice = Some(("cannot delete the root directory".to_string(), true));
            return;
        }
        self.confirm = Some(idx);
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
        let mut scanner = match Scanner::new(Path::new(&path)) {
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
        // The delete confirmation takes precedence: y confirms, anything else
        // (including Esc / q) cancels.
        if let Some(idx) = self.confirm {
            self.confirm = None;
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.job = Some(Job::Delete(idx)),
                _ => self.notice = Some(("delete cancelled".to_string(), false)),
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
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::PageUp => self.move_by(-(self.view_height.max(1) as i64)),
            KeyCode::PageDown => self.move_by(self.view_height.max(1) as i64),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = self.sorted.len().saturating_sub(1)
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
            KeyCode::Char('d') => self.request_delete(),
            KeyCode::Char('r') => self.job = Some(Job::Refresh),
            KeyCode::Char('b') => self.job = Some(Job::Shell),
            _ => {}
        }
    }
}
