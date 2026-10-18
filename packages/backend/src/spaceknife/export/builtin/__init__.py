"""Built-in export templates: SpaceSniffer 2.0.5.18's four, plus SpaceKnife's folder tree.

The templates are the ``*.sktpl`` files next to this module (users can copy one as a starting
point); this module only names them. A template's name is its SpaceSniffer name, which is what
``spaceknife run … export "<name>" <file>`` accepts; title and description are what the UI shows,
in the interface language (M7.5: read them when they are shown).
"""

from __future__ import annotations

from dataclasses import dataclass
from importlib import resources

from ...i18n import _


@dataclass(frozen=True, slots=True)
class BuiltinInfo:
    name: str  # SpaceSniffer's name (command line, settings); never translated
    file: str
    title_id: str  # the Chinese source text (message id) of :attr:`title`
    description_id: str  # … of :attr:`description`
    spacesniffer: bool  # also built into SpaceSniffer

    @property
    def title(self) -> str:
        """The title in the current interface language."""
        return _(self.title_id)

    @property
    def description(self) -> str:
        """The description in the current interface language."""
        return _(self.description_id)


_BUILTINS: tuple[BuiltinInfo, ...] = (
    BuiltinInfo(
        "Grouped by folder",
        "grouped-by-folder.sktpl",
        "按文件夹分组",
        "每个直接含有文件的文件夹一段：完整路径和占用，下面列出其中的文件（占用右对齐）。",
        True,
    ),
    BuiltinInfo(
        "First level folders",
        "first-level-folders.sktpl",
        "第一层文件夹",
        "根下第一层的每个文件夹：完整路径和占用。",
        True,
    ),
    BuiltinInfo(
        "Simple file list",
        "simple-file-list.sktpl",
        "文件清单",
        "每个文件的完整路径，一行一个。",
        True,
    ),
    BuiltinInfo("Summary", "summary.sktpl", "汇总", "总占用、文件夹数和文件数。", True),
    BuiltinInfo(
        "Folder tree",
        "folder-tree.sktpl",
        "文件夹树",
        "按层级缩进的文件夹和占用。",
        False,
    ),
)


def _read(file: str) -> str:
    return resources.files(__name__).joinpath(file).read_text(encoding="utf-8")


BUILTIN_INFO: dict[str, BuiltinInfo] = {b.name: b for b in _BUILTINS}
BUILTIN_TEMPLATES: dict[str, str] = {b.name: _read(b.file) for b in _BUILTINS}
