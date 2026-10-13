/* ncdurs - scan phase
 *
 * Model (same as the C implementation, src/dir_scan.c + src/dir_mem.c):
 * a directory's size is the sum of its own occupied space and the accumulated
 * sizes of all its children. Hard links are counted only once via a global set
 * of file identities; all OS specific details live in platform.rs.
 *
 * The walk is incremental: a Scanner keeps an explicit stack of directories
 * and step() processes a bounded number of entries, so the caller can keep the
 * UI responsive and abort at any time. The walk itself uses chdir + relative
 * names (like the C version), because resolving an absolute path for every
 * entry is noticeably slower on deep trees.
 *
 * Names are kept in one big string (Tree::names) and nodes only store an
 * offset/length pair. This keeps the per-entry allocation count low, which is
 * the dominant cost when scanning hundreds of thousands of files.
 *
 * Note: chdir() changes process-global state, so only one scan may be in
 * progress at a time. The original working directory is restored when the scan
 * finishes, aborts or fails.
 */

use std::collections::{HashSet, VecDeque};
use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::platform;

/// Initial node capacity: avoids repeated reallocation on large trees. The
/// memory is only committed once nodes are actually pushed.
const INITIAL_NODES: usize = 1 << 16;
/// Initial capacity of the name arena.
const INITIAL_NAMES: usize = 1 << 20;

/// Reference to an entry name stored in the tree's name arena.
#[derive(Debug, Clone, Copy)]
pub struct NameRef {
    start: u32,
    len: u32,
}

#[derive(Debug)]
pub struct Node {
    /// Name of this entry (an offset/length pair into `Tree::names`).
    pub name: NameRef,
    /// Disk usage: st_blocks * 512, includes the directory's own blocks.
    pub size: u64,
    /// Apparent size: st_size.
    pub asize: u64,
    /// Number of entries in this subtree (files and directories), excluding self.
    pub items: u64,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub is_dir: bool,
    pub error: bool,
    /// Neither a directory nor a regular file (symlink, socket, fifo, device).
    /// The C version records this as `notreg` in its exports.
    pub notreg: bool,
}

pub struct Tree {
    pub nodes: Vec<Node>,
    /// All entry names, concatenated (name arena).
    names: String,
}

impl Tree {
    pub(crate) fn new() -> Tree {
        Tree {
            nodes: Vec::with_capacity(INITIAL_NODES),
            names: String::with_capacity(INITIAL_NAMES),
        }
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// Creates a node for `name` under `parent` and returns its index. Used
    /// when loading an exported file; the caller fills in sizes/items.
    pub(crate) fn push_node(&mut self, parent: Option<usize>, name: &str, is_dir: bool) -> usize {
        let name = self.intern(name);
        let idx = self.push(Node {
            name,
            size: 0,
            asize: 0,
            items: 0,
            parent,
            children: Vec::new(),
            is_dir,
            error: false,
            notreg: false,
        });
        self.attach(parent, idx);
        idx
    }

    fn attach(&mut self, parent: Option<usize>, child: usize) {
        if let Some(p) = parent {
            self.nodes[p].children.push(child);
        }
    }

    /// Adds a name to the arena and returns a reference to it.
    fn intern(&mut self, name: &str) -> NameRef {
        let start = self.names.len();
        debug_assert!(start + name.len() <= u32::MAX as usize, "name arena overflow");
        self.names.push_str(name);
        NameRef {
            start: start as u32,
            len: name.len() as u32,
        }
    }

    /// Name of a node.
    pub fn name(&self, idx: usize) -> &str {
        self.name_of(self.nodes[idx].name)
    }

    /// Name behind a reference (internal helper for the walk).
    fn name_of(&self, r: NameRef) -> &str {
        &self.names[r.start as usize..(r.start as usize + r.len as usize)]
    }

    /// Path of a node, built from the root. The root node's name is already an
    /// absolute path; the platform's own separator is used for the rest.
    pub fn path_of(&self, mut idx: usize) -> String {
        let mut chain = Vec::new();
        loop {
            chain.push(idx);
            match self.nodes[idx].parent {
                Some(p) => idx = p,
                None => break,
            }
        }
        let mut path = PathBuf::new();
        for &i in chain.iter().rev() {
            path.push(self.name(i));
        }
        if path.as_os_str().is_empty() {
            "/".to_string()
        } else {
            path.to_string_lossy().into_owned()
        }
    }

    /// Detaches a node from its parent's child list.
    fn detach(&mut self, idx: usize) {
        if let Some(parent) = self.nodes[idx].parent {
            if let Some(pos) = self.nodes[parent].children.iter().position(|&c| c == idx) {
                self.nodes[parent].children.remove(pos);
            }
        }
    }

    /// Removes a node together with its subtree: the totals of all ancestors
    /// are reduced, then the node is detached from its parent.
    ///
    /// The nodes themselves stay in the arena as unreachable garbage - a
    /// compact pass can be added later if it ever matters. Nothing holds
    /// indices into a removed subtree, so this is safe.
    pub fn remove_subtree(&mut self, idx: usize) {
        let n = &self.nodes[idx];
        if n.parent.is_none() {
            return; // the root cannot be removed
        }
        let (size, asize, items) = (n.size, n.asize, n.items + 1);
        let mut par = n.parent;
        while let Some(p) = par {
            let pn = &mut self.nodes[p];
            pn.size = pn.size.saturating_sub(size);
            pn.asize = pn.asize.saturating_sub(asize);
            pn.items = pn.items.saturating_sub(items);
            par = pn.parent;
        }
        self.detach(idx);
    }

    /// Replaces `target` (and its subtree) with the freshly scanned tree
    /// `other`, whose root becomes the new content of `target`; the ancestor
    /// totals are adjusted by the difference and the old subtree becomes
    /// garbage. Used by the `r` (refresh) action.
    pub fn graft(&mut self, target: usize, other: Tree) {
        let old_size = self.nodes[target].size;
        let old_asize = self.nodes[target].asize;
        let old_items = self.nodes[target].items + 1;
        let parent = self.nodes[target].parent;
        let old_name = self.nodes[target].name;

        let base = self.nodes.len();
        // other's index i -> our index (0 becomes `target`, the rest are appended)
        let remap = |i: usize| if i == 0 { target } else { base + i - 1 };

        // Move the whole name arena over in one go: only the offsets change.
        let offset = self.names.len() as u32;
        self.names.push_str(&other.names);

        let mut nodes = other.nodes.into_iter();
        if let Some(mut root) = nodes.next() {
            // Keep the slot identity: same name, same place in the parent.
            root.name = old_name;
            root.parent = parent;
            for c in root.children.iter_mut() {
                *c = remap(*c);
            }
            self.nodes[target] = root;
        }
        for mut n in nodes {
            n.name.start += offset;
            n.parent = n.parent.map(remap);
            for c in n.children.iter_mut() {
                *c = remap(*c);
            }
            self.nodes.push(n);
        }

        // Adjust the ancestors by the size/items difference.
        let (new_size, new_asize, new_items) = {
            let n = &self.nodes[target];
            (n.size, n.asize, n.items + 1)
        };
        let mut par = parent;
        while let Some(p) = par {
            let pn = &mut self.nodes[p];
            pn.size = pn.size.saturating_sub(old_size).saturating_add(new_size);
            pn.asize = pn.asize.saturating_sub(old_asize).saturating_add(new_asize);
            pn.items = pn.items.saturating_sub(old_items).saturating_add(new_items);
            par = pn.parent;
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanStats {
    pub entries: u64,
    pub errors: u64,
}

/// Snapshot of the scan progress, used by the UI.
pub struct Progress {
    pub root: String,
    pub current: String,
    pub entries: u64,
    pub errors: u64,
    /// Sum of the occupied sizes seen so far (a lower bound of the total).
    pub bytes: u64,
}

struct Frame {
    /// Index of the directory node in the tree.
    node: usize,
    /// Entries of this directory that still have to be processed.
    names: VecDeque<NameRef>,
    /// Accumulated contribution of the already processed children.
    size: u64,
    asize: u64,
    items: u64,
}

enum Action {
    Entry(NameRef),
    CloseDir(usize),
}

pub struct Scanner {
    tree: Tree,
    stack: Vec<Frame>,
    links: HashSet<(u64, u64)>,
    stats: ScanStats,
    /// Occupied bytes seen so far, for progress reporting only.
    bytes: u64,
    /// Working directory at the time the scanner was created.
    start_cwd: PathBuf,
    done: bool,
    failed: Option<String>,
}

impl Scanner {
    /// Prepares a scan of `path`. The current working directory is changed to
    /// `path`; it is restored by `into_result()` or `abort()`.
    pub fn new(path: &Path) -> io::Result<Scanner> {
        let md = fs::symlink_metadata(path)?;
        if !md.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: not a directory", path.display()),
            ));
        }

        let abs = if path.is_absolute() {
            normalize(path)
        } else {
            normalize(&env::current_dir()?.join(path))
        };
        let start_cwd = env::current_dir()?;

        // From here on the walk uses relative names.
        env::set_current_dir(&abs)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {}", abs.display(), e)))?;

        let mut scanner = Scanner {
            tree: Tree::new(),
            stack: Vec::new(),
            links: HashSet::new(),
            stats: ScanStats {
                entries: 1,
                errors: 0,
            },
            bytes: 0,
            start_cwd,
            done: false,
            failed: None,
        };

        let own_size = platform::disk_usage(&abs, &md);
        let own_asize = md.len();
        let root_name = scanner.tree.intern(&abs.to_string_lossy());
        scanner.bytes = own_size;
        scanner.tree.push(Node {
            name: root_name,
            size: own_size,
            asize: own_asize,
            items: 0,
            parent: None,
            children: Vec::new(),
            is_dir: true,
            error: false,
            notreg: false,
        });

        let (names, list_failed) = scanner.read_names();
        let names = names.unwrap_or_default();
        if list_failed {
            scanner.tree.nodes[0].error = true;
        }
        scanner.tree.nodes[0].children.reserve(names.len());
        scanner.stack.push(Frame {
            node: 0,
            names,
            size: 0,
            asize: 0,
            items: 0,
        });
        Ok(scanner)
    }

    /// Processes at most `budget` entries. Returns true when the scan is done
    /// (also after a fatal error or abort).
    pub fn step(&mut self, budget: usize) -> bool {
        if self.done {
            return true;
        }
        for _ in 0..budget {
            let action = match self.stack.last_mut() {
                None => {
                    self.finish();
                    return true;
                }
                Some(frame) => match frame.names.pop_front() {
                    Some(name) => Action::Entry(name),
                    None => Action::CloseDir(frame.node),
                },
            };
            match action {
                Action::Entry(name) => self.process(name),
                Action::CloseDir(node) => self.close_dir(node),
            }
            if self.done {
                return true;
            }
        }
        false
    }

    pub fn progress(&self) -> Progress {
        let cur = self.stack.last().map(|f| f.node).unwrap_or(0);
        Progress {
            root: self.tree.name(0).to_string(),
            current: self.tree.path_of(cur),
            entries: self.stats.entries,
            errors: self.stats.errors,
            bytes: self.bytes,
        }
    }

    /// Fatal error that ended the scan early, if any.
    pub fn failure(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    /// Stops the scan and restores the working directory. Safe to call twice.
    pub fn abort(&mut self) {
        self.stack.clear();
        self.done = true;
        self.restore_cwd();
    }

    /// Consumes the scanner, restoring the working directory.
    pub fn into_result(self) -> (Tree, ScanStats) {
        self.restore_cwd();
        (self.tree, self.stats)
    }

    fn restore_cwd(&self) {
        let _ = env::set_current_dir(&self.start_cwd);
    }

    fn finish(&mut self) {
        self.done = true;
        self.restore_cwd();
    }

    /// Handles one directory entry of the current directory.
    fn process(&mut self, name_ref: NameRef) {
        let parent = match self.stack.last() {
            Some(f) => f.node,
            None => return,
        };

        let md = {
            let name = self.tree.name_of(name_ref);
            fs::symlink_metadata(name)
        };

        let md = match md {
            Ok(md) => md,
            Err(_) => {
                self.stats.errors += 1;
                let idx = self.tree.push(Node {
                    name: name_ref,
                    size: 0,
                    asize: 0,
                    items: 0,
                    parent: Some(parent),
                    children: Vec::new(),
                    is_dir: false,
                    error: true,
                    notreg: false,
                });
                self.tree.attach(Some(parent), idx);
                self.stats.entries += 1;
                self.add_to_parent(0, 0, 1);
                return;
            }
        };

        // Symlinks are not followed: symlink_metadata() reports the link
        // itself, so a symlink to a directory is not entered.
        let is_dir = md.is_dir();

        // Count a file with multiple hard links only once. If the platform
        // cannot provide a stable file identity, no de-duplication is attempted.
        let dup = !is_dir
            && platform::nlink(&md) > 1
            && platform::file_id(&md).is_some_and(|id| !self.links.insert(id));
        let (own_size, own_asize) = if dup {
            (0, 0)
        } else {
            let name = self.tree.name_of(name_ref);
            (platform::disk_usage(Path::new(name), &md), md.len())
        };

        let idx = self.tree.push(Node {
            name: name_ref,
            size: own_size,
            asize: own_asize,
            items: 0,
            parent: Some(parent),
            children: Vec::new(),
            is_dir,
            error: false,
            // Symlinks, sockets, fifos and device nodes: what the C version
            // records as "notreg" (neither a directory nor a regular file).
            notreg: !is_dir && !md.is_file(),
        });
        self.tree.attach(Some(parent), idx);
        self.stats.entries += 1;
        self.bytes += own_size;

        if !is_dir {
            self.add_to_parent(own_size, own_asize, 1);
            return;
        }

        // Directories are entered and processed by later step() calls.
        let entered = {
            let name = self.tree.name_of(name_ref);
            env::set_current_dir(name).is_ok()
        };
        if !entered {
            self.tree.nodes[idx].error = true;
            self.stats.errors += 1;
            self.add_to_parent(own_size, own_asize, 1);
            return;
        }

        match self.read_names() {
            (Some(names), list_failed) => {
                if list_failed {
                    self.tree.nodes[idx].error = true;
                }
                // The number of children is known: size the vector once.
                self.tree.nodes[idx].children.reserve(names.len());
                self.stack.push(Frame {
                    node: idx,
                    names,
                    size: 0,
                    asize: 0,
                    items: 0,
                });
            }
            (None, _) => {
                // Could not list the directory: keep it as a leaf.
                self.tree.nodes[idx].error = true;
                let _ = env::set_current_dir("..");
                self.add_to_parent(own_size, own_asize, 1);
            }
        }
    }

    /// Finalizes a directory: writes the accumulated totals into its node and
    /// returns to the parent directory.
    fn close_dir(&mut self, node: usize) {
        let frame = match self.stack.pop() {
            Some(f) => f,
            None => return,
        };
        let is_root = self.stack.is_empty();

        if !is_root && env::set_current_dir("..").is_err() {
            // Not being able to go back is fatal, same as in the C version.
            self.failed = Some(format!(
                "error going back to the parent directory of {}",
                self.tree.path_of(node)
            ));
            self.stack.clear();
            self.finish();
            return;
        }

        let (size, asize) = {
            let n = &mut self.tree.nodes[node];
            n.size += frame.size;
            n.asize += frame.asize;
            n.items = frame.items;
            (n.size, n.asize)
        };

        if is_root {
            self.finish();
        } else {
            self.add_to_parent(size, asize, frame.items + 1);
        }
    }

    /// Adds the contribution of a finished child to the directory on top of
    /// the stack.
    fn add_to_parent(&mut self, size: u64, asize: u64, items: u64) {
        if let Some(frame) = self.stack.last_mut() {
            frame.size += size;
            frame.asize += asize;
            frame.items += items;
        }
    }

    /// Reads the entry names of the current directory (".") and interns them.
    /// Returns None when the directory could not be opened at all; `failed`
    /// reports entries that could not be read (or the open failure itself).
    fn read_names(&mut self) -> (Option<VecDeque<NameRef>>, bool) {
        match fs::read_dir(".") {
            Ok(rd) => {
                let mut names = VecDeque::new();
                let mut failed = false;
                for entry in rd {
                    match entry {
                        Ok(entry) => {
                            let file_name = entry.file_name();
                            let name = file_name.to_string_lossy();
                            names.push_back(self.tree.intern(&name));
                        }
                        Err(_) => {
                            self.stats.errors += 1;
                            failed = true;
                        }
                    }
                }
                (Some(names), failed)
            }
            Err(_) => {
                self.stats.errors += 1;
                (None, true)
            }
        }
    }
}

/// Scans a whole tree in one go; used by the tests.
#[cfg(test)]
pub fn scan(path: &Path) -> io::Result<(Tree, ScanStats)> {
    let mut scanner = Scanner::new(path)?;
    while !scanner.step(4096) {}
    if let Some(err) = scanner.failure() {
        return Err(io::Error::other(err.to_string()));
    }
    Ok(scanner.into_result())
}

/// Removes "." components (and resolves ".." lexically) without touching the
/// filesystem, so symlinks are not resolved.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            _ => out.push(c.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push("/");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The walk changes the process working directory, so tests that scan must
    /// not run concurrently.
    static SCAN_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let p = env::temp_dir().join(format!("ncdurs-test-{}-{}", tag, std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn own_size(path: &Path) -> u64 {
        crate::platform::disk_usage(path, &fs::symlink_metadata(path).unwrap())
    }

    fn own_asize(path: &Path) -> u64 {
        fs::symlink_metadata(path).unwrap().len()
    }

    fn find_child(tree: &Tree, parent: usize, name: &str) -> usize {
        tree.nodes[parent]
            .children
            .iter()
            .copied()
            .find(|&i| tree.name(i) == name)
            .unwrap_or_else(|| panic!("child {} not found", name))
    }

    #[test]
    fn accumulates_child_sizes() {
        let _guard = lock();
        let tmp = TempDir::new("acc");
        fs::create_dir(tmp.0.join("sub")).unwrap();
        fs::write(tmp.0.join("sub/f1"), vec![b'x'; 10_000]).unwrap();
        fs::write(tmp.0.join("f2"), vec![b'y'; 20_000]).unwrap();

        let (tree, stats) = scan(&tmp.0).unwrap();
        assert_eq!(stats.errors, 0);

        let sub_idx = find_child(&tree, 0, "sub");
        let f1_idx = find_child(&tree, sub_idx, "f1");
        let f2_idx = find_child(&tree, 0, "f2");
        let root = &tree.nodes[0];
        let sub = &tree.nodes[sub_idx];
        let f1 = &tree.nodes[f1_idx];
        let f2 = &tree.nodes[f2_idx];

        // Root = own blocks + sub + f2; sub = own blocks + f1.
        assert_eq!(root.size, own_size(&tmp.0) + sub.size + f2.size);
        assert_eq!(root.asize, own_asize(&tmp.0) + sub.asize + f2.asize);
        assert_eq!(sub.size, own_size(&tmp.0.join("sub")) + f1.size);
        assert_eq!(sub.asize, own_asize(&tmp.0.join("sub")) + f1.asize);
        assert_eq!(sub.items, 1);
        assert_eq!(root.items, 3); // sub, f1, f2
        assert!(root.is_dir);
        assert!(!f1.is_dir);
        // Names live in the arena and path_of() rebuilds paths from it.
        assert!(tree.path_of(f1_idx).contains("sub"));
        assert!(tree.path_of(f1_idx).ends_with("f1"));
    }

    #[test]
    fn hard_links_counted_once() {
        let _guard = lock();
        let tmp = TempDir::new("hlink");
        fs::create_dir(tmp.0.join("sub")).unwrap();
        fs::write(tmp.0.join("sub/f1"), vec![b'x'; 10_000]).unwrap();
        fs::hard_link(tmp.0.join("sub/f1"), tmp.0.join("sub/f2")).unwrap();

        let (tree, stats) = scan(&tmp.0).unwrap();
        assert_eq!(stats.errors, 0);

        let sub_idx = find_child(&tree, 0, "sub");
        let sub = &tree.nodes[sub_idx];
        let f1_size = own_size(&tmp.0.join("sub/f1"));

        // f2 is a second link to f1's inode: listed, but counted as 0 bytes.
        assert_eq!(sub.size, own_size(&tmp.0.join("sub")) + f1_size);
        assert_eq!(sub.items, 2);
    }

    #[test]
    fn stepped_scan_matches_bulk_scan() {
        let _guard = lock();
        let tmp = TempDir::new("stepped");
        fs::create_dir_all(tmp.0.join("a/b")).unwrap();
        fs::create_dir(tmp.0.join("c")).unwrap();
        fs::write(tmp.0.join("a/b/f1"), vec![b'x'; 5_000]).unwrap();
        fs::write(tmp.0.join("a/f2"), vec![b'y'; 7_000]).unwrap();
        fs::write(tmp.0.join("c/f3"), vec![b'z'; 3_000]).unwrap();
        fs::hard_link(tmp.0.join("a/f2"), tmp.0.join("c/f4")).unwrap();

        let (bulk, bulk_stats) = scan(&tmp.0).unwrap();

        // One entry per step: the result has to be identical, names included.
        let mut scanner = Scanner::new(&tmp.0).unwrap();
        while !scanner.step(1) {}
        let (stepped, stepped_stats) = scanner.into_result();

        assert_eq!(bulk_stats.entries, stepped_stats.entries);
        assert_eq!(bulk.nodes.len(), stepped.nodes.len());
        assert_eq!(bulk.nodes[0].size, stepped.nodes[0].size);
        assert_eq!(bulk.nodes[0].asize, stepped.nodes[0].asize);
        assert_eq!(bulk.nodes[0].items, stepped.nodes[0].items);
        for i in 0..bulk.nodes.len() {
            assert_eq!(bulk.name(i), stepped.name(i));
        }
    }

    #[test]
    fn abort_and_finish_restore_working_directory() {
        let _guard = lock();
        let tmp = TempDir::new("cwd");
        fs::create_dir(tmp.0.join("sub")).unwrap();
        fs::write(tmp.0.join("sub/f1"), vec![b'x'; 1_000]).unwrap();

        let before = env::current_dir().unwrap();

        let mut scanner = Scanner::new(&tmp.0).unwrap();
        scanner.step(1);
        scanner.abort();
        assert_eq!(env::current_dir().unwrap(), before);

        let _ = scan(&tmp.0).unwrap();
        assert_eq!(env::current_dir().unwrap(), before);
    }

    #[test]
    fn remove_subtree_updates_ancestors() {
        let _guard = lock();
        let tmp = TempDir::new("rm");
        fs::create_dir(tmp.0.join("sub")).unwrap();
        fs::write(tmp.0.join("sub/f1"), vec![b'x'; 10_000]).unwrap();
        fs::write(tmp.0.join("f2"), vec![b'y'; 5_000]).unwrap();

        let (mut tree, _) = scan(&tmp.0).unwrap();
        let root_size = tree.nodes[0].size;
        let root_items = tree.nodes[0].items;
        let sub_idx = find_child(&tree, 0, "sub");
        let sub_size = tree.nodes[sub_idx].size;
        let sub_items = tree.nodes[sub_idx].items;

        tree.remove_subtree(sub_idx);

        assert_eq!(tree.nodes[0].size, root_size - sub_size);
        assert_eq!(tree.nodes[0].items, root_items - sub_items - 1);
        assert!(tree.nodes[0].children.iter().all(|&c| c != sub_idx));
        // The arena stays intact: the detached node is just unreachable.
        assert!(tree.path_of(sub_idx).ends_with("sub"));
    }

    #[test]
    fn graft_replaces_subtree_consistently() {
        let _guard = lock();
        let tmp = TempDir::new("graft");
        fs::create_dir(tmp.0.join("d")).unwrap();
        fs::write(tmp.0.join("d/f1"), vec![b'x'; 4_000]).unwrap();
        fs::write(tmp.0.join("keep"), vec![b'y'; 2_000]).unwrap();

        let (mut tree, _) = scan(&tmp.0).unwrap();
        let d_idx = find_child(&tree, 0, "d");
        let root_size = tree.nodes[0].size;
        let old_d_size = tree.nodes[d_idx].size;
        let root_items = tree.nodes[0].items;
        let name = tree.name(d_idx).to_string();

        let (fresh, _) = scan(&tmp.0.join("d")).unwrap();
        tree.graft(d_idx, fresh);

        // The slot keeps its identity ...
        assert_eq!(tree.name(d_idx), name);
        assert_eq!(tree.nodes[d_idx].parent, Some(0));
        // ... its children are remapped and reachable ...
        let f1 = find_child(&tree, d_idx, "f1");
        assert!(tree.path_of(f1).ends_with("f1"));
        assert_eq!(
            tree.nodes[d_idx].size,
            own_size(&tmp.0.join("d")) + own_size(&tmp.0.join("d/f1"))
        );
        // ... and the ancestors see exactly the difference.
        assert_eq!(
            tree.nodes[0].size,
            root_size - old_d_size + tree.nodes[d_idx].size
        );
        assert_eq!(tree.nodes[0].items, root_items);
        assert_eq!(
            tree.nodes[0].children.iter().filter(|&&c| c == d_idx).count(),
            1
        );
    }

    #[test]
    fn refresh_picks_up_changes() {
        let _guard = lock();
        let tmp = TempDir::new("refresh");
        fs::create_dir(tmp.0.join("a")).unwrap();
        fs::write(tmp.0.join("a/f1"), vec![b'x'; 4_000]).unwrap();

        let (tree, _) = scan(&tmp.0).unwrap();
        let mut app = crate::app::App::new(tree, false, false);
        app.cur = find_child(&app.tree, 0, "a");
        let dir_before = app.tree.nodes[app.cur].size;
        let root_before = app.tree.nodes[0].size;

        // Something new appears on disk, then the user presses r.
        fs::write(tmp.0.join("a/f2"), vec![b'y'; 3_000]).unwrap();
        app.run_refresh();

        let f2 = find_child(&app.tree, app.cur, "f2");
        assert!(app.tree.path_of(f2).ends_with("f2"));
        assert!(app.tree.nodes[app.cur].size >= dir_before + own_size(&tmp.0.join("a/f2")));
        // The root sees exactly the difference of the refreshed subtree.
        assert_eq!(
            app.tree.nodes[0].size,
            root_before - dir_before + app.tree.nodes[app.cur].size
        );
    }

    #[cfg(unix)]
    #[test]
    fn marks_non_regular_files() {
        let _guard = lock();
        let tmp = TempDir::new("notreg");
        fs::write(tmp.0.join("regular"), b"x").unwrap();
        std::os::unix::fs::symlink("regular", tmp.0.join("link")).unwrap();

        let (tree, _) = scan(&tmp.0).unwrap();
        let regular = find_child(&tree, 0, "regular");
        let link = find_child(&tree, 0, "link");

        assert!(!tree.nodes[regular].notreg);
        assert!(tree.nodes[link].notreg);
        assert!(!tree.nodes[link].is_dir);
        assert!(!tree.nodes[link].error);
        // Symlinks are not followed: no disk blocks of their own.
        assert_eq!(tree.nodes[link].size, 0);
    }
}
