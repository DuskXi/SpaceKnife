"""The extension registry: every contribution of the built-in features and of the plugins, by kind.

A contribution is registered with its owner (``"builtin"`` or a plugin id); a :class:`Point` per kind
checks it and wires it into the subsystem that uses it (the visualization registry, the rule sets,
the class table …). Turning a plugin off is :meth:`Extensions.unregister_owner`. Subsystems that read
contributions when they need them (commands, settings, filters) key their caches on
:attr:`Extensions.version`; listeners hear which kinds changed (the plugin host turns that into the
``extensions.changed`` event).
"""

from __future__ import annotations

import threading
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from typing import Any, Protocol

from loguru import logger

BUILTIN = "builtin"


def shipped(owner: str) -> bool:
    """``owner`` is part of the app: the native built-ins or a plugin shipped with it (``builtin.*``)."""
    return owner == BUILTIN or owner.startswith(BUILTIN + ".")


@dataclass(frozen=True, slots=True)
class Contribution:
    kind: str  # "command", "report", "visualization", …
    id: str  # full id: plugins "<plugin id>.<local name>"; built-ins keep their ids ("view.rescan", "csv")
    owner: str  # BUILTIN or a plugin id
    spec: Any = None  # the validated declaration (an SDK spec model; None for native built-ins)
    impl: Any = None  # the host side: an adapter calling the plugin, a native object, a compiled rule …
    order: int = 0

    @property
    def builtin(self) -> bool:
        return self.owner == BUILTIN

    @property
    def shipped(self) -> bool:
        """Part of the app: a native built-in or a plugin shipped with it (``builtin.*``)."""
        return shipped(self.owner)

    @property
    def local(self) -> str:
        """The plugin-local name (the full id without the owner)."""
        if self.owner != BUILTIN and self.id.startswith(self.owner + "."):
            return self.id[len(self.owner) + 1 :]
        return self.id


class ExtensionError(ValueError):
    """A contribution was refused; ``issues`` says why (nothing of it was registered)."""

    def __init__(self, issues: list[str]) -> None:
        self.issues = issues
        super().__init__("; ".join(issues))


class Point(Protocol):
    kind: str
    api: int

    def check(self, c: Contribution) -> list[str]:
        """Problems of ``c`` (checked before anything is registered)."""
        ...

    def add(self, c: Contribution) -> None:
        """Wire ``c`` into its subsystem."""
        ...

    def remove(self, c: Contribution) -> None: ...


class BasePoint:
    """A point whose subsystem reads :meth:`Extensions.of` itself: nothing to wire."""

    api = 1

    def __init__(self, kind: str) -> None:
        self.kind = kind

    def check(self, c: Contribution) -> list[str]:
        return []

    def add(self, c: Contribution) -> None:
        return None

    def remove(self, c: Contribution) -> None:
        return None


Listener = Callable[[frozenset[str]], None]


class Extensions:
    def __init__(self) -> None:
        self._lock = threading.RLock()
        self._items: dict[str, dict[str, Contribution]] = {}
        self._points: dict[str, Point] = {}
        self._seq: dict[tuple[str, str], int] = {}  # registration order: ties keep declaration order
        self._next = 0
        self.version = 0
        self.listeners: dict[str, Listener] = {}

    # ------------------------------------------------------------- points

    def point(self, p: Point) -> Point:
        with self._lock:
            self._points[p.kind] = p
        return p

    def point_of(self, kind: str) -> Point:
        with self._lock:
            p = self._points.get(kind)
            if p is None:
                p = self._points[kind] = BasePoint(kind)
            return p

    # ------------------------------------------------------------- changes

    def register(self, c: Contribution) -> None:
        self.register_many([c])

    def register_many(self, contributions: Iterable[Contribution]) -> None:
        """All or nothing: checks every contribution first, then wires them in order."""
        items = list(contributions)
        if not items:
            return
        with self._lock:
            issues: list[str] = []
            seen: set[tuple[str, str]] = set()
            for c in items:
                key = (c.kind, c.id)
                if key in seen or c.id in self._items.get(c.kind, {}):
                    issues.append(f"{c.kind} {c.id!r} is already registered")
                    continue
                seen.add(key)
                if c.owner != BUILTIN and not c.id.startswith(c.owner + "."):
                    issues.append(f"{c.kind} {c.id!r} must start with its plugin id {c.owner!r}")
                    continue
                try:
                    issues += [f"{c.kind} {c.id!r}: {i}" for i in self.point_of(c.kind).check(c)]
                except Exception as exc:
                    issues.append(f"{c.kind} {c.id!r}: {exc}")
            if issues:
                raise ExtensionError(issues)
            done: list[Contribution] = []
            try:
                for c in items:
                    self.point_of(c.kind).add(c)
                    self._items.setdefault(c.kind, {})[c.id] = c
                    self._next += 1
                    self._seq[(c.kind, c.id)] = self._next
                    done.append(c)
            except Exception as exc:
                for c in reversed(done):
                    self._drop(c)
                raise ExtensionError([f"{items[len(done)].kind} {items[len(done)].id!r}: {exc}"]) from exc
            self.version += 1
        self._notify(frozenset(c.kind for c in items))

    def _drop(self, c: Contribution) -> None:
        self._items.get(c.kind, {}).pop(c.id, None)
        self._seq.pop((c.kind, c.id), None)
        try:
            self.point_of(c.kind).remove(c)
        except Exception:
            logger.exception("removing {} {} failed", c.kind, c.id)

    def unregister_owner(self, owner: str) -> list[Contribution]:
        """Remove everything ``owner`` registered (in reverse order); returns what went."""
        with self._lock:
            gone = [c for by_id in self._items.values() for c in by_id.values() if c.owner == owner]
            for c in reversed(gone):
                self._drop(c)
            if gone:
                self.version += 1
        if gone:
            self._notify(frozenset(c.kind for c in gone))
        return gone

    def unregister(self, kind: str, id: str) -> Contribution | None:
        with self._lock:
            c = self._items.get(kind, {}).get(id)
            if c is None:
                return None
            self._drop(c)
            self.version += 1
        self._notify(frozenset({kind}))
        return c

    def touch(self, kinds: frozenset[str]) -> None:
        """Contributions of ``kinds`` answer differently now (a plugin's settings changed): a new
        version, so frontends ask again for what they cached by it."""
        if not kinds:
            return
        with self._lock:
            self.version += 1
        self._notify(kinds)

    def _notify(self, kinds: frozenset[str]) -> None:
        for listener in list(self.listeners.values()):
            try:
                listener(kinds)
            except Exception:
                logger.exception("extensions listener failed")

    # ------------------------------------------------------------- reading

    def of(self, kind: str) -> list[Contribution]:
        """Contributions of ``kind``: by order, built-ins first (native, then the shipped plugins), then
        by plugin and as declared."""
        with self._lock:
            items = list(self._items.get(kind, {}).values())
            seq = dict(self._seq)
        return sorted(
            items,
            key=lambda c: (
                c.order,
                not shipped(c.owner),
                c.owner != BUILTIN,
                c.owner,
                seq.get((c.kind, c.id), 0),
            ),
        )

    def get(self, kind: str, id: str) -> Contribution | None:
        with self._lock:
            return self._items.get(kind, {}).get(id)

    def owned(self, owner: str) -> list[Contribution]:
        with self._lock:
            return [c for by_id in self._items.values() for c in by_id.values() if c.owner == owner]

    def kinds(self) -> list[str]:
        with self._lock:
            return sorted(k for k, v in self._items.items() if v)


EXTENSIONS = Extensions()

__all__ = [
    "BUILTIN",
    "EXTENSIONS",
    "BasePoint",
    "Contribution",
    "ExtensionError",
    "Extensions",
    "Point",
    "shipped",
]
