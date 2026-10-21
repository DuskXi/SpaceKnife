"""Built-in file classes, shared by the backend (filter, treemap colours) and every frontend.

A class is a named set of extensions; ``id`` is also the design-token key ``treemap.class.<id>``.
Names are matched case-insensitively against the display name, the aliases and the id.
SpaceSniffer 2.0.5.18's class names are aliases, so its filters keep working
(``:class:Audio/Music``), and its extension lists are included, extended with current formats.
``code`` and ``data`` are SpaceKnife's own: source code and build output, and models, datasets
and databases (on a developer's disk most of what used to be 「其他」).
An extension may belong to several classes (``3gp``): the first class in order gives the colour,
every class matches it in filters. ``ts`` and ``mts`` are both video and code: the backend tells a
file apart by its size (``spaceknife.classes.STREAM_SPLITS``). ``other`` holds every file in no
class.
"""

from __future__ import annotations

from pydantic import Field

from .base import Params, Payload
from .filter import FilterClassInfo

OTHER = "other"


def _c(id_: str, name: str, aliases: str, exts: str) -> FilterClassInfo:
    return FilterClassInfo(
        id=id_, name=name, aliases=aliases.split("|") if aliases else [], extensions=exts.split()
    )


BUILTIN_CLASSES: tuple[FilterClassInfo, ...] = (
    _c(
        "video",
        "视频",
        "Video|Videos|Movies|Video/Movies",
        "mp4 mkv mov avi wmv webm m4v flv ts m2ts mts vob mpg mpeg mpe m1s qt rm rmvb 3gp f4v ogv"
        # camera, broadcast and game video
        " mxf dv divx y4m m2t m2v h264 h265 hevc ivf asf wtv mk3d ogm lrv insv braw r3d bik bk2 usm",
    ),
    _c(
        "image",
        "图片",
        "Image|Images|Picture|Pictures|Pictures/Images|Photos",
        "jpg jpeg png gif bmp tif tiff heic heif webp avif jxl svg ico psd"
        " raw nef rw2 cr2 cr3 arw dng orf raf"
        # more camera RAW, HDR and textures, design and vector files
        " srw x3f pef rwl mrw dcr kdc erf mef mos nrw sr2 srf iiq 3fr"
        " jfif jpe jp2 j2k jxr hdp wdp apng qoi svgz exr hdr dds ktx ktx2 tga pcx pbm pgm ppm pnm"
        " icns cur ani psb xcf kra clip pdn sai afphoto afdesign ai eps emf wmf cdr sketch fig xd",
    ),
    _c(
        "audio",
        "音频",
        "Audio|Music|Audio/Music",
        "mp3 flac wav m4a aac ogg opus wma aif aiff alp au ac3 amr mp2 voc ape mid midi dsf 3gp"
        " m4b m4r caf mka wv tta dts mpc spx weba oga gsm ra aifc w64 rf64 dff xm s3m it sf2 wem bnk",
    ),
    _c(
        "archive",
        "压缩包/镜像",
        "压缩包|镜像|Archive|Archives|Disk images|Archives/Disk images",
        "zip 7z 7zip rar tar gz tgz bz2 xz zst lz lzma cab iso img dmg wim esd vhd vhdx vmdk qcow2"
        " tbz tbz2 txz tzst lz4 lzo br sz zpaq zipx arj lha lzh cpio xar sit sitx"
        # virtual machine disks and memory, disc images
        " vdi qcow avhdx hdd hds vmem vmsn ova xva sparseimage nrg",
    ),
    _c(
        "exec",
        "程序/系统",
        "程序|系统|Program|Programs|Executable|Executables|System files|Executables/System files",
        "exe dll sys msi msix appx com bat cmd ps1 sh so dylib ko efi drv ocx cpl ini log"
        # app packages and extensions, runtime modules, updates, dumps and event logs
        " apk aab xapk ipa deb rpm snap appimage jar war ear vsix crx xpi pyd node wasm"
        " msp msu mui tlb winmd dmp mdmp etl evtx",
    ),
    _c(
        "doc",
        "文档",
        "Document|Documents|Docs",
        "pdf doc docx xls xlsx ppt pptx odt ods odp rtf txt md csv epub pages numbers key"
        " docm dotx dotm xlsm xlsb pptm odg wps et dps hwp ofd xps oxps djvu chm mobi azw azw3 fb2"
        " tex rst org tsv vsd vsdx one eml msg pst ost mbox",
    ),
    _c(
        "code",
        "代码/开发",
        "代码|开发|Code|Source code|Development|Dev",
        "c h cc cpp cxx hh hpp hxx cs java kt kts scala groovy gradle go rs swift m mm"
        " py pyi pyw pyx ipynb js mjs cjs jsx ts tsx mts cts vue svelte css scss sass less"
        " php rb pl pm lua r jl dart ex exs erl hs elm clj cljs fs fsx vb cmake"
        # build output and caches of compilers and package managers
        " pyc pyo class o lib a pdb ilk idb pch ipch tlog whl egg gem nupkg",
    ),
    _c(
        "data",
        "数据/模型",
        "数据|模型|Data|Datasets|Models|Databases",
        # model weights, datasets and scientific data
        "safetensors gguf ggml ckpt pt pth onnx tflite mlmodel h5 hdf5 he5 nc npy npz pkl pickle"
        " joblib parquet arrow feather avro orc tfrecord jsonl ndjson json fits fts fit mat"
        # databases
        " db sqlite sqlite3 db3 duckdb mdb accdb ldb vscdb realm ibd myd myi mdf ldf ndf dbf kdbx sql",
    ),
    _c(OTHER, "其他", "Other|Others|Unclassified", ""),
)


# The built-in classes' names in the English interface (M7.5). Each is already one of the class's
# aliases, so filters read the same in both languages.
ENGLISH_NAMES: dict[str, str] = {
    "video": "Video",
    "image": "Pictures",
    "audio": "Audio",
    "archive": "Archives",
    "exec": "Programs",
    "doc": "Documents",
    "code": "Code",
    "data": "Data",
    OTHER: "Other",
}


def localized(c: FilterClassInfo, language: str) -> FilterClassInfo:
    """A class as shown in ``language``: a built-in class the user has not renamed takes its
    English name, and its Chinese name becomes an alias (``:class:视频`` keeps working)."""
    if language != "en":
        return c
    builtin = next((b for b in BUILTIN_CLASSES if b.id == c.id), None)
    english = ENGLISH_NAMES.get(c.id)
    if builtin is None or english is None or c.name != builtin.name:
        return c
    aliases = [a for a in c.aliases if a.casefold() != english.casefold()]
    return c.model_copy(update={"name": english, "aliases": [*aliases, builtin.name]})


# --------------------------------------------------------------------------- editing (W7 文件类别)


class ClassEdit(Params):
    """One class as the editor sends it; order in the list is the display (and colour) order."""

    id: str | None = Field(default=None, max_length=64, description="None: a new class")
    name: str = Field(min_length=1, max_length=64)
    extensions: list[str] = Field(default_factory=list, max_length=4096)
    color: str = Field(default="", pattern=r"^(#[0-9A-Fa-f]{6})?$")


class ClassesState(Payload):
    classes: list[FilterClassInfo]
    defaults: list[FilterClassInfo]
    custom: bool = Field(description="The table differs from the built-in one")


class ClassesUpdateParams(Params):
    classes: list[ClassEdit] = Field(min_length=1, max_length=256)


class ClassesEvent(Payload):
    state: ClassesState
