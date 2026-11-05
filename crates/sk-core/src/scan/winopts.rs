//! Windows listing options the scanner sets per root and per call (scanner.py `listed_alloc`,
//! worker.py `SliceOptions` defaults; docs/native.md 2.2). They change nothing off Windows.
//!
//! * [`listed_alloc`]: files on local NTFS, ReFS and WinBtrfs volumes take their size on disk
//!   from their listing entries (`SPACEKNIFE_WIN_LISTED_ALLOC=0`: estimated from the size).
//! * [`apply_env`]: `SPACEKNIFE_WIN_SKIP_FINAL=1`, `SPACEKNIFE_WIN_DOT_CHECK=1` turn on
//!   `skip_final` / `dot_check`; `SPACEKNIFE_WIN_KEEP_WHERE_UPDATED=0` turns off
//!   `keep_where_updated`. `0`, `false`, `no`, `off` (any case) are off, any other non-empty value
//!   on, empty or unset the default.

use sk_native::hardscan::{ListedAlloc, SliceOptions};

use crate::volume::VolumeInfo;

/// An on/off environment switch (worker.py `_env_switch`).
pub fn env_switch(name: &str, default: bool) -> bool {
    switch_value(std::env::var(name).ok().as_deref(), default)
}

fn switch_value(value: Option<&str>, default: bool) -> bool {
    let v = value.unwrap_or("").trim().to_ascii_lowercase();
    if v.is_empty() {
        default
    } else {
        !matches!(v.as_str(), "0" | "false" | "no" | "off")
    }
}

/// `RootOptions::listed_alloc` for a root on `volume`: the listing's AllocationSize on local
/// NTFS, ReFS and Btrfs volumes (where it was measured against the file system's own count);
/// elsewhere (FAT, exFAT, network shares, other systems, not Windows), or with
/// `SPACEKNIFE_WIN_LISTED_ALLOC=0`, estimated from the size.
pub fn listed_alloc(volume: Option<&VolumeInfo>) -> ListedAlloc {
    listed_alloc_with(
        volume,
        cfg!(windows),
        env_switch("SPACEKNIFE_WIN_LISTED_ALLOC", true),
    )
}

fn listed_alloc_with(volume: Option<&VolumeInfo>, windows: bool, enabled: bool) -> ListedAlloc {
    match volume {
        Some(v) if windows && enabled && !v.is_remote => ListedAlloc::from_fs_name(&v.fs_type),
        _ => ListedAlloc::Off,
    }
}

/// Sets `skip_final`, `dot_check` and `keep_where_updated` from the environment (read on every
/// call, as worker.SliceOptions did when it was created).
pub fn apply_env(opts: &mut SliceOptions) {
    opts.skip_final = env_switch("SPACEKNIFE_WIN_SKIP_FINAL", false);
    opts.dot_check = env_switch("SPACEKNIFE_WIN_DOT_CHECK", false);
    opts.keep_where_updated = env_switch("SPACEKNIFE_WIN_KEEP_WHERE_UPDATED", true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vol(fs: &str, remote: bool) -> VolumeInfo {
        VolumeInfo {
            mount_point: "X:\\".into(),
            device: "X:".into(),
            fs_type: fs.into(),
            cluster_size: 4096,
            is_remote: remote,
            ..VolumeInfo::default()
        }
    }

    #[test]
    fn switches_read_like_the_worker() {
        for (v, want) in [
            (None, true),
            (Some(""), true),
            (Some("  "), true),
            (Some("0"), false),
            (Some("False"), false),
            (Some("NO"), false),
            (Some(" off "), false),
            (Some("1"), true),
            (Some("yes"), true),
        ] {
            assert_eq!(switch_value(v, true), want, "{v:?}");
        }
        assert!(!switch_value(None, false));
        assert!(switch_value(Some("on"), false));
    }

    #[test]
    fn listed_alloc_is_for_local_ntfs_refs_and_btrfs() {
        let got: Vec<ListedAlloc> = ["NTFS", "ReFS", "btrfs", "exFAT", "FAT32"]
            .iter()
            .map(|f| listed_alloc_with(Some(&vol(f, false)), true, true))
            .collect();
        assert_eq!(
            got,
            [
                ListedAlloc::Ntfs,
                ListedAlloc::Refs,
                ListedAlloc::Btrfs,
                ListedAlloc::Off,
                ListedAlloc::Off
            ]
        );
        assert_eq!(
            listed_alloc_with(Some(&vol("NTFS", true)), true, true),
            ListedAlloc::Off
        );
        assert_eq!(listed_alloc_with(None, true, true), ListedAlloc::Off);
        // SPACEKNIFE_WIN_LISTED_ALLOC=0, and other systems
        assert_eq!(
            listed_alloc_with(Some(&vol("NTFS", false)), true, false),
            ListedAlloc::Off
        );
        assert_eq!(
            listed_alloc_with(Some(&vol("NTFS", false)), false, true),
            ListedAlloc::Off
        );
    }

    #[test]
    fn defaults_match_main() {
        // with none of the variables set (the test process does not set them)
        let mut o = SliceOptions::default();
        if [
            "SPACEKNIFE_WIN_SKIP_FINAL",
            "SPACEKNIFE_WIN_DOT_CHECK",
            "SPACEKNIFE_WIN_KEEP_WHERE_UPDATED",
        ]
        .iter()
        .all(|k| std::env::var_os(k).is_none())
        {
            apply_env(&mut o);
            assert!(!o.skip_final && !o.dot_check && o.keep_where_updated);
        }
    }
}
