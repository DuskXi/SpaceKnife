"""Exports (M6, docs/manage.md §4, docs/export-templates.md): SpaceSniffer templates, CSV / JSON / HTML."""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload
from .settings import SettingControl, SettingOption, SettingValue


class ExportOption(Payload):
    """A field of the export dialog that a plugin's format declares (``options`` of
    ``export_format``); its value goes to export.run ``options`` under ``name``."""

    name: str
    title: str
    description: str = ""
    control: SettingControl
    default: SettingValue
    options: list[SettingOption] = Field(default_factory=list)
    min: float | None = None
    max: float | None = None
    step: float | None = None
    unit: str = Field(default="", description="B: a size in bytes (shown and typed as a size)")
    placeholder: str = ""


class ExportTarget(Payload):
    """Something to export with: a built-in template or a format."""

    name: str = Field(
        description="What export.run takes: the template's SpaceSniffer name, or csv / json / html"
    )
    title: str
    description: str = ""
    kind: Literal["template", "format"]
    extension: str = Field(description="Suggested file extension, without the dot")
    file_name: str = Field(default="", description="The template's own suggested file name")
    spacesniffer: bool = Field(default=False, description="Also built into SpaceSniffer")
    plugin: str | None = Field(default=None, description="The plugin that adds it (name is its full id)")
    options: list[ExportOption] = Field(default_factory=list, description="Fields of a plugin's format")


class ExportTargets(Payload):
    targets: list[ExportTarget]


class ExportIssue(Payload):
    severity: Literal["error", "warning"]
    code: str
    message: str
    section: str
    line: int
    column: int


class ExportCheckParams(Params):
    source: str = Field(max_length=1 << 20)


class ExportCheckResult(Payload):
    ok: bool
    issues: list[ExportIssue]


class ExportRunParams(Params):
    view_id: str = Field(min_length=1, max_length=64)
    target: str = Field(default="", max_length=256, description="A target name; empty with source")
    source: str | None = Field(default=None, max_length=1 << 20, description="A template's text instead")
    path: str = Field(min_length=1, max_length=32767, description="The file to write (its folder must exist)")
    scope: Literal["zoom", "all", "selection", "node"] = Field(
        default="zoom",
        description="The view's zoom root, every scan root, the selected nodes (selection, else the "
        "view's), or one node (node)",
    )
    use_filter: bool = Field(default=True, description="Only what the view's filter matches")
    node: int | None = Field(default=None, ge=0, description="scope node: the node")
    selection: list[int] = Field(default_factory=list, max_length=100_000, description="scope selection")
    options: dict[str, SettingValue] = Field(
        default_factory=dict, description="Values of the format's options (by name)"
    )


class ExportRunResult(Payload):
    path: str
    bytes: int
    issues: list[ExportIssue] = Field(default_factory=list, description="Warnings of the template")
