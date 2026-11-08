"""Normalization and comparison of transcripts (README.md, 'Normalization').

Every rule here has a twin in crates/sk-server/tests/parity_replay.rs; change both together.
"""

from __future__ import annotations

import json
import math
import re
import sys
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent.parent
WINDOWS = sys.platform == "win32"

# ISO 8601 date-time as both backends write it (any number of fraction digits, Z or an offset)
ISO_TIME = re.compile(r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?$")

KEY_NODE_MASK = (1 << 30) - 1
# a snapshot file's time stamp (20261108-030158) inside a path
STAMP = re.compile(r"(?<![0-9])[0-9]{8}-[0-9]{6}(?![0-9])")


def load_rules(extra: dict[str, Any] | None = None) -> dict[str, Any]:
    """normalize.json, with a scenario's `normalize` additions (lists are extended)."""
    rules = json.loads((HERE / "normalize.json").read_text(encoding="utf-8"))
    for k, v in (extra or {}).items():
        if isinstance(v, list):
            rules[k] = list(rules.get(k, [])) + v
        elif isinstance(v, dict):
            rules[k] = {**rules.get(k, {}), **v}
        else:
            rules[k] = v
    return rules


class Normalizer:
    """One backend's transcript: id symbols, node paths and path prefixes."""

    def __init__(self, rules: dict[str, Any], places: dict[str, str]) -> None:
        self.rules = rules
        self.drop = set(rules["drop_keys"])
        self.server_drop = set(rules["server_info_drop"])
        self.server_marker = set(rules["server_info_marker"])
        self.times = set(rules["time_keys"])
        self.id_keys: dict[str, str] = rules["id_keys"]
        self.node_keys = set(rules["node_keys"])
        self.key_keys = set(rules["key_keys"])
        self.node_info = set(rules["node_info_marker"])
        self.drop_in: dict[str, list[str]] = rules.get("drop_in", {})
        self.shape_keys = set(rules.get("shape_keys", []))
        self.markers: list[dict[str, Any]] = rules.get("symbol_markers", [])
        # longest real path first, so $TMP/… never shadows $ROOT/… below it
        self.places = sorted(places.items(), key=lambda kv: -len(kv[1]))
        self.symbols: dict[str, str] = {}
        self.counts: dict[str, int] = {}
        self.nodes: dict[tuple[str, int], str] = {}
        self.scan = ""

    # ---------------------------------------------------------------- ids and nodes

    def symbol(self, kind: str, raw: str) -> str:
        if raw not in self.symbols:
            self.counts[kind] = self.counts.get(kind, 0) + 1
            self.symbols[raw] = f"<{kind}#{self.counts[kind]}>"
        return self.symbols[raw]

    def node_ids(self, v: Any) -> set[int]:
        """Node ids a value names (node keys, layout keys, NodeInfo ids) to resolve first."""
        out: set[int] = set()

        def walk(x: Any) -> None:
            if isinstance(x, dict):
                info = self.node_info <= x.keys()
                for k, val in x.items():
                    if isinstance(val, bool):
                        continue
                    if isinstance(val, int):
                        if k in self.node_keys or (info and k == "id"):
                            if val >= 0:
                                out.add(val)
                        elif k in self.key_keys and val >= 0:
                            out.add(val & KEY_NODE_MASK)
                    else:
                        walk(val)
            elif isinstance(x, list):
                for i in x:
                    walk(i)

        walk(v)
        return out

    def node_ref(self, nid: int) -> str:
        if nid < 0:
            return "@none"
        path = self.nodes.get((self.scan, nid))
        return f"@{path}" if path is not None else f"@?{nid}"

    def key_ref(self, key: int) -> str:
        tag, low = key >> 30, key & KEY_NODE_MASK
        if tag == 0:
            return self.node_ref(low)
        if tag == 1:
            return "@small:" + self.node_ref(low)
        if tag == 2:
            return "@more:" + self.node_ref(low)
        return {1: "@free", 2: "@unknown"}.get(low, f"@k3:{low}")

    # ---------------------------------------------------------------- values

    @staticmethod
    def variants(real: str) -> list[str]:
        """How a backend may spell a folder: as is, with `/`, and (Windows) as WSL sees it."""
        out = [real, real.replace("\\", "/")]
        if "\\" in real:
            out.append(real.replace("\\", "\\\\"))  # inside JSON text (exports)
        if len(real) > 2 and real[1] == ":" and real[0].isalpha():
            out.append(f"/mnt/{real[0].lower()}/" + real[3:].replace("\\", "/"))
        return out

    def path_text(self, s: str) -> str:
        hit = False
        for name, real in self.places:
            for variant in self.variants(real):
                if not variant:
                    continue
                if WINDOWS:
                    low, vl = s.lower(), variant.lower()
                    i = low.find(vl)
                    while i >= 0:
                        s = s[:i] + name + s[i + len(variant) :]
                        hit = True
                        low = s.lower()
                        i = low.find(vl, i + len(name))
                elif variant in s:
                    s = s.replace(variant, name)
                    hit = True
        if hit:
            s = s.replace("\\\\?\\$", "$")
            if WINDOWS:
                s = s.replace("\\\\", "/").replace("\\", "/")
            # snapshot files are named by the time they were saved
            s = STAMP.sub("<stamp>", s)
        return s

    def text(self, s: str) -> str:
        if s in self.symbols:
            return self.symbols[s]
        if ISO_TIME.match(s):
            return "<time>"
        return self.path_text(s)

    def time(self, v: Any) -> Any:
        if v is None:
            return None
        if isinstance(v, bool):
            return "<bad-time>"
        if isinstance(v, (int, float)):
            return "<time>" if v > 0 else v
        if isinstance(v, str) and ISO_TIME.match(v):
            return "<time>"
        return "<bad-time>"

    def norm(self, v: Any, parent: str = "") -> Any:
        if isinstance(v, dict):
            server = self.server_marker <= v.keys() and bool(self.server_drop & v.keys() - {"version"})
            info = self.node_info <= v.keys()
            inner = set(self.drop_in.get(parent, []))
            out: dict[str, Any] = {}
            for k, val in v.items():
                if k in self.drop or k in inner or (server and k in self.server_drop):
                    continue
                if k in self.shape_keys and val is not None:
                    out[k] = shape(val)
                elif k in self.times:
                    out[k] = self.time(val)
                elif k in self.id_keys and isinstance(val, str):
                    out[k] = self.symbol(self.id_keys[k], val)
                elif isinstance(val, str) and (kind := self.marked_id(v, k)):
                    out[k] = self.symbol(kind, val)
                elif isinstance(val, int) and not isinstance(val, bool) and (
                    k in self.node_keys or (info and k == "id")
                ):
                    out[k] = self.node_ref(val)
                elif isinstance(val, int) and not isinstance(val, bool) and k in self.key_keys:
                    out[k] = self.key_ref(val)
                else:
                    out[k] = self.norm(val, k)
            return out
        if isinstance(v, list):
            return [self.norm(i, parent) for i in v]
        if isinstance(v, str):
            return self.text(v)
        return v

    def marked_id(self, obj: dict[str, Any], key: str) -> str:
        """The symbol kind of `obj[key]` when `obj` has every key of a `symbol_markers` entry."""
        for m in self.markers:
            if m["key"] == key and set(m["marker"]) <= obj.keys():
                return str(m["kind"])
        return ""

    def error(self, e: dict[str, Any]) -> dict[str, Any]:
        """Code and the first item's `loc` (Rust reports one item, Python all of them)."""
        loc = None
        data = e.get("data")
        if isinstance(data, list) and data and isinstance(data[0], dict) and "loc" in data[0]:
            loc = [self.norm(x) for x in data[0]["loc"]]
        return {"code": e.get("code"), "loc": loc}

    def frame(self, header: dict[str, Any], columns: dict[str, tuple[str, list[Any]]], blobs: dict[str, bytes]) -> dict[str, Any]:
        from .frames import split_names

        cols: dict[str, Any] = {}
        for name, (dtype, values) in sorted(columns.items()):
            if name == "key":
                values = [self.key_ref(int(x)) for x in values]
            elif name == "node":
                values = [self.node_ref(int(x)) for x in values]
            cols[name] = {"dtype": dtype, "values": values}
        out: dict[str, Any] = {
            "stream": self.text_stream(str(header.get("stream", ""))),
            "layout": header.get("layout"),
            "seq": header.get("seq"),
            "final": header.get("final", True),
            "count": header.get("count"),
            "columns": cols,
        }
        names = blobs.get("names")
        if names is not None and "name_len" in columns:
            out["names"] = [self.path_text(n) for n in split_names(names, [int(x) for x in columns["name_len"][1]])]
        other = sorted(k for k in blobs if k != "names")
        if other:
            out["blobs"] = {k: len(blobs[k]) for k in other}
        return out

    def text_stream(self, s: str) -> str:
        head, sep, tail = s.partition(":")
        if sep and tail in self.symbols:
            return f"{head}:{self.symbols[tail]}"
        return s


# -------------------------------------------------------------------- shapes


def shape(v: Any) -> Any:
    """Types only: objects keep their non-null keys, lists the merged shape of their items."""
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "bool"
    if isinstance(v, (int, float)):
        return "number"
    if isinstance(v, str):
        return "string"
    if isinstance(v, list):
        if not v:
            return []
        merged = shape(v[0])
        for item in v[1:]:
            merged = merge(merged, shape(item))
        return [merged]
    return {k: shape(val) for k, val in sorted(v.items()) if val is not None}


def merge(a: Any, b: Any) -> Any:
    if isinstance(a, dict) and isinstance(b, dict):
        out = dict(a)
        for k, v in b.items():
            out[k] = merge(out[k], v) if k in out else v
        return dict(sorted(out.items()))
    if isinstance(a, list) and isinstance(b, list):
        if not a or not b:
            return a or b
        return [merge(a[0], b[0])]
    return a if a == b else "mixed"


# -------------------------------------------------------------------- comparison


def keyed(transcript: list[dict[str, Any]]) -> dict[str, Any]:
    """Records by their step index, so a difference names the scenario step (`steps/<i>/…`)."""
    return {str(r["i"]): {k: v for k, v in r.items() if k != "i"} for r in transcript}


def escape(key: str) -> str:
    return key.replace("~", "~0").replace("/", "~1")


def compare(exp: Any, act: Any, rules: dict[str, Any], path: str = "") -> list[dict[str, Any]]:
    """Differences as {path, expected, actual}; numbers within the relative tolerance are equal."""
    out: list[dict[str, Any]] = []
    tol = float(rules.get("float_rel_tol", 1e-9))
    f4 = float(rules.get("f4_rel_tol", 1e-6))

    def num(x: Any) -> bool:
        return isinstance(x, (int, float)) and not isinstance(x, bool)

    def walk(e: Any, a: Any, p: str, t: float) -> None:
        if isinstance(e, dict) and isinstance(a, dict):
            # a decoded column: its dtype sets the tolerance of its values
            t2 = f4 if e.get("dtype") == "<f4" else t
            for k in sorted(set(e) | set(a)):
                q = f"{p}/{escape(k)}"
                if k not in a:
                    out.append({"path": q, "expected": e[k], "actual": "<missing>"})
                elif k not in e:
                    out.append({"path": q, "expected": "<missing>", "actual": a[k]})
                else:
                    walk(e[k], a[k], q, t2)
        elif isinstance(e, list) and isinstance(a, list):
            for i in range(min(len(e), len(a))):
                walk(e[i], a[i], f"{p}/{i}", t)
            if len(e) != len(a):
                out.append({"path": f"{p}/#len", "expected": len(e), "actual": len(a)})
        elif num(e) and num(a):
            if e != a and not (
                math.isfinite(e) and math.isfinite(a) and abs(e - a) <= t * max(abs(e), abs(a))
            ):
                out.append({"path": p, "expected": e, "actual": a})
        elif e != a or type(e) is not type(a) and not (num(e) and num(a)):
            out.append({"path": p, "expected": e, "actual": a})

    walk(exp, act, path, tol)
    return out


def glob_match(pattern: str, path: str) -> bool:
    """`*` matches within one path segment, `**` any number of segments."""
    from fnmatch import fnmatchcase

    pat = [s for s in pattern.strip("/").split("/")]
    segs = [s for s in path.strip("/").split("/")]

    def m(i: int, j: int) -> bool:
        if i == len(pat):
            return j == len(segs)
        if pat[i] == "**":
            return any(m(i + 1, k) for k in range(j, len(segs) + 1))
        return j < len(segs) and fnmatchcase(segs[j], pat[i]) and m(i + 1, j + 1)

    return m(0, 0)


def split_known(diffs: list[dict[str, Any]], known: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """(unexpected, known) differences."""
    bad, ok = [], []
    for d in diffs:
        hit = next((k for k in known if glob_match(k["path"], d["path"])), None)
        if hit is None:
            bad.append(d)
        else:
            ok.append({**d, "why": hit.get("why", ""), "bug": hit.get("bug")})
    return bad, ok
