/* ncdurs - importing an ncdu export file (cf. src/dir_import.c)
 *
 * The parser is deliberately small and forgiving: the format is a fixed
 * nesting of arrays and objects, and unknown fields are skipped. It follows
 * the same rules as the C implementation.
 *
 * Semantics that matter for compatibility:
 * - `asize`/`dsize` in the file hold the item's *own* size; the totals are
 *   accumulated here, exactly like a scan would, so a loaded tree behaves
 *   like a scanned one (sizes, item counts, hard link de-duplication).
 * - `items` is not part of the format; it is computed while loading.
 * - Items carrying `hlnkc`/`ino` are de-duplicated by (dev, ino); `dev` is
 *   inherited from the parent directory when the field is absent.
 * - Names are converted lossily when they are not valid UTF-8, matching what
 *   the scanner does.
 */

use std::collections::HashSet;
use std::io::Read;

use crate::scan::Tree;

/// Loads a whole export file into a tree.
pub fn read<R: Read>(mut input: R) -> Result<Tree, String> {
    let mut buf = Vec::new();
    input
        .read_to_end(&mut buf)
        .map_err(|e| format!("read error: {e}"))?;

    let mut p = Parser { buf: &buf, pos: 0 };
    let mut tree = Tree::new();
    let mut ctx = Ctx::default();

    // Header: "[<major>,<minor>,{metadata}," then the root directory item.
    p.skip_ws();
    p.expect(b'[')?;
    let major = p.u64()?;
    if major != 1 {
        return Err(format!(
            "unsupported export format (major version {major}, expected 1)"
        ));
    }
    p.skip_ws();
    p.expect(b',')?;
    let _minor = p.u64()?; // ignored, like in the C version
    p.skip_ws();
    p.expect(b',')?;
    p.skip_value()?; // metadata block (progname/progver/timestamp)
    p.skip_ws();
    p.expect(b',')?;
    p.skip_ws();
    if p.peek() != Some(b'[') {
        return Err("the top-level item must be a directory".to_string());
    }

    let root = parse_item(&mut p, &mut tree, None, 0, &mut ctx)?;
    debug_assert_eq!(root.idx, 0);

    p.skip_ws();
    p.expect(b']')?; // closes the top-level array
    p.skip_ws();
    if p.pos != p.buf.len() {
        return Err(p.err("trailing data after the document"));
    }
    Ok(tree)
}

/// One parsed item, with the totals of its subtree.
struct Item {
    idx: usize,
    size: u64,
    asize: u64,
    /// Entries in this subtree, excluding the item itself.
    items: u64,
}

#[derive(Default)]
struct Ctx {
    /// (dev, ino) of the hard links seen so far.
    links: HashSet<(u64, u64)>,
}

/// Parses one item: an object, optionally prefixed with '[' when it is a
/// directory (whose remaining array elements are its children).
fn parse_item(
    p: &mut Parser,
    tree: &mut Tree,
    parent: Option<usize>,
    parent_dev: u64,
    ctx: &mut Ctx,
) -> Result<Item, String> {
    p.skip_ws();
    let is_dir = p.peek() == Some(b'[');
    if is_dir {
        p.pos += 1;
    }

    let mut name: Option<String> = None;
    let mut asize = 0u64;
    let mut dsize = 0u64;
    let mut dev = parent_dev;
    let mut ino = 0u64;
    let mut hlnkc = false;
    let mut read_error = false;
    let mut notreg = false;
    let mut excluded = false;

    p.skip_ws();
    p.expect(b'{')?;
    let mut first = true;
    loop {
        p.skip_ws();
        if p.peek() == Some(b'}') {
            p.pos += 1;
            break;
        }
        if !first {
            p.expect(b',')?;
            p.skip_ws();
        }
        first = false;

        let key = p.parse_string()?;
        p.skip_ws();
        p.expect(b':')?;
        p.skip_ws();
        match key.as_str() {
            "name" => name = Some(p.parse_string()?),
            "asize" => asize = p.u64()?,
            "dsize" => dsize = p.u64()?,
            "dev" => dev = p.u64()?,
            "ino" => ino = p.u64()?,
            "hlnkc" => hlnkc = p.boolean()?,
            "nlink" => {
                if p.u64()? > 1 {
                    hlnkc = true;
                }
            }
            "read_error" => read_error = p.boolean()?,
            "notreg" => notreg = p.boolean()?,
            // The reason an item was left out: "pattern", "othfs" or "kernfs".
            "excluded" => {
                let _reason = p.parse_string()?;
                excluded = true;
            }
            // Extended information (-e: uid/gid/mode/mtime) is skipped for now.
            _ => p.skip_value()?,
        }
    }
    let name = name.ok_or_else(|| p.err("item without a \"name\" field"))?;

    // A file with multiple hard links is counted only once, like in a scan.
    let dup = hlnkc && !ctx.links.insert((dev, ino));
    let (own_size, own_asize) = if dup { (0, 0) } else { (dsize, asize) };

    let idx = tree.push_node(parent, &name, is_dir);
    tree.nodes[idx].error = read_error;
    tree.nodes[idx].notreg = notreg;
    tree.nodes[idx].excluded = excluded;

    if !is_dir {
        tree.nodes[idx].size = own_size;
        tree.nodes[idx].asize = own_asize;
        return Ok(Item {
            idx,
            size: own_size,
            asize: own_asize,
            items: 0,
        });
    }

    let (sub_size, sub_asize, sub_items) = parse_dir_items(p, tree, idx, dev, ctx)?;
    let size = own_size + sub_size;
    let asize = own_asize + sub_asize;
    let node = &mut tree.nodes[idx];
    node.size = size;
    node.asize = asize;
    node.items = sub_items;
    Ok(Item {
        idx,
        size,
        asize,
        items: sub_items,
    })
}

/// Parses the children of a directory (its own object and the opening '['
/// are already consumed) and returns the totals of those children.
fn parse_dir_items(
    p: &mut Parser,
    tree: &mut Tree,
    dir: usize,
    dev: u64,
    ctx: &mut Ctx,
) -> Result<(u64, u64, u64), String> {
    let mut size = 0u64;
    let mut asize = 0u64;
    let mut items = 0u64;
    loop {
        p.skip_ws();
        if p.peek() == Some(b']') {
            p.pos += 1;
            break;
        }
        // Every child is preceded by ',' - the directory's own object is the
        // first element of the array, so even the first child has one.
        p.expect(b',')?;
        p.skip_ws();

        let child = parse_item(p, tree, Some(dir), dev, ctx)?;
        size += child.size;
        asize += child.asize;
        items += child.items + 1;
    }
    Ok((size, asize, items))
}

struct Parser<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn err(&self, msg: &str) -> String {
        format!("{msg} (at byte {})", self.pos)
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", byte as char)))
        }
    }

    fn u64(&mut self) -> Result<u64, String> {
        self.skip_ws();
        let start = self.pos;
        let mut value = 0u64;
        while let Some(b) = self.peek() {
            if !b.is_ascii_digit() {
                break;
            }
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(b - b'0')))
                .ok_or_else(|| self.err("integer out of range"))?;
            self.pos += 1;
        }
        if self.pos == start {
            return Err(self.err("expected an integer"));
        }
        Ok(value)
    }

    fn boolean(&mut self) -> Result<bool, String> {
        if self.buf[self.pos..].starts_with(b"true") {
            self.pos += 4;
            Ok(true)
        } else if self.buf[self.pos..].starts_with(b"false") {
            self.pos += 5;
            Ok(false)
        } else {
            Err(self.err("expected true or false"))
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.skip_ws();
        self.expect(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let b = self
                .peek()
                .ok_or_else(|| self.err("unexpected end of input in a string"))?;
            match b {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    self.pos += 1;
                    let esc = self
                        .peek()
                        .ok_or_else(|| self.err("unexpected end of input after a backslash"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let cp = self.unicode_escape()?;
                            let ch = char::from_u32(cp)
                                .ok_or_else(|| self.err("invalid unicode escape"))?;
                            let mut tmp = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                        }
                        _ => return Err(self.err("invalid escape sequence")),
                    }
                }
                0..=0x1F | 0x7F => return Err(self.err("invalid character in a string")),
                _ => {
                    out.push(b);
                    self.pos += 1;
                }
            }
        }
        // Invalid UTF-8 is replaced, matching the lossy conversion used by the
        // scanner when it stores names.
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn unicode_escape(&mut self) -> Result<u32, String> {
        let high = self.hex4()?;
        if (0xDC00..0xE000).contains(&high) {
            return Err(self.err("unexpected low surrogate"));
        }
        if (0xD800..0xDC00).contains(&high) {
            // Surrogate pair, e.g. \uD83D\uDE00.
            if self.peek() != Some(b'\\') {
                return Err(self.err("expected a low surrogate"));
            }
            self.pos += 1;
            if self.peek() != Some(b'u') {
                return Err(self.err("expected a low surrogate"));
            }
            self.pos += 1;
            let low = self.hex4()?;
            if !(0xDC00..0xE000).contains(&low) {
                return Err(self.err("expected a low surrogate"));
            }
            return Ok(0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00));
        }
        Ok(high)
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let mut value = 0u32;
        for _ in 0..4 {
            let b = self
                .peek()
                .ok_or_else(|| self.err("unexpected end of input in a \\u escape"))?;
            let digit = (b as char)
                .to_digit(16)
                .ok_or_else(|| self.err("invalid \\u escape"))?;
            value = value * 16 + digit;
            self.pos += 1;
        }
        Ok(value)
    }

    /// Skips any JSON value; used for fields we do not model.
    fn skip_value(&mut self) -> Result<(), String> {
        self.skip_ws();
        match self.peek() {
            Some(b'"') => {
                self.parse_string()?;
                Ok(())
            }
            Some(b'{') => {
                self.pos += 1;
                self.skip_ws();
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    self.skip_ws();
                    self.parse_string()?; // key
                    self.skip_ws();
                    self.expect(b':')?;
                    self.skip_value()?;
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            break;
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
                Ok(())
            }
            Some(b'[') => {
                self.pos += 1;
                self.skip_ws();
                if self.peek() == Some(b']') {
                    self.pos += 1;
                    return Ok(());
                }
                loop {
                    self.skip_value()?;
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
                Ok(())
            }
            Some(b't') | Some(b'f') | Some(b'n') => {
                // true / false / null: consume the word, value is ignored.
                while let Some(b) = self.peek() {
                    if b.is_ascii_alphanumeric() {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Ok(())
            }
            Some(_) => {
                // Number; may be fractional (e.g. "mtime": 123.456).
                let start = self.pos;
                while let Some(b) = self.peek() {
                    if b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E') {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                if self.pos == start {
                    return Err(self.err("invalid JSON value"));
                }
                Ok(())
            }
            None => Err(self.err("unexpected end of input")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_small_document() {
        let json = r#"[1,2,{"progname":"ncdu","progver":"1.22","timestamp":0},
[{"name":"/tmp/demo","asize":4096,"dsize":4096},
[{"name":"sub","asize":4096,"dsize":4096},
{"name":"f1","asize":100,"dsize":512}],
{"name":"f2","asize":10,"dsize":512}]]
"#;
        let tree = read(json.as_bytes()).unwrap();
        assert_eq!(tree.nodes.len(), 4);
        assert_eq!(tree.name(0), "/tmp/demo");
        assert!(tree.nodes[0].is_dir);
        // Own sizes plus the accumulated children.
        assert_eq!(tree.nodes[0].size, 4096 + 4096 + 512 + 512);
        assert_eq!(tree.nodes[0].asize, 4096 + 4096 + 100 + 10);
        assert_eq!(tree.nodes[0].items, 3);
        assert_eq!(tree.nodes[0].children.len(), 2);

        let names: Vec<&str> = tree.nodes[0].children.iter().map(|&c| tree.name(c)).collect();
        assert_eq!(names, vec!["sub", "f2"]);

        let sub = tree.nodes[0].children[0];
        assert_eq!(tree.nodes[sub].size, 4096 + 512);
        assert_eq!(tree.nodes[sub].items, 1);
        assert_eq!(tree.name(tree.nodes[sub].children[0]), "f1");
    }

    #[test]
    fn decodes_escapes_and_surrogate_pairs() {
        let json = r#"[1,2,{},
[{"name":"/r"},
{"name":"a\"b\\c\td\u00e9\ud83d\ude00"}]]
"#;
        let tree = read(json.as_bytes()).unwrap();
        assert_eq!(tree.name(1), "a\"b\\c\tdé😀");
    }

    #[test]
    fn parses_notreg_and_skips_unknown_fields() {
        let json = r#"[1,2,{"whatever":[1,{"x":null}]},
[{"name":"/r","dsize":10,"excluded":"kernfs","notreg":true,"uid":0,"mtime":1.5},
{"name":"x","asize":7}]]
"#;
        let tree = read(json.as_bytes()).unwrap();
        assert_eq!(tree.nodes[0].size, 10 + 0);
        assert_eq!(tree.nodes[0].children.len(), 1);
        assert_eq!(tree.nodes[1].asize, 7);
        // `notreg` is understood (and is not an error); `excluded`, uid and
        // mtime are skipped for now.
        assert!(tree.nodes[0].notreg);
        assert!(!tree.nodes[0].error);
        assert!(!tree.nodes[1].notreg);
    }

    #[test]
    fn counts_hard_links_once() {
        let json = r#"[1,2,{},
[{"name":"/r"},
{"name":"f1","asize":100,"dsize":512,"ino":7,"hlnkc":true,"nlink":2},
{"name":"f2","asize":100,"dsize":512,"ino":7,"hlnkc":true,"nlink":2}]]
"#;
        let tree = read(json.as_bytes()).unwrap();
        assert_eq!(tree.nodes[0].size, 512); // counted once
        assert_eq!(tree.nodes[0].items, 2); // but both entries are listed
        assert_eq!(tree.nodes[2].size, 0);
    }

    #[test]
    fn rejects_broken_documents() {
        assert!(read(&b"not json"[..]).is_err());
        assert!(read(&b"[2,2,{},\n[{\"name\":\"/r\"}]]"[..]).is_err()); // major version
        assert!(read(&b"[1,2,{},\n[{\"dsize\":1}]]"[..]).is_err()); // missing name
        assert!(read(&b"[1,2,{},\n[{\"name\":\"/r\"}]\ngarbage"[..]).is_err());
    }
}
