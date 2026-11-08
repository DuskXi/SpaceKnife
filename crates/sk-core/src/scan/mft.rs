//! NTFS `$MFT` fast scan: an [`MftTable`] (sk-native's parser, read here or by the elevated
//! helper) → a whole [`Store`] in one go (port of `scan/mft.py`'s `table_to_store`;
//! docs/native.md 2.1, docs/memory.md §6).
//!
//! Record 5 becomes the root named `root_path`; metafiles (records 0–15 and the `$Extend` tree)
//! go under `$NTFS 元数据`; names whose parent is missing, not a folder or part of a parent
//! cycle go under `$孤立文件` (each pseudo folder only when non-empty). Sizes follow the hard
//! scan: folders and links occupy nothing, the second and later names of a file are
//! `HARDLINK_DUP`; alternate data streams count only with `include_ads`, except in the metafile
//! folder, where they always count toward allocation (`$UsnJrnl:$J`, `$Secure:$SDS` …).
//! `$BadClus:$Bad` never counts (it is as long as the volume).
//!
//! The build — parents, breadth-first order, parent cycles, extensions, the name image, the
//! columns and the subtree totals, every step over all rows in parallel on a pool of its own —
//! is the performance layer's ([`sk_native::mft::build_tree`], whose docs list the steps). This
//! module decides when an MFT scan applies, names the pseudo folders in the interface language
//! and gives the build the store's memory: a [`StoreBuilder`] of the size it asks for.

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sk_native::columns::NodeColumns;
pub use sk_native::mft::build::{BADCLUS_RECORD, CYCLE_LIMIT, FIRST_USER_RECORD, ROOT_RECORD};
use sk_native::mft::{BuildError, PseudoNames, TreeOptions, TreeSink, build_tree};
pub use sk_native::mft::{MftTable, ScanStats, VolumeInfo as MftVolumeInfo};
use sk_store::{Store, StoreBuilder, StoreError, StoreOptions};

use crate::i18n::t;
use crate::scan::RootInfo;
use crate::tr;

/// Name of the metafile folder (message id).
pub const META_FOLDER: &str = "$NTFS 元数据";
/// Name of the folder of unreachable entries (message id).
pub const ORPHAN_FOLDER: &str = "$孤立文件";

/// How [`table_to_store`] builds.
#[derive(Clone, Debug, Default)]
pub struct MftBuild {
    /// Count alternate data streams.
    pub include_ads: bool,
    /// Every name of a hard-linked file counts.
    pub count_links_each: bool,
    /// Names of the pseudo folders (None: [`META_FOLDER`], [`ORPHAN_FOLDER`] in the interface
    /// language).
    pub folder_names: Option<(String, String)>,
}

/// Why an MFT fast scan cannot read `roots` (the scanner's roots), or None when it can: one root
/// that is a whole local NTFS volume on Windows, whose `$MFT` holds exactly that tree. The reason
/// is a sentence for the user (`scan/mft.py`'s `mft_unsupported`; the app and
/// `spaceknife scan --mft` share it).
pub fn mft_unsupported(roots: &[RootInfo]) -> Option<String> {
    if !cfg!(windows) {
        return Some(t("MFT 快速扫描只在 Windows 上可用").to_owned());
    }
    if roots.len() != 1 {
        return Some(tr!(
            "一次只能读一个卷，这次有 {count} 个扫描位置",
            count = roots.len()
        ));
    }
    let root = &roots[0];
    let path = root.path.display().to_string();
    let Some(vol) = &root.volume else {
        return Some(tr!("认不出 {path} 所在的卷", path = path));
    };
    if vol.is_remote {
        return Some(tr!("{path} 在网络位置上", path = path));
    }
    if !vol.fs_type.eq_ignore_ascii_case("NTFS") {
        let fs = if vol.fs_type.is_empty() {
            t("未知文件系统").to_owned()
        } else {
            vol.fs_type.clone()
        };
        return Some(tr!(
            "{mount} 是 {fs} 卷，只有 NTFS 卷有 $MFT",
            mount = &vol.mount_point,
            fs = fs
        ));
    }
    if !root.whole_volume {
        return Some(tr!(
            "只能扫整个卷（例如 {mount}），{path} 只是其中的一个文件夹",
            mount = &vol.mount_point,
            path = path
        ));
    }
    None
}

/// Reads and parses the `$MFT` of a local NTFS volume (`"C:"`; administrator rights, Windows):
/// 8 MiB windows, several read at once, each parsed as it arrives and joined in record order.
/// `duty` in (0, 1] is the disk's busy share (I/O limit); `progress(done, total)` follows the
/// reading (bytes of `$MFT`, once per window).
pub fn scan_volume(
    volume: &str,
    duty: f64,
    progress: Option<&mut dyn FnMut(u64, u64)>,
    cancel: Option<Arc<AtomicBool>>,
) -> io::Result<(MftTable, MftVolumeInfo)> {
    sk_native::mft::scan_mft(
        volume,
        sk_native::mft::ScanOptions {
            duty,
            progress,
            cancel,
            ..Default::default()
        },
    )
}

/// [`scan_volume`] on an image file holding an NTFS volume (tests).
pub fn scan_volume_image(
    path: &std::path::Path,
    window: usize,
) -> io::Result<(MftTable, MftVolumeInfo)> {
    sk_native::mft::scan_mft_image(
        path,
        sk_native::mft::ScanOptions {
            window,
            ..Default::default()
        },
    )
}

/// Builds the node store of a whole volume from `$MFT` columns (see the module docs), with the
/// app's store options. The table is consumed: its columns go as soon as the build is done with
/// them.
pub fn table_to_store(
    table: MftTable,
    root_path: &str,
    opts: &MftBuild,
) -> Result<Store, StoreError> {
    table_to_store_with(table, root_path, opts, StoreOptions::default())
}

/// [`table_to_store`] with the store's options (its mapping policy, for instance).
pub fn table_to_store_with(
    table: MftTable,
    root_path: &str,
    opts: &MftBuild,
    store: StoreOptions,
) -> Result<Store, StoreError> {
    let (meta_name, orphan_name) = opts.folder_names.clone().unwrap_or_else(|| {
        // in the interface language of the moment the tree is built
        (t(META_FOLDER).to_owned(), t(ORPHAN_FOLDER).to_owned())
    });
    let names = PseudoNames {
        root: root_path.as_bytes(),
        meta: meta_name.as_bytes(),
        orphans: orphan_name.as_bytes(),
    };
    let tree = TreeOptions {
        include_ads: opts.include_ads,
        count_links_each: opts.count_links_each,
    };
    let mut sink = StoreSink {
        opts: Some(store),
        builder: None,
    };
    let report = build_tree(table, &names, &tree, &mut sink).map_err(|e| match e {
        BuildError::Table(why) => StoreError::Inconsistent(why),
        BuildError::TooManyRows => StoreError::Full,
        BuildError::NameTooLong(len) => StoreError::NameTooLong(len),
        BuildError::Sink(e) => e,
    })?;
    let store = sink
        .builder
        .ok_or(StoreError::Inconsistent("the build asked for no columns"))?
        .finish()?;
    if report.cycles > 0 {
        tracing::warn!(
            "MFT: {} parent cycles broken (corrupt directory entries)",
            report.cycles
        );
    }
    tracing::debug!(
        "MFT: {} names -> {} nodes, {} orphans",
        report.names,
        report.nodes,
        report.orphans
    );
    Ok(store)
}

/// The store's memory for the build: a [`StoreBuilder`] of the size the build asks for.
struct StoreSink {
    opts: Option<StoreOptions>,
    builder: Option<StoreBuilder>,
}

impl TreeSink for StoreSink {
    type Error = StoreError;

    fn columns(
        &mut self,
        nodes: usize,
        image_len: usize,
        ext_table: Vec<String>,
    ) -> Result<(NodeColumns<'_>, &mut [u8]), StoreError> {
        let opts = self.opts.take().ok_or(StoreError::Inconsistent(
            "the build asked for columns twice",
        ))?;
        let mut b = StoreBuilder::new(nodes, image_len, opts);
        b.set_ext_table(ext_table);
        Ok(self.builder.insert(b).parts())
    }

    /// Escaped where it is not valid Unicode, as on the protocol and as 0.3.4 showed it (never
    /// U+FFFD), like the hard scan's extensions ([`crate::scan::entry_batch`]).
    fn ext_text(raw: &[u8]) -> String {
        sk_store::names::printable(raw).into_owned()
    }
}
