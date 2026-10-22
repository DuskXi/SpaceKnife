"""Exports (M6, docs/manage.md §4, docs/export-templates.md): SpaceSniffer templates, CSV / JSON / HTML."""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload


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
    scope: Literal["zoom", "all"] = Field(
        default="zoom", description="The view's zoom root, or every scan root"
    )
    use_filter: bool = Field(default=True, description="Only what the view's filter matches")


class ExportRunResult(Payload):
    path: str
    bytes: int
    issues: list[ExportIssue] = Field(default_factory=list, description="Warnings of the template")
