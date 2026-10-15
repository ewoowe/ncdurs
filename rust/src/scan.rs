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

use crate::exclude::{self, Excludes};
use crate::platform::ExtInfo;

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
    /// Left out of the statistics by an --exclude pattern or by
    /// --exclude-caches; the C version marks these with '<'.
    pub excluded: bool,
    /// On another filesystem, so it was not entered (`-x`); marked '>'.
    pub othfs: bool,
    /// Linux pseudo filesystem excluded by `--exclude-kernfs`; marked '^'.
    pub kernfs: bool,
    /// Something *below* this directory could not be read, so its size may be
    /// wrong (the C version's FF_SERR); marked '.'.
    pub serr: bool,
    /// This is a file with more than one hard link (the C version's FF_HLNKC);
    /// marked 'H'. Only the first link counts towards the totals, but every
    /// link keeps its own size in the listing and in the export.
    pub hlnkc: bool,
    /// st_ino, written out for hard links (and 0 otherwise).
    pub ino: u64,
    /// st_nlink, written out for hard links.
    pub nlink: u32,
    /// Reached through a firmlink and left un-entered (`--exclude-firmlinks`,
    /// macOS); marked 'F'. Unlike kernfs such a directory keeps its own size.
    pub frmlnk: bool,
    /// Whether this entry contributed to its parent's totals. False for a
    /// repeated hard link: its size is listed and exported, but counted once.
    pub counted: bool,
    /// Extended information (`-e`). Zeroed when `has_ext` is false.
    pub ext: ExtInfo,
    /// Whether `ext` holds real data (the C version's FF_EXT flag).
    pub has_ext: bool,
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
            excluded: false,
            othfs: false,
            kernfs: false,
            serr: false,
            hlnkc: false,
            ino: 0,
            nlink: 0,
            frmlnk: false,
            counted: true,
            ext: ExtInfo::default(),
            has_ext: false,
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
        debug_assert!(
            start + name.len() <= u32::MAX as usize,
            "name arena overflow"
        );
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
        if let Some(parent) = self.nodes[idx].parent
            && let Some(pos) = self.nodes[parent].children.iter().position(|&c| c == idx)
        {
            self.nodes[parent].children.remove(pos);
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
    pub current: String,
    pub entries: u64,
    pub errors: u64,
    /// Sum of the occupied sizes seen so far (a lower bound of the total).
    pub bytes: u64,
}

/// What the scanner should leave out (`--exclude`, `-x`, `--exclude-caches`).
/// `Default` is written out by hand because `follow_firmlinks` starts as true.
#[derive(Clone)]
pub struct ScanOptions {
    pub excludes: Excludes,
    /// `-x`: do not cross filesystem boundaries.
    pub one_file_system: bool,
    /// `--exclude-caches`: skip directories holding a CACHEDIR.TAG.
    pub exclude_caches: bool,
    /// `--exclude-kernfs`: skip Linux pseudo filesystems.
    pub exclude_kernfs: bool,
    /// `-L`: count a symlink to a file as the file it points at.
    pub follow_symlinks: bool,
    /// `--follow-firmlinks` (the default) vs `--exclude-firmlinks`, macOS only.
    pub follow_firmlinks: bool,
    /// `-e`: collect uid/gid/mode/mtime for every entry.
    pub extended: bool,
}

impl Default for ScanOptions {
    fn default() -> ScanOptions {
        ScanOptions {
            excludes: Excludes::default(),
            one_file_system: false,
            exclude_caches: false,
            exclude_kernfs: false,
            follow_symlinks: false,
            // Firmlinks are followed unless asked otherwise (main.c:45). A
            // derived Default would make this false, and on macOS that turns
            // every path below a firmlink (/private, /Users, ...) into a
            // directory that is silently not scanned.
            follow_firmlinks: true,
            extended: false,
        }
    }
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
    /// This directory sits inside an excluded subtree, so its totals must not
    /// reach the ancestors - the contents are still scanned so that they can be
    /// browsed, they just do not count (`--exclude`).
    excluded: bool,
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
    /// Exclusion rules and `-x`.
    options: ScanOptions,
    /// Device of the scanned root, for `-x`.
    root_dev: Option<u64>,
    done: bool,
    failed: Option<String>,
}

impl Scanner {
    /// Prepares a scan of `path`. The current working directory is changed to
    /// `path`; it is restored by `into_result()` or `abort()`.
    pub fn new(path: &Path, options: ScanOptions) -> io::Result<Scanner> {
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
        // The root goes through its own construction below, so collect its
        // extended information here - before `options` is moved into the
        // scanner.
        let root_ext = if options.extended {
            platform::ext_info(&md)
        } else {
            None
        };

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
            options,
            root_dev: platform::file_info(&md, path).map(|info| info.dev),
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
            excluded: false,
            othfs: false,
            kernfs: false,
            serr: false,
            hlnkc: false,
            ino: 0,
            nlink: 0,
            frmlnk: false,
            counted: true,
            ext: root_ext.unwrap_or_default(),
            has_ext: root_ext.is_some(),
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
            excluded: false,
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
                    excluded: false,
                    othfs: false,
                    kernfs: false,
                    serr: false,
                    hlnkc: false,
                    ino: 0,
                    nlink: 0,
                    frmlnk: false,
                    counted: true,
                    ext: ExtInfo::default(),
                    has_ext: false,
                });
                self.tree.attach(Some(parent), idx);
                self.stats.entries += 1;
                self.add_to_parent(0, 0, 1);
                return;
            }
        };

        // Symlinks are not followed by default: symlink_metadata() reports the
        // link itself, so a symlink to a directory is not entered.
        //
        // With -L a symlink to a *file* is counted as the file it points at -
        // links to directories are never followed, exactly like the C version
        // does it (src/dir_scan.c:304-309). Everything below uses this
        // metadata, so such a link also stops being "notreg" ('@').
        let md = if self.options.follow_symlinks && md.file_type().is_symlink() {
            let name = self.tree.name_of(name_ref);
            fs::metadata(name)
                .ok()
                .filter(|target| !target.is_dir())
                .unwrap_or(md)
        } else {
            md
        };

        let is_dir = md.is_dir();

        // `-e`: collect uid/gid/mode/mtime. Only asked for when the option is
        // given; the C version fills the same fields unconditionally but only
        // keeps them in extended mode.
        let ext = if self.options.extended {
            platform::ext_info(&md)
        } else {
            None
        };

        // Exclusion rules: --exclude matches the full path (the C version tries
        // the whole path and every suffix of it), -x stops at filesystem
        // boundaries, --exclude-caches skips cache directories. Everything
        // below an excluded directory inherits that state.
        let parent_excluded = self.stack.last().is_some_and(|f| f.excluded);
        let mut excluded = parent_excluded;
        let mut othfs = false;
        let mut kernfs = false;
        let mut frmlnk = false;
        let mut skip_contents = false;
        if !parent_excluded {
            if !self.options.excludes.is_empty() {
                let full = format!(
                    "{}/{}",
                    self.tree.path_of(parent),
                    self.tree.name_of(name_ref)
                );
                if self.options.excludes.matches(&full) {
                    excluded = true;
                    // The C version does not descend into an excluded
                    // directory: its contents never show up in the listing.
                    skip_contents = is_dir;
                }
            }
            if is_dir && !excluded && !self.options.follow_firmlinks {
                // macOS: a directory reached through a firmlink. Its own size
                // still counts (dir_scan.c:133 does not zero FF_FRMLNK), only
                // the contents are skipped.
                if platform::is_firmlink(Path::new(self.tree.name_of(name_ref))) {
                    frmlnk = true;
                    skip_contents = true;
                }
            }
            if is_dir && !excluded && !skip_contents {
                if self.options.exclude_kernfs
                    && platform::is_kernfs(Path::new(self.tree.name_of(name_ref)))
                {
                    // Keep the entry, skip the contents and mark it '^'.
                    kernfs = true;
                    skip_contents = true;
                } else if self.options.one_file_system
                    && self.on_other_filesystem(&md, Path::new(self.tree.name_of(name_ref)))
                {
                    othfs = true;
                    skip_contents = true;
                } else if self.options.exclude_caches
                    && exclude::has_cachedir_tag(Path::new(self.tree.name_of(name_ref)))
                {
                    excluded = true;
                    skip_contents = true;
                }
            }
        }

        // Count a file with multiple hard links only once. If the platform
        // cannot provide a stable file identity, no de-duplication is attempted.
        // One file_info() call per entry: on Windows it opens a handle, so it
        // must not be repeated. The result is reused below for the H flag.
        let info = platform::file_info(&md, Path::new(self.tree.name_of(name_ref)));
        let dup = !is_dir
            && info.is_some_and(|i| i.nlink > 1)
            && info.is_some_and(|i| !self.links.insert((i.dev, i.ino)));
        // The C version keeps the sizes at zero for entries it does not scan
        // (src/dir_scan.c:133: FF_OTHFS, FF_EXL and FF_KERNFS all skip it).
        // A repeated hard link is *not* one of them: every link carries its own
        // size (only the totals count it once).
        let (own_size, own_asize) = if excluded || othfs || kernfs {
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
            excluded: false,
            othfs: false,
            kernfs: false,
            serr: false,
            hlnkc: false,
            ino: 0,
            nlink: 0,
            frmlnk: false,
            counted: true,
            ext: ext.unwrap_or_default(),
            has_ext: ext.is_some(),
        });
        self.tree.attach(Some(parent), idx);
        self.stats.entries += 1;
        self.bytes += own_size;
        {
            let n = &mut self.tree.nodes[idx];
            n.excluded = excluded;
            n.othfs = othfs;
            n.kernfs = kernfs;
            n.frmlnk = frmlnk;
            n.counted = !dup;
            // Hard link bookkeeping: the C version sets FF_HLNKC for non
            // directories with nlink > 1 and exports ino/hlnkc/nlink for them.
            if !is_dir {
                let links = info.map_or(1, |i| i.nlink);
                n.hlnkc = links > 1;
                n.nlink = links as u32;
                n.ino = info.map_or(0, |i| i.ino);
            }
        }

        if !is_dir {
            // Excluded files stay visible but are left out of the statistics.
            // A repeated hard link still counts as an item, but its size is
            // added only for the first link.
            if !excluded {
                let (size, asize) = if dup { (0, 0) } else { (own_size, own_asize) };
                self.add_to_parent(size, asize, 1);
            }
            return;
        }

        // Not ours to scan (other filesystem, cache directory): keep the entry
        // but skip its contents.
        if skip_contents {
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
            if !excluded {
                self.add_to_parent(own_size, own_asize, 1);
            }
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
                    excluded,
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

        // Anything that failed below this directory (a child that could not be
        // read, or a child that has such a descendant itself) makes this
        // directory's size unreliable: that is the C version's FF_SERR, drawn
        // as '.' (src/dir_scan.c returns the failure upwards).
        let serr = self.tree.nodes[node]
            .children
            .iter()
            .any(|&c| self.tree.nodes[c].error || self.tree.nodes[c].serr);

        let (size, asize) = {
            let n = &mut self.tree.nodes[node];
            n.size += frame.size;
            n.asize += frame.asize;
            n.items = frame.items;
            n.serr = serr;
            (n.size, n.asize)
        };

        if is_root {
            self.finish();
        } else if !frame.excluded {
            // An excluded subtree keeps its own totals (so it can be browsed)
            // but must not reach the ancestors: that is what "excluded from the
            // statistics" means in the C version.
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

    /// True when `md` lives on another filesystem than the scan root (`-x`).
    fn on_other_filesystem(&self, md: &fs::Metadata, path: &Path) -> bool {
        match (self.root_dev, platform::file_info(md, path)) {
            (Some(root), Some(info)) => info.dev != root,
            _ => false,
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
    let mut scanner = Scanner::new(path, ScanOptions::default())?;
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

    /// Hard links are counted once, with every link keeping its own size.
    /// Runs on all three CI platforms: Windows gets the file index and the link
    /// count from GetFileInformationByHandle() (see platform.rs), so this test
    /// is what proves the de-duplication works there.
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
        let mut scanner = Scanner::new(&tmp.0, ScanOptions::default()).unwrap();
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

        let mut scanner = Scanner::new(&tmp.0, ScanOptions::default()).unwrap();
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
            tree.nodes[0]
                .children
                .iter()
                .filter(|&&c| c == d_idx)
                .count(),
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
        let mut app = crate::app::App::new(tree, false, false, crate::theme::Theme::default());
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

    #[test]
    fn excludes_are_left_out_of_the_statistics() {
        let _guard = lock();
        let tmp = TempDir::new("excl");
        fs::create_dir(tmp.0.join("keep")).unwrap();
        fs::create_dir(tmp.0.join(".git")).unwrap();
        fs::write(tmp.0.join("keep/f"), vec![b'x'; 4_000]).unwrap();
        fs::write(tmp.0.join(".git/big"), vec![b'y'; 8_000]).unwrap();

        let (plain, _) = scan(&tmp.0).unwrap();
        let size_with = plain.nodes[0].size;
        let items_with = plain.nodes[0].items;
        let git_plain = find_child(&plain, 0, ".git");
        assert_eq!(plain.nodes[git_plain].children.len(), 1);

        let mut options = ScanOptions::default();
        options.excludes.add(".git");
        let mut scanner = Scanner::new(&tmp.0, options).unwrap();
        while !scanner.step(usize::MAX) {}
        let (tree, _) = scanner.into_result();

        // The excluded directory is still listed, but it neither counts nor
        // gets its contents scanned - same as the C version.
        let git = find_child(&tree, 0, ".git");
        assert!(tree.nodes[git].excluded);
        assert!(tree.nodes[git].children.is_empty());
        assert!(tree.nodes[0].size < size_with, "size must shrink");
        assert!(tree.nodes[0].items < items_with, "items must shrink");
        // An excluded entry keeps a size of zero, like the C version: it never
        // reaches the statistics (src/dir_scan.c:133 skips FF_EXL there), so
        // the listing shows "0.0 B" rather than the directory block.
        assert_eq!(tree.nodes[git].size, 0);
        assert_eq!(tree.nodes[git].asize, 0);
    }

    /// Firmlinks only exist on macOS (`--exclude-firmlinks`).
    ///
    /// macOS 11+ puts several system paths behind firmlinks onto the data
    /// volume; the list below is Apple's. Rather than insisting on one exact
    /// path (the layout has changed between releases), the test probes them all
    /// and reports what getattrlist() actually answered when none is found -
    /// which is the diagnostic needed if the detection itself is broken.
    #[cfg(target_os = "macos")]
    #[test]
    fn recognises_firmlinks_on_macos() {
        use crate::platform::is_firmlink;

        // /usr sits on the system volume and is never a firmlink.
        assert!(!is_firmlink(Path::new("/usr")), "/usr is not a firmlink");
        // A missing path makes getattrlist() fail, which counts as "not one".
        assert!(
            !is_firmlink(Path::new("/definitely/not/here")),
            "a missing path is not a firmlink"
        );

        let probes = ["/Users", "/Applications", "/private", "/opt", "/usr/local"];
        let mut report = String::new();
        let mut hits = 0;
        for p in probes {
            let yes = is_firmlink(Path::new(p));
            hits += usize::from(yes);
            // The raw syscall result, so a failure can be told apart from a
            // plain "not a firmlink".
            let plain = crate::platform::firmlink_path(Path::new(p), false);
            let extended = crate::platform::firmlink_path(Path::new(p), true);
            let show = |r: &Result<Vec<u8>, i32>| match r {
                Ok(bytes) => format!("ok({})", String::from_utf8_lossy(bytes)),
                Err(e) => format!("err({e})"),
            };
            report.push_str(&format!(
                "\n  {p} -> {yes}  plain={}  extended={}",
                show(&plain),
                show(&extended)
            ));
        }
        assert!(
            hits > 0,
            "none of the known firmlink paths was recognised:{report}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn follow_symlinks_counts_the_target_file() {
        let _guard = lock();
        let tmp = TempDir::new("follow");
        fs::create_dir(tmp.0.join("real")).unwrap();
        fs::write(tmp.0.join("real/f"), vec![b'x'; 5_000]).unwrap();
        std::os::unix::fs::symlink(tmp.0.join("real/f"), tmp.0.join("link")).unwrap();
        std::os::unix::fs::symlink(tmp.0.join("real"), tmp.0.join("dirlink")).unwrap();

        // Without -L the link itself is counted, and it is "notreg".
        let (tree, _) = scan(&tmp.0).unwrap();
        let link = find_child(&tree, 0, "link");
        assert!(tree.nodes[link].notreg, "a symlink is not a regular file");
        assert!(tree.nodes[link].asize < 5_000);

        // With -L the file it points at is counted instead.
        let options = ScanOptions {
            follow_symlinks: true,
            ..Default::default()
        };
        let mut scanner = Scanner::new(&tmp.0, options).unwrap();
        while !scanner.step(usize::MAX) {}
        let (tree, _) = scanner.into_result();
        let link = find_child(&tree, 0, "link");
        assert!(!tree.nodes[link].notreg, "-L turns it into a regular file");
        assert_eq!(tree.nodes[link].asize, 5_000, "-L counts the target's size");

        // A link to a directory is still not followed.
        let dirlink = find_child(&tree, 0, "dirlink");
        assert!(tree.nodes[dirlink].notreg);
        assert!(tree.nodes[dirlink].children.is_empty());
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
