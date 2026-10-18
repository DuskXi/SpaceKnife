"""``spaceknife run``: the SpaceSniffer command line parser and headless command sequences."""

from __future__ import annotations

import csv
import io
import json
import os
from pathlib import Path

import pytest
from rich.console import Console

from spaceknife import cli_run
from spaceknife.cli_run import (
    Command,
    command_tail,
    group,
    main,
    parse_command_line,
    split_line,
    split_tokens,
    tail_matches,
)
from spaceknife.store import snapshot

# ------------------------------------------------------------------------------------- parsing


def test_internal_parser_keeps_backslashes_before_quotes() -> None:
    raw = r'scan "C:\" filter "*.txt;>1mb" export "Simple file list" "C:\out dir\list.txt" autoclose'
    words, errors, to_os = split_line(raw)
    assert words == [
        "scan",
        "C:\\",
        "filter",
        "*.txt;>1mb",
        "export",
        "Simple file list",
        "C:\\out dir\\list.txt",
        "autoclose",
    ]
    assert errors == [] and not to_os


def test_delimiter_meta_commands() -> None:
    words, errors, _ = split_line('@delim={} scan {C:\\a "quoted" b} @delim=| filter |*.txt| export x y')
    assert words == ["scan", 'C:\\a "quoted" b', "filter", "*.txt", "export", "x", "y"]
    assert errors == []
    words, errors, _ = split_line('scan "a" "@delim={}" {b}')  # a delimited meta command is a word
    assert words == ["scan", "a", "@delim={}", "{b}"]
    _, errors, _ = split_line("@delim=abc scan x")
    assert len(errors) == 1 and "@delim=abc" in errors[0]
    words, errors, _ = split_line('scan "unclosed')
    assert words == ["scan", "unclosed"] and len(errors) == 1


def test_parser_os_reparses_from_scratch() -> None:
    tokens = ["@delim={}", "scan", "{a", "b}", "@parser=OS", "@parser=OS"]
    parsed = parse_command_line(tokens, raw="@delim={} scan {a b} @parser=OS @parser=OS")
    assert parsed.parser == "os"  # the system's words: "{a" and "b}" are two of them
    assert parsed.errors[0] == cli_run.TEXT["meta_after_os"].format(word="@delim={}")
    assert len(parsed.errors) == 2 and "b}" in parsed.errors[1]
    parsed = parse_command_line(["@parser=OS", "scan", "{a", "autoclose"])
    assert parsed.parser == "os" and parsed.errors == []
    assert parsed.commands == [Command("scan", ("{a",)), Command("autoclose", ())]


def test_token_parser_joins_delimited_tokens() -> None:
    words, errors, _ = split_tokens(["@delim={}", "scan", "{/a", "b}", "filter", "{x}y", "z"])
    assert words == ["scan", "/a b", "filter", "x", "y", "z"]
    assert errors == []
    words, _, _ = split_tokens(["scan", "/path with spaces", "autoclose"])  # already split by the shell
    assert words == ["scan", "/path with spaces", "autoclose"]
    words, errors, _ = split_tokens(['"never', "closed"])
    assert words == ["never closed"] and len(errors) == 1


def test_grouping_and_order_checks() -> None:
    commands, errors = group(
        ["SCAN", "/a", "Filter", "*.txt", "export", "Summary", "o.txt", "autoclose", "help"]
    )
    assert commands == [
        Command("scan", ("/a",)),
        Command("filter", ("*.txt",)),
        Command("export", ("Summary", "o.txt")),
        Command("autoclose", ()),
        Command("help", ()),
    ]
    assert errors == []
    _, errors = group(["filter", "*.txt", "scan", "/a"])
    assert errors == [cli_run.TEXT["no_view"].format(name="filter")]
    _, errors = group(["scan", "/a", "frobnicate", "export", "x"])
    assert len(errors) == 2 and "frobnicate" in errors[0] and "export" in errors[1]


def test_raw_command_line_tail() -> None:
    raw = '"C:\\Program Files\\Python\\python.exe" -m spaceknife run scan "C:\\" autoclose'
    assert command_tail(raw) == ' scan "C:\\" autoclose'
    assert command_tail('"C:\\run\\spaceknife.exe" RUN help') == " help"
    assert command_tail("spaceknife scan x") is None
    assert tail_matches(' scan "C:\\" autoclose', ["scan", 'C:" autoclose'])
    assert not tail_matches(" -k something", ["scan"])
    assert tail_matches("   ", []) and not tail_matches(" scan", [])
    assert cli_run.windows_command_tail(["scan"]) is None  # pytest's own command line has no "run …"


# ------------------------------------------------------------------------------------- running


def consoles() -> tuple[Console, Console, io.StringIO, io.StringIO]:
    out, err = io.StringIO(), io.StringIO()
    return (
        Console(file=out, width=200, color_system=None),
        Console(file=err, width=200, color_system=None),
        out,
        err,
    )


def run(tokens: list[str]) -> tuple[int, str, str]:
    out, err, out_buf, err_buf = consoles()
    code = main(tokens, out=out, err=err)
    return code, out_buf.getvalue(), err_buf.getvalue()


@pytest.fixture
def tree(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    monkeypatch.setenv("SPACEKNIFE_HOME", str(tmp_path / "home"))  # never the real user's data
    root = tmp_path / "tree"
    files = {
        "docs/a.txt": 1200,
        "docs/b.txt": 300,
        "docs/deep/c.txt": 50,
        "media/v.bin": 9000,
        "readme.md": 10,
    }
    for rel, size in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"x" * size)
    (root / "empty").mkdir()
    return root


def lines(path: Path) -> list[str]:
    return path.read_text(encoding="utf-8").splitlines()


def test_scan_filter_export_save_load(tree: Path, tmp_path: Path) -> None:
    out = tmp_path / "out"
    out.mkdir()
    snap = out / "tree.sks"
    code, stdout, stderr = run(
        [
            "scan",
            str(tree),
            "filter",
            "*.txt",
            "export",
            "Simple file list",
            str(out / "list.txt"),
            "export",
            "csv",
            str(out / "t.csv"),
            "export",
            "json",
            str(out / "t.json"),
            "export",
            "html",
            str(out / "t.html"),
            "save",
            str(snap),
            "autoclose",
        ]
    )
    assert code == 0, stderr
    assert stdout == ""  # autoclose: no view summary
    expected = [str(tree / "docs" / n) for n in ("a.txt", "b.txt")] + [str(tree / "docs" / "deep" / "c.txt")]
    assert sorted(lines(out / "list.txt")) == sorted(expected)
    raw_csv = (out / "t.csv").read_bytes()
    assert raw_csv.startswith(b"\xef\xbb\xbf")  # BOM: Excel reads UTF-8
    rows = list(csv.DictReader(io.StringIO(raw_csv.decode("utf-8-sig"), newline="")))
    assert {r["name"] for r in rows} == {"tree", "docs", "deep", "a.txt", "b.txt", "c.txt"}
    doc = json.loads((out / "t.json").read_text(encoding="utf-8"))
    assert doc["filter"] == "*.txt" and doc["roots"][0]["path"] == str(tree)
    page = (out / "t.html").read_text(encoding="utf-8")
    assert page.startswith("<!doctype html>") and "a.txt" in page and "v.bin" not in page
    # the snapshot holds the whole tree, not the filtered view
    store, header = snapshot.load(snap)
    assert len(store) == header.node_count
    assert header.roots == [str(tree)] and header.node_count - 1 == 10
    assert int(header.stats["files"]) == 5

    code, _, stderr = run(
        ["load", str(snap), "export", "Simple file list", str(out / "all.txt"), "autoclose"]
    )
    assert code == 0, stderr
    assert len(lines(out / "all.txt")) == 5
    code, _, stderr = run(
        ["scan", str(tree), "export", "Simple file list", str(out / "again.txt"), "autoclose"]
    )
    assert code == 0, stderr
    assert lines(out / "again.txt") == lines(out / "all.txt")  # a loaded snapshot exports like the scan


def test_template_file_and_view_summary(tree: Path, tmp_path: Path) -> None:
    template = tmp_path / "mine.sktpl"
    template.write_text(
        "@sort=name\n@order=asc\n@detail\n{nest{ }}<%file%>|<%filecount%>{&br}", encoding="utf-8"
    )
    target = tmp_path / "mine.txt"
    code, stdout, stderr = run(["scan", str(tree), "export", str(template), str(target)])
    assert code == 0, stderr
    assert lines(target) == [
        "tree|5",
        " docs|3",
        "  a.txt|0",
        "  b.txt|0",
        "  deep|1",
        "   c.txt|0",
        " empty|0",
        " media|1",
        "  v.bin|0",
        " readme.md|0",
    ]
    # without autoclose the views are summarised on stdout (the headless stand-in for a window)
    assert cli_run.TEXT["view_title"].format(index=1, label=f"scan {tree}") in stdout
    assert f"docs{os.sep}" in stdout and "readme.md" in stdout


def test_several_roots_in_one_scan(tree: Path, tmp_path: Path) -> None:
    target = tmp_path / "two.txt"
    both = f"{tree / 'docs'};{tree / 'media'}"
    code, _, stderr = run(["scan", both, "export", "Summary", str(target), "autoclose"])
    assert code == 0, stderr
    text = target.read_text(encoding="utf-8")
    assert text.count("Total size:") == 2  # one export per root of the view
    assert "Files     : 3" in text and "Files     : 1" in text
    odd = tmp_path / "a;b"
    odd.mkdir()
    (odd / "f.txt").write_bytes(b"1")
    code, _, stderr = run(["scan", str(odd), "export", "Simple file list", str(target), "autoclose"])
    assert code == 0, stderr  # a folder whose name holds ";" is one path
    assert lines(target) == [str(odd / "f.txt")]


def test_errors_stop_before_anything_runs(tree: Path, tmp_path: Path) -> None:
    target = tmp_path / "never.txt"
    code, _, stderr = run(["scan", str(tree), "export", "Summary", str(target), "bogus"])
    assert code == 2 and "bogus" in stderr and not target.exists()
    code, _, stderr = run(["export", "Summary", str(target)])
    assert code == 2 and "export" in stderr
    code, _, stderr = run(["scan", str(tree), "export", "Summary"])
    assert code == 2 and cli_run.USAGE["export"] in stderr


def test_failing_commands(tree: Path, tmp_path: Path) -> None:
    target = tmp_path / "x.txt"
    code, _, stderr = run(["scan", str(tmp_path / "missing"), "autoclose"])
    assert code == 2 and "missing" in stderr
    code, _, stderr = run(
        ["scan", str(tree), "filter", ">1zb", "export", "Summary", str(target), "autoclose"]
    )
    assert code == 2 and ">1zb" in stderr and cli_run.FILTER_REASONS["unknown_unit"] in stderr
    assert not target.exists()
    code, _, stderr = run(["scan", str(tree), "export", "No such template", str(target), "autoclose"])
    assert code == 2 and "No such template" in stderr
    broken = tmp_path / "broken.sktpl"
    broken.write_text("<%nosuch%>", encoding="utf-8")
    code, _, stderr = run(["scan", str(tree), "export", str(broken), str(target), "autoclose"])
    assert code == 2 and "nosuch" in stderr and not target.exists()
    code, _, stderr = run(
        ["scan", str(tree), "export", "Summary", str(tmp_path / "no" / "dir.txt"), "autoclose"]
    )
    assert code == 2
    fake = tmp_path / "old.sns"
    fake.write_bytes(b"SpaceSniffer binary snapshot\x00\x01")
    code, _, stderr = run(["load", str(fake), "autoclose"])
    assert code == 2 and ".sns" in stderr
    code, _, stderr = run(["load", f"{fake};{fake}", "autoclose"])
    assert code == 2 and cli_run.TEXT["load_many"] in stderr


def test_filter_warnings_do_not_stop(tree: Path, tmp_path: Path) -> None:
    target = tmp_path / "w.txt"
    code, _, stderr = run(
        ["scan", str(tree), "filter", ":attr:ads", "export", "Summary", str(target), "autoclose"]
    )
    assert code == 0, stderr  # "ads" is not collected yet: a warning, the filter matches nothing
    assert cli_run.FILTER_REASONS["unsupported"] in stderr
    assert "Files     : 0" in target.read_text(encoding="utf-8")


def test_help_and_empty_command_line() -> None:
    code, stdout, _ = run(["help"])
    assert (
        code == 0 and "spaceknife run" in stdout and "Grouped by folder" in stdout and "@parser=OS" in stdout
    )
    code, stdout2, _ = run([])
    assert code == 0 and stdout2 == stdout


def test_typer_wiring(tree: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Registered as cli.py will register it: the arguments after ``run`` reach the parser untouched."""
    import typer
    from typer.testing import CliRunner

    monkeypatch.setattr("spaceknife.log.setup_logging", lambda *a, **k: None)  # keep pytest's log sinks
    app = typer.Typer()
    app.command("run", context_settings=cli_run.CONTEXT_SETTINGS)(cli_run.run)

    @app.command()
    def other() -> None:  # a second command, so that "run" stays a subcommand
        """Unused."""

    target = tmp_path / "t.txt"
    args = [
        "run",
        "scan",
        str(tree),
        "filter",
        "s>1kb",
        "export",
        "Simple file list",
        str(target),
        "autoclose",
    ]
    result = CliRunner().invoke(app, args)
    assert result.exit_code == 0, result.output
    assert lines(target) == [str(tree / "media" / "v.bin"), str(tree / "docs" / "a.txt")]


def test_documented_delimiter_example() -> None:
    raw = r'@delim={} scan {D:\Photos} filter {"a;b.jpg";>1mb} export {Grouped by folder} {D:\out\files.txt}'
    parsed = parse_command_line([], raw=raw)
    assert parsed.errors == []
    assert parsed.commands == [
        Command("scan", (r"D:\Photos",)),
        Command("filter", ('"a;b.jpg";>1mb',)),
        Command("export", ("Grouped by folder", r"D:\out\files.txt")),
    ]
