"""Test plugin: results past their limits (the host cuts or refuses them)."""

from __future__ import annotations

from spaceknife_sdk import Context, Payload, Plugin, Report, ReportContext, r

plugin = Plugin()


class Huge(Payload):
    text: str


@plugin.method
async def huge(ctx: Context) -> Huge:
    return Huge(text="x" * (9 << 20))


@plugin.report("很长的表", name="long")
async def long(ctx: Context, c: ReportContext) -> Report:
    rows = [{"n": i} for i in range(3000)]
    return Report(blocks=[r.table([r.col("n", "序号", "count")], rows=rows)])


@plugin.report("很多块", name="many")
async def many(ctx: Context, c: ReportContext) -> Report:
    return Report(blocks=[r.md(f"块 {i}") for i in range(300)])
