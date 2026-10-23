"""The method behind the panel (web/app.js calls it as ``sk.call("histogram")``)."""

from __future__ import annotations

import datetime as dt

import numpy as np

from spaceknife_sdk import Context, Params, Payload, Plugin

plugin = Plugin()


class HistogramParams(Params):
    under: int | None = None  # a folder of the scan (default: everything)


class Year(Payload):
    year: int
    files: int
    bytes: int


class Histogram(Payload):
    years: list[Year]
    unknown: int  # files without a modification time


@plugin.method(latest=True)
def histogram(ctx: Context, p: HistogramParams) -> Histogram:
    """Files and bytes per year of last modification (a plain def runs on a worker thread)."""
    rows = ctx.tree.query(kind="file", under=p.under, columns=("mtime", "bytes"), limit=1_000_000)
    mtime = rows["mtime"].astype(np.int64)
    known = mtime > 0
    years = np.array([dt.datetime.fromtimestamp(int(t), dt.UTC).year for t in mtime[known]], dtype=np.int64)
    out = []
    if len(years):
        sizes = rows["bytes"][known]
        for year in range(int(years.min()), int(years.max()) + 1):
            sel = years == year
            out.append(Year(year=year, files=int(sel.sum()), bytes=int(sizes[sel].sum())))
    return Histogram(years=out, unknown=int((~known).sum()))
