"""tools/build_package.py: the AppImage's update information (AppImageSpec: ELF section .upd_info)."""

from __future__ import annotations

import struct
from pathlib import Path

import pytest

import build_package


def elf64(sections: dict[str, int], tail: bytes = b"squashfs payload") -> bytes:
    """A minimal little-endian ELF64 file: the named sections (zero-filled, of these sizes), a
    section-name table, then ``tail`` (an AppImage's file system follows the runtime the same way)."""
    names = b"\x00" + b"".join(n.encode() + b"\x00" for n in [*sections, ".shstrtab"])
    body = bytearray(64)
    entries = [(0, 0, 0)]  # the null section
    pos = 1
    for name, size in sections.items():
        entries.append((pos, len(body), size))
        body += bytes(size)
        pos += len(name) + 1
    entries.append((pos, len(body), len(names)))
    body += names
    shoff = len(body)
    for name_at, offset, size in entries:
        body += struct.pack("<IIQQQQIIQQ", name_at, 1, 0, 0, offset, size, 0, 0, 1, 0)
    ident = b"\x7fELF" + bytes([2, 1, 1]) + bytes(9)
    n = len(entries)
    body[:64] = ident + struct.pack("<HHIQQQIHHHHHH", 2, 62, 1, 0, 0, shoff, 0, 64, 0, 0, 64, n, n - 1)
    return bytes(body) + tail


def test_update_information_goes_into_upd_info_only(tmp_path: Path) -> None:
    image = tmp_path / "SpaceKnife-0.3.2-x86_64.AppImage"
    before = elf64({".upd_info": 1024, ".sha256_sig": 1024})
    image.write_bytes(before)
    at, size = build_package.elf_section(image, ".upd_info") or (0, 0)
    assert size == 1024
    build_package.embed_update_info(image)
    after = image.read_bytes()
    info = build_package.APPIMAGE_UPDATE_INFO.encode()
    assert after[at : at + size] == info.ljust(size, b"\x00")
    # every other byte (the signature section, the payload) stays
    assert after[:at] == before[:at]
    assert after[at + size :] == before[at + size :]
    assert info.startswith(b"gh-releases-zsync|DuskXi|SpaceKnife|latest-all|")  # pre-releases count
    assert info.endswith(b"SpaceKnife-*-x86_64.AppImage.zsync")


def test_update_information_needs_room_and_an_elf64(tmp_path: Path) -> None:
    image = tmp_path / "a.AppImage"
    image.write_bytes(elf64({".upd_info": 16}))
    with pytest.raises(SystemExit, match=r"longer than \.upd_info"):
        build_package.embed_update_info(image)
    image.write_bytes(elf64({".text": 16}))
    with pytest.raises(SystemExit, match=r"no \.upd_info"):
        build_package.embed_update_info(image)
    image.write_bytes(b"#!/bin/sh\necho not an AppImage\n" + bytes(64))
    with pytest.raises(SystemExit, match="not a little-endian ELF64"):
        build_package.elf_section(image, ".upd_info")
