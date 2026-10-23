"""What a cleanup plan decides from: rules, manual choices, presets (docs/dupes.md §16.3–16.5).

A :class:`Policy` is plain data (it is saved, exported and diffed). Layers, earlier first:

1. locked (programs need it, a name Windows trims, several names, a network file, a VCS internal);
2. manual per copy (keep / not keep / action / target);
3. manual per group (the set of copies kept, or the whole group left alone);
4. folder-pair rules (keep side a, side b or both; optionally what the other side gets);
5. keep rules in order (selector + effect: keep_all, prefer, avoid, per_folder, keep_n, order);
6. the base criteria (a chain, ties go to the next one);
7. safety: every group keeps ``max(1, keep_n)`` copies, one per volume when that is on.

Constraints only ever *add* keepers; rules only decide who fills a place. Nothing but a manual
「不保留」 takes a keeper away. Actions of the copies not kept: manual → pair rule → action rules (first
match; its fallback chain when the action cannot be done, then 「不动」) → the preset's default.
Risk-condition rules can only turn an action into 「不动」 (so they settle in one pass).
"""

from __future__ import annotations

import copy as _copy
import os
import re
from dataclasses import asdict, dataclass, field
from typing import Any, Literal

KeepEffect = Literal["keep_all", "prefer", "avoid", "per_folder", "keep_n", "order"]
Criterion = Literal[
    "oldest_created",
    "oldest_modified",
    "newest_modified",
    "shortest_path",
    "longest_path",
    "shallowest",
    "deepest",
    "plain_name",
]
CRITERIA: tuple[str, ...] = (
    "oldest_created",
    "oldest_modified",
    "newest_modified",
    "shortest_path",
    "longest_path",
    "shallowest",
    "deepest",
    "plain_name",
)
DEFAULT_CRITERIA = ["oldest_created", "plain_name", "shortest_path"]
RULE_TO_CRITERION = {"oldest": "oldest_created", "newest": "newest_modified", "shortest": "shortest_path"}
PRESETS = ("watch", "conservative", "recommended", "links", "backup", "custom")


@dataclass(slots=True)
class Selector:
    under: list[str] = field(default_factory=list)
    not_under: list[str] = field(default_factory=list)
    volumes: list[str] = field(default_factory=list)
    fs: list[str] = field(default_factory=list)
    ext: list[str] = field(default_factory=list)
    classes: list[str] = field(default_factory=list)
    name_glob: str = ""
    path_glob: str = ""
    size_min: int | None = None
    size_max: int | None = None
    copies_min: int | None = None
    cross: bool | None = None
    same_folder: bool | None = None
    mtime_before: int | None = None  # unix seconds
    mtime_after: int | None = None
    tags: list[str] = field(default_factory=list)  # any of
    not_tags: list[str] = field(default_factory=list)  # none of
    can: list[str] = field(default_factory=list)  # every one of these actions can be done here
    same_volume: bool | None = None  # a keeper of its group is on the same volume
    risk_at_least: str | None = None  # risk-condition rules only
    risk_any: list[str] = field(default_factory=list)

    @property
    def risky(self) -> bool:
        return bool(self.risk_at_least or self.risk_any)

    def empty(self) -> bool:
        return self == Selector()


@dataclass(slots=True)
class Rule:
    id: int
    kind: Literal["keep", "action"]
    effect: str  # keep: a KeepEffect; action: an action name
    when: Selector = field(default_factory=Selector)
    enabled: bool = True
    label: str = ""  # the user's name for it ("" : made from the rule)
    label_key: str = ""  # a preset's rule: TEXT key of its name
    folders: list[str] = field(default_factory=list)  # prefer / avoid, in order
    n: int = 1  # keep_n
    criteria: list[str] = field(default_factory=list)  # order
    fallback: list[str] = field(default_factory=list)  # action rules: tried in order, then 「不动」
    target_under: str = ""  # links: point at the keeper under this folder
    symlink_style: str = ""  # "" (the plan's option) | relative | absolute


@dataclass(slots=True)
class PairRule:
    a: str
    b: str
    keep: Literal["a", "b", "both"] = "a"
    other_action: str = ""  # what the copies on the other side get ("" : the other rules decide)
    auto: bool = False  # made by 「文件夹一致」


@dataclass(slots=True)
class ManualCopy:
    keep: Literal["keep", "not", ""] = ""
    action: str = ""
    target: str = ""  # path of the keeper a link or clone points at


@dataclass(slots=True)
class ManualGroup:
    keep_set: list[str] | None = None  # 「只保留这几份」: every other copy is not kept
    excluded: bool = False  # 「这一组不动」


@dataclass(slots=True)
class Options:
    on_failure: Literal["continue", "stop"] = "continue"
    verify: Literal["auto", "content", "metadata"] = "auto"
    symlink_style: Literal["auto", "relative", "absolute"] = "auto"


@dataclass(slots=True)
class Policy:
    preset: str = "recommended"
    criteria: list[str] = field(default_factory=lambda: list(DEFAULT_CRITERIA))
    per_volume: bool = True
    folder_consistent: bool = True
    keep_rules: list[Rule] = field(default_factory=list)
    action_rules: list[Rule] = field(default_factory=list)
    pairs: list[PairRule] = field(default_factory=list)
    default_action: str = "trash"
    options: Options = field(default_factory=Options)
    manual: dict[tuple[str, str], ManualCopy] = field(default_factory=dict)  # (digest hex, path)
    groups: dict[str, ManualGroup] = field(default_factory=dict)  # digest hex
    reviewed: dict[str, int] = field(default_factory=dict)  # digest hex -> plan version when reviewed
    next_id: int = 1

    def clone(self) -> Policy:
        return _copy.deepcopy(self)

    def new_id(self) -> int:
        i = self.next_id
        self.next_id += 1
        return i

    def rule(self, rule_id: int) -> Rule | None:
        for r in (*self.keep_rules, *self.action_rules):
            if r.id == rule_id:
                return r
        return None

    def manual_count(self) -> int:
        n = sum(1 for m in self.manual.values() if m.keep or m.action or m.target)
        return n + sum(1 for g in self.groups.values() if g.keep_set is not None or g.excluded)

    # ---------------------------------------------------------------- JSON
    def to_json(self, *, with_manual: bool = True) -> dict[str, Any]:
        out: dict[str, Any] = {
            "preset": self.preset,
            "criteria": list(self.criteria),
            "per_volume": self.per_volume,
            "folder_consistent": self.folder_consistent,
            "keep_rules": [asdict(r) for r in self.keep_rules],
            "action_rules": [asdict(r) for r in self.action_rules],
            "pairs": [asdict(p) for p in self.pairs],
            "default_action": self.default_action,
            "options": asdict(self.options),
            "next_id": self.next_id,
        }
        if with_manual:
            out["manual"] = [
                {"digest": d, "path": p, **asdict(m)}
                for (d, p), m in sorted(self.manual.items())
                if m.keep or m.action or m.target
            ]
            out["groups"] = [
                {"digest": d, **asdict(g)}
                for d, g in sorted(self.groups.items())
                if g.keep_set is not None or g.excluded
            ]
            out["reviewed"] = sorted(self.reviewed)
        return out

    @staticmethod
    def from_json(data: dict[str, Any]) -> Policy:
        def sel(d: dict[str, Any] | None) -> Selector:
            known = set(Selector.__slots__)
            return Selector(**{k: v for k, v in (d or {}).items() if k in known})

        def rule(d: dict[str, Any]) -> Rule:
            known = set(Rule.__slots__)
            kw = {k: v for k, v in d.items() if k in known and k != "when"}
            return Rule(when=sel(d.get("when")), **kw)

        p = Policy(
            preset=str(data.get("preset", "custom")),
            criteria=[c for c in data.get("criteria", DEFAULT_CRITERIA) if c in CRITERIA]
            or ["oldest_created"],
            per_volume=bool(data.get("per_volume", True)),
            folder_consistent=bool(data.get("folder_consistent", True)),
            keep_rules=[rule(r) for r in data.get("keep_rules", [])],
            action_rules=[rule(r) for r in data.get("action_rules", [])],
            pairs=[
                PairRule(**{k: v for k, v in pr.items() if k in PairRule.__slots__})
                for pr in data.get("pairs", [])
            ],
            default_action=str(data.get("default_action", "leave")),
            options=Options(**{k: v for k, v in data.get("options", {}).items() if k in Options.__slots__}),
            next_id=int(data.get("next_id", 1)),
        )
        for m in data.get("manual", []):
            p.manual[(str(m["digest"]), str(m["path"]))] = ManualCopy(
                keep=m.get("keep", ""), action=m.get("action", ""), target=m.get("target", "")
            )
        for g in data.get("groups", []):
            p.groups[str(g["digest"])] = ManualGroup(
                keep_set=g.get("keep_set"), excluded=bool(g.get("excluded"))
            )
        for d in data.get("reviewed", []):
            p.reviewed[str(d)] = 0
        ids = [r.id for r in (*p.keep_rules, *p.action_rules)]
        p.next_id = max([p.next_id, *[i + 1 for i in ids]])
        return p


def preset(name: str, base: Policy | None = None, *, backup_volume: str = "") -> Policy:
    """A policy with a preset's rules; the manual choices and options of ``base`` stay."""
    p = Policy() if base is None else base.clone()
    p.preset = name
    p.keep_rules = [r for r in p.keep_rules if not r.label_key]  # the user's own keep rules stay
    p.action_rules = []
    p.pairs = [x for x in p.pairs if not x.auto]

    def act(effect: str, when: Selector, key: str, fallback: list[str] | None = None) -> None:
        p.action_rules.append(
            Rule(p.new_id(), "action", effect, when, label_key=key, fallback=fallback or [])
        )

    if name == "watch":
        p.default_action = "leave"
        act("leave", Selector(), "rule_all_leave")
    elif name == "conservative":
        p.default_action = "trash"
        act("trash", Selector(), "rule_all_trash")
    elif name == "links":
        p.default_action = "leave"
        docs = ["document", "in_place"]
        act("trash", Selector(tags=docs), "rule_docs_trash")
        act("hardlink", Selector(same_volume=True, not_tags=docs), "rule_same_volume_link")
        act("leave", Selector(risk_at_least="high"), "rule_high_leave")
    elif name == "backup":
        p.default_action = "leave"
        p.per_volume = False
        if backup_volume:
            p.keep_rules.insert(
                0,
                Rule(
                    p.new_id(),
                    "keep",
                    "keep_all",
                    Selector(volumes=[backup_volume]),
                    label_key="rule_keep_backup",
                ),
            )
            act(
                "symlink",
                Selector(),
                "rule_other_link",
                fallback=["leave"],
            )
            p.action_rules[-1].target_under = backup_volume
        act("leave", Selector(risk_at_least="high"), "rule_high_leave")
    elif name == "custom":
        pass
    else:  # recommended
        p.preset = "recommended"
        p.default_action = "trash"
        act(
            "leave",
            Selector(
                tags=[
                    "sync",
                    "vcs",
                    "app",
                    "program",
                    "game",
                    "media_library",
                    "hidden_temp_root",
                    "link_target",
                ]
            ),
            "rule_sync_vcs_app",
        )
        act("clone", Selector(can=["clone"]), "rule_clone")
        act("trash", Selector(), "rule_rest_trash")
        act("leave", Selector(risk_at_least="high"), "rule_high_leave")
    return p


# ------------------------------------------------------------------ names that look like copies
_COPY_PATTERNS = [
    re.compile(r"^(?P<stem>.+?) \(\d{1,3}\)$"),  # x (1)
    re.compile(r"^(?P<stem>.+?)\(\d{1,3}\)$"),  # x(2)
    re.compile(
        r"^(?P<stem>.+?) - (?:Copy|copy|副本|コピー|Kopie|Copie|Copia)(?: \(\d{1,3}\))?$"  # i18n: ignore
    ),
    re.compile(r"^(?:Copy of |copy of )(?P<stem>.+)$"),
    re.compile(r"^(?P<stem>.+?)[_ ](?:copy|Copy|副本)(?:\d{0,3})$"),  # i18n: ignore
    re.compile(r"^(?P<stem>.+?)的副本$"),  # i18n: ignore
]


def copy_stem(name: str) -> str | None:
    """The name without a 「副本」 style (``x (1).jpg`` -> ``x.jpg``), or None when it has none."""
    base, ext = os.path.splitext(name)
    for pat in _COPY_PATTERNS:
        m = pat.match(base)
        if m:
            return m.group("stem") + ext
    return None


def plain_name_keys(names: list[str]) -> list[int]:
    """0 for a name that does not look like a copy, 1 for one that does: only when the name with
    its 「副本」 style taken away is another member's name (``x (1).jpg`` beside ``x.jpg``)."""
    lower = [n.lower() for n in names]
    have = set(lower)
    out: list[int] = []
    for n in names:
        stem = copy_stem(n)
        out.append(1 if stem is not None and stem.lower() in have and stem.lower() != n.lower() else 0)
    return out
