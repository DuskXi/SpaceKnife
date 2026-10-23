//! Reason codes shared with `spaceknife.dupes.codes` (keep both in step).

pub const OK: u8 = 0;
pub const GONE: u8 = 1; // no longer there
pub const DENIED: u8 = 2; // access denied
pub const NOT_FILE: u8 = 3; // no longer a regular file
#[allow(dead_code)] // set by the Windows and macOS checks only
pub const CLOUD: u8 = 4; // cloud placeholder / offline / dataless: reading would download it
pub const SURROGATE: u8 = 5; // a symlink or other name surrogate now stands here
pub const AMBIGUOUS: u8 = 6; // the open reached a name that differs only in case
pub const CHANGED: u8 = 7; // identity, size or times changed between two looks
pub const IN_USE: u8 = 8; // another program holds it without sharing reads
pub const ATIME: u8 = 9; // the access time could not be kept: not read (keep_atime = skip)
pub const READ_ERROR: u8 = 10; // any other error
pub const UNKNOWN_KEY: u8 = 11; // reader: no such member
pub const UNSUPPORTED: u8 = 13; // block maps: not available here

/// Windows file attributes that mean "reading would recall it from somewhere else".
#[cfg(windows)]
pub const RECALL_ATTRS: u32 = 0x0004_0000 | 0x0040_0000 | 0x0000_1000; // RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS | OFFLINE

/// Windows: IO_REPARSE_TAG_CLOUD and its 16 variants (0x9000x01A).
#[cfg(windows)]
pub fn cloud_tag(tag: u32) -> bool {
    tag & 0xFFFF_0FFF == 0x9000_001A
}

/// Windows: a name surrogate (symlink, junction, ...).
#[cfg(windows)]
pub fn surrogate_tag(tag: u32) -> bool {
    tag & 0x2000_0000 != 0
}

/// macOS st_flags: SF_DATALESS (iCloud / File Provider content not on disk).
#[cfg(target_os = "macos")]
pub const SF_DATALESS: u32 = 0x4000_0000;
