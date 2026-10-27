"""tools/release.py: checking the build runs' artifacts, collecting the release, filling the notes."""

from __future__ import annotations

import hashlib
from pathlib import Path

import pytest

import release

V = "0.3.4"


def _write(folder: Path, name: str, data: bytes) -> str:
    folder.mkdir(parents=True, exist_ok=True)
    (folder / name).write_bytes(data)
    return hashlib.sha256(data).hexdigest()


def artifacts(root: Path, version: str = V) -> Path:
    """The artifacts of a release's build runs, laid out as ``gh run download`` writes them."""
    pv = release.deb_version(version)
    groups = {
        "SpaceKnife-installers-win-x64": (
            [f"SpaceKnife-{version}-Setup-x64.exe"],
            "SHA256SUMS-installers-win-x64.txt",
        ),
        "SpaceKnife-installers-linux-x64": (
            [
                f"SpaceKnife-{version}-x86_64.AppImage",
                f"spaceknife_{pv}_amd64.deb",
                f"spaceknife-{pv}.x86_64.rpm",
            ],
            "SHA256SUMS-installers-linux-x64.txt",
        ),
    }
    for target, ext in (
        ("win-x64", "zip"),
        ("win-arm64", "zip"),
        ("linux-x64", "tar.gz"),
        ("linux-arm64", "tar.gz"),
    ):
        groups[f"SpaceKnife-portable-{target}"] = (
            [f"SpaceKnife-{version}-{target}-portable.{ext}"],
            f"SHA256SUMS-portable-{target}.txt",
        )
    for a in ("arm64", "x64"):
        groups[f"SpaceKnife-mac-{a}"] = (
            [f"SpaceKnife-{version}-mac-{a}.dmg", f"SpaceKnife-{version}-mac-{a}-portable.zip"],
            f"SHA256SUMS-mac-{a}.txt",
        )
    for folder, (names, sums) in groups.items():
        d = root / folder
        lines = [f"{_write(d, n, n.encode() * 3)}  {n}\n" for n in names]
        (d / sums).write_text("".join(lines), encoding="utf-8")
    _write(root / "SpaceKnife-installers-linux-x64", f"SpaceKnife-{version}-x86_64.AppImage.zsync", b"zsync")
    return root


def test_a_complete_set_becomes_the_release(tmp_path: Path) -> None:
    done = release.assemble(V, artifacts(tmp_path / "a"), tmp_path / "out")
    names = {p.name for p in (tmp_path / "out").iterdir()}
    assert names == {e.name for e in release.expected(V)} | set(release.published_sums(V))
    assert "SHA256SUMS-installers-win-x64.txt" not in names
    merged = release.parse_sums((tmp_path / "out" / "SHA256SUMS.txt").read_text(encoding="utf-8"))
    assert set(merged) == {
        f"SpaceKnife-{V}-Setup-x64.exe",
        f"SpaceKnife-{V}-x86_64.AppImage",
        f"spaceknife_{V}_amd64.deb",
        f"spaceknife-{V}.x86_64.rpm",
    }
    for name, digest in merged.items():
        assert release.sha256(tmp_path / "out" / name) == digest == done.hashes[name]


def test_a_wrong_checksum_a_missing_file_and_a_stray_file_are_refused(tmp_path: Path) -> None:
    a = artifacts(tmp_path / "a")
    (a / "SpaceKnife-portable-win-x64" / f"SpaceKnife-{V}-win-x64-portable.zip").write_bytes(b"changed")
    (a / "SpaceKnife-mac-x64" / f"SpaceKnife-{V}-mac-x64.dmg").unlink()
    (a / "SpaceKnife-mac-x64" / "SHA256SUMS-mac-x64.txt").write_text(
        (a / "SpaceKnife-mac-x64" / "SHA256SUMS-mac-x64.txt").read_text(encoding="utf-8").splitlines()[1]
        + "\n",
        encoding="utf-8",
    )
    _write(a / "SpaceKnife-portable-win-x64", "debug.log", b"x")
    with pytest.raises(release.ReleaseError) as err:
        release.assemble(V, a, tmp_path / "out")
    text = str(err.value)
    assert f"SpaceKnife-{V}-win-x64-portable.zip: SHA-256" in text
    assert f"missing: SpaceKnife-{V}-mac-x64.dmg" in text
    assert "not part of a release: debug.log" in text
    assert not (tmp_path / "out").exists()


def test_a_file_of_another_version_is_not_taken(tmp_path: Path) -> None:
    a = artifacts(tmp_path / "a", "0.3.3")
    with pytest.raises(release.ReleaseError) as err:
        release.assemble(V, a, tmp_path / "out")
    assert f"missing: SpaceKnife-{V}-Setup-x64.exe" in str(err.value)


def test_a_prerelease_deb_and_rpm_carry_a_tilde(tmp_path: Path) -> None:
    v = "0.4.0-beta.1"
    release.assemble(v, artifacts(tmp_path / "a", v), tmp_path / "out")
    assert (tmp_path / "out" / "spaceknife_0.4.0~beta.1_amd64.deb").is_file()


def test_the_notes_get_sizes_version_and_runs(tmp_path: Path) -> None:
    done = release.assemble(V, artifacts(tmp_path / "a"), tmp_path / "out")
    (tmp_path / "out" / f"SpaceKnife-{V}-Setup-x64.exe").write_bytes(b"x" * (3 << 20) + b"x" * 600_000)
    notes = release.fill_notes(
        "## SpaceKnife {{version}}\n{{build_runs}}\n| `x` | {{ size:SpaceKnife-0.3.4-Setup-x64.exe }} |\n",
        V,
        done.files,
        ["https://github.com/DuskXi/SpaceKnife/actions/runs/123"],
    )
    assert "## SpaceKnife 0.3.4" in notes
    assert "[run 123](https://github.com/DuskXi/SpaceKnife/actions/runs/123)" in notes
    assert "| 4 MB |" in notes  # 3.57 MiB rounds to 4, written MB as in every release


def test_bad_placeholders_are_refused(tmp_path: Path) -> None:
    done = release.assemble(V, artifacts(tmp_path / "a"), tmp_path / "out")
    with pytest.raises(release.ReleaseError) as err:
        release.fill_notes("0.3.4 {{size:nope.exe}} {{colour}} {{build_runs}}", V, done.files, [])
    text = str(err.value)
    assert "nope.exe" in text
    assert "{{colour}}" in text
    assert "--build-run" in text


def test_the_command_line(tmp_path: Path) -> None:
    a = artifacts(tmp_path / "a")
    notes = tmp_path / "n.md"
    notes.write_text("SpaceKnife {{version}}", encoding="utf-8")
    assert (
        release.main(
            [
                "assemble",
                V,
                str(a),
                str(tmp_path / "out"),
                "--notes",
                str(notes),
                "--notes-out",
                str(tmp_path / "filled.md"),
            ]
        )
        == 0
    )
    assert (tmp_path / "filled.md").read_text(encoding="utf-8") == "SpaceKnife 0.3.4"
    assert (
        release.main(
            [
                "assemble",
                V,
                str(a),
                str(tmp_path / "out"),
                "--notes",
                str(notes),
                "--notes-out",
                str(tmp_path / "filled.md"),
            ]
        )
        == 1
    )  # out is not empty now
