/* ncdurs - browsing state and key handling
 *
 * The counterpart of the C version's src/dirlist.c: it keeps track of the
 * currently opened directory, the sorted view of its children and the cursor.
 */

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;

use crate::scan::{Node, Tree};

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
        let mut chain = Vec::new();
        let mut idx = Some(self.cur);
        while let Some(i) = idx {
            chain.push(i);
            idx = self.node(i).parent;
        }
        let mut path = PathBuf::new();
        for &i in chain.iter().rev() {
            path.push(&self.node(i).name);
        }
        if path.as_os_str().is_empty() {
            "/".to_string()
        } else {
            path.to_string_lossy().into_owned()
        }
    }

    /// Re-sorts the children of the current directory.
    pub fn resort(&mut self) {
        let mut view = self.tree.nodes[self.cur].children.clone();
        let nodes = &self.tree.nodes;
        let col = self.sort_col;
        let desc = self.sort_desc;
        let show_as = self.show_as;

        view.sort_by(|&a, &b| {
            let (na, nb) = (&nodes[a], &nodes[b]);
            let ord = match col {
                SortCol::Size => {
                    let (sa, sb) = if show_as {
                        (na.asize, nb.asize)
                    } else {
                        (na.size, nb.size)
                    };
                    sa.cmp(&sb)
                }
                SortCol::Name => na.name.to_lowercase().cmp(&nb.name.to_lowercase()),
                SortCol::Items => na.items.cmp(&nb.items),
            };
            // Fall back to the name for a stable, predictable order.
            let ord = ord.then_with(|| na.name.cmp(&nb.name));
            if desc { ord.reverse() } else { ord }
        });

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

    fn cycle_sort(&mut self) {
        self.sort_col = match self.sort_col {
            SortCol::Size => SortCol::Name,
            SortCol::Name => SortCol::Items,
            SortCol::Items => SortCol::Size,
        };
        // Names read best ascending, everything else starts with the biggest.
        self.sort_desc = self.sort_col != SortCol::Name;
        self.selected = 0;
        self.resort();
    }

    pub fn on_key(&mut self, key: KeyEvent) {
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
            KeyCode::Char('s') => self.cycle_sort(),
            KeyCode::Char('d') => {
                self.sort_desc = !self.sort_desc;
                self.resort();
            }
            KeyCode::Char('a') => {
                self.show_as = !self.show_as;
                self.resort();
            }
            _ => {}
        }
    }
}
