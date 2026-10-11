/* ncdurs - scan phase
 *
 * Mirrors the model of the C implementation (src/dir_scan.c + src/dir_mem.c):
 * a directory's size is the sum of its own occupied space and the (already
 * accumulated) sizes of all its children. Hard links are counted only once by
 * keeping a global set of file identities. All OS specific details (occupied
 * size, file identity) live in platform.rs.
 */

use std::collections::HashSet;
use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::platform;

#[derive(Debug)]
pub struct Node {
    pub name: String,
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
}

pub struct Tree {
    pub nodes: Vec<Node>,
}

impl Tree {
    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn attach(&mut self, parent: Option<usize>, child: usize) {
        if let Some(p) = parent {
            self.nodes[p].children.push(child);
        }
    }
}

#[derive(Debug, Default)]
pub struct ScanStats {
    pub entries: u64,
    pub errors: u64,
}

/// Scans `path` and returns the resulting tree plus a few stats.
/// The root node (index 0) has the absolute path as its name.
pub fn scan(path: &Path) -> io::Result<(Tree, ScanStats)> {
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

    let mut tree = Tree { nodes: Vec::new() };
    let mut stats = ScanStats::default();
    let mut links = HashSet::new();
    add_entry(
        &mut tree,
        path,
        abs.to_string_lossy().into_owned(),
        None,
        &mut links,
        &mut stats,
    );
    Ok((tree, stats))
}

/// Adds `path` (named `name`) as a child of `parent` and recurses into it when
/// it is a directory. Returns the contribution of this entry to its parent:
/// (disk usage, apparent size, number of entries).
fn add_entry(
    tree: &mut Tree,
    path: &Path,
    name: String,
    parent: Option<usize>,
    links: &mut HashSet<(u64, u64)>,
    stats: &mut ScanStats,
) -> (u64, u64, u64) {
    let md = match fs::symlink_metadata(path) {
        Ok(md) => md,
        Err(_) => {
            stats.errors += 1;
            let idx = tree.push(Node {
                name,
                size: 0,
                asize: 0,
                items: 0,
                parent,
                children: Vec::new(),
                is_dir: false,
                error: true,
            });
            tree.attach(parent, idx);
            return (0, 0, 1);
        }
    };

    // Symlinks are not followed: symlink_metadata() reports the link itself,
    // so is_dir() is false for a symlink pointing to a directory.
    let is_dir = md.is_dir();

    // Count a file with multiple hard links only once. If the platform cannot
    // provide a stable file identity, no de-duplication is attempted.
    let dup = !is_dir
        && platform::nlink(&md) > 1
        && platform::file_id(&md).is_some_and(|id| !links.insert(id));
    let (own_size, own_asize) = if dup {
        (0, 0)
    } else {
        (platform::disk_usage(path, &md), md.len())
    };

    let idx = tree.push(Node {
        name,
        size: own_size,
        asize: own_asize,
        items: 0,
        parent,
        children: Vec::new(),
        is_dir,
        error: false,
    });
    tree.attach(parent, idx);
    stats.entries += 1;

    if !is_dir {
        return (own_size, own_asize, 1);
    }

    let mut size = own_size;
    let mut asize = own_asize;
    let mut items = 0;

    match fs::read_dir(path) {
        Ok(rd) => {
            for entry in rd {
                match entry {
                    Ok(entry) => {
                        let (s, a, i) = add_entry(
                            tree,
                            &entry.path(),
                            entry.file_name().to_string_lossy().into_owned(),
                            Some(idx),
                            links,
                            stats,
                        );
                        size += s;
                        asize += a;
                        items += i;
                    }
                    Err(_) => {
                        stats.errors += 1;
                        tree.nodes[idx].error = true;
                    }
                }
            }
        }
        Err(_) => {
            stats.errors += 1;
            tree.nodes[idx].error = true;
        }
    }

    tree.nodes[idx].size = size;
    tree.nodes[idx].asize = asize;
    tree.nodes[idx].items = items;
    // +1 counts this directory itself
    (size, asize, items + 1)
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
            .find(|&i| tree.nodes[i].name == name)
            .unwrap_or_else(|| panic!("child {} not found", name))
    }

    #[test]
    fn accumulates_child_sizes() {
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
        assert_eq!(root.is_dir, true);
        assert_eq!(f1.is_dir, false);
    }

    #[test]
    fn hard_links_counted_once() {
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
}
