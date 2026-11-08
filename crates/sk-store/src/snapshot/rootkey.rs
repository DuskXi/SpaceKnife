//! The key of a set of scan roots exactly as SpaceKnife 0.3.x (the Python backend) made it:
//! `<label>-<the first 12 hex digits of a SHA-1>`. It names the auto-save snapshot folder, the
//! tag file (`tags/<key>.json`), the palette's jumps of a scan target and the 「本机磁盘」 page's
//! history of a volume, so it must stay what 0.3.x wrote, byte for byte, or all of them are lost
//! on the upgrade.
//!
//! Python computed, with CPython 3.12:
//!
//! ```python
//! norm = "\n".join(sorted(os.path.normcase(os.path.normpath(r)) for r in roots))
//! digest = hashlib.sha1(norm.encode("utf-8", "surrogatepass")).hexdigest()[:12]
//! label = "".join(ch if ch.isalnum() else "_" for ch in os.path.basename(roots[0].rstrip("\\/")) or roots[0])
//! key = f"{label.strip('_')[:24] or 'root'}-{digest}"
//! ```
//!
//! so this module ports, not approximates, each piece: `normpath` (CPython's
//! `_Py_normpath_and_size`), `normcase` (Windows: `LCMapStringEx` with the invariant locale's
//! lower case, which is what `ntpath.normcase` calls; elsewhere the path itself), `basename`
//! (`ntpath.splitroot` on Windows) and `isalnum` (Python 3.12's table, [`super::py_alnum`]).
//! The path rules are a parameter ([`Rules`]), so both are tested everywhere. These are only for
//! the key; path comparisons elsewhere use [`crate::names`].
//!
//! The roots are the system's own strings: what Python hashed was the text it made of a path
//! ([`py_bytes`]), which for a path no UTF-8 text can hold (a lone surrogate on Windows, bytes that
//! are not UTF-8 on POSIX) is not what a lossy `String` of it holds.

use std::ffi::OsStr;

use sha1::{Digest, Sha1};

use super::py_alnum::RANGES;

/// Whose path rules: Windows (`\` and `/` both separators, drives, `\\` prefixes, case folded)
/// or POSIX.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rules {
    pub windows: bool,
}

impl Rules {
    pub(crate) const NATIVE: Rules = Rules {
        windows: cfg!(windows),
    };

    fn is_sep(self, b: u8) -> bool {
        b == b'/' || (self.windows && b == b'\\')
    }

    fn sep(self) -> u8 {
        if self.windows { b'\\' } else { b'/' }
    }

    /// Windows: the bytes of a drive (`C:`) the path starts with. CPython looks at its second
    /// UTF-16 unit, so the first character must be one unit (not one beyond the BMP).
    fn drive_len(self, path: &[u8]) -> Option<usize> {
        if !self.windows {
            return None;
        }
        let first = match *path.first()? {
            0 => return None,
            b if b < 0x80 => 1,
            b if b & 0xE0 == 0xC0 => 2,
            b if b & 0xF0 == 0xE0 => 3,
            _ => return None, // four bytes: two UTF-16 units
        };
        (path.get(first) == Some(&b':')).then_some(first + 1)
    }

    /// `os.path.normpath` of Python 3.12 (its C implementation, `_Py_normpath_and_size`, which
    /// `ntpath` and `posixpath` both use), on the bytes of a path. Separators and dots are
    /// ASCII, so working on UTF-8 / WTF-8 bytes gives what working on the code points gave.
    pub(crate) fn normpath(self, path: &[u8]) -> Vec<u8> {
        let sep = self.sep();
        let mut out: Vec<u8> = Vec::with_capacity(path.len());
        let at = |i: usize| path.get(i).copied();
        let sep_at = |i: usize| at(i).is_some_and(|b| self.is_sep(b));
        let end_at = |i: usize| i >= path.len();
        let mut p1 = 0usize;
        let mut min_p2 = 0usize;
        let mut last: u8 = 0;
        if path.is_empty() {
            return b".".to_vec();
        }
        if at(0) == Some(b'.') && sep_at(1) {
            // skip a leading `./` (and the separators after it)
            p1 = 2;
            while sep_at(p1) {
                p1 += 1;
            }
            last = sep;
        } else if let Some(drive) = self.drive_len(path) {
            // the drive
            out.extend_from_slice(&path[..drive]);
            p1 = drive;
            min_p2 = drive;
            last = b':';
        } else if self.windows && sep_at(0) && sep_at(1) {
            // `\\server\share\`, `\\?\`, `\\.\`: the prefix up to its second separator after `\\`
            out.push(sep);
            out.push(sep);
            p1 = 2;
            let mut seps = 2;
            while !end_at(p1) && seps > 0 {
                if sep_at(p1) {
                    seps -= 1;
                    out.push(sep);
                    last = sep;
                } else {
                    out.push(path[p1]);
                    last = path[p1];
                }
                p1 += 1;
            }
            min_p2 = out.len() - 1;
        } else if !self.windows && sep_at(0) && sep_at(1) && !sep_at(2) {
            // exactly two leading separators stay (POSIX leaves their meaning to the system)
            out.extend_from_slice(&path[..2]);
            p1 = 2;
            min_p2 = 1;
            last = sep;
        }
        while !end_at(p1) {
            let mut c = path[p1];
            if self.is_sep(c) {
                c = sep;
            }
            if last == sep {
                if c == b'.' {
                    let sep_or_end = |i: usize| end_at(i) || sep_at(i);
                    let sep_at_1 = sep_or_end(p1 + 1);
                    let sep_at_2 = !sep_at_1 && sep_or_end(p1 + 2);
                    if sep_at_2 && at(p1 + 1) == Some(b'.') {
                        // `..`: drop the part before it, if there is one to drop
                        let p2 = out.len();
                        let mut p3 = p2;
                        while p3 != min_p2 {
                            p3 -= 1;
                            if out[p3] != sep {
                                break;
                            }
                        }
                        while p3 != min_p2 && out[p3 - 1] != sep {
                            p3 -= 1;
                        }
                        let parent_is_dotdot = out.get(p3) == Some(&b'.')
                            && out.get(p3 + 1) == Some(&b'.')
                            && out.get(p3 + 2).is_some_and(|&b| self.is_sep(b));
                        if p2 == min_p2 || parent_is_dotdot {
                            out.extend_from_slice(b"..");
                            last = b'.';
                        } else if out.get(p3) == Some(&sep) {
                            out.truncate(p3 + 1); // absolute: the part goes, its separator stays
                        } else {
                            out.truncate(p3);
                        }
                        p1 += 1;
                    } else if sep_at_1 {
                        // `.`: nothing
                    } else {
                        out.push(c);
                        last = c;
                    }
                } else if c == sep {
                    // a separator after a separator: nothing
                } else {
                    out.push(c);
                    last = c;
                }
            } else {
                out.push(c);
                last = c;
            }
            p1 += 1;
        }
        // no trailing separators, except where the prefix ends
        if out.len() != min_p2 {
            while out.len() > min_p2 + 1 && out.last() == Some(&sep) {
                out.pop();
            }
        }
        if out.is_empty() {
            out.push(b'.');
        }
        out
    }

    /// `os.path.normcase` of Python 3.12: with Windows' rules the lower case of the invariant
    /// locale (`LCMapStringEx`, so only on Windows itself) with `/` as `\`; POSIX's leave the
    /// path as it is.
    pub(crate) fn normcase(self, path: &[u8]) -> Vec<u8> {
        if !self.windows || path.is_empty() {
            return path.to_vec();
        }
        #[cfg(windows)]
        {
            let wide: Vec<u16> = crate::names::wtf8_to_wide(path)
                .into_iter()
                .map(|u| if u == b'/' as u16 { b'\\' as u16 } else { u })
                .collect();
            match lcmap_lowercase(&wide) {
                Some(lower) => wide_to_wtf8(&lower),
                // cannot happen for valid input; the case as given beats no key at all
                None => wide_to_wtf8(&wide),
            }
        }
        #[cfg(not(windows))]
        {
            // Windows' own lower case is not to be had here: ASCII is what can be done
            path.iter()
                .map(|&b| {
                    if b == b'/' {
                        b'\\'
                    } else {
                        b.to_ascii_lowercase()
                    }
                })
                .collect()
        }
    }

    /// `os.path.basename` of Python 3.12: with Windows' rules what `ntpath.splitroot` leaves of
    /// the path after its last separator (so a bare `\\server\share` or `C:` has none); POSIX's,
    /// what follows the last `/`.
    fn basename(self, p: &str) -> &str {
        let tail = if self.windows { splitroot_tail(p) } else { p };
        match tail.rfind(|c: char| c == '/' || (self.windows && c == '\\')) {
            Some(i) => &tail[i + 1..],
            None => tail,
        }
    }

    /// The readable part of a key: the first root's last name, every character Python 3.12 does
    /// not call alphanumeric as `_`, without `_` at either end, at most 24 characters (`root` if
    /// nothing is left).
    fn label(self, first: &str) -> String {
        let trimmed = first.trim_end_matches(['\\', '/']);
        let base = self.basename(trimmed);
        let base = if base.is_empty() { first } else { base };
        let label: String = base
            .chars()
            .map(|c| if py_isalnum(c) { c } else { '_' })
            .collect();
        let label: String = label.trim_matches('_').chars().take(24).collect();
        if label.is_empty() {
            "root".to_string()
        } else {
            label
        }
    }

    /// The key of roots that are text (see the module docs).
    #[cfg(test)]
    pub(crate) fn root_key(self, roots: &[String]) -> String {
        let py: Vec<Vec<u8>> = roots.iter().map(|r| r.as_bytes().to_vec()).collect();
        self.root_key_py(&py)
    }

    /// The key of roots given as the text Python had of them, encoded with surrogatepass
    /// ([`py_bytes`]).
    pub(crate) fn root_key_py(self, roots: &[Vec<u8>]) -> String {
        let mut norm: Vec<Vec<u8>> = roots
            .iter()
            .map(|r| self.normcase(&self.normpath(r)))
            .collect();
        // Python sorted the strings by code point: the order of their (generalised) UTF-8 bytes
        norm.sort();
        let digest = Sha1::digest(norm.join(&b'\n'));
        let hex: String = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
        let first = roots.first().map_or_else(String::new, |r| py_chars(r));
        format!("{}-{hex}", self.label(&first))
    }
}

/// The key of `roots` (the system's own strings) with this system's path rules.
pub(crate) fn root_key<S: AsRef<OsStr>>(roots: &[S]) -> String {
    let py: Vec<Vec<u8>> = roots.iter().map(|r| py_bytes(r.as_ref())).collect();
    Rules::NATIVE.root_key_py(&py)
}

/// The text 0.3.x had of a path from the system, as `str.encode("utf-8", "surrogatepass")`:
/// on Windows its UTF-16 units (a valid pair one character, a lone surrogate a code point of its
/// own: WTF-8); on POSIX `os.fsdecode`, so each byte that is not UTF-8 is U+DC80..U+DCFF
/// ([`surrogateescape`]). Text that is valid Unicode is its UTF-8 on both.
pub(crate) fn py_bytes(root: &OsStr) -> Vec<u8> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = root.encode_wide().collect();
        wide_to_wtf8(&wide)
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        surrogateescape(root.as_bytes())
    }
    #[cfg(not(any(windows, unix)))]
    {
        root.to_string_lossy().into_owned().into_bytes()
    }
}

/// `raw.decode("utf-8", "surrogateescape").encode("utf-8", "surrogatepass")`: the valid UTF-8
/// stays, every other byte `b` becomes U+DC00 + `b` in its 3-byte form. Python's decoder and
/// [`<[u8]>::utf8_chunks`] agree on what is valid (strict UTF-8: no surrogates, no overlong
/// forms, nothing past U+10FFFF), and each byte outside it is escaped on its own, so where an
/// invalid run is cut does not matter.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn surrogateescape(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 2);
    for chunk in raw.utf8_chunks() {
        out.extend_from_slice(chunk.valid().as_bytes());
        for &b in chunk.invalid() {
            push_utf8(&mut out, 0xDC00 + u32::from(b));
        }
    }
    out
}

/// The characters of generalised UTF-8 (what [`py_bytes`] gives), one per code point, a
/// surrogate as U+FFFD: like a surrogate to Python's `isalnum` (not alphanumeric) and to the
/// path rules (not a separator), so the label is the one Python made.
fn py_chars(py: &[u8]) -> String {
    char::decode_utf16(crate::names::wtf8_to_wide(py))
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

#[cfg(windows)]
fn lcmap_lowercase(src: &[u16]) -> Option<Vec<u16>> {
    use windows_sys::Win32::Globalization::{LCMAP_LOWERCASE, LCMapStringEx};
    // LOCALE_NAME_INVARIANT is the empty string
    let locale: [u16; 1] = [0];
    let len = i32::try_from(src.len()).ok()?;
    // SAFETY: src is `len` UTF-16 units; a null destination asks for the size.
    let need = unsafe {
        LCMapStringEx(
            locale.as_ptr(),
            LCMAP_LOWERCASE,
            src.as_ptr(),
            len,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    };
    if need <= 0 {
        return None;
    }
    let mut out = vec![0u16; need as usize];
    // SAFETY: `out` has room for `need` units, as the first call said.
    let got = unsafe {
        LCMapStringEx(
            locale.as_ptr(),
            LCMAP_LOWERCASE,
            src.as_ptr(),
            len,
            out.as_mut_ptr(),
            need,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    };
    if got <= 0 {
        return None;
    }
    out.truncate(got as usize);
    Some(out)
}

/// UTF-16 (lone surrogates allowed) to WTF-8, as Python's `encode("utf-8", "surrogatepass")`.
#[cfg_attr(not(windows), allow(dead_code))]
fn wide_to_wtf8(wide: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(wide.len() * 3);
    let mut i = 0;
    while i < wide.len() {
        let u = wide[i] as u32;
        let cp = if (0xD800..0xDC00).contains(&u)
            && wide
                .get(i + 1)
                .is_some_and(|&l| (0xDC00..0xE000).contains(&(l as u32)))
        {
            let l = wide[i + 1] as u32;
            i += 1;
            0x10000 + ((u - 0xD800) << 10) + (l - 0xDC00)
        } else {
            u
        };
        push_utf8(&mut out, cp);
        i += 1;
    }
    out
}

/// One code point as (generalised) UTF-8: surrogates are encoded like any other code point.
fn push_utf8(out: &mut Vec<u8>, cp: u32) {
    if cp < 0x80 {
        out.push(cp as u8);
    } else if cp < 0x800 {
        out.extend_from_slice(&[0xC0 | (cp >> 6) as u8, 0x80 | (cp & 0x3F) as u8]);
    } else if cp < 0x10000 {
        out.extend_from_slice(&[
            0xE0 | (cp >> 12) as u8,
            0x80 | ((cp >> 6) & 0x3F) as u8,
            0x80 | (cp & 0x3F) as u8,
        ]);
    } else {
        out.extend_from_slice(&[
            0xF0 | (cp >> 18) as u8,
            0x80 | ((cp >> 12) & 0x3F) as u8,
            0x80 | ((cp >> 6) & 0x3F) as u8,
            0x80 | (cp & 0x3F) as u8,
        ]);
    }
}

/// Python 3.12's `str.isalnum()` of one character.
fn py_isalnum(c: char) -> bool {
    let cp = c as u32;
    if cp < 0x80 {
        return c.is_ascii_alphanumeric();
    }
    RANGES
        .binary_search_by(|&(a, b)| {
            if b < cp {
                std::cmp::Ordering::Less
            } else if a > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// The tail `ntpath.splitroot` gives (what follows the drive and the root). Python indexes the
/// string by code point, so this does too.
fn splitroot_tail(p: &str) -> &str {
    // (byte offset, character with `/` as `\`)
    let norm: Vec<(usize, char)> = p
        .char_indices()
        .map(|(i, c)| (i, if c == '/' { '\\' } else { c }))
        .collect();
    let ch = |k: usize| norm.get(k).map(|&(_, c)| c);
    let from = |k: usize| norm.get(k).map_or(p.len(), |&(i, _)| i);
    let find = |start: usize| (start..norm.len()).find(|&k| norm[k].1 == '\\');
    if ch(0) == Some('\\') {
        if ch(1) == Some('\\') {
            // UNC drives (`\\server\share`, `\\?\UNC\server\share`) and devices (`\\?\C:`)
            let unc: Vec<char> = "\\\\?\\UNC\\".chars().collect();
            let start = if norm.len() >= 8
                && norm[..8]
                    .iter()
                    .zip(&unc)
                    .all(|(&(_, c), u)| c.to_uppercase().eq(std::iter::once(*u)))
            {
                8
            } else {
                2
            };
            let Some(index) = find(start) else { return "" };
            let Some(index2) = find(index + 1) else {
                return "";
            };
            return &p[from(index2 + 1)..];
        }
        return &p[from(1)..]; // `\Windows`: rooted, no drive
    }
    if ch(1) == Some(':') {
        return if ch(2) == Some('\\') {
            &p[from(3)..]
        } else {
            &p[from(2)..]
        };
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: Rules = Rules { windows: true };
    const POSIX: Rules = Rules { windows: false };

    fn np(rules: Rules, p: &str) -> String {
        String::from_utf8(rules.normpath(p.as_bytes())).unwrap()
    }

    #[test]
    fn normpath_like_python() {
        for (input, want) in [
            ("C:\\", "C:\\"),
            ("C:", "C:"),
            ("c:/data/", "c:\\data"),
            ("C:\\a\\\\b\\.\\c\\..\\d", "C:\\a\\b\\d"),
            ("\\\\server\\share", "\\\\server\\share"),
            ("\\\\server\\share\\", "\\\\server\\share\\"),
            ("//server/share/x/", "\\\\server\\share\\x"),
            ("C:\\..", "C:\\"),
            ("a\\..\\..", ".."),
            ("./", "."),
            ("", "."),
        ] {
            assert_eq!(np(WIN, input), want, "{input:?}");
        }
        for (input, want) in [
            ("/", "/"),
            ("//", "//"),
            ("///", "/"),
            ("//a//b/", "//a/b"),
            ("///a/b", "/a/b"),
            ("/a/./b/../c/", "/a/c"),
            ("/..", "/"),
            ("a/../..", ".."),
            ("./a", "a"),
            ("a\\b", "a\\b"),
            ("", "."),
        ] {
            assert_eq!(np(POSIX, input), want, "{input:?}");
        }
    }

    #[test]
    fn labels_like_python() {
        for rules in [WIN, POSIX] {
            assert_eq!(rules.label("/home/lenny/下载"), "下载");
            assert_eq!(rules.label("/x/___"), "root");
            assert_eq!(rules.label(&format!("/{}", "a".repeat(40))), "a".repeat(24));
            // a combining mark is not alphanumeric to Python (Rust's is_alphanumeric says it is)
            assert_eq!(rules.label("/cafe\u{301}"), "cafe");
            assert_eq!(rules.label("/Ⓐ x"), "x");
        }
        assert_eq!(WIN.label("\\\\server\\share"), "server_share");
        assert_eq!(WIN.label("C:\\"), "C");
        assert_eq!(POSIX.label("/a/b\\c"), "b_c");
    }

    fn fixture_keys(platform: &str) -> Vec<(Vec<String>, String)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sk-core/tests/data/v034")
            .join(platform)
            .join("root_keys.json");
        let cases: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        cases
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    serde_json::from_value(c["roots"].clone()).unwrap(),
                    c["key"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    /// POSIX's rules on every system: the keys 0.3.4 gave on Linux.
    #[test]
    fn posix_keys_of_0_3_4() {
        for (roots, want) in fixture_keys("posix") {
            assert_eq!(POSIX.root_key(&roots), want, "{roots:?}");
        }
    }

    /// Windows' rules (its own lower case needs Windows): the keys 0.3.4 gave on Windows.
    #[cfg(windows)]
    #[test]
    fn windows_keys_of_0_3_4() {
        for (roots, want) in fixture_keys("windows") {
            assert_eq!(WIN.root_key(&roots), want, "{roots:?}");
        }
    }

    /// `root_keys_os.json`: roots as the system names them (hex: the bytes on POSIX, UTF-16LE
    /// units on Windows) and the key 0.3.4 gave them.
    fn fixture_os_keys(platform: &str) -> Vec<(Vec<Vec<u8>>, String)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sk-core/tests/data/v034")
            .join(platform)
            .join("root_keys_os.json");
        let cases: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let hex = |s: &str| -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        };
        cases
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let roots = c["roots_hex"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| hex(r.as_str().unwrap()))
                    .collect();
                (roots, c["key"].as_str().unwrap().to_owned())
            })
            .collect()
    }

    #[cfg(windows)]
    fn units(le: &[u8]) -> Vec<u16> {
        le.chunks(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect()
    }

    #[test]
    fn surrogateescape_like_python() {
        // valid UTF-8 stays; every other byte b is U+DC00 + b (ED B2/B3 xx)
        assert_eq!(surrogateescape("/下载/é".as_bytes()), "/下载/é".as_bytes());
        assert_eq!(surrogateescape(b"a\xffb"), b"a\xed\xb3\xbfb");
        assert_eq!(surrogateescape(b"\x80"), b"\xed\xb2\x80");
        // a cut sequence, a UTF-8 surrogate, an overlong form: every byte on its own
        assert_eq!(
            surrogateescape(b"\xf0\x9f\x98a"),
            b"\xed\xb3\xb0\xed\xb2\x9f\xed\xb2\x98a"
        );
        assert_eq!(
            surrogateescape(b"\xed\xa0\x80"),
            b"\xed\xb3\xad\xed\xb2\xa0\xed\xb2\x80"
        );
        assert_eq!(surrogateescape(b"\xc0\xaf"), b"\xed\xb3\x80\xed\xb2\xaf");
    }

    /// POSIX roots with bytes that are not UTF-8 (`os.fsdecode` text in 0.3.4), on every system.
    #[test]
    fn posix_os_keys_of_0_3_4() {
        for (roots, want) in fixture_os_keys("posix") {
            let py: Vec<Vec<u8>> = roots.iter().map(|r| surrogateescape(r)).collect();
            assert_eq!(POSIX.root_key_py(&py), want, "{roots:x?}");
        }
    }

    /// Windows roots with lone surrogates (Windows' rules: its own lower case needs Windows).
    #[cfg(windows)]
    #[test]
    fn windows_os_keys_of_0_3_4() {
        for (roots, want) in fixture_os_keys("windows") {
            let py: Vec<Vec<u8>> = roots.iter().map(|r| wide_to_wtf8(&units(r))).collect();
            assert_eq!(WIN.root_key_py(&py), want, "{roots:x?}");
        }
    }

    /// The system's own strings through [`root_key`]: Windows with lone surrogates, POSIX with
    /// bytes that are not UTF-8 (on Linux and macOS), keyed as 0.3.4 keyed them.
    #[cfg(any(windows, unix))]
    #[test]
    fn os_strings_are_keyed_as_0_3_4_keyed_them() {
        #[cfg(windows)]
        let (platform, os): (_, fn(&[u8]) -> std::ffi::OsString) = ("windows", |r| {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&units(r))
        });
        #[cfg(unix)]
        let (platform, os): (_, fn(&[u8]) -> std::ffi::OsString) = ("posix", |r| {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(r.to_vec())
        });
        let cases = fixture_os_keys(platform);
        assert!(cases.len() > 10);
        for (roots, want) in cases {
            let strings: Vec<std::ffi::OsString> = roots.iter().map(|r| os(r)).collect();
            assert!(strings.iter().any(|s| s.to_str().is_none()), "{strings:?}");
            assert_eq!(root_key(&strings), want, "{strings:?}");
            // a lossy string of the root is another root: not its key
            let lossy: Vec<String> = strings
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            assert_ne!(root_key(&lossy), want, "{strings:?}");
        }
        // text roots: the same key as before
        for (roots, want) in fixture_keys(platform) {
            assert_eq!(root_key(&roots), want, "{roots:?}");
        }
    }
}
