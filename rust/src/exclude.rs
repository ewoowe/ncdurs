/* ncdurs - exclude patterns and the CACHEDIR.TAG check
 *
 * Mirrors src/exclude.c:
 *
 * - A pattern is matched with fnmatch(pattern, path, 0). Note the missing
 *   FNM_PATHNAME: a wildcard also crosses '/', so "*x*" can match "a/x/b".
 * - A path matches when the whole path or any of its suffixes - the part after
 *   a '/' - matches ("--exclude .git" hits "/home/u/proj/.git").
 * - --exclude-caches looks for a CACHEDIR.TAG with the official signature.
 */

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Name and signature of a cache directory tag (src/exclude.c:107-108).
pub const CACHEDIR_TAG: &str = "CACHEDIR.TAG";
const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// The `--exclude` patterns, in the order they were given.
#[derive(Default, Clone)]
pub struct Excludes {
    patterns: Vec<String>,
}

impl Excludes {
    pub fn add(&mut self, pattern: &str) {
        self.patterns.push(pattern.to_string());
    }

    /// exclude_addfile(): patterns from a file, one per line; blank lines and
    /// CR/LF endings are ignored.
    pub fn add_file(&mut self, file: &str) -> io::Result<()> {
        let text = std::fs::read_to_string(file)?;
        for line in text.lines() {
            let line = line.trim_end_matches(['\r', '\n']);
            if !line.is_empty() {
                self.add(line);
            }
        }
        Ok(())
    }

    /// True when no patterns were given; the scanner skips the matching work
    /// for every entry in that case.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// exclude_match() (src/exclude.c:76): the whole path, then every suffix
    /// that starts after a '/'.
    pub fn matches(&self, path: &str) -> bool {
        self.patterns.iter().any(|pattern| {
            glob_match(pattern, path)
                || path
                    .char_indices()
                    .filter(|&(_, c)| c == '/')
                    .any(|(i, _)| glob_match(pattern, &path[i + 1..]))
        })
    }
}

/// has_cachedir_tag() (src/exclude.c:110): the directory holds a CACHEDIR.TAG
/// that starts with the official signature.
pub fn has_cachedir_tag(dir: &Path) -> bool {
    let mut buf = [0u8; CACHEDIR_SIGNATURE.len()];
    match File::open(dir.join(CACHEDIR_TAG)).and_then(|mut f| f.read_exact(&mut buf)) {
        Ok(()) => buf == CACHEDIR_SIGNATURE,
        Err(_) => false,
    }
}

/// fnmatch(pattern, text, 0): `*` matches any run of characters (including
/// '/'), `?` exactly one, `[...]` a character class with `!`/`^` negation and
/// ranges, and `\` escapes the next character.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    glob_rec(&pattern, &text)
}

fn glob_rec(pattern: &[char], text: &[char]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    while pi < pattern.len() {
        match pattern[pi] {
            '*' => {
                // Try every split; repeated '*' just wastes a few steps.
                for skip in ti..=text.len() {
                    if glob_rec(&pattern[pi + 1..], &text[skip..]) {
                        return true;
                    }
                }
                return false;
            }
            '?' => {
                if ti >= text.len() {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
            '[' => match class_match(pattern, pi, text.get(ti).copied()) {
                Some((matched, next)) => {
                    if !matched {
                        return false;
                    }
                    pi = next;
                    ti += 1;
                }
                // An unterminated '[' is a literal, like in fnmatch.
                None => {
                    if text.get(ti) != Some(&'[') {
                        return false;
                    }
                    pi += 1;
                    ti += 1;
                }
            },
            '\\' => {
                pi += 1;
                if pi < pattern.len() {
                    if text.get(ti) != Some(&pattern[pi]) {
                        return false;
                    }
                    pi += 1;
                    ti += 1;
                }
            }
            c => {
                if text.get(ti) != Some(&c) {
                    return false;
                }
                pi += 1;
                ti += 1;
            }
        }
    }
    ti == text.len()
}

/// Matches a `[...]` class that starts at `pattern[start]`, returning whether
/// it matched `c` plus the index just past the closing `]`. None means the
/// class is not terminated.
fn class_match(pattern: &[char], start: usize, c: Option<char>) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negate = matches!(pattern.get(i), Some('!') | Some('^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    loop {
        let &ch = pattern.get(i)?;
        if ch == ']' && !first {
            i += 1;
            break;
        }
        first = false;
        // A range like a-z, but a trailing '-' before ']' is a literal.
        if pattern.get(i + 1) == Some(&'-') && pattern.get(i + 2).is_some_and(|&e| e != ']') {
            let high = pattern[i + 2];
            if c.is_some_and(|c| ch <= c && c <= high) {
                matched = true;
            }
            i += 3;
        } else {
            if c == Some(ch) {
                matched = true;
            }
            i += 1;
        }
    }
    Some((matched != negate, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(list: &[&str]) -> Excludes {
        let mut e = Excludes::default();
        for p in list {
            e.add(p);
        }
        e
    }

    #[test]
    fn matches_whole_paths_and_suffixes() {
        let e = patterns(&[".git"]);
        assert!(e.matches("/home/u/project/.git"));
        assert!(e.matches(".git"));
        assert!(!e.matches("/home/u/project/.github"));
        // The *contents* of an excluded directory do not match by themselves;
        // the scanner simply stops counting once a parent is excluded.
        assert!(!e.matches("/home/u/project/.git/objects"));

        let e = patterns(&["*.log", "*.tmp"]);
        assert!(e.matches("/var/log/sys.log"));
        assert!(e.matches("build.tmp"));
        assert!(!e.matches("/var/log/sys.txt"));
    }

    #[test]
    fn star_crosses_slashes_like_fnmatch_without_pathname() {
        let e = patterns(&["/var/*/sys.log"]);
        assert!(e.matches("/var/a/b/sys.log"));
        assert!(e.matches("/var/x/sys.log"));
        assert!(!e.matches("/var/sys.log"));
    }

    #[test]
    fn supports_question_marks_classes_and_escapes() {
        let e = patterns(&["file?.log"]);
        assert!(e.matches("/x/file1.log"));
        assert!(!e.matches("/x/file12.log"));

        let e = patterns(&["file[0-9].log"]);
        assert!(e.matches("/x/file7.log"));
        assert!(!e.matches("/x/filex.log"));

        let e = patterns(&["file[!0-9].log"]);
        assert!(e.matches("/x/filex.log"));
        assert!(!e.matches("/x/file7.log"));

        // A literal asterisk has to be escaped.
        let e = patterns(&["star\\*name"]);
        assert!(e.matches("/x/star*name"));
        assert!(!e.matches("/x/starXname"));
    }
}
