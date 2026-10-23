"""A tree image starts with a header the plugin checks: mapping a segment that is no longer there
fails loudly (on Windows a missing name would otherwise map as fresh zeros)."""

from __future__ import annotations

import pytest

from spaceknife.plugins.image import HEADER, MAGIC, export
from spaceknife_sdk.image import ImageGone, map_image

from .store_builder import build


def test_the_header_is_checked() -> None:
    store = build({"a": {"b.txt": 10, "c.txt": 20}})
    e = export("scan-x", store, ["size", "kind"])
    try:
        assert bytes(e.shm.mm[: len(MAGIC)]) == MAGIC and e.desc["columns"][0]["offset"] >= HEADER
        image = map_image(e.desc)
        assert image.columns["size"].tolist() == store.cols["size"][: len(store)].tolist()
        image.close()
        wrong = {**e.desc, "head": (MAGIC + b"\0" * (HEADER - len(MAGIC))).hex()}
        with pytest.raises(ImageGone):
            map_image(wrong)
    finally:
        e.release()
    with pytest.raises(
        OSError, match=r"gone|No such file|cannot find"
    ):  # shm_open fails / the header differs
        map_image(e.desc)
