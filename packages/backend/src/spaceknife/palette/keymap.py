"""The single key table: every command's keys, the built-in ones and what plugins bind.

Precedence (docs/plugins.md, 「键位」): a plugin binding with ``override`` > the built-in default >
any other plugin binding. Between plugins of the same rank the plugin id decides. A binding that
loses keeps nothing of the key; :func:`conflicts` lists every clash for the plugins page and
``plugin check``. The frontends build their key handling from ``palette.commands`` alone.
"""

from __future__ import annotations

from collections.abc import Iterable, Sequence
from dataclasses import dataclass, replace
from typing import Any

MODIFIERS = ("Ctrl", "Alt", "Shift", "Meta")
_ALIASES = {
    "control": "Ctrl",
    "ctrl": "Ctrl",
    "cmd": "Ctrl",
    "command": "Ctrl",
    "alt": "Alt",
    "option": "Alt",
    "shift": "Shift",
    "meta": "Meta",
    "win": "Meta",
    "super": "Meta",
}


def normalize(key: str) -> str:
    """``ctrl+alt+d`` -> ``Ctrl+Alt+D`` (modifiers in a fixed order, a letter in upper case)."""
    parts = [p.strip() for p in key.replace(" ", "").split("+")]
    if key.endswith("++"):  # "Ctrl++"
        parts = [*parts[:-2], "+"]
    mods: set[str] = set()
    base = ""
    for p in parts:
        if not p:
            continue
        m = _ALIASES.get(p.lower())
        if m is not None and m in MODIFIERS:
            mods.add(m)
        else:
            base = p.upper() if len(p) == 1 else p[:1].upper() + p[1:]
    return "+".join([*(m for m in MODIFIERS if m in mods), base])


@dataclass(frozen=True, slots=True)
class Binding:
    key: str  # normalized
    command: str
    owner: str  # "builtin" or a plugin id
    override: bool = False

    @property
    def rank(self) -> int:
        if self.owner != "builtin" and self.override:
            return 0
        return 1 if self.owner == "builtin" else 2


@dataclass(frozen=True, slots=True)
class Conflict:
    key: str
    winner: Binding
    loser: Binding

    def text(self) -> str:
        loser, winner = self.loser, self.winner
        return f"{self.key}: {loser.command} ({loser.owner}) loses to {winner.command} ({winner.owner})"


def bindings(commands: Sequence[Any]) -> list[Binding]:
    """Every binding: each command's own keys, then the plugins' ``keybinding`` contributions."""
    from ..extensions import EXTENSIONS

    out: list[Binding] = []
    ids = {c.id for c in commands}
    for c in commands:
        for k in c.keys:
            out.append(Binding(normalize(k), c.id, c.owner))
    for kb in EXTENSIONS.of("keybinding"):
        spec = kb.spec
        target = spec.command if "." in spec.command else f"{kb.owner}.{spec.command}"
        if target in ids:
            out.append(Binding(normalize(spec.key), target, kb.owner, bool(spec.override)))
    return out


def resolve(binds: Iterable[Binding]) -> tuple[dict[str, list[str]], list[Conflict]]:
    """command id -> its keys (in declaration order), and the conflicts that were settled.

    Built-in commands may share a key between themselves (the frontends tell them apart by focus);
    a plugin's binding never shares one."""
    by_key: dict[str, list[Binding]] = {}
    for b in binds:
        by_key.setdefault(b.key, []).append(b)
    keys: dict[str, list[str]] = {}
    conflicts: list[Conflict] = []
    for key, group in by_key.items():
        ordered = sorted(group, key=lambda b: (b.rank, b.owner, b.command))
        best = ordered[0]
        keep = [
            b for b in ordered if b.rank == best.rank and (best.owner == "builtin" and b.owner == "builtin")
        ]
        if not keep:
            keep = [best]
        for b in ordered:
            if b in keep:
                continue
            if b.command == best.command:
                continue  # the same command twice
            conflicts.append(Conflict(key, best, b))
        for b in keep:
            keys.setdefault(b.command, [])
            if key not in keys[b.command]:
                keys[b.command].append(key)
    return keys, conflicts


def apply(commands: Sequence[Any]) -> list[Any]:
    """The commands with their effective keys; built-in keys keep their spelling and order."""
    keys, _conflicts = resolve(bindings(commands))
    out = []
    for c in commands:
        got = keys.get(c.id, [])
        own = [k for k in c.keys if normalize(k) in got]
        extra = [k for k in got if k not in {normalize(x) for x in own}]
        new = tuple(own + extra)
        out.append(c if new == tuple(c.keys) else replace(c, keys=new))
    return out


def conflicts(commands: Sequence[Any]) -> list[Conflict]:
    return resolve(bindings(commands))[1]


__all__ = ["Binding", "Conflict", "apply", "bindings", "conflicts", "normalize", "resolve"]
