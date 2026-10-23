"""The edits of a cleanup plan kept between sessions (docs/dupes.md §16.7.7).

``data_dir()/dupes/cleanup/plans/<fingerprint of the scan roots>.json``: rules, options, manual
choices (digest hex, path), review marks; never acknowledgements (they are given again, for the
copies then shown). Opening a plan over the same roots offers to take them.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import os
import time
from typing import Any

from .journal import plans_root
from .policy import Policy


def fingerprint(roots: list[str]) -> str:
    key = "\0".join(sorted(os.path.normcase(r) for r in roots))
    return hashlib.blake2b(key.encode("utf-8", "surrogatepass"), digest_size=10).hexdigest()


def save(roots: list[str], policy: Policy) -> None:
    d = plans_root()
    with contextlib.suppress(OSError):
        d.mkdir(parents=True, exist_ok=True)
        data = {"saved": time.time(), "roots": roots, "policy": policy.to_json()}
        tmp = d / f"{fingerprint(roots)}.json.tmp"
        with open(tmp, "w", encoding="utf-8", errors="surrogatepass") as f:
            json.dump(data, f, ensure_ascii=False)
        os.replace(tmp, d / f"{fingerprint(roots)}.json")


def load(roots: list[str]) -> tuple[float, Policy] | None:
    p = plans_root() / f"{fingerprint(roots)}.json"
    try:
        with open(p, encoding="utf-8", errors="surrogatepass") as f:
            data: dict[str, Any] = json.load(f)
        return float(data.get("saved", 0)), Policy.from_json(data.get("policy", {}))
    except (OSError, ValueError, KeyError, TypeError):
        return None


def drop(roots: list[str]) -> None:
    with contextlib.suppress(OSError):
        (plans_root() / f"{fingerprint(roots)}.json").unlink()
