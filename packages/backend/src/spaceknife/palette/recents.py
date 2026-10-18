"""Palette recents (UI-W4-08, W4 待定 7): path jumps per scan target, filters shared by all.

Kept newest first, without duplicates (running an entry again moves it to the front), in one JSON
file in the data directory. Writes go through a temporary file and an atomic rename.
"""

from __future__ import annotations

import json
import os
import threading
import time
from dataclasses import dataclass
from pathlib import Path

from .. import paths

KEEP = 50


@dataclass(frozen=True, slots=True)
class Entry:
    kind: str  # jump | filter
    text: str
    at: float


class Recents:
    def __init__(self, file: Path | None = None) -> None:
        self.file = file or paths.data_dir() / "palette_recent.json"
        self._lock = threading.Lock()
        self._jumps: dict[str, list[Entry]] = {}
        self._filters: list[Entry] = []
        self._last = 0.0
        self._load()
        self._last = max(
            (e.at for e in [*self._filters, *(e for v in self._jumps.values() for e in v)]), default=0.0
        )

    def _now(self) -> float:
        """Strictly increasing time stamps, so entries made in the same clock tick keep their order."""
        self._last = max(time.time(), self._last + 1e-6)
        return self._last

    def _load(self) -> None:
        try:
            data = json.loads(self.file.read_text(encoding="utf-8"))
            self._jumps = {
                str(k): [Entry("jump", str(e["text"]), float(e["at"])) for e in v][:KEEP]
                for k, v in data.get("jumps", {}).items()
            }
            self._filters = [
                Entry("filter", str(e["text"]), float(e["at"])) for e in data.get("filters", [])
            ][:KEEP]
        except (OSError, ValueError, KeyError, TypeError, AttributeError):
            self._jumps, self._filters = {}, []

    def _save(self) -> None:
        payload = {
            "version": 1,
            "jumps": {k: [{"text": e.text, "at": e.at} for e in v] for k, v in self._jumps.items() if v},
            "filters": [{"text": e.text, "at": e.at} for e in self._filters],
        }
        self.file.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.file.with_suffix(".tmp")
        tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=1), encoding="utf-8")
        os.replace(tmp, self.file)

    @staticmethod
    def _push(items: list[Entry], entry: Entry, same: object) -> list[Entry]:
        return [entry, *(e for e in items if same(e))][:KEEP]  # type: ignore[operator]

    def add_jump(self, target: str, path: str) -> None:
        key = os.path.normcase(os.path.normpath(path))
        with self._lock:
            items = self._jumps.get(target, [])
            entry = Entry("jump", path, self._now())
            self._jumps[target] = self._push(
                items, entry, lambda e: os.path.normcase(os.path.normpath(e.text)) != key
            )
            self._save()

    def add_filter(self, text: str) -> None:
        with self._lock:
            self._filters = self._push(
                self._filters, Entry("filter", text, self._now()), lambda e: e.text != text
            )
            self._save()

    def clear(self, target: str | None) -> None:
        """Clear one scan target's jumps and the shared filters (UI-W4-11: the group disappears)."""
        with self._lock:
            if target is not None:
                self._jumps.pop(target, None)
            self._filters = []
            self._save()

    def entries(self, target: str | None) -> list[Entry]:
        """Jumps of ``target`` and all filters, newest first."""
        with self._lock:
            jumps = list(self._jumps.get(target, [])) if target is not None else []
            return sorted([*jumps, *self._filters], key=lambda e: -e.at)


__all__ = ["Entry", "Recents"]
