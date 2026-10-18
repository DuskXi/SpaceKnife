"""Built-in file classes, shared by the backend (filter, treemap colours) and every frontend.

A class is a named set of extensions; ``id`` is also the design-token key ``treemap.class.<id>``.
Names are matched case-insensitively against the display name, the aliases and the id.
SpaceSniffer 2.0.5.18's class names are aliases, so its filters keep working
(``:class:Audio/Music``), and its extension lists are included, extended with current formats.
An extension may belong to several classes (``3gp``): the first class in order gives the colour,
every class matches it in filters. ``other`` holds every file in no class.
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
        "mp4 mkv mov avi wmv webm m4v flv ts m2ts mts vob mpg mpeg mpe m1s qt rm rmvb 3gp f4v ogv",
    ),
    _c(
        "image",
        "图片",
        "Image|Images|Picture|Pictures|Pictures/Images|Photos",
        "jpg jpeg png gif bmp tif tiff heic heif webp avif jxl svg ico psd"
        " raw nef rw2 cr2 cr3 arw dng orf raf",
    ),
    _c(
        "audio",
        "音频",
        "Audio|Music|Audio/Music",
        "mp3 flac wav m4a aac ogg opus wma aif aiff alp au ac3 amr mp2 voc ape mid midi dsf 3gp",
    ),
    _c(
        "archive",
        "压缩包/镜像",
        "压缩包|镜像|Archive|Archives|Disk images|Archives/Disk images",
        "zip 7z 7zip rar tar gz tgz bz2 xz zst lz lzma cab iso img dmg wim esd vhd vhdx vmdk qcow2",
    ),
    _c(
        "exec",
        "程序/系统",
        "程序|系统|Program|Programs|Executable|Executables|System files|Executables/System files",
        "exe dll sys msi msix appx com bat cmd ps1 sh so dylib ko efi drv ocx cpl ini log",
    ),
    _c(
        "doc",
        "文档",
        "Document|Documents|Docs",
        "pdf doc docx xls xlsx ppt pptx odt ods odp rtf txt md csv epub pages numbers key",
    ),
    _c(OTHER, "其他", "Other|Others|Unclassified", ""),
)


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
