"""Settings (W7): one backend-owned document, described by metadata so every frontend renders the
same page from it (UI-W7-05, UI-W7-27, UI-W7-28).

Values are flat ``{key: value}``; keys are dotted (``scan.cross_mounts``). The backend validates
every write, saves atomically, and pushes ``settings.changed`` to all connected sessions.
"""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload
from .filter import FilterClassInfo

SettingCategory = Literal[
    "general", "appearance", "treemap", "scan", "classes", "tags", "keys", "advanced", "about", "plugins"
]
SettingControl = Literal[
    "switch", "segmented", "select", "multi", "number", "slider", "color", "text", "textarea", "list"
]
"""``select``: a drop-down (many options); ``multi``: several options at once, the value is their
values joined by commas (``"name,size"``); ``textarea``: text of several lines; ``list``: entries of
the same few fields (``fields``), the value is a list of ``{field: value}``."""
SettingApplies = Literal["now", "next_scan", "new_views"]
SettingScalar = bool | int | float | str
SettingEntry = dict[str, SettingScalar]
SettingValue = bool | int | float | str | list[SettingEntry]
TEXT_MAX = 64  # the longest text value when a setting does not say (max_length)


class SettingOption(Payload):
    value: SettingScalar
    label: str
    reason: str = Field(default="", description="Why this option cannot be chosen here ('' = available)")


class SettingField(Payload):
    """One field of a ``list`` setting's entries (a plugin's setting), in the interface language."""

    name: str = Field(description="Its key in each entry")
    title: str
    description: str = ""
    control: SettingControl = Field(description="Never list")
    default: SettingScalar
    options: list[SettingOption] = Field(default_factory=list)
    min: float | None = None
    max: float | None = None
    step: float | None = None
    unit: str = ""
    placeholder: str = ""
    max_length: int | None = Field(default=None, description="text, textarea: the longest value")


class SettingMeta(Payload):
    key: str
    category: SettingCategory
    group: str
    group_note: str = ""
    name: str
    description: str
    control: SettingControl
    options: list[SettingOption] = Field(default_factory=list)
    min: float | None = None
    max: float | None = None
    step: float | None = None
    unit: str = ""
    applies: SettingApplies = "now"
    synonyms: list[str] = Field(
        default_factory=list, description="Extra search words (English, abbreviations)"
    )
    enabled: bool = True
    reason: str = Field(
        default="", description="Why the setting is disabled (platform, or the milestone that wires it)"
    )
    depends: str | None = Field(
        default=None,
        description="'key' (a switch that must be on) or 'key=value' for this row to be editable",
    )
    plugin: str | None = Field(default=None, description="The plugin it belongs to (category 'plugins')")
    placeholder: str = Field(default="", description="Shown in an empty text field")
    max_length: int | None = Field(
        default=None, description="text, textarea: the longest value in characters (None: 64)"
    )
    fields: list[SettingField] = Field(default_factory=list, description="list: the fields of each entry")
    max_items: int | None = Field(default=None, description="list: the most entries it holds")


class SettingsState(Payload):
    version: int = Field(description="Bumped by every change; send it back with settings.update")
    values: dict[str, SettingValue]
    defaults: dict[str, SettingValue]
    meta: list[SettingMeta]
    path: str = Field(description="The settings file (UI-W7-25)")
    logs: str = Field(description="The log folder (W7 高级)")
    snapshots: str = Field(description="The auto-saved snapshots folder (W1 最近记录)")
    error: str = Field(
        default="", description="The settings file on disk is invalid; the last valid values are in use"
    )
    unit_base: Literal[1000, 1024] = Field(description="Size unit base after resolving 'system'")
    language: Literal["zh-CN", "en"] = Field(
        default="zh-CN", description="Interface language after resolving 'system'"
    )


class SettingsUpdateParams(Params):
    values: dict[str, SettingValue] = Field(description="Keys to change; others keep their value")
    version: int | None = Field(default=None, description="Reject the change if the document moved on since")


class SettingsResetParams(Params):
    category: SettingCategory | None = Field(default=None, description="None resets every setting")


class SettingsChangedEvent(Payload):
    """``settings.changed``: pushed to every session after any change (also edits of the file)."""

    state: SettingsState


# --------------------------------------------------------------------------- 「高级」 (UI-W7-36)


class SettingsFileParams(Params):
    path: str = Field(min_length=1, max_length=32767)


class SettingChange(Payload):
    key: str
    name: str
    before: SettingValue
    after: SettingValue


class SettingsImportParams(Params):
    path: str = Field(min_length=1, max_length=32767)
    apply: bool = Field(default=False, description="False: only tell what would change (the user confirms)")


class SettingsImportResult(Payload):
    source: Literal["spaceknife", "spacesniffer"]
    changes: list[SettingChange]
    classes: list[FilterClassInfo] | None = Field(
        default=None, description="The new file class table, if it changes"
    )
    skipped: list[str] = Field(default_factory=list, description="What could not be taken over, and why")
    applied: bool = False


class MaintenanceInfo(Payload):
    helper_running: bool
    helper_elevated: bool
    cache_dir: str
    cache_bytes: int
    snapshots_dir: str
    snapshots_bytes: int
    snapshot_count: int
    saved_tags: int = Field(description="Scan roots with remembered tags")
