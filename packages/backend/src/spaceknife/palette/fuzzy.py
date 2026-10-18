"""Fuzzy matching for the command palette (UI-W4-30): case-insensitive subsequence with bonuses.

A query matches a text when its characters appear in order (spaces in the query are ignored).
Contiguous runs, word starts and a match at the very start score higher; gaps cost a little.
The best alignment is found by dynamic programming, so "dkr" in "Docker" lights up D, k, r.
"""

from __future__ import annotations

MATCH = 16
WORD_START = 10
FIRST_CHAR = 6
CONSECUTIVE = 12
GAP_OPEN = 3
GAP_EXTEND = 1
LEADING_MAX = 6
EXACT = 400
PREFIX = 120

_SEPARATORS = frozenset(" _-./\\:;,()[]{}<>·…|+")


def _is_cjk(ch: str) -> bool:
    o = ord(ch)
    return 0x3400 <= o <= 0x9FFF or 0xF900 <= o <= 0xFAFF or 0x20000 <= o <= 0x3134F


def _word_start(text: str, j: int) -> bool:
    if j == 0:
        return True
    prev, cur = text[j - 1], text[j]
    if prev in _SEPARATORS or _is_cjk(cur):
        return True
    if prev.islower() and cur.isupper():
        return True
    return prev.isdigit() != cur.isdigit()


def score(query: str, text: str) -> tuple[int, list[tuple[int, int]]] | None:
    """(score, matched index runs as ``[start, end)`` over ``text``), or None if it does not match."""
    q = [c.lower() for c in query if not c.isspace()]
    if not q:
        return 0, []
    t = [c.lower() for c in text]
    n, m = len(q), len(t)
    if n > m:
        return None
    # quick reject: subsequence test
    k = 0
    for ch in t:
        if ch == q[k]:
            k += 1
            if k == n:
                break
    if k < n:
        return None
    bonus = [WORD_START if _word_start(text, j) else 0 for j in range(m)]
    neg = -(1 << 30)
    prev = [neg] * m
    back: list[list[int]] = []
    for j in range(m):
        if t[j] == q[0]:
            prev[j] = MATCH + bonus[j] + (FIRST_CHAR if j == 0 else -min(j, LEADING_MAX))
    back.append([-1] * m)
    for i in range(1, n):
        cur = [neg] * m
        ptr = [-1] * m
        best, best_k = neg, -1  # best over k < j - 1 of prev[k] - gap(j - k - 1), kept incrementally
        for j in range(i, m):
            # extend the running best by one more gap character
            if best > neg:
                best -= GAP_EXTEND
            if j - 2 >= 0 and prev[j - 2] > neg:
                cand = prev[j - 2] - GAP_OPEN
                if cand > best:
                    best, best_k = cand, j - 2
            if t[j] != q[i]:
                continue
            via_gap = best
            via_run = prev[j - 1] + CONSECUTIVE if prev[j - 1] > neg else neg
            if via_run >= via_gap and via_run > neg:
                cur[j], ptr[j] = via_run + MATCH + bonus[j], j - 1
            elif via_gap > neg:
                cur[j], ptr[j] = via_gap + MATCH + bonus[j], best_k
        back.append(ptr)
        prev = cur
    end = max(range(m), key=lambda j: prev[j])
    total = prev[end]
    if total <= neg:
        return None
    picks = [end]
    for i in range(n - 1, 0, -1):
        picks.append(back[i][picks[-1]])
    picks.reverse()
    if "".join(t) == "".join(q):
        total += EXACT
    elif picks[0] == 0 and picks[-1] == n - 1:
        total += PREFIX
    return total, runs(picks)


def runs(indices: list[int]) -> list[tuple[int, int]]:
    out: list[tuple[int, int]] = []
    for i in indices:
        if out and out[-1][1] == i:
            out[-1] = (out[-1][0], i + 1)
        else:
            out.append((i, i + 1))
    return out


def utf16_spans(text: str, spans: list[tuple[int, int]]) -> list[tuple[int, int]]:
    """Python index runs -> UTF-16 offsets (what JavaScript and Qt strings index by)."""
    if text.isascii() or all(ord(c) < 0x10000 for c in text):
        return spans
    offsets = [0]
    for ch in text:
        offsets.append(offsets[-1] + (2 if ord(ch) >= 0x10000 else 1))
    return [(offsets[a], offsets[b]) for a, b in spans]


__all__ = ["runs", "score", "utf16_spans"]
