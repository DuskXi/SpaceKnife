"""ops.paths: the four copy formats (UI-W5-10..13) and the protected-path lists of every platform."""

from __future__ import annotations

import os
import shlex
import sys
from pathlib import Path

import pytest

from spaceknife.ops import paths as P
from spaceknife.ops.paths import TEXT, match_rules, path_forms, platform_rules, protected_reason

# --------------------------------------------------------------------------- path forms


def test_windows_forms_spaces_and_unicode() -> None:
    f = path_forms(r"C:\Users\lenny\Videos\2026-08 青海自驾.mp4", system="windows")
    assert f.raw == r"C:\Users\lenny\Videos\2026-08 青海自驾.mp4"
    assert f.quoted == r'"C:\Users\lenny\Videos\2026-08 青海自驾.mp4"'
    assert f.posix == "/mnt/c/Users/lenny/Videos/2026-08 青海自驾.mp4"  # WSL form (UI-W5-12)
    # UI-W5-13's example, byte for byte
    assert f.uri == "file:///C:/Users/lenny/Videos/2026-08%20%E9%9D%92%E6%B5%B7%E8%87%AA%E9%A9%BE.mp4"


def test_windows_forms_plain_and_reserved() -> None:
    f = path_forms(r"C:\Users\lenny\Downloads\Win11_25H2_Chinese_Simplified_x64.iso", system="windows")
    assert f.uri == "file:///C:/Users/lenny/Downloads/Win11_25H2_Chinese_Simplified_x64.iso"
    g = path_forms(r"D:\a#b\c%d;e&f.txt", system="windows")
    assert g.uri == "file:///D:/a%23b/c%25d%3Be%26f.txt"
    assert path_forms("C:\\", system="windows").uri == "file:///C:/"


def test_windows_forms_unc() -> None:
    f = path_forms(r"\\nas\share\photo album\x.jpg", system="windows")
    assert f.posix == "//nas/share/photo album/x.jpg"
    assert f.uri == "file://nas/share/photo%20album/x.jpg"
    assert f.quoted == r'"\\nas\share\photo album\x.jpg"'


def test_windows_forms_verbatim_prefix() -> None:
    f = path_forms("\\\\?\\C:\\very\\long", system="windows")
    assert f.raw == "\\\\?\\C:\\very\\long"
    assert f.posix == "/mnt/c/very/long"
    assert f.uri == "file:///C:/very/long"
    g = path_forms("\\\\?\\UNC\\host\\share\\x", system="windows")
    assert g.posix == "//host/share/x"
    assert g.uri == "file://host/share/x"


def test_windows_forms_lone_surrogate_is_encoded() -> None:
    f = path_forms("C:\\bad\udc80name", system="windows")
    assert f.uri == "file:///C:/bad%ED%B2%80name"  # WTF-8, as NTFS names may hold it


def test_posix_forms_quote_and_unicode() -> None:
    path = '/home/u/it\'s here/文件 $HOME `x` "q".txt'
    f = path_forms(path, system="linux")
    assert f.raw == f.posix == path
    assert f.quoted == "'/home/u/it'\\''s here/文件 $HOME `x` \"q\".txt'"
    assert shlex.split(f.quoted) == [path]  # one shell word, nothing expanded
    assert f.uri == ("file:///home/u/it%27s%20here/%E6%96%87%E4%BB%B6%20%24HOME%20%60x%60%20%22q%22.txt")


def test_posix_forms_undecodable_bytes() -> None:
    f = path_forms("/tmp/\udcff\udcfe", system="darwin")
    assert f.uri == "file:///tmp/%FF%FE"  # the original bytes, not U+DCFF


def test_forms_default_to_this_system(tmp_path: Path) -> None:
    path = str(tmp_path / "a b.txt")
    expected = path_forms(path, system="windows" if sys.platform == "win32" else "linux")
    assert path_forms(path) == expected
    assert path_forms(path).uri == (tmp_path / "a b.txt").as_uri()  # agrees with pathlib


# --------------------------------------------------------------------------- Windows rules


WIN_ENV = {
    "SystemDrive": "C:",
    "SystemRoot": r"C:\Windows",
    "ProgramFiles": r"C:\Program Files",
    "ProgramFiles(x86)": r"C:\Program Files (x86)",
    "ProgramData": r"C:\ProgramData",
}


@pytest.fixture
def win() -> P.Rules:
    return platform_rules(
        "windows",
        home=r"C:\Users\lenny",
        env=WIN_ENV,
        app_dirs=[r"C:\Users\lenny\AppData\Local\SpaceKnife"],
        program_dirs=[r"C:\Apps\SpaceKnife"],
    )


@pytest.mark.parametrize(
    ("path", "reason"),
    [
        ("C:\\", "root"),
        ("D:\\", "root"),
        ("C:", "root"),
        (r"\\nas\share", "root"),
        (r"\\nas\share\\", "root"),
        (r"C:\Windows", "system"),
        (r"c:\WINDOWS\System32\drivers\etc", "system"),
        ("\\\\?\\C:\\Windows\\explorer.exe", "system"),
        (r"C:\Windows\Temp", "system"),
        (r"C:\Windows\Temp\setup.log", None),
        (r"C:\Windows\Temp\sub\x.tmp", None),
        (r"C:\Program Files", "program_root"),
        (r"C:\Program Files (x86)", "program_root"),
        (r"C:\Program Files\Some App", None),
        (r"C:\ProgramData", "data_root"),
        (r"C:\ProgramData\Package Cache", None),
        (r"C:\Users", "users_root"),
        (r"C:\Users\lenny", "home"),
        (r"C:\Users\Public", "home"),
        (r"C:\Users\Default", "home"),
        (r"C:\Users\lenny\Downloads", None),
        (r"C:\Users\lenny\AppData", "profile"),
        (r"C:\Users\lenny\AppData\Local", "profile"),
        (r"C:\Users\lenny\AppData\Local\Temp", None),
        (r"C:\Users\lenny\AppData\Local\SpaceKnife", "app"),
        (r"C:\Users\lenny\AppData\Local\SpaceKnife\snapshots\x.sks", "app"),
        (r"C:\Apps\SpaceKnife\python.exe", "program"),
        (r"C:\$Recycle.Bin", "trash"),
        (r"D:\$RECYCLE.BIN\S-1-5-21-1\$RABC.txt", "trash"),
        (r"E:\System Volume Information\tracking.log", "system"),
        (r"C:\pagefile.sys", "system_file"),
        (r"D:\pagefile.sys", "system_file"),
        (r"D:\backup\pagefile.sys", None),
        (r"C:\Recovery\WindowsRE", "boot"),
        (r"D:\Recovery", None),  # only the system drive's
        (r"C:\bootmgr", "boot"),
        (r"D:\Games\Steam", None),
        (r"D:\Users", None),
    ],
)
def test_windows_rules(win: P.Rules, path: str, reason: str | None) -> None:
    assert match_rules(path, win) == reason


# --------------------------------------------------------------------------- Linux rules


@pytest.fixture
def linux() -> P.Rules:
    return platform_rules(
        "linux",
        home="/home/u",
        env={},
        app_dirs=["/home/u/.local/share/SpaceKnife"],
        program_dirs=["/opt/spaceknife"],
    )


@pytest.mark.parametrize(
    ("path", "reason"),
    [
        ("/", "root"),
        ("/usr", "system"),
        ("/usr/lib/x86_64-linux-gnu/libc.so.6", "system"),
        ("/usr/local/bin/tool", "system"),
        ("/etc/passwd", "system"),
        ("/boot/vmlinuz", "system"),
        ("/proc/1/fd", "system"),
        ("/run", "system"),
        ("/run/media", "system"),
        ("/run/media/u/USB/movie.mkv", None),
        ("/run/user/1000/doc", None),
        ("/var", "shared_root"),
        ("/var/log", "shared_root"),
        ("/var/log/journal/abc", None),
        ("/var/cache/dnf", None),
        ("/var/lib/dpkg/status", "package_db"),
        ("/var/lib/rpm", "package_db"),
        ("/var/lib/docker", None),
        ("/tmp", "shared_root"),
        ("/tmp/x", None),
        ("/home", "users_root"),
        ("/home/u", "home"),
        ("/home/other", "home"),
        ("/root", "home"),
        ("/home/u/Downloads/a.iso", None),
        ("/home/u/.cache", None),
        ("/home/u/.config", "profile"),
        ("/home/u/.config/Code/Cache", None),
        ("/home/u/.local/share", "profile"),
        ("/home/u/.local/share/Trash", "trash"),
        ("/home/u/.local/share/Trash/files/old.txt", "trash"),
        ("/mnt/data/.Trash-1000/files/x", "trash"),
        ("/mnt/data/.Trash/1000", "trash"),
        ("/home/u/.local/share/SpaceKnife/snapshots", "app"),
        ("/opt/spaceknife/bin/python", "program"),
        ("/opt", "shared_root"),
        ("/opt/other", None),
        ("/home/u/./Downloads/../.config", "profile"),  # normalised before matching
    ],
)
def test_linux_rules(linux: P.Rules, path: str, reason: str | None) -> None:
    assert match_rules(path, linux) == reason


def test_linux_rules_follow_xdg_data_home() -> None:
    rules = platform_rules("linux", home="/home/u", env={"XDG_DATA_HOME": "/data/xdg"})
    assert match_rules("/data/xdg/Trash/files/a", rules) == "trash"
    assert match_rules("/data/xdg/other", rules) is None
    relative = platform_rules("linux", home="/home/u", env={"XDG_DATA_HOME": "rel/xdg"})
    assert match_rules("/home/u/.local/share/Trash/x", relative) == "trash"  # a relative value is ignored


# --------------------------------------------------------------------------- macOS rules


@pytest.fixture
def mac() -> P.Rules:
    return platform_rules(
        "darwin", home="/Users/u", env={}, app_dirs=["/Users/u/Library/Application Support/SpaceKnife"]
    )


@pytest.mark.parametrize(
    ("path", "reason"),
    [
        ("/", "root"),
        ("/System", "system"),
        ("/System/Library/CoreServices", "system"),
        ("/usr/bin/true", "system"),
        ("/private/var/vm/swapfile0", "system"),
        ("/private/var/db/receipts", "system"),
        ("/private/var/folders/xy/cache", None),
        ("/Applications", "program_root"),
        ("/Applications/Foo.app", None),
        ("/Library", "shared_root"),
        ("/Library/Caches/com.x", None),
        ("/Volumes", "shared_root"),
        ("/Volumes/USB/movie.mov", None),
        ("/Users", "users_root"),
        ("/Users/u", "home"),
        ("/Users/Shared", "home"),
        ("/Users/u/Library", "profile"),
        ("/Users/u/Library/Caches/com.apple.Safari", None),
        ("/Users/u/Library/Mobile Documents", "profile"),
        ("/Users/u/.Trash", "trash"),
        ("/Users/u/.Trash/old.dmg", "trash"),
        ("/Volumes/USB/.Trashes/501/x", "trash"),
        ("/Users/u/Library/Application Support/SpaceKnife/x", "app"),
        ("/Users/u/Movies/a.mov", None),
    ],
)
def test_mac_rules(mac: P.Rules, path: str, reason: str | None) -> None:
    assert match_rules(path, mac) == reason


def test_every_reason_has_text(win: P.Rules, linux: P.Rules, mac: P.Rules) -> None:
    for rules in (win, linux, mac):
        reasons = {
            *rules.exact.values(),
            *rules.tree.values(),
            *rules.children.values(),
            *rules.root_names.values(),
            *rules.root_trees.values(),
            *(r for _, r in rules.names),
        }
        assert reasons <= set(TEXT)
    assert {"root", "mount"} <= set(TEXT)


# --------------------------------------------------------------------------- this system


def test_protected_reason_on_this_system(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "skhome"))
    plain = tmp_path / "plain.txt"
    plain.write_bytes(b"x")
    assert protected_reason(str(plain)) is None
    assert protected_reason(str(tmp_path / "not there")) is None
    assert protected_reason(os.path.expanduser("~")) == TEXT["home"]
    anchor = Path(tmp_path).anchor
    assert protected_reason(anchor) == TEXT["root"]
    data = tmp_path / "skhome" / "data"
    assert protected_reason(str(data / "snapshots" / "c.sks")) == TEXT["app"]
    assert protected_reason(sys.prefix) == TEXT["program"]


def test_protected_reason_sees_through_links(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """A path whose parent folders lead into a protected place is protected (links are resolved)."""
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "skhome"))
    data = tmp_path / "skhome" / "data"
    data.mkdir(parents=True)
    (data / "snap.sks").write_bytes(b"x")
    link = tmp_path / "way-in"
    if sys.platform == "win32":
        import _winapi

        _winapi.CreateJunction(str(data), str(link))  # a junction needs no privilege
    else:
        link.symlink_to(data, target_is_directory=True)
    try:
        assert protected_reason(str(link / "snap.sks")) == TEXT["app"]
        assert protected_reason(str(link)) is None  # the link itself may go: it is only a name
    finally:
        os.unlink(link) if sys.platform != "win32" else os.rmdir(link)
    assert (data / "snap.sks").exists()


def test_protected_reason_mount_points(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    mounted = str(tmp_path / "mnt")
    (tmp_path / "mnt").mkdir()
    (tmp_path / "file").write_bytes(b"")
    real = os.path.ismount
    monkeypatch.setattr(
        P.os.path, "ismount", lambda p: os.path.normcase(str(p)) == os.path.normcase(mounted) or real(p)
    )
    assert protected_reason(mounted) == TEXT["mount"]
    assert protected_reason(str(tmp_path / "mnt" / "file")) is None
    monkeypatch.setattr(P.os.path, "ismount", lambda p: True)
    assert protected_reason(str(tmp_path / "file")) is None  # only folders are asked (files cannot be)
