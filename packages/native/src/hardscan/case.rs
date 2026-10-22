//! Case-only siblings (docs/native.md 2.2): the pure rules of how a found subfolder is opened when
//! another entry of its listing has the same name in another case, or the very same name.
//!
//! Windows opens names case-insensitively. Where a folder's entries are stored case-sensitively
//! (WinBtrfs with trees Linux made, NTFS names in the POSIX namespace, case-sensitive NTFS folders)
//! an open by name can reach another entry than the one the listing named: on WinBtrfs the first
//! whose upcased name matches. So a folder is listed under a name only when the handle that lists
//! it has been shown to be that entry: its file id is the listed one or, where the listing gives no
//! usable id, its stored name is the listed name. Otherwise it is opened by its file id where the
//! file system can do that, or it is a listing error. The I/O is in win.rs; this part also builds
//! and is tested on POSIX.

#![cfg_attr(not(windows), allow(dead_code))]

/// Marks of a found subfolder (`Hint.mark` on Windows).
pub mod mark {
    /// Another entry of the listing has the same name under the Windows upcase.
    pub const TWIN: u8 = 1;
    /// Another entry has exactly the same name (WinBtrfs shows invalid UTF-8 as U+FFFD).
    pub const DUP: u8 = 2;
    /// The name has a `\` or `/` (WinBtrfs shows Linux names as they are): not a single name.
    pub const BADNAME: u8 = 4;
    /// A reparse point that is descended (cloud folders, ProjFS, DFS links).
    pub const REPARSE: u8 = 8;
    /// The marks that make a subfolder strict: it must be proven to be the listed entry.
    pub const STRICT: u8 = TWIN | DUP | BADNAME;
}

/// Listing errors of folders a name cannot reach; worker.py has the same texts, and
/// events.denied_reason gives them their log wording.
pub mod msg {
    pub const CASE: &str =
        "cannot be opened by its name: another entry in this folder has the same name in a different case";
    pub const DUP: &str =
        "cannot be opened by its name: another entry in this folder has the same name as Windows shows it";
    pub const BADNAME: &str = "cannot be opened by its name: it contains a backslash or slash";
    pub const CAP: &str =
        "not read: too many entries below folders that can only be opened by file id in one pass";
    pub const INCOMPLETE: &str = "listed incompletely when opened by its file id";
}

/// The file system of a root, as far as opening folders goes.
pub mod fs {
    pub const UNKNOWN: u8 = 0;
    pub const NTFS: u8 = 1;
    pub const REFS: u8 = 2;
    pub const BTRFS: u8 = 3;
    /// FAT, exFAT, network drives (SMB reports the server's file system name), anything else.
    pub const OTHER: u8 = 4;
}

/// A file system name (FileFsAttributeInformation) as one of `fs`; a remote device is OTHER
/// whatever name it reports (an SMB share of an NTFS volume says "NTFS", and cannot open by id).
pub fn fs_of(name: &[u16], remote: bool) -> u8 {
    let is = |want: &str| {
        name.len() == want.len()
            && name
                .iter()
                .zip(want.bytes())
                .all(|(&u, b)| u < 0x80 && (u as u8).eq_ignore_ascii_case(&b))
    };
    if remote {
        fs::OTHER
    } else if is("NTFS") {
        fs::NTFS
    } else if is("ReFS") {
        fs::REFS
    } else if is("Btrfs") {
        fs::BTRFS
    } else {
        fs::OTHER
    }
}

/// One UTF-16 unit under the Windows upcase: ASCII inline, the rest from `up` (RtlUpcaseUnicodeChar
/// of every unit; never Rust's or Unicode's case rules, which differ). Surrogates are units of
/// their own, as Windows treats them.
#[inline]
pub fn upcase_unit(u: u16, up: &[u16]) -> u16 {
    if u < 0x80 {
        if (b'a' as u16..=b'z' as u16).contains(&u) {
            u - 0x20
        } else {
            u
        }
    } else {
        up.get(u as usize).copied().unwrap_or(u)
    }
}

/// FNV-1a of a name's upcased units: names Windows takes for the same have the same hash.
pub fn upcase_hash(units: &[u16], up: &[u16]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &u in units {
        h = (h ^ upcase_unit(u, up) as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// One entry of a listing for `mark_twins`: its upcase hash, where its WTF-8 name lies in the
/// arena's names, its index in the listing and whether it is a subfolder.
#[derive(Clone, Copy, Debug)]
pub struct CaseKey {
    pub hash: u64,
    pub start: usize,
    pub len: usize,
    pub j: u32,
    pub dir: bool,
}

/// Subfolders of one listing that share their name with another entry (`(j, marks)`, in no order):
/// TWIN for every subfolder of a group of two or more entries with the same upcase hash (files
/// count too: a file can win the open of a folder's name), DUP besides when another entry's name
/// is the very same (compared as WTF-8, so no UTF-16 copy has to outlive the directory buffer).
/// A hash collision only marks a folder that did not need it: it costs one more check.
pub fn mark_twins(keys: &mut [CaseKey], names: &[u8], marks: &mut Vec<(u32, u8)>) {
    marks.clear();
    if keys.len() < 2 || !keys.iter().any(|k| k.dir) {
        return;
    }
    keys.sort_unstable_by_key(|k| k.hash);
    let name = |k: &CaseKey| &names[k.start..k.start + k.len];
    let mut lo = 0;
    while lo < keys.len() {
        let mut hi = lo + 1;
        while hi < keys.len() && keys[hi].hash == keys[lo].hash {
            hi += 1;
        }
        let group = &keys[lo..hi];
        if group.len() > 1 {
            for (a, k) in group.iter().enumerate() {
                if k.dir {
                    let dup = group
                        .iter()
                        .enumerate()
                        .any(|(b, o)| b != a && name(o) == name(k));
                    marks.push((k.j, if dup { mark::TWIN | mark::DUP } else { mark::TWIN }));
                }
            }
        }
        lo = hi;
    }
}

fn is_sep(u: u16) -> bool {
    u == b'\\' as u16 || u == b'/' as u16
}

/// The names of a Windows path, without the empty ones of doubled or trailing separators.
fn names(path: &[u16]) -> impl DoubleEndedIterator<Item = &[u16]> {
    path.split(|&u| is_sep(u)).filter(|n| !n.is_empty())
}

/// The last name of a stored name (FileNameInformation: `\usr\share\terminfo\a` gives `a`).
pub fn last_name(stored: &[u16]) -> &[u16] {
    names(stored).next_back().unwrap_or(&[])
}

/// The names of task `path` below the scan root `root` (as given: the root part is never compared,
/// so roots typed in another case, SUBST drives and mount folders work). None for the root itself,
/// a path that is not below it, or no root given.
pub fn below_root<'a>(path: &'a [u16], root: &[u16]) -> Option<Vec<&'a [u16]>> {
    if root.is_empty() || !path.starts_with(root) {
        return None;
    }
    let rest = &path[root.len()..];
    let joined = root.last().is_some_and(|&u| is_sep(u)) || rest.first().is_some_and(|&u| is_sep(u));
    if !joined {
        return None; // `C:\data2` is not below `C:\data`
    }
    let below: Vec<&[u16]> = names(rest).collect();
    (!below.is_empty()).then_some(below)
}

/// Whether the last names of a stored name are `below`, exactly (UTF-16 units); None when it has
/// fewer names than that (nothing to tell from).
pub fn tail_is(stored: &[u16], below: &[&[u16]]) -> Option<bool> {
    let all: Vec<&[u16]> = names(stored).collect();
    if all.len() < below.len() {
        return None;
    }
    Some(all[all.len() - below.len()..] == *below)
}

/// A file id that is no id: all bits set, judged in the width the listing gave it (a 64-bit -1 is
/// not u128::MAX).
pub fn all_ones(id: u128, bits: u8) -> bool {
    match bits {
        64 => id == u64::MAX as u128,
        128 => id == u128::MAX,
        _ => false,
    }
}

/// The ObjectName of an open by file id: the id as the listing gave it, 8 or 16 bytes, never
/// widened or narrowed (a zero-extended 64-bit id fails on ReFS and WinBtrfs). On Btrfs a 64-bit id
/// (FileIdBoth: subvolume << 40 | inode) becomes the 16-byte {inode, subvolume}: the 8-byte form
/// looks the inode up in the parent's subvolume, which misses a folder that is a subvolume's root.
pub fn id_name(id: u128, bits: u8, btrfs: bool) -> ([u8; 16], usize) {
    let mut out = [0u8; 16];
    if bits == 64 {
        let id = id as u64;
        if btrfs {
            out[..8].copy_from_slice(&(id & 0xFF_FFFF_FFFF).to_le_bytes());
            out[8..].copy_from_slice(&(id >> 40).to_le_bytes());
            return (out, 16);
        }
        out[..8].copy_from_slice(&id.to_le_bytes());
        return (out, 8);
    }
    (id.to_le_bytes(), 16)
}

/// How a found subfolder is opened first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum First {
    /// By its file id at once: a name with a `\` would be taken for a path.
    ById,
    /// Not at all: a listing error.
    Fail(&'static str),
    /// By its one name relative to the parent's handle, whether or not relative opens are on (the
    /// parent was opened by file id, so no path leads there).
    Relative,
    /// By name: relative to the parent where relative opens are on, else by full path.
    Name,
    /// By full path (a descended reparse point below a folder opened by name, as before).
    Path,
}

/// `marks` of the subfolder, `trusted`: its listing gave it a file id to check against (not 0,
/// not all ones, not shared, not a reparse point's), `pinned`: its parent was opened by file id or
/// lies below such a folder.
pub fn first_open(marks: u8, trusted: bool, pinned: bool) -> First {
    if marks & mark::BADNAME != 0 {
        return if trusted {
            First::ById
        } else {
            First::Fail(msg::BADNAME)
        };
    }
    if pinned {
        First::Relative
    } else if marks & mark::REPARSE != 0 {
        First::Path
    } else {
        First::Name
    }
}

/// What the rest of the open of a found subfolder follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// No other entry shares its name: as before case-only siblings were handled.
    Plain,
    /// A TWIN, DUP or BADNAME mark: it must be proven to be the listed entry.
    Strict,
    /// Below a folder opened by file id, with no mark.
    Pinned,
    /// A descended reparse point below a folder opened by file id: listed as the open finds it.
    PinnedReparse,
}

pub fn class_of(marks: u8, pinned: bool) -> Class {
    if marks & mark::STRICT != 0 {
        Class::Strict
    } else if !pinned {
        Class::Plain
    } else if marks & mark::REPARSE != 0 {
        Class::PinnedReparse
    } else {
        Class::Pinned
    }
}

/// What the open by name found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The handle has the listed file id.
    Same,
    /// The listing gave no file id to check the handle against.
    NoId,
    /// The handle has another file id. `renamed`: its stored name is not the listed name, or the
    /// entry has an exact duplicate (a folder replaced meanwhile keeps the name, a twin does not).
    Other { renamed: bool },
    /// The handle's file id cannot be read.
    Unreadable,
    /// The open failed (NOT_A_DIRECTORY when a file of the same name wins, denied, gone...).
    Failed,
}

/// What follows the open by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    /// List the handle.
    List,
    /// The stored name decides (`by_stored_name`): listed, or a listing error.
    StoredName,
    /// Open it by its file id.
    ById,
    /// What plain relative opens always did: give the folder to the open by full path.
    ByPath,
    /// The open's own error.
    Fail,
}

/// The rest of the open of a found subfolder; `Plain` only for relative opens (a plain folder
/// opened by full path is listed as before, unchecked).
pub fn after_open(class: Class, outcome: Outcome, trusted: bool) -> Then {
    use Outcome::*;
    match class {
        Class::Plain => match outcome {
            Same | NoId => Then::List,
            Other { renamed: true } => Then::ById,
            Other { renamed: false } | Unreadable | Failed => Then::ByPath,
        },
        Class::Strict => match outcome {
            Same | Other { renamed: false } => Then::List,
            Other { renamed: true } => Then::ById,
            NoId | Unreadable => Then::StoredName,
            Failed if trusted => Then::ById,
            Failed => Then::Fail,
        },
        Class::Pinned => match outcome {
            Same | NoId | Other { renamed: false } => Then::List,
            Other { renamed: true } => Then::ById,
            Unreadable => Then::StoredName,
            Failed => Then::Fail,
        },
        Class::PinnedReparse => match outcome {
            Failed => Then::Fail,
            _ => Then::List,
        },
    }
}

/// `Then::StoredName`: the handle is the listed entry when its stored last name is the listed name,
/// unless another entry has the very same name (which one an open reaches is then not known).
pub fn by_stored_name(named: bool, marks: u8) -> Result<(), &'static str> {
    if marks & mark::DUP != 0 {
        Err(msg::DUP)
    } else if named {
        Ok(())
    } else {
        Err(msg::CASE)
    }
}

/// The error of a folder the open by file id did not settle: the open by name's own error when
/// that failed (denied, gone), else why no name reaches it.
pub fn by_id_failed(marks: u8, name_error: Option<String>) -> String {
    if let Some(e) = name_error {
        return e;
    }
    if marks & mark::BADNAME != 0 {
        msg::BADNAME
    } else if marks & mark::DUP != 0 {
        msg::DUP
    } else {
        msg::CASE
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The units RtlUpcaseUnicodeChar changes that the tests need (identity elsewhere): it maps ÿ
    /// and ⱥ (whose upper cases have UTF-8 forms of another length) and σ, and leaves ı, ß, ς, the
    /// Kelvin sign and surrogates alone.
    fn table() -> Vec<u16> {
        let mut up: Vec<u16> = (0..=u16::MAX).collect();
        for (lower, upper) in [(0xFF, 0x178), (0x2C65, 0x23A), (0x3C3, 0x3A3), (0xE9, 0xC9)] {
            up[lower] = upper;
        }
        up
    }

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn same(a: &[u16], b: &[u16]) -> bool {
        let up = table();
        upcase_hash(a, &up) == upcase_hash(b, &up)
    }

    #[test]
    fn names_windows_takes_for_the_same_hash_the_same() {
        assert!(same(&w("terminfo"), &w("TermInfo")));
        assert!(same(&w("a"), &w("A")));
        assert!(same(&w("ÿ"), &w("Ÿ"))); // 2 and 2 UTF-8 bytes, but U+00FF vs U+0178
        assert!(same(&w("ⱥ"), &w("Ⱥ"))); // 3 and 2 UTF-8 bytes: a WTF-8 compare would miss it
        assert!(same(&w("σ"), &w("Σ")));
        assert!(same(&w("café"), &w("CAFÉ")));
        // the Windows upcase leaves these alone, whatever Unicode's case rules say
        assert!(!same(&w("ı"), &w("I")));
        assert!(!same(&w("ß"), &w("SS")));
        assert!(!same(&w("ς"), &w("σ")));
        assert!(!same(&w("\u{212A}"), &w("K"))); // Kelvin sign
        assert!(!same(&w("\u{10400}"), &w("\u{10428}"))); // surrogate pairs are not decoded
        assert!(!same(&w("ab"), &w("abc")));
        // lone surrogates are units like any other
        assert!(same(&[0x61, 0xD800], &[0x41, 0xD800]));
        assert!(!same(&[0xD800], &[0xDC00]));
        let up = table();
        assert_eq!(upcase_unit(b'z' as u16, &up), b'Z' as u16);
        assert_eq!(upcase_unit(b'{' as u16, &up), b'{' as u16);
        assert_eq!(upcase_unit(0xFF, &up), 0x178);
    }

    /// Keys and WTF-8 names of a listing: `(name, is a folder)` in listing order.
    fn listing(entries: &[(&str, bool)]) -> (Vec<CaseKey>, Vec<u8>) {
        let up = table();
        let mut names = Vec::new();
        let mut keys = Vec::new();
        for (j, (name, dir)) in entries.iter().enumerate() {
            let start = names.len();
            names.extend_from_slice(name.as_bytes());
            keys.push(CaseKey {
                hash: upcase_hash(&w(name), &up),
                start,
                len: name.len(),
                j: j as u32,
                dir: *dir,
            });
        }
        (keys, names)
    }

    fn marks_of(entries: &[(&str, bool)]) -> Vec<(u32, u8)> {
        let (mut keys, names) = listing(entries);
        let mut marks = vec![(99, 99)]; // cleared first
        mark_twins(&mut keys, &names, &mut marks);
        marks.sort_unstable();
        marks
    }

    #[test]
    fn subfolders_sharing_a_name_are_marked() {
        use mark::{DUP, TWIN};
        assert_eq!(marks_of(&[("a", true), ("b", true), ("A", true)]), [(0, TWIN), (2, TWIN)]);
        // a file can win the open of a folder's name: the folder is marked, the file needs nothing
        assert_eq!(marks_of(&[("B", false), ("b", true)]), [(1, TWIN)]);
        assert_eq!(marks_of(&[("x", false), ("X", false), ("d", true)]), []);
        // the very same name (WinBtrfs shows invalid UTF-8 as U+FFFD)
        assert_eq!(
            marks_of(&[("n\u{FFFD}", true), ("n\u{FFFD}", true), ("N\u{FFFD}", false)]),
            [(0, TWIN | DUP), (1, TWIN | DUP)]
        );
        assert_eq!(
            marks_of(&[("n\u{FFFD}", false), ("n\u{FFFD}", true)]),
            [(1, TWIN | DUP)]
        );
        // three in a group, and two groups
        assert_eq!(
            marks_of(&[("pod", true), ("Pod", true), ("POD", false), ("ⱥ", true), ("Ⱥ", true)]),
            [(0, TWIN), (1, TWIN), (3, TWIN), (4, TWIN)]
        );
        // a listing without subfolders, or of one entry, is not looked at
        assert_eq!(marks_of(&[("a", false), ("A", false)]), []);
        assert_eq!(marks_of(&[("a", true)]), []);
        assert_eq!(marks_of(&[]), []);
        assert_eq!(marks_of(&[("ı", true), ("I", true), ("ß", true), ("SS", true)]), []);
    }

    #[test]
    fn twins_are_found_across_directory_buffers() {
        // more than 64 KiB of names: a listing that takes several NtQueryDirectoryFile buffers
        let mut entries: Vec<(String, bool)> = vec![("Twin".into(), true)];
        entries.extend((0..3000).map(|i| (format!("file-with-a-fairly-long-name-{i:05}.log"), false)));
        entries.push(("tWIN".into(), true));
        let refs: Vec<(&str, bool)> = entries.iter().map(|(n, d)| (n.as_str(), *d)).collect();
        let (_, names) = listing(&refs);
        assert!(names.len() * 2 > 64 * 1024);
        assert_eq!(marks_of(&refs), [(0, mark::TWIN), (3001, mark::TWIN)]);
    }

    #[test]
    fn task_paths_against_stored_names() {
        let root = w("I:\\var\\lib");
        let task = w("I:\\var\\lib\\flatpak\\terminfo\\a");
        let below = below_root(&task, &root).unwrap();
        assert_eq!(below, [&w("flatpak")[..], &w("terminfo"), &w("a")]);
        // the stored name of the winner `A`, and of the folder itself (the root part may differ:
        // a SUBST drive or mount folder shows the volume's own path)
        assert_eq!(tail_is(&w("\\var\\lib\\flatpak\\terminfo\\A"), &below), Some(false));
        assert_eq!(tail_is(&w("\\VAR\\lib\\flatpak\\terminfo\\a"), &below), Some(true));
        assert_eq!(tail_is(&w("\\mnt\\flatpak\\terminfo\\a"), &below), Some(true));
        assert_eq!(tail_is(&w("\\terminfo\\a"), &below), None);
        assert_eq!(last_name(&w("\\usr\\share\\terminfo\\a")), w("a"));
        assert_eq!(last_name(&w("\\")), w(""));
        // the root itself, beside it, or no root at all: nothing to check
        assert_eq!(below_root(&root, &root), None);
        assert_eq!(below_root(&w("I:\\var\\lib\\"), &root), None);
        assert_eq!(below_root(&w("I:\\var\\library\\x"), &root), None);
        assert_eq!(below_root(&task, &w("")), None);
        assert_eq!(below_root(&w("I:\\x\\y"), &w("I:\\")).unwrap(), [&w("x")[..], &w("y")]);
        assert_eq!(below_root(&w("I:\\x\\\\y\\"), &w("I:\\")).unwrap(), [&w("x")[..], &w("y")]);
    }

    #[test]
    fn ids_keep_their_width() {
        let ones = u64::MAX as u128;
        assert!(all_ones(ones, 64) && !all_ones(ones, 128));
        assert!(all_ones(u128::MAX, 128) && !all_ones(u128::MAX - 1, 128));
        assert!(!all_ones(0, 0));
        // 64 bits: the 8 bytes; 128 bits: the 16 bytes, never converted
        let (b, n) = id_name(0x1122_3344_5566_7788, 64, false);
        assert_eq!((&b[..n], n), (&0x1122_3344_5566_7788u64.to_le_bytes()[..], 8));
        let big = 0x0102_0304_0506_0708_090A_0B0C_0D0E_0F10u128;
        let (b, n) = id_name(big, 128, false);
        assert_eq!((b, n), (big.to_le_bytes(), 16));
        assert_eq!(id_name(big, 128, true), (big.to_le_bytes(), 16));
        // Btrfs: subvolume << 40 | inode becomes {inode, subvolume}
        let (b, n) = id_name(0x50000121511, 64, true);
        assert_eq!(n, 16);
        assert_eq!(u64::from_le_bytes(b[..8].try_into().unwrap()), 0x121511);
        assert_eq!(u64::from_le_bytes(b[8..].try_into().unwrap()), 5);
        let (b, _) = id_name((0x100u128 << 40) | 0xFF_FFFF_FFFF, 64, true);
        assert_eq!(u64::from_le_bytes(b[..8].try_into().unwrap()), 0xFF_FFFF_FFFF);
        assert_eq!(u64::from_le_bytes(b[8..].try_into().unwrap()), 0x100);
    }

    #[test]
    fn file_systems_by_name() {
        assert_eq!(fs_of(&w("NTFS"), false), fs::NTFS);
        assert_eq!(fs_of(&w("ReFS"), false), fs::REFS);
        assert_eq!(fs_of(&w("Btrfs"), false), fs::BTRFS);
        assert_eq!(fs_of(&w("btrfs"), false), fs::BTRFS);
        assert_eq!(fs_of(&w("exFAT"), false), fs::OTHER);
        assert_eq!(fs_of(&w("NTFS2"), false), fs::OTHER);
        // an SMB share reports the server's file system: still a network drive
        assert_eq!(fs_of(&w("NTFS"), true), fs::OTHER);
        assert_eq!(fs_of(&w("Btrfs"), true), fs::OTHER);
    }

    #[test]
    fn the_first_open() {
        use mark::{BADNAME, DUP, REPARSE, TWIN};
        for pinned in [false, true] {
            assert_eq!(first_open(BADNAME, true, pinned), First::ById);
            assert_eq!(first_open(BADNAME | TWIN, true, pinned), First::ById);
            assert_eq!(first_open(BADNAME, false, pinned), First::Fail(msg::BADNAME));
        }
        for marks in [0, TWIN, TWIN | DUP, REPARSE, REPARSE | TWIN] {
            for trusted in [false, true] {
                assert_eq!(first_open(marks, trusted, true), First::Relative);
            }
        }
        for marks in [0, TWIN, TWIN | DUP] {
            assert_eq!(first_open(marks, true, false), First::Name);
            assert_eq!(first_open(marks, false, false), First::Name);
        }
        assert_eq!(first_open(REPARSE, false, false), First::Path);
        assert_eq!(first_open(REPARSE | TWIN, false, false), First::Path);
    }

    #[test]
    fn classes() {
        use mark::{BADNAME, DUP, REPARSE, TWIN};
        assert_eq!(class_of(0, false), Class::Plain);
        assert_eq!(class_of(REPARSE, false), Class::Plain);
        for pinned in [false, true] {
            for marks in [TWIN, TWIN | DUP, BADNAME, REPARSE | TWIN] {
                assert_eq!(class_of(marks, pinned), Class::Strict);
            }
        }
        assert_eq!(class_of(0, true), Class::Pinned);
        assert_eq!(class_of(REPARSE, true), Class::PinnedReparse);
    }

    #[test]
    fn after_the_open_by_name() {
        use Class::*;
        use Outcome::*;
        let renamed = Other { renamed: true };
        let replaced = Other { renamed: false };
        // every cell of the table in docs/native.md 2.2 (rows: outcome; columns: class)
        let cells: [(Outcome, [Then; 3]); 6] = [
            (Same, [Then::List, Then::List, Then::List]),
            (NoId, [Then::List, Then::StoredName, Then::List]),
            (replaced, [Then::ByPath, Then::List, Then::List]),
            (renamed, [Then::ById, Then::ById, Then::ById]),
            (Unreadable, [Then::ByPath, Then::StoredName, Then::StoredName]),
            (Failed, [Then::ByPath, Then::ById, Then::Fail]),
        ];
        for (outcome, want) in cells {
            for (class, then) in [Plain, Strict, Pinned].into_iter().zip(want) {
                assert_eq!(after_open(class, outcome, true), then, "{class:?} {outcome:?}");
            }
        }
        // a strict folder whose open failed goes by id only with an id to go by
        assert_eq!(after_open(Strict, Failed, false), Then::Fail);
        assert_eq!(after_open(Pinned, Failed, false), Then::Fail);
        // a descended reparse point below a pinned folder: whatever the open finds
        for outcome in [Same, NoId, replaced, renamed, Unreadable] {
            assert_eq!(after_open(PinnedReparse, outcome, false), Then::List);
        }
        assert_eq!(after_open(PinnedReparse, Failed, true), Then::Fail);
    }

    #[test]
    fn stored_names_and_failed_opens_by_id() {
        use mark::{BADNAME, DUP, TWIN};
        assert_eq!(by_stored_name(true, TWIN), Ok(()));
        assert_eq!(by_stored_name(false, TWIN), Err(msg::CASE));
        assert_eq!(by_stored_name(true, TWIN | DUP), Err(msg::DUP));
        assert_eq!(by_stored_name(false, TWIN | DUP), Err(msg::DUP));
        assert_eq!(by_id_failed(TWIN, None), msg::CASE);
        assert_eq!(by_id_failed(TWIN | DUP, None), msg::DUP);
        assert_eq!(by_id_failed(BADNAME, None), msg::BADNAME);
        assert_eq!(by_id_failed(0, None), msg::CASE); // a plain folder that turned out renamed
        assert_eq!(by_id_failed(TWIN, Some("Access is denied".into())), "Access is denied");
    }
}
