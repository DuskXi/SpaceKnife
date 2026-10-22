"""The runtime interface: where a plugin's code runs, independent of what it does.

Two implementations: :class:`~.process.ProcessRuntime` (the default: one process per plugin) and
:class:`~.inprocess.InProcessRuntime` (built-in plugins, and developer mode). A WASM runtime would
implement the same interface with the same messages and tree images (docs/plugins.md, 「WASM」).
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from collections.abc import Callable, Coroutine, Sequence
from dataclasses import dataclass, field
from typing import Any, Literal

from spaceknife_protocol.plugins import PluginProcess
from spaceknife_sdk.plugin import Description

ProcessState = Literal["stopped", "starting", "running", "idle", "crashed"]
Chunk = Callable[[Any, list[bytes]], None]
# (method, params, frames) -> (value, frames): the host's answer to a plugin's request
RequestFn = Callable[[str, dict[str, Any], list[bytes]], Coroutine[Any, Any, tuple[Any, list[bytes]]]]
NotifyFn = Callable[[str, dict[str, Any]], None]  # event, state, toast, log, progress, image_drop
ExitFn = Callable[[int | None, str], None]  # (exit code, why) when it ended without being stopped


class RuntimeCallError(Exception):
    """A call failed in the runtime (protocol error code, message for the user, data)."""

    def __init__(self, code: int, message: str, data: Any = None) -> None:
        super().__init__(message)
        self.code = int(code)
        self.message = message
        self.data = data


@dataclass(slots=True)
class Callbacks:
    request: RequestFn
    notify: NotifyFn
    exited: ExitFn


@dataclass(slots=True)
class StartInfo:
    """What the plugin gets when it starts (the ``init`` message)."""

    plugin: str
    version: str
    folder: str
    storage: str
    lang: str
    settings: dict[str, Any]
    granted: list[str]
    roots: list[str]
    net_hosts: list[str]
    exec: list[str]
    builtin: bool = False
    dev: bool = False
    memory_mb: int = 1024
    states: dict[str, dict[str, Any]] = field(default_factory=dict)

    def message(self) -> dict[str, Any]:
        return {
            "type": "init",
            "plugin": self.plugin,
            "version": self.version,
            "dir": self.folder,
            "storage": self.storage,
            "lang": self.lang,
            "settings": self.settings,
            "granted": self.granted,
            "roots": self.roots,
            "net_hosts": self.net_hosts,
            "exec": self.exec,
            "builtin": self.builtin,
            "dev": self.dev,
            "memory_mb": self.memory_mb,
            "states": self.states,
        }


class PluginRuntime(ABC):
    kind: Literal["process", "inprocess"]
    state: ProcessState = "stopped"

    @abstractmethod
    async def start(self) -> Description:
        """Start the code and return its description (raises RuntimeCallError when it cannot)."""

    @abstractmethod
    async def call(
        self,
        call_id: int,
        kind: str,
        name: str,
        params: Any,
        ctx: dict[str, Any],
        frames: Sequence[bytes] = (),
        on_chunk: Chunk | None = None,
    ) -> tuple[Any, list[bytes]]:
        """One call; raises RuntimeCallError (PLUGIN_ERROR, PLUGIN_UNAVAILABLE …)."""

    @abstractmethod
    async def cancel(self, call_id: int) -> None: ...

    @abstractmethod
    async def update(self, fields: dict[str, Any]) -> None:
        """Language, settings, grants or scan roots changed."""

    @abstractmethod
    async def stop(self, grace: float = 2.0) -> None:
        """Stop: the stop hook gets ``grace`` seconds, then the code is gone."""

    @abstractmethod
    def kill(self) -> None: ...

    def busy(self) -> int:
        """Calls in flight."""
        return 0

    def info(self) -> PluginProcess:
        return PluginProcess(state=self.state)


__all__ = [
    "Callbacks",
    "Chunk",
    "PluginRuntime",
    "ProcessState",
    "RuntimeCallError",
    "StartInfo",
]
