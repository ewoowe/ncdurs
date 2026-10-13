/* ncdurs - exporting the tree in ncdu's JSON format (cf. src/dir_export.c)
 *
 * The output is interchangeable with the C version, e.g.
 * `ncdurs -o - /usr | ncdu -f -` and the other way around both work.
 *
 * Format notes - all of this is load-bearing for compatibility, see
 * src/dir_export.c and src/dir_import.c for the reference implementation:
 *
 * - The document starts with `[1,2,{"progname":...}` (major/minor format
 *   version and a metadata block) and ends with `]` closing that array.
 * - The root item follows the header and is always a directory; directories
 *   are nested arrays whose first element is the directory's own object,
 *   followed by its children.
 * - `asize`/`dsize` hold the size of the item *itself*, not the subtree
 *   total: the C version accumulates while importing. Our tree stores the
 *   totals, so the own size is derived by subtracting the children.
 * - Fields are omitted when zero/absent, exactly like the C writer.
 */

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::scan::Tree;
use crate::{NAME, VERSION};

/// Writes the whole tree as a single JSON document.
pub fn write<W: Write>(tree: &Tree, out: &mut W) -> io::Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    write!(
        out,
        "[1,2,{{\"progname\":\"{NAME}\",\"progver\":\"{VERSION}\",\"timestamp\":{timestamp}}}"
    )?;
    write_item(tree, 0, out)?;
    out.write_all(b"]")?;
    out.flush()
}

/// Writes one item (and, for directories, its whole subtree) in depth-first
/// pre-order, mirroring the C writer's output layout.
fn write_item<W: Write>(tree: &Tree, idx: usize, out: &mut W) -> io::Result<()> {
    let node = &tree.nodes[idx];
    let (own_size, own_asize) = own_sizes(tree, idx);

    out.write_all(b",\n")?;
    if node.is_dir {
        out.write_all(b"[")?;
    }

    out.write_all(b"{\"name\":\"")?;
    write_escaped(out, tree.name(idx))?;
    out.write_all(b"\"")?;

    // An excluded item carries no sizes at all, only the reason - that is how
    // the C writer marks them: {"name":".git","excluded":"pattern"}.
    if node.excluded {
        if node.othfs {
            out.write_all(b",\"excluded\":\"othfs\"")?;
        } else {
            out.write_all(b",\"excluded\":\"pattern\"")?;
        }
    } else {
        // Zero sizes are omitted, like the C writer does for items that could
        // not be stat'ed.
        if own_asize > 0 {
            write!(out, ",\"asize\":{own_asize}")?;
        }
        if own_size > 0 {
            write!(out, ",\"dsize\":{own_size}")?;
        }
    }
    // The `-e` fields, in the same order as the C writer puts them.
    if node.has_ext {
        write!(
            out,
            ",\"uid\":{},\"gid\":{},\"mode\":{},\"mtime\":{}",
            node.ext.uid, node.ext.gid, node.ext.mode, node.ext.mtime
        )?;
    }
    if node.error {
        out.write_all(b",\"read_error\":true")?;
    }
    // Like the C writer: items that failed to stat are "unknown" here, so
    // notreg is only written for items that were actually inspected.
    if node.notreg && !node.error {
        out.write_all(b",\"notreg\":true")?;
    }
    out.write_all(b"}")?;

    if node.is_dir {
        for &child in &node.children {
            write_item(tree, child, out)?;
        }
        out.write_all(b"]")?;
    }
    Ok(())
}

/// Size of an item excluding its children (the tree stores the subtree totals).
fn own_sizes(tree: &Tree, idx: usize) -> (u64, u64) {
    let node = &tree.nodes[idx];
    let mut child_size = 0u64;
    let mut child_asize = 0u64;
    for &child in &node.children {
        // Excluded children never reached their parent's totals, so they must
        // not be subtracted from them here either.
        if tree.nodes[child].excluded {
            continue;
        }
        child_size += tree.nodes[child].size;
        child_asize += tree.nodes[child].asize;
    }
    (
        node.size.saturating_sub(child_size),
        node.asize.saturating_sub(child_asize),
    )
}

/// Same escaping rules as the C writer's output_string().
fn write_escaped<W: Write>(out: &mut W, value: &str) -> io::Result<()> {
    for &b in value.as_bytes() {
        match b {
            b'\n' => out.write_all(b"\\n")?,
            b'\r' => out.write_all(b"\\r")?,
            0x08 => out.write_all(b"\\b")?,
            b'\t' => out.write_all(b"\\t")?,
            0x0C => out.write_all(b"\\f")?,
            b'\\' => out.write_all(b"\\\\")?,
            b'"' => out.write_all(b"\\\"")?,
            b if b <= 31 || b == 127 => write!(out, "\\u00{b:02x}")?,
            b => out.write_all(&[b])?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import;

    /// A small tree built by hand; sizes are the accumulated totals, exactly
    /// as a scan would leave them.
    fn sample_tree() -> Tree {
        let mut tree = Tree::new();
        let root = tree.push_node(None, "/demo", true);
        let sub = tree.push_node(Some(root), "sub", true);
        let f1 = tree.push_node(Some(sub), "f1", false);
        let f2 = tree.push_node(Some(root), "link\"quote", false);
        let link = tree.push_node(Some(root), "symlink", false);

        tree.nodes[f1].size = 512;
        tree.nodes[f1].asize = 100;
        tree.nodes[f2].size = 512;
        tree.nodes[f2].asize = 10;
        // A symlink: apparent size is the target length, no disk blocks.
        tree.nodes[link].asize = 9;
        tree.nodes[link].notreg = true;
        tree.nodes[sub].size = 4096 + 512;
        tree.nodes[sub].asize = 4096 + 100;
        tree.nodes[sub].items = 1;
        tree.nodes[root].size = 4096 + tree.nodes[sub].size + tree.nodes[f2].size;
        tree.nodes[root].asize = 4096 + tree.nodes[sub].asize + tree.nodes[f2].asize + 9;
        tree.nodes[root].items = 4;
        tree
    }

    #[test]
    fn writes_the_c_format() {
        let mut out = Vec::new();
        write(&sample_tree(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();

        assert!(text.starts_with("[1,2,{\"progname\":\"ncdurs\""));
        assert!(text.ends_with("]]"));
        // The root object carries its own size, not the subtree total.
        assert!(text.contains("{\"name\":\"/demo\",\"asize\":4096,\"dsize\":4096}"));
        // Directories are arrays whose first element is their own object.
        assert!(text.contains("[{\"name\":\"sub\","));
        // Children carry their own size as well.
        assert!(text.contains("{\"name\":\"f1\",\"asize\":100,\"dsize\":512}"));
        // Quotes/backslashes are escaped.
        assert!(text.contains("link\\\"quote"));
        // Non-regular files keep their flag.
        assert!(text.contains("{\"name\":\"symlink\",\"asize\":9,\"notreg\":true}"));
    }

    #[test]
    fn round_trips_through_the_importer() {
        let mut out = Vec::new();
        write(&sample_tree(), &mut out).unwrap();

        let original = sample_tree();
        let back = import::read(&out[..]).unwrap();

        assert_eq!(back.nodes.len(), original.nodes.len());
        for i in 0..original.nodes.len() {
            assert_eq!(back.name(i), original.name(i), "name of node {i}");
        }
        assert_eq!(back.nodes[0].size, original.nodes[0].size);
        assert_eq!(back.nodes[0].asize, original.nodes[0].asize);
        assert_eq!(back.nodes[0].items, original.nodes[0].items);
        assert_eq!(back.nodes[1].items, original.nodes[1].items);
        assert!(back.nodes[0].is_dir);
        assert!(!back.nodes[3].is_dir);
        // `notreg` survives the round trip ...
        assert!(back.nodes[4].notreg);
        assert!(!back.nodes[3].notreg);

        // ... but a failed-to-stat item does not carry it (the C writer treats
        // such items as "unknown" and omits the field).
        let mut broken = sample_tree();
        broken.nodes[3].error = true;
        broken.nodes[3].notreg = true;
        let mut out = Vec::new();
        write(&broken, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains(
            "{\"name\":\"link\\\"quote\",\"asize\":10,\"dsize\":512,\"read_error\":true}"
        ));
    }
}
