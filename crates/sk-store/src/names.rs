//! Names as bytes, and paths built from them.
//!
//! The store keeps names as the platform's encoded OS-string bytes
//! ([`OsStr::as_encoded_bytes`]): WTF-8 on Windows (UTF-8 that may hold lone surrogates), the raw
//! bytes on POSIX. Valid UTF-8 names (nearly all) are plain UTF-8 everywhere. [`name_to_os`] turns
//! them back into an `OsString` without trusting the bytes; [`decode_lossy`] is for display.
//!
//! Path comparison follows Python's `os.path.normcase`/`normpath`: on Windows `/` and `\` are both
//! separators and case is folded; on POSIX neither.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};

/// The platform's main separator as a byte.
pub const SEP: u8 = std::path::MAIN_SEPARATOR as u8;

/// Bytes of an OS string, as the store keeps names.
pub fn encode_name(name: &OsStr) -> &[u8] {
    name.as_encoded_bytes()
}

/// A name for display: invalid sequences (lone surrogates, non-UTF-8 bytes) become U+FFFD.
pub fn decode_lossy(raw: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(raw)
}

/// A name or path for the protocol and the logs, as 0.3.4 wrote it: valid UTF-8 as it is, the
/// parts that are not escaped (`\udc80` for a lone surrogate on Windows, `\xff` for a byte that is
/// not UTF-8 on POSIX), never U+FFFD. Not a path the system can be handed back: the escape is
/// text (a POSIX name may hold `\xff` itself).
pub fn printable(raw: &[u8]) -> Cow<'_, str> {
    printable_for(raw, cfg!(windows))
}

/// [`printable`] as Windows (`windows`: the bytes are WTF-8, a lone surrogate is `\udc80`) or a
/// POSIX system (a byte that is not UTF-8 is `\xff`) writes it, whichever this one is.
pub fn printable_for(raw: &[u8], windows: bool) -> Cow<'_, str> {
    if let Ok(s) = std::str::from_utf8(raw) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(raw.len() + 8);
    if windows {
        for r in char::decode_utf16(wtf8_to_wide(raw)) {
            match r {
                Ok(c) => out.push(c),
                Err(e) => out.push_str(&format!("\\u{:04x}", e.unpaired_surrogate())),
            }
        }
    } else {
        for chunk in raw.utf8_chunks() {
            out.push_str(chunk.valid());
            for b in chunk.invalid() {
                out.push_str(&format!("\\x{b:02x}"));
            }
        }
    }
    Cow::Owned(out)
}

/// A name as 0.3.4 put it in a binary frame's `names` blob (`name.encode("utf-8", "replace")`):
/// valid UTF-8 as it is, each part that is not Unicode as one `?` (a lone surrogate on Windows, a
/// byte that is not UTF-8 on POSIX: what [`printable`] escapes).
pub fn frame_label(raw: &[u8]) -> Cow<'_, str> {
    if let Ok(s) = std::str::from_utf8(raw) {
        return Cow::Borrowed(s);
    }
    #[cfg(windows)]
    {
        Cow::Owned(
            char::decode_utf16(wtf8_to_wide(raw))
                .map(|r| r.unwrap_or('?'))
                .collect(),
        )
    }
    #[cfg(not(windows))]
    {
        let mut out = String::with_capacity(raw.len());
        for chunk in raw.utf8_chunks() {
            out.push_str(chunk.valid());
            out.extend(std::iter::repeat_n('?', chunk.invalid().len()));
        }
        Cow::Owned(out)
    }
}

/// The OS string a name's bytes encode. Never trusts the bytes: on Windows, WTF-8 is decoded by
/// hand (lone surrogates kept, invalid bytes as U+FFFD); on POSIX the bytes are the name.
pub fn name_to_os(raw: &[u8]) -> OsString {
    if let Ok(s) = std::str::from_utf8(raw) {
        return OsString::from(s);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        OsStr::from_bytes(raw).to_os_string()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&wtf8_to_wide(raw))
    }
    #[cfg(not(any(unix, windows)))]
    {
        OsString::from(String::from_utf8_lossy(raw).into_owned())
    }
}

/// Generalised UTF-8 (surrogates allowed) to UTF-16; invalid bytes as U+FFFD.
pub(crate) fn wtf8_to_wide(raw: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        let (cp, len) = if b < 0x80 {
            (b as u32, 1)
        } else if b & 0xE0 == 0xC0 && i + 1 < raw.len() && raw[i + 1] & 0xC0 == 0x80 {
            (((b as u32 & 0x1F) << 6) | (raw[i + 1] as u32 & 0x3F), 2)
        } else if b & 0xF0 == 0xE0
            && i + 2 < raw.len()
            && raw[i + 1] & 0xC0 == 0x80
            && raw[i + 2] & 0xC0 == 0x80
        {
            (
                ((b as u32 & 0x0F) << 12)
                    | ((raw[i + 1] as u32 & 0x3F) << 6)
                    | (raw[i + 2] as u32 & 0x3F),
                3,
            )
        } else if b & 0xF8 == 0xF0
            && i + 3 < raw.len()
            && raw[i + 1] & 0xC0 == 0x80
            && raw[i + 2] & 0xC0 == 0x80
            && raw[i + 3] & 0xC0 == 0x80
        {
            (
                ((b as u32 & 0x07) << 18)
                    | ((raw[i + 1] as u32 & 0x3F) << 12)
                    | ((raw[i + 2] as u32 & 0x3F) << 6)
                    | (raw[i + 3] as u32 & 0x3F),
                4,
            )
        } else {
            (0xFFFD, 1)
        };
        if (0x10000..=0x10FFFF).contains(&cp) {
            let v = cp - 0x10000;
            out.push(0xD800 | (v >> 10) as u16);
            out.push(0xDC00 | (v & 0x3FF) as u16);
        } else if cp < 0x10000 {
            out.push(cp as u16);
        } else {
            out.push(0xFFFD);
        }
        i += len;
    }
    out
}

#[inline]
pub(crate) fn is_sep(b: u8) -> bool {
    b == b'/' || (cfg!(windows) && b == b'\\')
}

/// `os.path.normcase`: on Windows lower case with `\` separators; elsewhere the name itself.
pub fn normcase(raw: &[u8]) -> Cow<'_, [u8]> {
    if cfg!(windows) {
        let s = String::from_utf8_lossy(raw)
            .to_lowercase()
            .replace('/', "\\");
        Cow::Owned(s.into_bytes())
    } else {
        Cow::Borrowed(raw)
    }
}

/// `os.path.normpath` on bytes: one separator between parts (the platform's), no `.` parts,
/// `..` resolved where it can be, no trailing separator except on a bare root (`/`, `C:\`).
/// A Windows UNC prefix (`\\server`) keeps its two leading separators.
pub fn normpath(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return b".".to_vec();
    }
    let mut prefix: Vec<u8> = Vec::new();
    let mut rest = path;
    if cfg!(windows) {
        if rest.len() >= 2 && rest[1] == b':' {
            prefix.extend_from_slice(&rest[..2]);
            rest = &rest[2..];
        } else if rest.len() >= 2 && is_sep(rest[0]) && is_sep(rest[1]) {
            prefix.extend_from_slice(b"\\\\");
            rest = &rest[2..];
        }
    }
    let absolute = rest.first().is_some_and(|&b| is_sep(b));
    if absolute && prefix != b"\\\\" {
        prefix.push(SEP);
    }
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in rest.split(|&b| is_sep(b)) {
        match part {
            b"" | b"." => {}
            b".." => {
                if parts.last().is_some_and(|p| *p != b"..") {
                    parts.pop();
                } else if !absolute {
                    parts.push(part);
                }
            }
            _ => parts.push(part),
        }
    }
    let mut out = prefix;
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(SEP);
        }
        out.extend_from_slice(p);
    }
    if out.is_empty() {
        out.push(b'.');
    }
    out
}

/// The names of `path` below the scan root `root`, in their own case, or None when it is not
/// inside it (empty: the root itself). Both are normalised; the root part is compared with
/// [`normcase`], so a root typed in another case is the same root.
pub fn split_below(path: &[u8], root: &[u8]) -> Option<Vec<Vec<u8>>> {
    let target = normpath(path);
    let root = normpath(root);
    if target.len() < root.len() || normcase(&target[..root.len()]) != normcase(&root) {
        return None;
    }
    let mut rest = &target[root.len()..];
    if rest.is_empty() {
        return Some(Vec::new());
    }
    if !root.last().is_some_and(|&b| is_sep(b)) {
        if !is_sep(rest[0]) {
            return None; // C:\data2 is not inside C:\data
        }
        rest = &rest[1..];
    }
    Some(rest.split(|&b| is_sep(b)).map(<[u8]>::to_vec).collect())
}

/// Joins path parts as `os.path.join` does: a separator unless the part before ends with one.
pub(crate) fn join_into(out: &mut Vec<u8>, part: &[u8]) {
    if !out.is_empty() && !out.last().is_some_and(|&b| is_sep(b)) {
        out.push(SEP);
    }
    out.extend_from_slice(part);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odd_names_roundtrip() {
        for name in ["普通", "emoji-🙂", "tab\tname", "trailing space "] {
            let os = OsString::from(name);
            assert_eq!(name_to_os(encode_name(&os)), os);
            assert_eq!(decode_lossy(encode_name(&os)), name);
        }
    }

    #[test]
    fn printable_escapes_what_is_not_unicode() {
        // what 0.3.4 sent for such names (events::printable): never U+FFFD
        assert!(matches!(
            printable("普通 a/b".as_bytes()),
            Cow::Borrowed("普通 a/b")
        ));
        let os = odd_name();
        let shown = printable(encode_name(&os));
        if cfg!(windows) {
            assert_eq!(shown, "a\\udc80b");
        } else {
            assert_eq!(shown, "a\\xffb");
        }
        assert!(!shown.contains('\u{FFFD}'));
    }

    #[test]
    fn frame_labels_replace_what_is_not_unicode_with_a_question_mark() {
        // 0.3.4's binary frames: name.encode("utf-8", "replace")
        assert!(matches!(
            frame_label("普通 🙂".as_bytes()),
            Cow::Borrowed("普通 🙂")
        ));
        assert_eq!(frame_label(encode_name(&odd_name())), "a?b");
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            // two lone surrogates, and a pair between them that is a character
            let os = OsString::from_wide(&[0xD800, 0x41, 0xD83D, 0xDE42, 0xDC80]);
            assert_eq!(frame_label(encode_name(&os)), "?A🙂?");
        }
        #[cfg(unix)]
        {
            // surrogateescape: one per byte, also of a sequence cut short or an encoded surrogate
            assert_eq!(frame_label(b"\xf0\x9f\x98a"), "???a");
            assert_eq!(frame_label(b"\xed\xa0\x80"), "???");
        }
    }

    /// `a`, something that is not valid Unicode, `b`.
    fn odd_name() -> OsString {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0x61, 0xDC80, 0x62])
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![b'a', 0xFF, b'b'])
        }
    }

    #[cfg(windows)]
    #[test]
    fn lone_surrogates_roundtrip() {
        use std::os::windows::ffi::OsStringExt;
        let os = OsString::from_wide(&[0x61, 0xD800, 0x62]);
        assert_eq!(name_to_os(encode_name(&os)), os);
    }

    #[test]
    fn split_below_roots() {
        fn s(p: &str) -> &[u8] {
            p.as_bytes()
        }
        assert_eq!(
            split_below(s("/data/a/b"), s("/data")),
            Some(vec![b"a".to_vec(), b"b".to_vec()])
        );
        assert_eq!(split_below(s("/data"), s("/data")), Some(vec![]));
        assert_eq!(split_below(s("/data2/x"), s("/data")), None);
        assert_eq!(split_below(s("/x"), s("/")), Some(vec![b"x".to_vec()]));
        assert_eq!(
            split_below(s("/data/./a//b/"), s("/data/")),
            Some(vec![b"a".to_vec(), b"b".to_vec()])
        );
        if cfg!(windows) {
            assert_eq!(
                split_below(s("c:/Data/X"), s("C:\\data")),
                Some(vec![b"X".to_vec()])
            );
            assert_eq!(
                split_below(s("C:\\x"), s("C:\\")),
                Some(vec![b"x".to_vec()])
            );
        }
    }
}
