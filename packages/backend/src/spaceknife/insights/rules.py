"""Insights rules engine: what a known folder or file is, and whether it is safe to clean.

A rule (docs/insights.md) names path globs or bare names, plus optional conditions — the parent
folder's name, platforms, node kinds, a minimum size, exclusions — and says what the item is and
how to reclaim its space. Rules are TOML: the built-in ``builtin.toml`` (the shipped plugin
``builtin.insights``), the enabled plugins' rule files, then the user's own files; a later rule
overrides an earlier one by id.

Lookups cost a few dict probes per path segment: every pattern is indexed by its most selective
literal part (its last segment, its parent segment, its extension, or the first three characters
of its last segment), so only a handful of precompiled regexes run per lookup. A folder rule also
covers everything inside the folder (``subtree``). Among the matches of a path and its ancestors
the winner has the highest priority, then the deepest match, then the most specific pattern; when
the winning rule also matches further up, the outermost match is reported (the container is the
unit: everything in ``a/node_modules/x/node_modules`` belongs to ``a/node_modules``).

A path in Windows form (drive letter, UNC, ``\\\\?\\``) is matched against the Windows rules,
case-insensitively; any other path against the host's POSIX platform — Linux case-sensitively,
macOS case-insensitively (APFS default) — so a snapshot from another platform still gets its own
platform's rules. ``RuleSet`` is immutable once built and safe to share between threads.

Texts come in the interface language unless a lookup asks for another one (M7.5): a rule's
``i18n`` table translates its own texts; problems with rule files are reported in the interface
language of the moment they are found.
"""

from __future__ import annotations

import re
import sys
import threading
import tomllib
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from itertools import accumulate
from pathlib import Path
from typing import Any, Literal, cast, get_args

from loguru import logger

from ..i18n import Texts, _
from ..i18n import language as interface_language
from ..paths import config_dir
from ..store.nodes import Kind, NodeStore

InsightKind = Literal["app", "cache", "build", "vm", "chat", "game", "system"]
Cleanup = Literal["safe", "tool", "review", "keep"]
NodeKind = str | Kind  # "file" / "dir" / "link" / "other", or a store Kind

KINDS: tuple[str, ...] = get_args(InsightKind)
CLEANUPS: tuple[str, ...] = get_args(Cleanup)
PLATFORMS = ("windows", "linux", "macos")
FORMAT_VERSION = 1
BUILTIN_SOURCE = "builtin.toml"

# --- user-facing text (log panel and the rule problems list), translated when read (M7.5) -----
MSG = Texts(
    {
        "file_unreadable": "无法读取洞察规则文件 {source}：{error}",
        "file_syntax": "洞察规则文件 {source} 不是有效的 TOML：{error}",
        "file_version": "洞察规则文件 {source} 的格式版本 {version} 不受支持（支持 {supported}），已跳过",
        "file_keys": "洞察规则文件 {source} 有未知的顶层字段 {keys}，已忽略",
        "file_rules": "洞察规则文件 {source} 的 rule 应为表数组（[[rule]]）",
        "rule_skipped": "跳过洞察规则 {rule}（{source}）：{error}",
        "rule_duplicate": "洞察规则 {rule} 在 {source} 中出现多次，以最后一条为准",
        "rule_override": "用户规则 {rule}（{source}）覆盖了同名规则",
        "rules_loaded": "已加载 {count} 条洞察规则（来自 {files} 个文件，跳过 {skipped} 处问题）",
    }
)
ERR = Texts(
    {
        "not_table": "规则应为表",
        "missing": "缺少字段 {key}",
        "type": "字段 {key} 应为{expected}",
        "unknown": "未知字段 {keys}",
        "bad_id": "id {value!r} 无效：只能用字母、数字和 . _ -，并以字母或数字开头",
        "bad_choice": "字段 {key} 的值 {value!r} 无效，可选 {choices}",
        "no_matcher": "至少要有一条 paths 或 names",
        "name_sep": "名称 {value!r} 不能为空或包含路径分隔符（多层路径请写进 paths）",
        "empty_choice": "字段 {key} 不能为空列表",
        "bad_glob": "通配符 {value!r} 无效：{error}",
        "empty_glob": "通配符不能为空",
        "unclosed": "[ 没有配对的 ]",
        "unclosed_brace": "{ 没有配对的 }",
        "too_many": "花括号展开后的模式超过 {limit} 个",
        "tilde": "不支持 ~，请改用相对模式（如 .cache/pip，可匹配任意层级）",
        "bad_size": '无法识别的大小 {value!r}（例：1048576、"500 MB"、"2 GiB"）',
        "bad_priority": "priority 应在 -1000 到 1000 之间",
        "bad_lang": "i18n 的语言代码 {value!r} 无效（例：en、zh-tw）",
        "bad_i18n": "i18n.{lang} 只能包含 title、reason、note、app、regenerate",
    }
)
TYPE_NAMES = Texts(
    {
        "str": "字符串",
        "strs": "字符串或字符串列表",
        "int": "整数",
        "bool": "true 或 false",
        "table": "表",
    }
)

_TEXT_FIELDS = ("title", "reason", "note", "app", "regenerate")
_MATCH_FIELDS = (
    "paths",
    "names",
    "parents",
    "exclude",
    "platforms",
    "node",
    "min_size",
    "contains",
    "beside",
)
_OTHER_FIELDS = ("id", "kind", "cleanup", "command", "priority", "subtree", "enabled", "i18n")
_RULE_KEYS = frozenset(_TEXT_FIELDS + _MATCH_FIELDS + _OTHER_FIELDS)
_PLATFORM_ALIASES = {
    "windows": ("windows",),
    "linux": ("linux",),
    "macos": ("macos",),
    "posix": ("linux", "macos"),
    "all": PLATFORMS,
}
_NODE_NAMES = ("file", "dir", "link", "other")
_ID = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*\Z")
_LANG = re.compile(r"[a-z]{2,3}(?:-[a-z0-9]{2,8})*\Z")
_SIZE = re.compile(r"\s*(\d+(?:\.\d+)?)\s*([kmgtp]?)(?:i?b)?\s*\Z", re.IGNORECASE)
_SIZE_POWER = {"": 0, "k": 1, "m": 2, "g": 3, "t": 4, "p": 5}
_DRIVE = re.compile(r"[A-Za-z]:(?:[\\/]|\Z)")
_CLASS_ESCAPE = re.compile(r"([\\\[\]&~|^])")  # "-" is kept: a-z ranges
_WILD = frozenset("*?[")
_MAX_EXPANSION = 256

_WIN, _LINUX, _MAC = 1, 2, 4
_PMASK = {"windows": _WIN, "linux": _LINUX, "macos": _MAC}
_FOLD = _WIN | _MAC  # case-insensitive file systems by default
_KIND_BITS = {name: 1 << int(k) for name, k in zip(_NODE_NAMES, Kind, strict=True)}
_DIR_BIT = 1 << int(Kind.DIR)


@dataclass(frozen=True, slots=True)
class Insight:
    """What a matched item is (contract: docs/manage.md section 5).

    ``rule`` / ``title`` / ``reason`` / ``kind`` are the contract fields; the rest are optional
    extras for the insight card (UI-MAIN-41/42) and the change annotations (UI-W6-30).
    """

    rule: str
    title: str
    reason: str
    kind: InsightKind
    note: str = ""  # short annotation after a name (W6 change reason), e.g. 「游戏更新」
    app: str = ""  # the owning application, for the subtitle line
    cleanup: Cleanup = "review"  # safe / tool (use the app or command) / review / keep
    command: str = ""  # the cleaning command when there is one: shown, never run
    path: str = ""  # the matched item: the path itself, or the folder it is inside
    inside: bool = False  # matched through an ancestor folder (``path`` is that folder)
    priority: int = 0  # the rule's (-1000…1000): the details panel orders its cards by it
    regenerate: str = ""  # how it comes back once deleted ("npm install puts it back"): the card says it


@dataclass(frozen=True, slots=True)
class RuleText:
    title: str
    reason: str
    note: str
    app: str
    regenerate: str = ""


@dataclass(frozen=True, slots=True, eq=False)
class Rule:
    """A validated rule, before compilation (patterns are normalised globs)."""

    id: str
    kind: InsightKind
    text: RuleText
    i18n: Mapping[str, Mapping[str, str]]  # language tag -> field -> text
    cleanup: Cleanup
    command: str
    patterns: tuple[str, ...]
    parents: tuple[str, ...]
    exclude: tuple[str, ...]
    platforms: frozenset[str]
    nodes: frozenset[str]
    min_size: int
    priority: int
    subtree: bool
    source: str
    contains: tuple[str, ...] = ()  # lower-case name globs: the matched folder holds one of them
    beside: tuple[str, ...] = ()  # … or the matched item has a sibling called one of them

    def texts(self, lang: str | None = None) -> RuleText:
        """Texts in ``lang`` (``en``, ``en-US``, ``zh-TW`` …), falling back field by field to the
        primary language subtag and then to the rule's own texts."""
        if not lang or not self.i18n:
            return self.text
        tag = lang.replace("_", "-").lower()
        tr = self.i18n.get(tag) or self.i18n.get(tag.split("-", 1)[0])
        if not tr:
            return self.text
        base = self.text
        title = tr.get("title", base.title)
        return RuleText(
            title=title,
            reason=tr.get("reason", base.reason),
            note=tr.get("note") or (title if "title" in tr else base.note),
            app=tr.get("app", base.app),
            regenerate=tr.get("regenerate", base.regenerate),
        )


@dataclass(frozen=True, slots=True)
class RuleProblem:
    """A file or rule that was skipped (or partly ignored), with a readable reason."""

    source: str
    rule: str  # the rule id, ``#n`` (1-based entry number) when it has none, "" for the file
    message: str


class _Invalid(Exception):
    pass


# --- globs ------------------------------------------------------------------------------------


def _expand(pattern: str) -> list[str]:
    """Brace alternatives, nested allowed: ``a/{b,c/d}`` -> ``a/b``, ``a/c/d`` (``[{]`` is a literal)."""
    out: list[str] = []
    pending = [pattern]
    while pending:
        p = pending.pop()
        start = depth = 0
        open_at = -1
        i, n = 0, len(p)
        while i < n:
            ch = p[i]
            if ch == "[":  # a character class: braces inside are literal
                j = i + 1
                if j < n and p[j] in "!^":
                    j += 1
                if j < n and p[j] == "]":
                    j += 1
                close = p.find("]", j)
                i = close + 1 if close > 0 else n
                continue
            if ch == "{":
                if depth == 0:
                    open_at = i
                depth += 1
            elif ch == "}" and depth:
                depth -= 1
                if depth == 0:
                    break
            i += 1
        if open_at < 0:
            out.append(p)
        elif depth:
            raise _Invalid(ERR["unclosed_brace"])
        else:
            body, alts, level = p[open_at + 1 : i], [], 0
            for j, ch in enumerate(body):
                if ch == "{":
                    level += 1
                elif ch == "}":
                    level -= 1
                elif ch == "," and level == 0:
                    alts.append(body[start:j])
                    start = j + 1
            alts.append(body[start:])
            pending.extend(p[:open_at] + alt + p[i + 1 :] for alt in reversed(alts))
        if len(out) + len(pending) > _MAX_EXPANSION:
            raise _Invalid(ERR["too_many"].format(limit=_MAX_EXPANSION))
    return out


def _expand_all(patterns: Iterable[str]) -> tuple[str, ...]:
    out: list[str] = []
    for p in patterns:
        out.extend(_expand(p))
    return tuple(dict.fromkeys(out))


def _split_glob(pattern: str) -> tuple[list[str], bool]:
    """Segments of a glob, with ``**`` prepended unless it is anchored; and whether it is."""
    p = pattern.strip().replace("\\", "/")
    if not p:
        raise _Invalid(ERR["empty_glob"])
    if p.startswith("~"):
        raise _Invalid(ERR["tilde"])
    absolute = p.startswith("/")
    segs = [s for s in p.split("/") if s]
    if absolute:
        segs.insert(0, "")
    if not segs or segs == [""]:
        raise _Invalid(ERR["empty_glob"])
    anchored = absolute or segs[0].endswith(":")  # POSIX root, or a drive (``C:`` / ``?:``)
    collapsed: list[str] = []
    for seg in segs:
        if seg == "**" and collapsed and collapsed[-1] == "**":
            continue
        collapsed.append(seg)
    if not anchored and collapsed[0] != "**":
        collapsed.insert(0, "**")
    return collapsed, anchored


def _segment(seg: str) -> tuple[str, int, int]:
    """(regex, literal characters, single-character wildcards) of one glob segment."""
    out: list[str] = []
    lit = single = 0
    i, n = 0, len(seg)
    while i < n:
        ch = seg[i]
        if ch == "*":
            while i < n and seg[i] == "*":
                i += 1
            out.append("[^/]*")
            continue
        if ch == "?":
            out.append("[^/]")
            single += 1
            i += 1
            continue
        if ch == "[":
            j = i + 1
            negate = j < n and seg[j] in "!^"
            if negate:
                j += 1
            start = j
            if j < n and seg[j] == "]":
                j += 1  # a leading ] is literal
            while j < n and seg[j] != "]":
                j += 1
            if j >= n:
                raise _Invalid(ERR["unclosed"])
            body = _CLASS_ESCAPE.sub(r"\\\1", seg[start:j])
            out.append(("[^/" if negate else "[") + body + "]")
            single += 1
            i = j + 1
            continue
        out.append(re.escape(ch))
        lit += 1
        i += 1
    return "".join(out), lit, single


def _glob_regex(segs: Sequence[str]) -> tuple[str, int]:
    """Regex for a split glob (fullmatched against a ``/``-joined path) and its specificity."""
    parts: list[str] = []
    score = 0
    last = len(segs) - 1
    for k, seg in enumerate(segs):
        if seg == "**":
            parts.append(".*" if k == last else "(?:.*/)?")
            continue
        rx, lit, single = _segment(seg)
        parts.append(rx if k == last else rx + "/")
        score += 2 * lit + single + (4 if lit and not single and lit == len(seg) else 0)
    return "".join(parts), score


def _literal(seg: str) -> bool:
    return not _WILD.intersection(seg)


def _anchor(segs: Sequence[str]) -> tuple[str, str, str | None] | None:
    """The index key of a pattern: its most selective literal part, or None (always tested).

    A literal last segment is keyed by itself and its literal parent segment, if any (``Code/Cache``
    and ``discord/Cache`` do not share a bucket); otherwise by the literal parent segment, the
    literal extension, or the first three characters of a literal prefix."""
    last = segs[-1]
    if last == "**":
        return None
    parent = segs[-2] if len(segs) >= 2 and segs[-2] != "**" and _literal(segs[-2]) else None
    if _literal(last):
        return "name", last.lower(), None if parent is None else parent.lower()
    if parent is not None:
        return "parent", parent.lower(), None
    dot = last.rfind(".")
    if dot >= 0 and _literal(last[dot:]) and "]" not in last[dot:]:
        return "ext", last[dot:].lower(), None
    cut = min((last.index(c) for c in _WILD if c in last), default=len(last))
    if cut >= 3:
        return "prefix", last[:cut].lower()[:3], None
    return None


def _compile(patterns: Iterable[str]) -> tuple[re.Pattern[str], re.Pattern[str]] | None:
    """One alternation of whole-path globs, case-sensitive and case-insensitive (None if empty)."""
    rx = [_glob_regex(_split_glob(p)[0])[0] for p in patterns]
    if not rx:
        return None
    joined = "(?:" + "|".join(rx) + ")"
    return re.compile(joined, re.DOTALL), re.compile(joined, re.DOTALL | re.IGNORECASE)


def _compile_names(names: Iterable[str]) -> tuple[re.Pattern[str], re.Pattern[str]] | None:
    rx = [_segment(n)[0] for n in names]
    if not rx:
        return None
    joined = "(?:" + "|".join(rx) + ")"
    return re.compile(joined, re.DOTALL), re.compile(joined, re.DOTALL | re.IGNORECASE)


# --- parsing ----------------------------------------------------------------------------------


def _type_error(key: str, expected: str) -> _Invalid:
    return _Invalid(ERR["type"].format(key=key, expected=TYPE_NAMES[expected]))


def _str(raw: Mapping[str, Any], key: str, default: str | None = None) -> str:
    value = raw.get(key, default)
    if value is None:
        raise _Invalid(ERR["missing"].format(key=key))
    if not isinstance(value, str):
        raise _type_error(key, "str")
    return value.strip()


def _strs(raw: Mapping[str, Any], key: str) -> tuple[str, ...]:
    value = raw.get(key, [])
    if isinstance(value, str):
        value = [value]
    if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
        raise _type_error(key, "strs")
    return tuple(v.strip() for v in value)


def _choices(raw: Mapping[str, Any], key: str, allowed: Mapping[str, Sequence[str]]) -> frozenset[str]:
    out: set[str] = set()
    values = _strs(raw, key)
    if not values:
        raise _Invalid(ERR["empty_choice"].format(key=key))
    for value in values:
        names = allowed.get(value.lower())
        if names is None:
            raise _Invalid(ERR["bad_choice"].format(key=key, value=value, choices=" / ".join(allowed)))
        out.update(names)
    return frozenset(out)


def parse_size(value: object) -> int:
    """Bytes from ``1048576`` or ``"500 MB"`` / ``"2 GiB"`` / ``"10m"`` (1 KB = 1024 B)."""
    if isinstance(value, bool):
        raise _Invalid(ERR["bad_size"].format(value=value))
    if isinstance(value, int):
        if value < 0:
            raise _Invalid(ERR["bad_size"].format(value=value))
        return value
    m = _SIZE.match(value) if isinstance(value, str) else None
    if m is None:
        raise _Invalid(ERR["bad_size"].format(value=value))
    return int(float(m.group(1)) * 1024 ** _SIZE_POWER[m.group(2).lower()])


def _i18n(raw: Mapping[str, Any]) -> dict[str, dict[str, str]]:
    table = raw.get("i18n", {})
    if not isinstance(table, dict):
        raise _type_error("i18n", "table")
    out: dict[str, dict[str, str]] = {}
    for lang, fields in table.items():
        tag = lang.replace("_", "-").lower()
        if not _LANG.match(tag):
            raise _Invalid(ERR["bad_lang"].format(value=lang))
        if not isinstance(fields, dict) or not set(fields) <= set(_TEXT_FIELDS):
            raise _Invalid(ERR["bad_i18n"].format(lang=lang))
        texts: dict[str, str] = {}
        for name, value in fields.items():
            if not isinstance(value, str):
                raise _type_error(f"i18n.{lang}.{name}", "str")
            texts[name] = value.strip()
        out[tag] = texts
    return out


def _parse_rule(raw: object, source: str) -> tuple[str, Rule | None]:
    """(id, rule), or (id, None) for ``enabled = false``; raises _Invalid with a reason."""
    if not isinstance(raw, dict):
        raise _Invalid(ERR["not_table"])
    rule_id = _str(raw, "id")
    if not _ID.match(rule_id):
        raise _Invalid(ERR["bad_id"].format(value=rule_id))
    enabled = raw.get("enabled", True)
    if not isinstance(enabled, bool):
        raise _type_error("enabled", "bool")
    unknown = sorted(set(raw) - _RULE_KEYS)
    if unknown:
        raise _Invalid(ERR["unknown"].format(keys=_("、").join(unknown)))
    if not enabled:
        return rule_id, None

    kind = _str(raw, "kind").lower()
    if kind not in KINDS:
        raise _Invalid(ERR["bad_choice"].format(key="kind", value=kind, choices=" / ".join(KINDS)))
    cleanup = _str(raw, "cleanup", "review").lower()
    if cleanup not in CLEANUPS:
        raise _Invalid(ERR["bad_choice"].format(key="cleanup", value=cleanup, choices=" / ".join(CLEANUPS)))
    title = _str(raw, "title")
    if not title:
        raise _Invalid(ERR["missing"].format(key="title"))
    reason = _str(raw, "reason")
    if not reason:
        raise _Invalid(ERR["missing"].format(key="reason"))
    text = RuleText(
        title=title,
        reason=reason,
        note=_str(raw, "note", "") or title,
        app=_str(raw, "app", ""),
        regenerate=_str(raw, "regenerate", ""),
    )

    names = _expand_all(_strs(raw, "names"))
    parents = _expand_all(_strs(raw, "parents"))
    contains = _expand_all(_strs(raw, "contains"))
    beside = _expand_all(_strs(raw, "beside"))
    for value in (*names, *parents, *contains, *beside):
        if not value or "/" in value or "\\" in value:
            raise _Invalid(ERR["name_sep"].format(value=value))
        try:
            _segment(value)
        except _Invalid as exc:
            raise _Invalid(ERR["bad_glob"].format(value=value, error=exc)) from None
    patterns = _expand_all((*_strs(raw, "paths"), *("**/" + n for n in names)))
    if not patterns:
        raise _Invalid(ERR["no_matcher"])
    exclude = _expand_all(_strs(raw, "exclude"))
    for value in (*patterns, *exclude):
        try:
            _glob_regex(_split_glob(value)[0])
        except _Invalid as exc:
            raise _Invalid(ERR["bad_glob"].format(value=value, error=exc)) from None

    platforms = _choices(raw, "platforms", _PLATFORM_ALIASES) if "platforms" in raw else frozenset(PLATFORMS)
    node_choices = {n: (n,) for n in _NODE_NAMES} | {"any": _NODE_NAMES}
    nodes = _choices(raw, "node", node_choices) if "node" in raw else frozenset(_NODE_NAMES)
    min_size = parse_size(raw.get("min_size", 0))
    priority = raw.get("priority", 0)
    if not isinstance(priority, int) or isinstance(priority, bool):
        raise _type_error("priority", "int")
    if not -1000 <= priority <= 1000:
        raise _Invalid(ERR["bad_priority"])
    subtree = raw.get("subtree", True)
    if not isinstance(subtree, bool):
        raise _type_error("subtree", "bool")
    return rule_id, Rule(
        id=rule_id,
        kind=cast(InsightKind, kind),
        text=text,
        i18n=_i18n(raw),
        cleanup=cast(Cleanup, cleanup),
        command=_str(raw, "command", ""),
        patterns=patterns,
        parents=parents,
        exclude=exclude,
        platforms=platforms,
        nodes=nodes,
        min_size=min_size,
        priority=priority,
        subtree=subtree,
        source=source,
        contains=tuple(x.lower() for x in contains),
        beside=tuple(x.lower() for x in beside),
    )


def _parse_file(text: str, source: str) -> tuple[list[tuple[str, Rule | None]], list[RuleProblem]]:
    problems: list[RuleProblem] = []
    try:
        data = tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        return [], [RuleProblem(source, "", MSG["file_syntax"].format(source=source, error=exc))]
    version = data.get("version", FORMAT_VERSION)
    if version != FORMAT_VERSION:
        msg = MSG["file_version"].format(source=source, version=version, supported=FORMAT_VERSION)
        return [], [RuleProblem(source, "", msg)]
    extra = sorted(set(data) - {"version", "rule"})
    if extra:
        problems.append(
            RuleProblem(source, "", MSG["file_keys"].format(source=source, keys=_("、").join(extra)))
        )
    entries = data.get("rule", [])
    if not isinstance(entries, list):
        problems.append(RuleProblem(source, "", MSG["file_rules"].format(source=source)))
        return [], problems
    parsed: list[tuple[str, Rule | None]] = []
    seen: set[str] = set()
    for n, raw in enumerate(entries, 1):
        label = raw.get("id") if isinstance(raw, dict) and isinstance(raw.get("id"), str) else f"#{n}"
        try:
            rule_id, rule = _parse_rule(raw, source)
        except _Invalid as exc:
            msg = MSG["rule_skipped"].format(rule=label, source=source, error=exc)
            problems.append(RuleProblem(source, str(label), msg))
            continue
        if rule_id in seen:
            msg = MSG["rule_duplicate"].format(rule=rule_id, source=source)
            problems.append(RuleProblem(source, rule_id, msg))
        seen.add(rule_id)
        parsed.append((rule_id, rule))
    return parsed, problems


# --- the compiled rule set --------------------------------------------------------------------


class _Compiled:
    """Per-rule filters, precomputed for the lookup loop."""

    __slots__ = ("exclude", "min_size", "nmask", "parents", "pmask", "prio", "rule")

    def __init__(self, rule: Rule) -> None:
        self.rule = rule
        self.pmask = sum(_PMASK[p] for p in rule.platforms)
        self.nmask = sum(_KIND_BITS[n] for n in rule.nodes)
        self.min_size = rule.min_size
        self.prio = -rule.priority
        self.parents = _compile_names(rule.parents)
        self.exclude = _compile(rule.exclude)


class _Matcher:
    __slots__ = ("anchor", "ci", "cs", "order", "rank", "rule", "specificity")

    def __init__(self, rule: _Compiled, pattern: str, order: int) -> None:
        segs, anchored = _split_glob(pattern)
        rx, score = _glob_regex(segs)
        self.rule = rule
        self.cs = re.compile(rx, re.DOTALL)
        self.ci = re.compile(rx, re.DOTALL | re.IGNORECASE)
        self.anchor = _anchor(segs)
        self.specificity = score + (8 if anchored else 0)
        self.order = order
        self.rank = 0


_Cand = tuple[_Matcher, int]  # (matcher, index of the path segment it is tested at)


class _Index:
    """Matchers keyed by their anchor; ``collect`` gathers the ones that may match a segment."""

    __slots__ = ("ext", "generic", "name", "parent", "prefix")

    def __init__(self) -> None:
        self.name: dict[str, dict[str | None, list[_Matcher]]] = {}  # name -> parent name or None
        self.parent: dict[str, list[_Matcher]] = {}
        self.ext: dict[str, list[_Matcher]] = {}
        self.prefix: dict[str, list[_Matcher]] = {}
        self.generic: list[_Matcher] = []

    def add(self, m: _Matcher) -> None:
        if m.anchor is None:
            self.generic.append(m)
            return
        where, key, parent = m.anchor
        if where == "name":
            self.name.setdefault(key, {}).setdefault(parent, []).append(m)
        else:
            {"parent": self.parent, "ext": self.ext, "prefix": self.prefix}[where].setdefault(key, []).append(
                m
            )

    def collect(self, low: Sequence[str], start: int, stop: int, pmask: int, out: list[_Cand]) -> None:
        """Candidates at segments ``start .. stop-1`` that apply to the path's platform."""
        name, parent, ext, prefix, generic = self.name, self.parent, self.ext, self.prefix, self.generic
        prev = low[start - 1] if start else None
        for i in range(start, stop):
            seg = low[i]
            buckets = name.get(seg)
            if buckets is not None:
                found = buckets.get(None)
                if found:
                    out.extend((m, i) for m in found if m.rule.pmask & pmask)
                if prev is not None:
                    found = buckets.get(prev)
                    if found:
                        out.extend((m, i) for m in found if m.rule.pmask & pmask)
            if prev is not None:
                found = parent.get(prev)
                if found:
                    out.extend((m, i) for m in found if m.rule.pmask & pmask)
            dot = seg.rfind(".")
            if dot >= 0:
                found = ext.get(seg[dot:])
                if found:
                    out.extend((m, i) for m in found if m.rule.pmask & pmask)
            found = prefix.get(seg[:3])
            if found:
                out.extend((m, i) for m in found if m.rule.pmask & pmask)
            if generic:
                out.extend((m, i) for m in generic if m.rule.pmask & pmask)
            prev = seg


def _cand_key(c: _Cand) -> tuple[int, int, int]:
    return c[0].rule.prio, -c[1], c[0].rank  # priority, then depth, then specificity / order


@dataclass(slots=True)
class _Path:
    norm: str  # "/"-separated, no trailing separator
    segs: list[str]
    low: list[str]
    cum: list[int]  # cumulative segment lengths: norm[: cum[i] + i] is the path of segment i
    pmask: int
    win: bool
    # (segment index, "contains" | "beside", lower-case name globs) -> whether the tree has such a
    # child / sibling there; None: no tree to ask (a rule with contains / beside does not match)
    probe: Callable[[int, str, tuple[str, ...]], bool] | None = field(default=None)

    def upto(self, i: int) -> str:
        return self.norm[: self.cum[i] + i]


class _Ancestors:
    """Subtree matches on the folders containing an item, shared by siblings in ``match_many``."""

    __slots__ = ("best", "cands", "outer")

    def __init__(self, cands: list[_Cand], best: _Cand | None) -> None:
        self.cands = cands  # sorted by _cand_key
        self.best = best
        self.outer: dict[_Compiled, _Cand | None] = {}  # rule -> its shallowest match (lazily)


def _text_language(lang: str | None) -> str:
    """The language of a lookup's texts: ``lang``, else the interface language ("" = the rules' own)."""
    return interface_language() if lang is None else lang


def host_platform() -> str:
    if sys.platform == "win32":
        return "windows"
    return "macos" if sys.platform == "darwin" else "linux"


def _kind_bit(kind: NodeKind) -> int:
    if isinstance(kind, str):
        bit = _KIND_BITS.get(kind.lower())
        if bit is None:
            raise ValueError(f"unknown node kind {kind!r} (file / dir / link / other)")
        return bit
    return 1 << int(kind)


class RuleSet:
    """Compiled rules: ``match`` / ``match_many`` / ``match_node``."""

    def __init__(
        self, rules: Iterable[Rule], *, problems: Iterable[RuleProblem] = (), host: str | None = None
    ) -> None:
        """``rules`` in tie-break order (earlier wins on equal priority, depth and specificity);
        ``host`` ("windows" / "linux" / "macos") decides how POSIX-form paths are matched."""
        self.rules: tuple[Rule, ...] = tuple(rules)
        self.problems: tuple[RuleProblem, ...] = tuple(problems)
        self.host = host or host_platform()
        if self.host not in PLATFORMS:
            raise ValueError(f"unknown host platform {self.host!r}")
        self._by_id = {r.id: r for r in self.rules}
        self._win_host = self.host == "windows"
        self._posix_mask = _LINUX | _MAC if self._win_host else _PMASK[self.host]
        matchers = [
            _Matcher(compiled, pattern, order)
            for order, compiled in enumerate(_Compiled(r) for r in self.rules)
            for pattern in compiled.rule.patterns
        ]
        matchers.sort(key=lambda m: (m.rule.prio, -m.specificity, m.order))
        self._self = _Index()
        self._sub = _Index()
        for rank, m in enumerate(matchers):
            m.rank = rank
            self._self.add(m)
            if m.rule.rule.subtree and m.rule.nmask & _DIR_BIT:
                self._sub.add(m)
        self.unanchored = len(self._self.generic)  # patterns tested on every lookup (keep at 0)

    def __len__(self) -> int:
        return len(self.rules)

    def get(self, rule_id: str) -> Rule | None:
        return self._by_id.get(rule_id)

    # --- lookups ------------------------------------------------------------------------------

    def match(
        self, path: str, kind: NodeKind, *, size: int | None = None, lang: str | None = None
    ) -> Insight | None:
        """The insight for ``path`` (a ``kind`` node; ``size`` in bytes enables ``min_size``), with
        texts in ``lang`` (default: the interface language; "" for the rule's own texts)."""
        p = self._prepare(path)
        hit = self._find(p, _kind_bit(kind), size, None)
        return None if hit is None else self._insight(p, hit, _text_language(lang))

    def match_many(
        self,
        paths: Sequence[str],
        kinds: NodeKind | Sequence[NodeKind] = "dir",
        *,
        sizes: Sequence[int | None] | None = None,
        lang: str | None = None,
    ) -> list[Insight | None]:
        """``match`` over a list; siblings share the work on their common ancestors."""
        n = len(paths)
        if isinstance(kinds, (str, int)):
            bits = [_kind_bit(kinds)] * n
        else:
            if len(kinds) != n:
                raise ValueError("kinds and paths differ in length")
            bits = [_kind_bit(k) for k in kinds]
        if sizes is not None and len(sizes) != n:
            raise ValueError("sizes and paths differ in length")
        cache: dict[tuple[int, str], _Ancestors] = {}
        out: list[Insight | None] = []
        text_lang = _text_language(lang)
        for j, path in enumerate(paths):
            p = self._prepare(path)
            hit = self._find(p, bits[j], None if sizes is None else sizes[j], cache)
            out.append(None if hit is None else self._insight(p, hit, text_lang))
        return out

    def match_node(self, store: NodeStore, node: int, *, lang: str | None = None) -> Insight | None:
        """``match`` for a store node (its path, kind and allocated size; ``lang`` as there). Rules
        with ``contains`` / ``beside`` look at the node's tree."""
        return self.match_nodes(store, [node], lang=lang)[0]

    def match_nodes(
        self, store: NodeStore, nodes: Sequence[int], *, lang: str | None = None
    ) -> list[Insight | None]:
        """:meth:`match_node` for several nodes of one store."""
        c = store.cols
        text_lang = _text_language(lang)
        out: list[Insight | None] = []
        # siblings share the work on their folders (the same folders, so the same answers)
        cache: dict[tuple[int, str], _Ancestors] = {}
        for node in nodes:
            kind = Kind(int(c["kind"][node]))
            column = "tot_alloc" if kind == Kind.DIR else "alloc"
            p = self._prepare(store.path(node))
            p.probe = _tree_probe(self, store, int(node), p)
            hit = self._find(p, _kind_bit(kind), int(c[column][node]), cache)
            out.append(None if hit is None else self._insight(p, hit, text_lang))
        return out

    # --- internals ----------------------------------------------------------------------------

    def _prepare(self, path: str) -> _Path:
        p = path
        if p.startswith("\\\\"):
            win = True
            if p[2:4] in ("?\\", ".\\"):  # \\?\C:\… and \\?\UNC\server\share\…
                p = p[4:]
                if p[:4].upper() == "UNC\\":
                    p = "\\\\" + p[4:]
        else:
            win = bool(_DRIVE.match(p)) or (self._win_host and not p.startswith("/"))
        if win:
            p = p.replace("\\", "/")
        if len(p) > 1 and p.endswith("/"):
            p = p.rstrip("/") or "/"
        segs = p.split("/")
        cum = list(accumulate(map(len, segs)))
        return _Path(p, segs, p.lower().split("/"), cum, _WIN if win else self._posix_mask, win)

    def _accepts(self, c: _Cand, p: _Path, kbit: int, size: int | None) -> bool:
        m, i = c
        r = m.rule
        pm = r.pmask & p.pmask
        if not pm or not r.nmask & kbit:
            return False
        if size is not None and size < r.min_size:
            return False
        fold = bool(pm & _FOLD)
        prefix = p.upto(i)
        if (m.ci if fold else m.cs).fullmatch(prefix) is None:
            return False
        if r.parents is not None and (i == 0 or r.parents[fold].fullmatch(p.segs[i - 1]) is None):
            return False
        if r.exclude is not None and r.exclude[fold].fullmatch(prefix) is not None:
            return False
        rule = r.rule
        if rule.contains or rule.beside:
            probe = p.probe
            if probe is None:
                return False
            if rule.contains and not probe(i, "contains", rule.contains):
                return False
            if rule.beside and not probe(i, "beside", rule.beside):
                return False
        return True

    def _ancestors(self, p: _Path, upto: int, cache: dict[tuple[int, str], _Ancestors] | None) -> _Ancestors:
        """Subtree matches among segments ``0 .. upto-1`` (the folders containing the item)."""
        key = (p.pmask, p.upto(upto - 1) if upto else "")
        if cache is not None:
            known = cache.get(key)
            if known is not None:
                return known
        cands: list[_Cand] = []
        self._sub.collect(p.low, 0, upto, p.pmask, cands)
        best: _Cand | None = None
        if cands:
            cands.sort(key=_cand_key)
            best = next((c for c in cands if self._accepts(c, p, _DIR_BIT, None)), None)
        found = _Ancestors(cands, best)
        if cache is not None:
            cache[key] = found
        return found

    def _outermost(self, anc: _Ancestors, rule: _Compiled, p: _Path) -> _Cand | None:
        """The shallowest ancestor match of ``rule`` (memoised on ``anc``)."""
        if rule in anc.outer:
            return anc.outer[rule]
        mine = sorted((c for c in anc.cands if c[0].rule is rule), key=lambda c: (c[1], c[0].rank))
        hit = next((c for c in mine if self._accepts(c, p, _DIR_BIT, None)), None)
        anc.outer[rule] = hit
        return hit

    def _find(
        self, p: _Path, kbit: int, size: int | None, cache: dict[tuple[int, str], _Ancestors] | None
    ) -> _Cand | None:
        last = len(p.segs) - 1
        anc = self._ancestors(p, last, cache)
        best = anc.best
        cands: list[_Cand] = []
        self._self.collect(p.low, last, last + 1, p.pmask, cands)
        if cands:
            cands.sort(key=_cand_key)
            bound = _cand_key(best) if best is not None else None
            for c in cands:
                if bound is not None and _cand_key(c) > bound:
                    break
                if self._accepts(c, p, kbit, size):
                    best = c
                    break
        if best is None:
            return None
        if not anc.cands:
            return best
        outer = self._outermost(anc, best[0].rule, p)  # the same rule further up: the container
        return outer if outer is not None and outer[1] < best[1] else best

    def _native(self, p: _Path, i: int) -> str:
        prefix = p.upto(i)
        if not p.win:
            return prefix or "/"
        prefix = prefix.replace("/", "\\")
        return prefix + "\\" if prefix.endswith(":") else prefix

    def _insight(self, p: _Path, hit: _Cand, lang: str | None) -> Insight:
        m, i = hit
        rule = m.rule.rule
        t = rule.texts(lang)
        return Insight(
            rule=rule.id,
            title=t.title,
            reason=t.reason,
            kind=rule.kind,
            note=t.note,
            app=t.app,
            cleanup=rule.cleanup,
            command=rule.command,
            path=self._native(p, i),
            inside=i < len(p.segs) - 1,
            priority=rule.priority,
            regenerate=t.regenerate,
        )


def _tree_probe(
    rules: RuleSet, store: NodeStore, node: int, p: _Path
) -> Callable[[int, str, tuple[str, ...]], bool]:
    """Answers a rule's ``contains`` / ``beside`` from the tree: the path's segment ``i`` is the
    node ``node`` or one of its folders (above the scan's root there is nothing to look at)."""
    from ..pathclass import _holds

    parent = store.cols["parent"]
    chain = []  # the node and its folders, the root last
    x = node
    while x > 0:
        chain.append(x)
        x = int(parent[x])
    last = len(p.segs) - 1
    seg_of: dict[int, int] = {}
    for depth, n in enumerate(chain):
        seg_of[last - depth] = n  # each folder up is one segment shorter

    def probe(i: int, what: str, names: tuple[str, ...]) -> bool:
        at = seg_of.get(i)
        if at is None:
            return False
        if what == "contains":
            return int(store.cols["kind"][at]) == Kind.DIR and _holds(store, at, names)
        return _holds(store, int(parent[at]), names, skip=at)

    return probe


# --- loading ----------------------------------------------------------------------------------


def compile_rules(
    sources: Iterable[tuple[str, str]],
    *,
    host: str | None = None,
    problems: Iterable[RuleProblem] = (),
) -> RuleSet:
    """A rule set from TOML texts ``(source name, text)``; a rule in a later source replaces the
    earlier rule with the same id (and wins ties), ``enabled = false`` removes it. Problems are
    logged as warnings and kept in ``RuleSet.problems``."""
    merged: dict[str, tuple[int, int, Rule]] = {}
    found: list[RuleProblem] = list(problems)
    files = 0
    for s_idx, (source, text) in enumerate(sources):
        files += 1
        parsed, probs = _parse_file(text, source)
        found.extend(probs)
        for pos, (rule_id, rule) in enumerate(parsed):
            previous = merged.pop(rule_id, None)
            if previous is not None and previous[0] != s_idx and rule is not None:
                logger.debug(MSG["rule_override"], rule=rule_id, source=source)
            if rule is not None:
                merged[rule_id] = (s_idx, pos, rule)
    for problem in found:
        logger.warning("{}", problem.message)
    ordered = sorted(merged.values(), key=lambda e: (-e[0], e[1]))
    rule_set = RuleSet((e[2] for e in ordered), problems=found, host=host)
    logger.debug(MSG["rules_loaded"], count=len(rule_set), files=files, skipped=len(found))
    return rule_set


BUILTIN_PLUGIN = "builtin.insights"


def builtin_file() -> Path:
    """The built-in rules: a rule file of the shipped plugin ``builtin.insights``."""
    from ..plugins.paths import builtin_dir

    return builtin_dir() / BUILTIN_PLUGIN / "insights" / BUILTIN_SOURCE


def builtin_text() -> str:
    return builtin_file().read_text("utf-8")


def user_rules_dir() -> Path:
    """Where the user's own rule files live (``*.toml``; ``SPACEKNIFE_HOME`` relocates it)."""
    return config_dir() / "insights"


_active: RuleSet | None = None
_lock = threading.Lock()
_plugin_sources: list[tuple[str, str]] = []  # (source name, TOML text) of the enabled plugins' rule files
_user_dir: Path | None = None  # the folder the active set was loaded with (its files' times below)
_user_sig: tuple[tuple[str, int, int], ...] = ()


def _signature(folder: Path | None) -> tuple[tuple[str, int, int], ...]:
    if folder is None:
        return ()
    try:
        return tuple(
            sorted(
                (f.name, f.stat().st_mtime_ns, f.stat().st_size) for f in folder.glob("*.toml") if f.is_file()
            )
        )
    except OSError:
        return ()


def set_plugin_sources(sources: list[tuple[str, str]]) -> RuleSet | None:
    """The registered rule files changed (the shipped plugin's first, then the plugins' by plugin id;
    the user's come after them): the active set is built again — later, while the shipped plugins
    are still registering."""
    global _plugin_sources, _active
    from ..plugins import builtins

    _plugin_sources = list(sources)
    if builtins.loading() or not builtins.loaded():
        # built on first use instead: loading the shipped plugins from here (inside the registry's
        # lock, maybe while another thread loads them) could deadlock
        _active = None
        return None
    return load_rules(_user_dir)


def refresh_user_rules() -> None:
    """Load the user's rule folder again when its files changed (before a lookup that uses it)."""
    if _user_dir is not None and _signature(_user_dir) != _user_sig:
        load_rules(_user_dir)


def load_rules(extra_dir: Path | None = None, *, host: str | None = None) -> RuleSet:
    """Built-in rules, the enabled plugins' rule files, then every ``*.toml`` in ``extra_dir`` (by
    file name; later sources override earlier ones by id), made the active set for the module-level
    ``match`` functions."""
    global _active, _user_dir, _user_sig
    from ..plugins import builtins

    builtins.ensure()  # the built-in rules are a plugin's: registered on first use
    _user_dir = extra_dir
    _user_sig = _signature(extra_dir)
    sources = list(_plugin_sources)
    if not any(name == BUILTIN_SOURCE for name, _text in sources):  # the shipped plugin did not load
        sources.insert(0, (BUILTIN_SOURCE, builtin_text()))
    problems: list[RuleProblem] = []
    if extra_dir is not None:
        try:
            files = sorted((f for f in extra_dir.glob("*.toml") if f.is_file()), key=lambda f: f.name)
        except OSError as exc:
            files = []
            problems.append(
                RuleProblem(str(extra_dir), "", MSG["file_unreadable"].format(source=extra_dir, error=exc))
            )
        for f in files:
            try:
                sources.append((str(f), f.read_text(encoding="utf-8")))
            except (OSError, UnicodeDecodeError) as exc:
                problems.append(RuleProblem(str(f), "", MSG["file_unreadable"].format(source=f, error=exc)))
    rule_set = compile_rules(sources, host=host, problems=problems)
    _active = rule_set
    return rule_set


def active_rules() -> RuleSet:
    """The active rule set (the built-in rules until ``load_rules`` is called)."""
    rule_set = _active
    if rule_set is None:
        with _lock:
            rule_set = _active if _active is not None else load_rules(_user_dir)
    return rule_set


def match(path: str, kind: NodeKind, *, size: int | None = None, lang: str | None = None) -> Insight | None:
    return active_rules().match(path, kind, size=size, lang=lang)


def match_many(
    paths: Sequence[str],
    kinds: NodeKind | Sequence[NodeKind] = "dir",
    *,
    sizes: Sequence[int | None] | None = None,
    lang: str | None = None,
) -> list[Insight | None]:
    return active_rules().match_many(paths, kinds, sizes=sizes, lang=lang)


def match_node(store: NodeStore, node: int, *, lang: str | None = None) -> Insight | None:
    return active_rules().match_node(store, node, lang=lang)


def match_nodes(store: NodeStore, nodes: Sequence[int], *, lang: str | None = None) -> list[Insight | None]:
    return active_rules().match_nodes(store, nodes, lang=lang)
