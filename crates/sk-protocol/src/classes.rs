//! Built-in file classes, shared by the backend (filter, treemap colours) and every frontend.
//!
//! A class is a named set of extensions; `id` is also the design-token key `treemap.class.<id>`.
//! Names are matched case-insensitively against the display name, the aliases and the id.
//! SpaceSniffer 2.0.5.18's class names are aliases, so its filters keep working
//! (`:class:Audio/Music`), and its extension lists are included, extended with current formats.
//! `code` and `data` are SpaceKnife's own: source code and build output, and models, datasets
//! and databases. `game` has no extensions: recognised folders fill it, and recognised project
//! folders and model caches fill `code` and `data` whatever the extensions inside.
//! An extension may belong to several classes (`3gp`): the first class in order gives the colour,
//! every class matches it in filters. `ts` and `mts` are both video and code: the backend tells a
//! file apart by its size. `other` holds every file in no class.

use std::sync::LazyLock;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::filter::FilterClassInfo;
use crate::validate::{self as v, Validate, ValidationResult};

pub const OTHER: &str = "other";

/// (id, name, aliases separated by `|`, extensions separated by spaces)
const BUILTIN: &[(&str, &str, &str, &str)] = &[
    (
        "video",
        "视频",
        "Video|Videos|Movies|Video/Movies",
        concat!(
            "mp4 mkv mov avi wmv webm m4v flv ts m2ts mts vob mpg mpeg mpe m1s qt rm rmvb 3gp f4v ogv",
            // camera, broadcast and game video
            " mxf dv divx y4m m2t m2v h264 h265 hevc ivf asf wtv mk3d ogm lrv insv braw r3d bik bk2 usm",
        ),
    ),
    (
        "image",
        "图片",
        "Image|Images|Picture|Pictures|Pictures/Images|Photos",
        concat!(
            "jpg jpeg png gif bmp tif tiff heic heif webp avif jxl svg ico psd",
            " raw nef rw2 cr2 cr3 arw dng orf raf",
            // more camera RAW, HDR and textures, design and vector files
            " srw x3f pef rwl mrw dcr kdc erf mef mos nrw sr2 srf iiq 3fr",
            " jfif jpe jp2 j2k jxr hdp wdp apng qoi svgz exr hdr dds ktx ktx2 tga pcx pbm pgm ppm pnm",
            " icns cur ani psb xcf kra clip pdn sai afphoto afdesign ai eps emf wmf cdr sketch fig xd",
        ),
    ),
    (
        "audio",
        "音频",
        "Audio|Music|Audio/Music",
        concat!(
            "mp3 flac wav m4a aac ogg opus wma aif aiff alp au ac3 amr mp2 voc ape mid midi dsf 3gp",
            " m4b m4r caf mka wv tta dts mpc spx weba oga gsm ra aifc w64 rf64 dff xm s3m it sf2 wem bnk",
        ),
    ),
    (
        "archive",
        "压缩包/镜像",
        "压缩包|镜像|Archive|Archives|Disk images|Archives/Disk images",
        concat!(
            "zip 7z 7zip rar tar gz tgz bz2 xz zst lz lzma cab iso img dmg wim esd vhd vhdx vmdk qcow2",
            " tbz tbz2 txz tzst lz4 lzo br sz zpaq zipx arj lha lzh cpio xar sit sitx",
            // virtual machine disks and memory, disc images
            " vdi qcow avhdx hdd hds vmem vmsn ova xva sparseimage nrg",
        ),
    ),
    (
        "exec",
        "程序/系统",
        "程序|系统|Program|Programs|Executable|Executables|System files|Executables/System files",
        concat!(
            "exe dll sys msi msix appx com bat cmd ps1 sh so dylib ko efi drv ocx cpl ini log",
            // app packages and extensions, runtime modules, updates, dumps and event logs
            " apk aab xapk ipa deb rpm snap appimage jar war ear vsix crx xpi pyd node wasm",
            " msp msu mui tlb winmd dmp mdmp etl evtx",
        ),
    ),
    (
        "doc",
        "文档",
        "Document|Documents|Docs",
        concat!(
            "pdf doc docx xls xlsx ppt pptx odt ods odp rtf txt md csv epub pages numbers key",
            " docm dotx dotm xlsm xlsb pptm odg wps et dps hwp ofd xps oxps djvu chm mobi azw azw3 fb2",
            " tex rst org tsv vsd vsdx one eml msg pst ost mbox",
        ),
    ),
    // filled by folders: everything in a recognised game install
    ("game", "游戏", "Game|Games", ""),
    (
        "code",
        "代码/开发",
        "代码|开发|Code|Source code|Development|Dev",
        concat!(
            "c h cc cpp cxx hh hpp hxx cs java kt kts scala groovy gradle go rs swift m mm",
            " py pyi pyw pyx ipynb js mjs cjs jsx ts tsx mts cts vue svelte css scss sass less",
            " php rb pl pm lua r jl dart ex exs erl hs elm clj cljs fs fsx vb cmake",
            // build output and caches of compilers and package managers
            " pyc pyo class o lib a pdb ilk idb pch ipch tlog whl egg gem nupkg",
        ),
    ),
    (
        "data",
        "数据/模型",
        "数据|模型|Data|Datasets|Models|Databases",
        concat!(
            // model weights, datasets and scientific data
            "safetensors gguf ggml ckpt pt pth onnx tflite mlmodel h5 hdf5 he5 nc npy npz pkl pickle",
            " joblib parquet arrow feather avro orc tfrecord jsonl ndjson json fits fts fit mat",
            // databases
            " db sqlite sqlite3 db3 duckdb mdb accdb ldb vscdb realm ibd myd myi mdf ldf ndf dbf kdbx sql",
        ),
    ),
    (OTHER, "其他", "Other|Others|Unclassified", ""),
];

static BUILTIN_CLASSES: LazyLock<Vec<FilterClassInfo>> = LazyLock::new(|| {
    BUILTIN
        .iter()
        .map(|&(id, name, aliases, exts)| FilterClassInfo {
            id: id.to_owned(),
            name: name.to_owned(),
            aliases: if aliases.is_empty() {
                Vec::new()
            } else {
                aliases.split('|').map(str::to_owned).collect()
            },
            extensions: exts.split_whitespace().map(str::to_owned).collect(),
            color: String::new(),
            color_light: String::new(),
        })
        .collect()
});

/// The built-in class table, in display (and colour precedence) order.
pub fn builtin_classes() -> &'static [FilterClassInfo] {
    &BUILTIN_CLASSES
}

/// The built-in classes' names in the English interface. Each is already one of the class's
/// aliases, so filters read the same in both languages.
pub const ENGLISH_NAMES: &[(&str, &str)] = &[
    ("video", "Video"),
    ("image", "Pictures"),
    ("audio", "Audio"),
    ("archive", "Archives"),
    ("exec", "Programs"),
    ("doc", "Documents"),
    ("game", "Games"),
    ("code", "Code"),
    ("data", "Data"),
    (OTHER, "Other"),
];

pub fn english_name(id: &str) -> Option<&'static str> {
    ENGLISH_NAMES
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, v)| *v)
}

/// A class as shown in `language`: a built-in class the user has not renamed takes its English
/// name, and its Chinese name becomes an alias (`:class:视频` keeps working).
pub fn localized(c: &FilterClassInfo, language: &str) -> FilterClassInfo {
    if language != "en" {
        return c.clone();
    }
    let builtin = builtin_classes().iter().find(|b| b.id == c.id);
    let (Some(builtin), Some(english)) = (builtin, english_name(&c.id)) else {
        return c.clone();
    };
    if c.name != builtin.name {
        return c.clone();
    }
    let english_fold = crate::filter_syntax::casefold(english);
    let mut aliases: Vec<String> = c
        .aliases
        .iter()
        .filter(|a| crate::filter_syntax::casefold(a) != english_fold)
        .cloned()
        .collect();
    aliases.push(builtin.name.clone());
    FilterClassInfo {
        name: english.to_owned(),
        aliases,
        ..c.clone()
    }
}

// --------------------------------------------------------------------------- editing (W7 文件类别)

/// One class as the editor sends it; order in the list is the display (and colour) order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassEdit {
    /// None: a new class
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    /// for the dark theme
    #[serde(default)]
    pub color: String,
    /// for the light theme
    #[serde(default)]
    pub color_light: String,
}

impl Validate for ClassEdit {
    fn validate(&self) -> ValidationResult {
        v::opt_str_len("id", &self.id, 0, 64)?;
        v::str_len("name", &self.name, 1, 64)?;
        v::list_len("extensions", &self.extensions, 0, 4096)?;
        v::pattern(
            "color",
            v::is_color_or_empty(&self.color),
            "^(#[0-9A-Fa-f]{6})?$",
        )?;
        v::pattern(
            "color_light",
            v::is_color_or_empty(&self.color_light),
            "^(#[0-9A-Fa-f]{6})?$",
        )
    }
}

/// The user's choice for one folder: its files take ``class_id`` whatever their extension.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClassFolder {
    pub path: String,
    /// A class id, or "" for by extension only (no folder class)
    pub class_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClassesState {
    pub classes: Vec<FilterClassInfo>,
    pub defaults: Vec<FilterClassInfo>,
    /// The table differs from the built-in one
    pub custom: bool,
    /// The user's folder choices, by path
    #[serde(default)]
    pub folders: Vec<ClassFolder>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassesFolderParams {
    pub path: String,
    /// A class id; "" for by extension only; None forgets the choice (automatic again)
    #[serde(default)]
    pub class_id: Option<String>,
}

impl Validate for ClassesFolderParams {
    fn validate(&self) -> ValidationResult {
        v::str_len("path", &self.path, 1, 4096)?;
        v::opt_str_len("class_id", &self.class_id, 0, 64)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassesUpdateParams {
    pub classes: Vec<ClassEdit>,
}

impl Validate for ClassesUpdateParams {
    fn validate(&self) -> ValidationResult {
        v::list_len("classes", &self.classes, 1, 256)?;
        for (i, c) in self.classes.iter().enumerate() {
            c.validate()
                .map_err(|e| e.at(i.to_string()).at("classes"))?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClassesEvent {
    pub state: ClassesState,
}
