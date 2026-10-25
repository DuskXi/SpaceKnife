from __future__ import annotations

from collections.abc import AsyncIterator
from pathlib import Path

import pytest

from .harness import FIXTURES, Backend


@pytest.fixture
async def backend(tmp_path: Path) -> AsyncIterator[Backend]:
    """A backend of its own: temporary home, free port, stopped afterwards."""
    b = Backend(tmp_path / "home")
    try:
        await b.start()
        yield b
    finally:
        await b.stop()


@pytest.fixture
async def probe(backend: Backend) -> str:
    """The acceptance probe installed and enabled (its id)."""
    return await backend.install(FIXTURES / "acc.probe")


@pytest.fixture
def data(tmp_path: Path) -> Path:
    """A folder for the files the test scans (not under the backend's home)."""
    d = tmp_path / "data"
    d.mkdir()
    return d
