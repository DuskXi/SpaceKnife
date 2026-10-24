"""File operations (M6, docs/manage.md): path formats, the delete basket and its execution (W5)."""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload

BasketItemKind = Literal["file", "dir", "link", "other", "group"]
BasketItemStatus = Literal["ready", "locked", "missing", "protected"]
OpMode = Literal["trash", "delete"]
OpCode = Literal[
    "ok",
    "missing",
    "denied",
    "locked",
    "protected",
    "no_trash",
    "exists",
    "cancelled",
    "ambiguous_name",
    "error",
]


# --------------------------------------------------------------------------- path formats (W5-09…14)


class PathFormsParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    node: int = Field(ge=1)


class PathForms(Payload):
    """One path four ways, formatted by the backend so every front end copies the same text."""

    raw: str
    quoted: str = Field(description="In double quotes (Windows) or shell-quoted (POSIX)")
    posix: str = Field(description="Forward slashes")
    uri: str = Field(description="file:// URI (RFC 8089), non-ASCII percent-encoded")
    on_disk: bool = Field(default=True, description="False for MFT pseudo folders: nothing to open")


# --------------------------------------------------------------------------- the basket (W5-25…40)


class BasketItem(Payload):
    id: str
    scan_id: str
    path: str
    name: str
    kind: BasketItemKind
    members: int = Field(default=0, description="Small items of an aggregate block (kind 'group')")
    alloc: int = Field(description="Bytes freed by deleting it (cloud placeholders, partial hard links: 0)")
    size: int
    files: int = 0
    dirs: int = 0
    volume: str = Field(description="Mount point of the volume holding it")
    status: BasketItemStatus = "ready"
    note: str = Field(default="", description="Why it cannot be deleted as is (locked by …, missing …)")
    tag: str | None = None
    origin: str | None = Field(default=None, description="The plugin cleanup that proposed it (full id)")
    reason: str = Field(default="", description="Why it was proposed (plugin cleanups)")


class BasketVolume(Payload):
    mount_point: str
    total: int
    free: int
    freeable: int = Field(description="Bytes the basket frees on this volume")
    trash: bool = Field(description="The volume has a recycle bin")
    trash_note: str = Field(default="", description="Where deleted items go (or why they cannot)")


class BasketState(Payload):
    items: list[BasketItem]
    volumes: list[BasketVolume]
    freeable: int = Field(description="Bytes freed in all, every volume")


class BasketAddParams(Params):
    scan_id: str = Field(min_length=1, max_length=64)
    nodes: list[int] = Field(default_factory=list, max_length=100_000)
    view_id: str | None = Field(default=None, description="With key: the view whose frame holds that block")
    key: int | None = Field(default=None, description="A block of the view's frame (an aggregate's members)")
    origin: str | None = Field(
        default=None, max_length=64, description="A plugin cleanup's full id (shown in the basket)"
    )
    reasons: list[str] | None = Field(
        default=None, max_length=100_000, description="Per node, why it was proposed (same order as nodes)"
    )


class Refusal(Payload):
    path: str
    reason: str


class BasketAddResult(Payload):
    state: BasketState
    added: int
    merged: int = Field(description="Already covered by an item (or one of its folders)")
    absorbed: int = Field(description="Items replaced by a folder that holds them")
    refused: list[Refusal] = Field(default_factory=list)


class BasketRemoveParams(Params):
    ids: list[str] = Field(max_length=100_000)


class BasketEvent(Payload):
    state: BasketState


# --------------------------------------------------------------------------- execution (W5-41…49)


class BasketExecuteParams(Params):
    mode: OpMode
    ids: list[str] | None = Field(default=None, description="None: every item")


class OpTask(Payload):
    task_id: str
    mode: OpMode
    total: int
    delay_s: float = Field(default=0.0, description="Permanent deletion waits this long (undo window)")


class OpTaskParams(Params):
    task_id: str = Field(min_length=1, max_length=64)


class OpItemResult(Payload):
    path: str
    ok: bool
    code: OpCode
    message: str = ""


class OpProgressEvent(Payload):
    task_id: str
    mode: OpMode = "trash"
    done: int
    total: int
    current: str = ""
    phase: Literal["waiting", "running", "done", "cancelled", "undone"]
    results: list[OpItemResult] = Field(default_factory=list, description="Filled once the task ends")
    freed: int = Field(default=0, description="Bytes moved to the trash / deleted (items that succeeded)")
    can_undo: bool = False
