"""Client side of views: pairs each ``ViewUpdate`` with its binary draw-list frame."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass

import numpy as np
from pydantic import BaseModel

from spaceknife_protocol import view as pv
from spaceknife_protocol.binary import BinaryFrameHeader
from spaceknife_protocol.methods import EVENT_VIEW_FRAME, MethodSpec

from .connection import Connection


@dataclass(frozen=True, slots=True)
class DrawFrame:
    view_id: str
    seq: int
    columns: dict[str, np.ndarray]
    names: list[str]

    def __len__(self) -> int:
        return len(self.names)

    def row(self, key: int) -> int | None:
        hit = np.nonzero(self.columns["key"] == key)[0]
        return int(hit[0]) if hit.size else None


def decode_draw(header: BinaryFrameHeader, columns: dict[str, np.ndarray]) -> DrawFrame:
    blob = columns["names"].tobytes()
    lens = columns["name_len"].astype(np.int64)
    ends = np.cumsum(lens)
    starts = ends - lens
    names = [blob[a:b].decode("utf-8") for a, b in zip(starts.tolist(), ends.tolist(), strict=True)]
    view_id = header.stream[len(pv.VIEW_STREAM_PREFIX) :]
    return DrawFrame(view_id, header.seq, {k: v for k, v in columns.items() if k != "names"}, names)


class ViewClient:
    """Collects draw-list frames and hands them out together with the method results."""

    def __init__(self, conn: Connection) -> None:
        self.conn = conn
        self._frames: dict[tuple[str, int], DrawFrame] = {}
        self._waiters: dict[tuple[str, int], asyncio.Future[DrawFrame]] = {}
        self.pushed: asyncio.Queue[pv.FrameInfo] = asyncio.Queue()
        conn.on_binary(self._on_binary)
        conn.on_event(EVENT_VIEW_FRAME, lambda ev: self.pushed.put_nowait(ev.frame))

    def _on_binary(self, header: BinaryFrameHeader, columns: dict[str, np.ndarray]) -> None:
        if header.layout != pv.DRAWLIST_LAYOUT or not header.stream.startswith(pv.VIEW_STREAM_PREFIX):
            return
        frame = decode_draw(header, columns)
        key = (frame.view_id, frame.seq)
        waiter = self._waiters.pop(key, None)
        if waiter is not None and not waiter.done():
            waiter.set_result(frame)
        else:
            self._frames[key] = frame
            if len(self._frames) > 64:
                self._frames.pop(next(iter(self._frames)))

    async def frame(self, view_id: str, seq: int) -> DrawFrame:
        """The draw list with this sequence number (wrap in ``asyncio.timeout`` to bound the wait)."""
        key = (view_id, seq)
        if key in self._frames:
            return self._frames.pop(key)
        fut = self._waiters.setdefault(key, asyncio.get_running_loop().create_future())
        return await fut

    async def call[P: BaseModel](
        self, spec: MethodSpec[P, pv.ViewUpdate], params: P
    ) -> tuple[pv.ViewUpdate, DrawFrame | None]:
        update = await self.conn.call(spec, params)
        if update.frame is None:
            return update, None
        async with asyncio.timeout(10):
            return update, await self.frame(update.state.view_id, update.frame.seq)
