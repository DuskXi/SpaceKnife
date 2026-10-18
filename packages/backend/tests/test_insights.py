"""Insights rules library (docs/insights.md; contract: docs/manage.md section 5).

Everything runs on path strings and in-memory TOML, except the user-directory test, which writes a
few small rule files under ``tmp_path``.
"""

from __future__ import annotations

import sys
import time
import tomllib
from collections.abc import Iterator
from pathlib import Path

import pytest
from loguru import logger

from spaceknife import insights
from spaceknife.insights import KINDS, Insight, RuleSet, compile_rules, load_rules
from spaceknife.insights import rules as rules_mod
from spaceknife.insights.rules import builtin_text, parse_size
from spaceknife.store.nodes import Kind

from .store_builder import build, find

DOCS = Path(__file__).resolve().parents[3] / "docs" / "insights.md"
MB = 1 << 20


def _ruleset(host: str, *extra: str) -> RuleSet:
    sources = [("builtin.toml", builtin_text())]
    sources += [(f"user{i}.toml", text) for i, text in enumerate(extra)]
    return compile_rules(sources, host=host)


@pytest.fixture(scope="module")
def win() -> RuleSet:
    return _ruleset("windows")


@pytest.fixture(scope="module")
def lin() -> RuleSet:
    return _ruleset("linux")


@pytest.fixture(scope="module")
def mac() -> RuleSet:
    return _ruleset("macos")


@pytest.fixture
def warnings() -> Iterator[list[str]]:
    seen: list[str] = []
    token = logger.add(lambda m: seen.append(m.record["message"]), level="WARNING", format="{message}")
    yield seen
    logger.remove(token)


@pytest.fixture
def keep_active() -> Iterator[None]:
    saved = rules_mod._active
    yield
    rules_mod._active = saved


def _rule(rid: str, body: str) -> str:
    return f'[[rule]]\nid = "{rid}"\nkind = "cache"\ntitle = "T {rid}"\nreason = "R {rid}"\n{body}\n'


# --- the built-in library -----------------------------------------------------------------------


def test_builtin_rules_load_cleanly(win: RuleSet) -> None:
    assert win.problems == ()
    assert len(win) >= 40
    assert win.unanchored == 0  # every built-in pattern is indexed: nothing is tested on every lookup
    assert {r.kind for r in win.rules} == set(KINDS)
    raw = tomllib.loads(builtin_text())["rule"]
    ids = [r["id"] for r in raw]
    assert len(ids) == len(set(ids)) == len(win)


def test_builtin_rules_are_translated_and_explained(win: RuleSet) -> None:
    for rule in win.rules:
        en = rule.i18n.get("en", {})
        assert en.get("title") and en.get("reason"), f"{rule.id}: no English title / reason"
        assert len(rule.text.reason) >= 20, rule.id
        assert rule.text.note, rule.id
        if rule.text.app and any("\u4e00" <= ch <= "\u9fff" for ch in rule.text.app):
            assert en.get("app"), f"{rule.id}: Chinese app name without an English one"


def test_builtin_rules_are_documented() -> None:
    text = DOCS.read_text(encoding="utf-8")
    missing = [r["id"] for r in tomllib.loads(builtin_text())["rule"] if f"`{r['id']}`" not in text]
    assert missing == []


# (host, path, node kind, expected rule id or None)
CASES: list[tuple[str, str, str, str | None]] = [
    # vm
    ("windows", r"C:\Users\lenny\AppData\Local\Docker", "dir", "docker-desktop"),
    ("windows", r"C:\Users\lenny\AppData\Local\Docker\wsl\disk\docker_data.vhdx", "file", "docker-desktop"),
    ("windows", r"C:\Users\lenny\AppData\Local\Docker\wsl\main\ext4.vhdx", "file", "docker-desktop"),
    ("windows", r"D:\WSL\Ubuntu\ext4.vhdx", "file", "wsl-distro-disk"),
    ("windows", r"E:\VMs\win11.vhdx", "file", "virtual-disk-file"),
    ("windows", r"C:\Users\Public\Documents\Hyper-V\Virtual Hard Disks\a.vhdx", "file", "hyperv-vms"),
    ("linux", "/home/a/VirtualBox VMs/ubuntu/ubuntu.vdi", "file", "virtualbox-vms"),
    ("linux", "/var/lib/docker/overlay2/x/diff/usr/lib/node_modules", "dir", "container-engine"),
    (
        "macos",
        "/Users/a/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
        "file",
        "docker-desktop-mac",
    ),
    ("macos", "/Users/a/Parallels/Windows 11.pvm", "dir", "parallels-vms"),
    ("linux", "/home/a/.android/avd/Pixel_8.avd", "dir", "android-avd"),
    # build
    ("windows", r"C:\src\app\node_modules", "dir", "node-modules"),
    ("linux", "/home/a/src/app/.venv", "dir", "python-venv"),
    ("linux", "/home/a/miniconda3/envs/torch", "dir", "conda-env"),
    ("macos", "/Users/a/Library/Developer/Xcode/DerivedData", "dir", "xcode-derived-data"),
    ("linux", "/home/a/src/crate/target", "dir", "cargo-target"),
    ("windows", r"C:\src\cpp\build", "dir", "build-output"),
    ("windows", r"C:\src\cpp\cmake-build-debug", "dir", "build-output"),
    ("windows", r"C:\src\repo\.git\objects\pack", "dir", "git-objects"),
    ("windows", r"C:\src\web\.next", "dir", "js-framework-build"),
    ("windows", r"C:\src\sln\.vs", "dir", "visual-studio-cache"),
    ("linux", "/home/a/game/Library/PackageCache", "dir", "unity-project-cache"),
    # cache
    ("windows", r"C:\Users\a\AppData\Local\pip\cache", "dir", "pip-cache"),
    ("linux", "/home/a/.cache/pip", "dir", "pip-cache"),
    ("macos", "/Users/a/Library/Caches/pip", "dir", "pip-cache"),
    ("windows", r"C:\Users\a\AppData\Local\uv\cache", "dir", "uv-cache"),
    ("windows", r"C:\Users\a\AppData\Local\npm-cache", "dir", "npm-cache"),
    ("linux", "/home/a/.local/share/pnpm/store/v3", "dir", "pnpm-store"),
    ("linux", "/home/a/.cargo/registry", "dir", "cargo-registry"),
    ("linux", "/home/a/go/pkg/mod", "dir", "go-cache"),
    ("windows", r"C:\Users\a\.m2\repository", "dir", "maven-repo"),
    ("linux", "/home/a/.gradle/caches", "dir", "gradle-cache"),
    ("windows", r"C:\Users\a\.nuget\packages", "dir", "nuget-cache"),
    ("windows", r"C:\Users\a\AppData\Local\JetBrains\PyCharm2024.3", "dir", "jetbrains-cache"),
    ("windows", r"C:\Users\a\AppData\Roaming\Code\CachedExtensionVSIXs", "dir", "vscode-cache"),
    ("windows", r"C:\Users\a\AppData\Local\Google\Chrome\User Data\Default\Cache", "dir", "chrome-cache"),
    (
        "windows",
        r"C:\Users\a\AppData\Local\Microsoft\Edge\User Data\Profile 1\Code Cache",
        "dir",
        "edge-cache",
    ),
    (
        "windows",
        r"C:\Users\a\AppData\Local\Mozilla\Firefox\Profiles\x.default\cache2",
        "dir",
        "firefox-cache",
    ),
    ("linux", "/home/a/.cache/google-chrome", "dir", "chrome-cache"),
    ("linux", "/home/a/.cache/huggingface/hub", "dir", "huggingface-cache"),
    ("linux", "/var/cache/apt/archives", "dir", "apt-cache"),
    ("linux", "/var/cache/dnf", "dir", "dnf-cache"),
    ("windows", r"C:\Users\a\AppData\Local\Temp", "dir", "windows-temp"),
    ("windows", r"D:\SteamLibrary\steamapps\shadercache", "dir", "steam-shadercache"),
    # chat
    ("windows", r"C:\Users\a\Documents\WeChat Files", "dir", "wechat-files"),
    ("windows", r"D:\Data\WeChat Files\wxid_7k2m9\FileStorage\Video", "dir", "wechat-files"),
    ("windows", r"C:\Users\a\Documents\WeChat Files\wxid_7k2m9\FileStorage\Cache", "dir", "wechat-cache"),
    ("windows", r"C:\Users\a\Documents\Tencent Files", "dir", "qq-files"),
    ("linux", "/home/a/.local/share/TelegramDesktop/tdata", "dir", "telegram-data"),
    ("macos", "/Users/a/Library/Containers/com.tencent.xinWeChat", "dir", "wechat-files"),
    # game
    ("windows", r"D:\SteamLibrary\steamapps\common\Cyberpunk 2077", "dir", "steam-game"),
    ("linux", "/home/a/.local/share/Steam/steamapps/common/Hades", "dir", "steam-game"),
    ("windows", r"C:\Program Files\Epic Games\Fortnite", "dir", "epic-game"),
    ("windows", r"C:\Program Files\Epic Games\UE_5.4", "dir", "unreal-engine"),
    ("windows", r"C:\XboxGames\Halo Infinite", "dir", "xbox-game"),
    ("windows", r"C:\Program Files (x86)\GOG Galaxy\Games\Witcher 3", "dir", "gog-game"),
    # system
    ("windows", r"C:\hiberfil.sys", "file", "hiberfil"),
    ("windows", r"D:\pagefile.sys", "file", "pagefile"),
    ("windows", r"C:\swapfile.sys", "file", "pagefile"),
    ("windows", r"C:\Windows.old", "dir", "windows-old"),
    ("windows", r"C:\Windows\WinSxS", "dir", "winsxs"),
    ("windows", r"C:\Windows\SoftwareDistribution\Download", "dir", "windows-update-download"),
    ("windows", r"C:\Windows\SoftwareDistribution\DataStore", "dir", "software-distribution"),
    ("windows", r"C:\$Recycle.Bin", "dir", "recycle-bin"),
    ("linux", "/swapfile", "file", "linux-swapfile"),
    ("linux", "/var/log/journal", "dir", "journal-logs"),
    ("macos", "/private/var/vm/sleepimage", "file", "macos-swap"),
    # app
    ("windows", r"C:\Users\a\.ollama\models", "dir", "ollama-models"),
    ("linux", "/usr/share/ollama/.ollama/models/blobs", "dir", "ollama-models"),
    ("macos", "/Users/a/Library/Application Support/MobileSync/Backup", "dir", "ios-backups"),
    ("windows", r"C:\Users\a\AppData\Local\Android\Sdk", "dir", "android-sdk"),
    ("linux", "/home/a/.vscode-server/extensions/ms-python.python/node_modules", "dir", "vscode-extensions"),
    (
        "windows",
        r"C:\Users\a\AppData\Local\Google\Chrome\User Data\Default\Bookmarks",
        "file",
        "browser-profile",
    ),
    ("macos", "/Users/a/Pictures/Photos Library.photoslibrary", "dir", "photos-library"),
    # nothing known
    ("windows", r"C:\Users\lenny\Music", "dir", None),
    ("windows", r"C:\Users\lenny\Code\Cache", "dir", None),  # a projects folder named Code is not VS Code
    ("linux", "/home/a/Documents/report.pdf", "file", None),
    ("windows", r"C:\Program Files\nodejs\node_modules\npm", "dir", None),  # bundled with an app
    ("windows", r"C:\Program Files\Epic Games\Launcher", "dir", None),
    ("linux", "/usr/lib/modules/6.8.0/build", "dir", None),
    ("windows", r"C:\Python312\Lib\venv", "dir", None),  # the standard library's venv package
    ("windows", r"C:\Users\a\AppData\Roaming\uv\python\cpython-3.12\Lib\venv", "dir", "uv-python"),
    ("linux", "/home/a/.npm/_npx/1f2e/node_modules", "dir", "npm-cache"),
    ("windows", r"C:\src\app\node_modules", "file", None),  # node kind filter
]


@pytest.mark.parametrize(("host", "path", "kind", "expected"), CASES)
def test_known_locations(
    host: str, path: str, kind: str, expected: str | None, request: pytest.FixtureRequest
) -> None:
    rule_set: RuleSet = request.getfixturevalue({"windows": "win", "linux": "lin", "macos": "mac"}[host])
    found = rule_set.match(path, kind)
    assert (found.rule if found else None) == expected
    if found is not None:
        assert found.kind == rule_set.get(found.rule).kind  # type: ignore[union-attr]
        assert found.title and found.reason and found.note


def test_cases_cover_every_kind(win: RuleSet) -> None:
    covered = {win.get(rid).kind for _, _, _, rid in CASES if rid}  # type: ignore[union-attr]
    assert covered == set(KINDS)


# --- path forms and platforms -------------------------------------------------------------------


@pytest.mark.parametrize(
    "path",
    [
        r"C:\Users\a\AppData\Local\Docker",
        "C:/Users/a/AppData/Local/Docker",
        r"c:\users\A\appdata\LOCAL\docker",
        "C:\\Users\\a\\AppData\\Local\\Docker\\",
        r"\\?\C:\Users\a\AppData\Local\Docker",
    ],
)
def test_windows_path_forms(win: RuleSet, lin: RuleSet, path: str) -> None:
    for rule_set in (win, lin):  # a Windows path is a Windows path on any host
        found = rule_set.match(path, "dir")
        assert found is not None and found.rule == "docker-desktop"
        assert found.path.lower() == r"c:\users\a\appdata\local\docker"
        assert not found.inside


def test_windows_unc_and_roots(win: RuleSet) -> None:
    found = win.match(r"\\nas\share\proj\node_modules\left-pad\index.js", "file")
    assert found is not None and found.rule == "node-modules"
    assert found.path == r"\\nas\share\proj\node_modules" and found.inside
    found = win.match(r"\\?\UNC\nas\share\proj\node_modules", "dir")
    assert found is not None and found.path == r"\\nas\share\proj\node_modules"
    found = win.match(r"\\?\C:\hiberfil.sys", "file")
    assert found is not None and found.rule == "hiberfil" and found.path == r"C:\hiberfil.sys"
    assert win.match("C:\\", "dir") is None
    assert win.match("", "dir") is None


def test_posix_case_rules(lin: RuleSet, mac: RuleSet) -> None:
    # Linux is case-sensitive: .cache/PIP is just something in the cache folder
    found = lin.match("/home/a/.cache/PIP", "dir")
    assert found is not None and found.rule == "xdg-cache" and found.path == "/home/a/.cache"
    # macOS (APFS default) is not
    found = mac.match("/users/a/library/caches/PIP", "dir")
    assert found is not None and found.rule == "pip-cache"


def test_platform_filters(win: RuleSet, lin: RuleSet, mac: RuleSet) -> None:
    assert lin.match("/home/a/AppData/Local/Docker", "dir") is None  # Windows-only rule
    assert mac.match("/var/cache/apt/archives", "dir") is None  # Linux-only rule
    assert lin.match("/Users/a/Library/Developer/Xcode/DerivedData", "dir") is None  # macOS-only rule
    # a POSIX path seen on Windows (a snapshot from Linux or macOS) gets the POSIX rules
    found = win.match("/var/cache/apt/archives", "dir")
    assert found is not None and found.rule == "apt-cache"
    found = win.match("/Users/a/Library/Developer/Xcode/DerivedData", "dir")
    assert found is not None and found.rule == "xcode-derived-data"


def test_node_kind_forms(win: RuleSet) -> None:
    for kind in ("dir", "DIR", Kind.DIR):
        assert win.match(r"C:\src\node_modules", kind) is not None
    assert win.match(r"C:\src\node_modules", Kind.FILE) is None
    with pytest.raises(ValueError, match="unknown node kind"):
        win.match(r"C:\src\node_modules", "folder")


# --- ranking: subtree, priority, depth, outermost ------------------------------------------------


def test_inside_a_matched_folder(win: RuleSet) -> None:
    found = win.match(r"C:\src\app\node_modules\react\cjs\react.js", "file")
    assert found is not None and found.rule == "node-modules" and found.inside
    assert found.path == r"C:\src\app\node_modules"


def test_outermost_match_of_the_same_rule(win: RuleSet) -> None:
    found = win.match(r"C:\src\app\node_modules\a\node_modules\b\node_modules", "dir")
    assert found is not None and found.rule == "node-modules"
    assert found.path == r"C:\src\app\node_modules" and found.inside


def test_owner_folders_win_over_what_is_inside(win: RuleSet, lin: RuleSet) -> None:
    found = win.match(r"D:\SteamLibrary\steamapps\common\Game\resources\app\node_modules", "dir")
    assert found is not None and found.rule == "steam-game"
    found = win.match(r"C:\src\x\.venv\Lib\site-packages\pkg\build", "dir")
    assert found is not None and found.rule == "python-venv"
    found = lin.match("/home/a/.cache/pip/http/a/b", "dir")
    assert found is not None and found.rule == "pip-cache"  # not the broad XDG cache rule


def test_deeper_match_wins_at_equal_priority(win: RuleSet) -> None:
    found = win.match(r"C:\src\repo\.git\objects", "dir")
    assert found is not None and found.rule == "git-objects" and not found.inside
    found = win.match(r"C:\src\repo\.git\config", "file")
    assert found is not None and found.rule == "git-repo" and found.inside
    found = win.match(r"C:\Windows\SoftwareDistribution\Download\x\y.cab", "file")
    assert found is not None and found.rule == "windows-update-download"


def test_specificity_breaks_ties() -> None:
    rule_set = compile_rules(
        [("t", _rule("broad", 'paths = ["*/b"]') + _rule("narrow", 'paths = ["a/b"]'))], host="linux"
    )
    found = rule_set.match("/x/a/b", "dir")
    assert found is not None and found.rule == "narrow"
    found = rule_set.match("/x/z/b", "dir")
    assert found is not None and found.rule == "broad"


def test_subtree_false_and_min_size() -> None:
    text = _rule("only-self", 'names = ["exact"]\nsubtree = false') + _rule(
        "big", 'names = ["huge"]\nmin_size = "1 MB"'
    )
    rule_set = compile_rules([("t", text)], host="linux")
    assert rule_set.match("/a/exact", "dir") is not None
    assert rule_set.match("/a/exact/child", "dir") is None
    assert rule_set.match("/a/huge", "dir", size=10) is None
    assert rule_set.match("/a/huge", "dir", size=2 * MB) is not None
    assert rule_set.match("/a/huge", "dir") is not None  # size unknown: the hint does not apply
    assert rule_set.match("/a/huge/x", "file", size=1) is not None  # the hint is for the item itself


def test_parents_and_exclude() -> None:
    text = _rule("objects", 'names = ["objects"]\nparents = [".git", ".hg"]') + _rule(
        "logs", 'names = ["logs"]\nexclude = ["/var/**"]'
    )
    rule_set = compile_rules([("t", text)], host="linux")
    assert rule_set.match("/r/.git/objects", "dir") is not None
    assert rule_set.match("/r/.hg/objects", "dir") is not None
    assert rule_set.match("/r/src/objects", "dir") is None
    assert rule_set.match("/objects", "dir") is None  # no parent folder: the condition fails
    assert rule_set.match("/home/a/logs", "dir") is not None
    assert rule_set.match("/var/log/app/logs", "dir") is None


# --- glob syntax ----------------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("glob", "hit", "miss"),
    [
        ("a/{b,c/d}", "/x/a/c/d", "/x/a/c"),
        ("a/{b,{c,e}}x", "/a/ex", "/a/dx"),
        ("data[0-9]", "/data7", "/datax"),
        ("v[!0-9]", "/vx", "/v1"),
        ("[{]a}", "/{a}", "/a"),
        ("?:/Windows.old", "C:\\Windows.old", "C:\\x\\Windows.old"),
        ("/opt/*/cache", "/opt/app/cache", "/opt/a/b/cache"),
        ("/opt/**/cache", "/opt/a/b/cache", "/srv/a/cache"),
        ("file?.bin", "/q/file1.bin", "/q/file12.bin"),
        ("*.tar.gz", "/q/a.tar.gz", "/q/a.gz"),
        ("pre*", "/q/prefix", "/q/pr"),
        (r"win\style\path", "/q/win/style/path", "/q/win/style"),
    ],
)
def test_glob_syntax(glob: str, hit: str, miss: str) -> None:
    rule_set = compile_rules([("t", _rule("g", f"paths = [{glob!r}]"))], host="linux")
    found = rule_set.match(hit, "dir")
    assert found is not None and found.rule == "g"
    assert rule_set.match(miss, "dir") is None


def test_parse_size() -> None:
    assert parse_size(123) == 123
    assert parse_size("500 MB") == 500 * MB
    assert parse_size("2GiB") == 2 << 30
    assert parse_size("1.5k") == 1536
    assert parse_size(" 10 b ") == 10


# --- user rules, overrides and malformed rules ---------------------------------------------------


def test_user_rules_override_and_disable(tmp_path: Path, keep_active: None) -> None:
    (tmp_path / "10-mine.toml").write_text(
        '[[rule]]\nid = "node-modules"\nkind = "build"\ntitle = "我的依赖"\nreason = "自定义说明"\n'
        'names = ["node_modules", "bower_components"]\n'
        + _rule("my-renders", 'kind = "app"\npaths = ["Videos/Renders"]').replace('kind = "cache"\n', ""),
        encoding="utf-8",
    )
    (tmp_path / "20-off.toml").write_text('[[rule]]\nid = "pycache"\nenabled = false\n', encoding="utf-8")
    (tmp_path / "notes.txt").write_text("not a rule file", encoding="utf-8")
    rule_set = load_rules(tmp_path, host="linux")
    assert rule_set.problems == ()
    assert insights.active_rules() is rule_set
    found = insights.match("/home/a/web/bower_components", "dir")
    assert found is not None and found.rule == "node-modules" and found.title == "我的依赖"
    assert rule_set.get("node-modules").source.endswith("10-mine.toml")  # type: ignore[union-attr]
    assert rule_set.get("pycache") is None
    assert insights.match("/home/a/proj/__pycache__", "dir") is None
    found = insights.match("/home/a/Videos/Renders/final.mov", "file")
    assert found is not None and found.rule == "my-renders" and found.kind == "app"
    assert len(rule_set) == len(_ruleset("linux"))  # one rule added, one disabled


def test_user_rule_wins_ties_with_builtin() -> None:
    clone = _rule("my-node-modules", 'kind = "build"\nnames = ["node_modules"]\npriority = 1').replace(
        'kind = "cache"\n', ""
    )
    found = _ruleset("linux", clone).match("/p/node_modules", "dir")
    assert found is not None and found.rule == "my-node-modules"


def test_malformed_rules_are_reported_and_skipped(warnings: list[str]) -> None:
    bad = "\n".join(
        [
            _rule("good", 'names = ["good"]'),
            '[[rule]]\nid = "no-reason"\nkind = "cache"\ntitle = "x"\nnames = ["a"]\n',
            _rule("bad-kind", 'names = ["a"]').replace('kind = "cache"', 'kind = "music"'),
            _rule("typo", 'names = ["a"]\nplatform = "linux"'),
            _rule("bad-glob", 'paths = ["foo[ab"]'),
            _rule("bad-brace", 'paths = ["foo{a,b"]'),
            _rule("tilde", 'paths = ["~/.cache/x"]'),
            _rule("sep-in-name", 'names = ["a/b"]'),
            _rule("bad-platform", 'names = ["a"]\nplatforms = ["beos"]'),
            _rule("bad-size", 'names = ["a"]\nmin_size = "lots"'),
            _rule("bad-priority", 'names = ["a"]\npriority = true'),
            _rule("no-matcher", ""),
            _rule("bad-lang", 'names = ["a"]\ni18n.english.title = "x"'),
            _rule("Bad Id!", 'names = ["a"]'),
            _rule("good", 'names = ["good2"]'),  # duplicate id: reported, the later one wins
            "[[rule]]\nkind = 1\n",
        ]
    )
    rule_set = compile_rules(
        [("bad.toml", bad), ("broken.toml", "[[rule]\nid = 1"), ("future.toml", "version = 2\n")],
        host="linux",
    )
    assert [r.id for r in rule_set.rules] == ["good"]
    assert rule_set.match("/x/good2", "dir") is not None and rule_set.match("/x/good", "dir") is None
    labels = [p.rule for p in rule_set.problems]
    for rid in ("no-reason", "bad-kind", "typo", "bad-glob", "bad-brace", "tilde", "sep-in-name"):
        assert rid in labels
    for rid in (
        "bad-platform",
        "bad-size",
        "bad-priority",
        "no-matcher",
        "bad-lang",
        "Bad Id!",
        "good",
        "#16",
    ):
        assert rid in labels
    assert {p.source for p in rule_set.problems} == {"bad.toml", "broken.toml", "future.toml"}
    assert len(warnings) == len(rule_set.problems) == 17
    assert any("未知字段 platform" in w for w in warnings)  # the unknown field is named


def test_unreadable_user_files_are_reported(tmp_path: Path, warnings: list[str], keep_active: None) -> None:
    (tmp_path / "latin1.toml").write_bytes(b'[[rule]]\nid = "x"\ntitle = "\xe9"\n')
    rule_set = load_rules(tmp_path, host="linux")
    assert len(rule_set.problems) == 1 and rule_set.problems[0].source.endswith("latin1.toml")
    assert len(rule_set) == len(_ruleset("linux"))
    assert load_rules(tmp_path / "missing", host="linux").problems == ()


# --- languages ----------------------------------------------------------------------------------


def test_languages(win: RuleSet) -> None:
    path = r"D:\SteamLibrary\steamapps\common\Cyberpunk 2077"
    zh = win.match(path, "dir")
    assert zh is not None and zh.title == "Steam 游戏" and zh.note == "游戏更新" and zh.app == "Steam 游戏"
    for lang in ("en", "en-US", "EN_gb"):
        en = win.match(path, "dir", lang=lang)
        assert en is not None and (en.title, en.note, en.app) == ("Steam game", "Game update", "Steam game")
    for lang in ("zh-CN", "fr", "", None):
        assert win.match(path, "dir", lang=lang) == zh


def test_partial_translation_falls_back_per_field() -> None:
    text = _rule("t", 'names = ["t"]\nnote = "短注"\ni18n.en.title = "Title"\ni18n.zh-tw.reason = "說明"')
    rule_set = compile_rules([("t", text)], host="linux")
    en = rule_set.match("/t", "dir", lang="en")
    assert en is not None and (en.title, en.reason, en.note) == ("Title", "R t", "Title")
    tw = rule_set.match("/t", "dir", lang="zh-TW")
    assert tw is not None and (tw.title, tw.reason, tw.note) == ("T t", "說明", "短注")


# --- lists, store nodes and the module-level functions --------------------------------------------


def test_match_many_equals_match(win: RuleSet) -> None:
    paths = [p for h, p, _, _ in CASES if h == "windows"]
    kinds = [k for h, _, k, _ in CASES if h == "windows"]
    assert win.match_many(paths, kinds) == [win.match(p, k) for p, k in zip(paths, kinds, strict=True)]
    siblings = [rf"C:\src\app\node_modules\pkg{i}" for i in range(50)] + [r"C:\src\app\src"]
    found = win.match_many(siblings)
    assert all(f is not None and f.path == r"C:\src\app\node_modules" for f in found[:50])
    assert found[50] is None
    sized = win.match_many([r"C:\a\node_modules"] * 2, "dir", sizes=[None, 5])
    assert sized[0] == sized[1] is not None
    with pytest.raises(ValueError, match="length"):
        win.match_many(["a", "b"], ["dir"])
    with pytest.raises(ValueError, match="length"):
        win.match_many(["a"], sizes=[1, 2])


def test_match_node() -> None:
    root = "C:\\work" if sys.platform == "win32" else "/work"
    store = build({"proj": {"node_modules": {"react": {"index.js": 100}}, "src": {"main.py": 10}}}, root=root)
    rule_set = _ruleset(rules_mod.host_platform())
    found = rule_set.match_node(store, find(store, "proj/node_modules"))
    assert found is not None and found.rule == "node-modules" and not found.inside
    found = rule_set.match_node(store, find(store, "proj/node_modules/react/index.js"))
    assert found is not None and found.inside and found.path.endswith("node_modules")
    assert rule_set.match_node(store, find(store, "proj/src/main.py")) is None


def test_module_functions_load_builtins_lazily(keep_active: None) -> None:
    rules_mod._active = None
    found = insights.match("/home/a/proj/node_modules", "dir")
    assert isinstance(found, Insight) and found.rule == "node-modules"
    many = insights.match_many(["/x/node_modules", "/x/src"])
    assert many[0] is not None and many[0].rule == "node-modules" and many[1] is None
    assert insights.active_rules().problems == ()
    assert insights.user_rules_dir().name == "insights"


# --- performance -----------------------------------------------------------------------------------


def _synthetic_paths(n: int) -> list[str]:
    shapes = [
        r"C:\Users\lenny\Code{i}\src\module{m}\file{i}.py",
        r"C:\Users\lenny\Code{i}\node_modules\pkg{i}\dist\index.js",
        r"D:\SteamLibrary\steamapps\common\Game{m}\data\pak{i}.pak",
        r"C:\Users\lenny\AppData\Local\Google\Chrome\User Data\Default\Cache\Cache_Data\f_{i}",
        r"C:\Windows\System32\DriverStore\FileRepository\drv{i}.inf_amd64\file{i}.sys",
        "/home/lenny/src/proj{i}/.venv/lib/python3.12/site-packages/pkg{m}/mod{i}.py",
        "/home/lenny/.cache/pip/http/{m}/{i}",
        "/var/lib/docker/overlay2/{i}/diff/usr/share/doc/pkg{m}/README",
    ]
    return [shapes[i % len(shapes)].format(i=i, m=i % 13) for i in range(n)]


def test_lookup_speed(win: RuleSet) -> None:
    paths = _synthetic_paths(10_000)
    assert sum(win.match(p, "file") is not None for p in paths) > 5000
    best = best_many = float("inf")
    for _ in range(3):
        t0 = time.perf_counter()
        for p in paths:
            win.match(p, "file")
        best = min(best, time.perf_counter() - t0)
        t0 = time.perf_counter()
        win.match_many(paths, "file")
        best_many = min(best_many, time.perf_counter() - t0)
    assert best < 0.2, f"10k lookups took {best:.3f} s"
    assert best_many < 0.2, f"match_many over 10k paths took {best_many:.3f} s"
