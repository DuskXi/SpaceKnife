"""Export templates (SpaceSniffer's language), the built-in templates and the CSV / JSON / HTML formats."""

from __future__ import annotations

import csv
import io
import json
import os
import time
from collections.abc import Iterator
from html.parser import HTMLParser
from pathlib import Path

import numpy as np
import pytest

from spaceknife import fmt
from spaceknife.export import (
    BUILTIN_INFO,
    BUILTIN_TEMPLATES,
    FORMATS,
    ExportError,
    ExportFormat,
    ExportTemplate,
    builtin_template,
    check,
    export_chunks,
    export_csv,
    export_html,
    export_json,
    parse_template,
    render,
    render_chunks,
    resolve,
    write_export,
)
from spaceknife.export.template import TAGS, Element, ExportTree, Ordering, compare
from spaceknife.filter.engine import FilterContext, evaluate
from spaceknife.store.nodes import ENTRY_COLUMNS, Flag, Kind, NodeStore, encode_name
from spaceknife_protocol.filter_syntax import parse

from .store_builder import build, find

SPEC = {
    "a": {"one.bin": 1000, "Two.TXT": 5000, "sub": {"movie.mp4": 123456}},
    "c.txt": 10,
    ".bashrc": 7,
    "archive.tar.gz": 3,
    "empty": {},
}
J = os.path.join
R = "/data"
STAMP = int(time.mktime((2024, 3, 5, 14, 7, 9, 0, 0, -1)))  # local time
NOW = STAMP + 10 * 86400 + 5


def fixture() -> NodeStore:
    """alloc = size rounded up to 4 KB: root 148 KB, a 136 KB, sub 124 KB, every small file 4 KB."""
    store = build(SPEC, root=R, cluster=4096)
    two = find(store, "a/Two.TXT")
    store.cols["mtime"][two] = STAMP
    store.cols["atime"][two] = STAMP + 3600
    return store


@pytest.fixture
def store() -> NodeStore:
    return fixture()


def root_of(store: NodeStore) -> int:
    return next(iter(store.roots()))


def element(store: NodeStore, rel: str, *, match: np.ndarray | None = None) -> Element:
    target = J(R, *rel.split("/")) if rel else R
    tree = ExportTree(store, root_of(store), match=match, now=NOW)
    return next(el for el in tree.walk() if el.path == target)


def tag(store: NodeStore, rel: str, name: str) -> str:
    return TAGS[name](element(store, rel))


def mask(store: NodeStore, expr: str) -> np.ndarray:
    result = parse(expr)
    assert result.normal is not None
    return evaluate(store, result.normal, FilterContext.current()).match


# ------------------------------------------------------------------------------------- tags


def test_name_and_path_tags(store: NodeStore) -> None:
    assert tag(store, "a/Two.TXT", "file") == "Two.TXT"
    assert tag(store, "a/Two.TXT", "filename") == "Two"
    assert tag(store, "a/Two.TXT", "fileext") == "TXT"  # as written, not lower-cased
    assert tag(store, "a/Two.TXT", "path") == J(R, "a")
    assert tag(store, "a/Two.TXT", "pathfile") == J(R, "a", "Two.TXT")
    assert (tag(store, "archive.tar.gz", "filename"), tag(store, "archive.tar.gz", "fileext")) == (
        "archive.tar",
        "gz",
    )
    assert (tag(store, ".bashrc", "filename"), tag(store, ".bashrc", "fileext")) == (".bashrc", "")
    assert (tag(store, "a/sub", "filename"), tag(store, "a/sub", "fileext")) == ("sub", "")
    # the export root: its own name, the folder above it, its full path
    assert tag(store, "", "file") == "data"
    assert tag(store, "", "pathfile") == R
    assert tag(store, "", "path") == os.path.dirname(R)


def test_size_tags(store: NodeStore) -> None:
    assert tag(store, "a/Two.TXT", "size") == fmt.size(5000)
    assert tag(store, "a/Two.TXT", "sizebytes") == "5000"
    assert tag(store, "a/Two.TXT", "disksize") == fmt.size(8192)
    assert tag(store, "a/Two.TXT", "disksizebytes") == "8192"
    assert tag(store, "a", "sizebytes") == str(1000 + 5000 + 123456)  # folders: subtree totals
    assert tag(store, "a", "disksizebytes") == str(4096 + 8192 + 126976)
    assert tag(store, "", "disksizebytes") == "151552"


def test_time_tags(store: NodeStore) -> None:
    assert tag(store, "a/Two.TXT", "filemodifydate") == "2024-03-05"
    assert tag(store, "a/Two.TXT", "filemodifytime") == "14:07:09"
    assert tag(store, "a/Two.TXT", "filemodifyage") == "10"  # whole days before "now"
    assert tag(store, "a/Two.TXT", "fileaccesstime") == "15:07:09"
    for unknown in ("filecreatedate", "filecreatetime", "filecreateage"):
        assert tag(store, "a/Two.TXT", unknown) == ""  # timestamp 0: unknown


def test_kind_and_count_tags(store: NodeStore) -> None:
    file_tags = {k: tag(store, "a/Two.TXT", k) for k in TAGS if k.startswith(("is", "contains"))}
    assert file_tags == {
        "isfile": "true",
        "isfolder": "false",
        "iscontainer": "false",
        "containsfiles": "false",
        "containsfolders": "false",
        "containscontainers": "false",
    }
    expected_a = {
        "count": "4",
        "localcount": "3",
        "filecount": "3",
        "localfilecount": "2",
        "foldercount": "1",
        "localfoldercount": "1",
        "containercount": "1",
        "localcontainercount": "1",
        "containsfiles": "true",
        "containsfolders": "true",
        "isfolder": "true",
        "iscontainer": "true",
    }
    assert {k: tag(store, "a", k) for k in expected_a} == expected_a
    assert {k: tag(store, "", k) for k in ("count", "localcount", "filecount", "foldercount")} == {
        "count": "9",
        "localcount": "5",
        "filecount": "6",
        "foldercount": "3",
    }
    assert tag(store, "a/sub", "containsfolders") == "false"
    assert tag(store, "empty", "containsfiles") == "false"
    assert [tag(store, p, "nestinglevel") for p in ("", "a", "a/sub", "a/sub/movie.mp4")] == [
        "0",
        "1",
        "2",
        "3",
    ]
    assert tag(store, "a/Two.TXT", "count") == "0"


def test_tags_are_case_insensitive_and_trimmed(store: NodeStore) -> None:
    sub = find(store, "a/sub")
    assert render(store, sub, "<%IsFile%>,") == "false,true,"
    tpl = parse_template("<% PathFile %>{&br}")
    assert tpl.ok
    assert render(store, sub, tpl).splitlines() == [J(R, "a", "sub"), J(R, "a", "sub", "movie.mp4")]
    assert render(store, sub, "< %file%>") == "< %file%>" * 2  # "<%" starts a tag, "< %" is text


def test_unknown_and_unclosed_tags(store: NodeStore) -> None:
    root = root_of(store)
    tpl = parse_template("x<%nosuch%>{&br}")
    assert [i.code for i in tpl.issues] == ["unknown_tag"]
    assert (tpl.issues[0].line, tpl.issues[0].column, tpl.issues[0].severity) == (1, 2, "error")
    assert "nosuch" in tpl.issues[0].message
    assert render(store, root, tpl) == "x*BAD TAG*\n" * 10
    tpl = parse_template("a<%file")
    assert [i.code for i in tpl.issues] == ["unclosed_tag"]
    assert render(store, root, tpl).startswith("a<%filea<%file")  # kept as text


# ------------------------------------------------------------------------------------- functions


def test_entities_escape_the_syntax(store: NodeStore) -> None:
    one = find(store, "a/sub")  # a folder whose subtree is one file: two elements
    out = render(
        store,
        one,
        "{if{<%isfile%>}=={true}?{{&curlyop}<%file%>{&curlycl}{&tab}{&angleop}%x%{&anglecl}{&br}}}",
    )
    assert out == "{movie.mp4}\t<%x%>\n"
    assert render(store, one, "{ &br }") == "\n\n"  # spaces inside the braces are allowed
    tpl = parse_template("{&nope}")
    assert [(i.code, i.severity) for i in tpl.issues] == [("unknown_entity", "warning")]
    assert render(store, one, tpl) == "&nope&nope"


def test_if_comparisons() -> None:
    assert compare("10", ">", "9")  # numbers compare as numbers
    assert compare("1.50", "==", "1.5")
    assert not compare("10", "<", "9")
    assert compare("b", ">", "a")  # text compares as text …
    assert compare("JPG", "==", "jpg")  # … without regard to case
    assert compare(" true ", "==", "true")
    assert compare("a", "!=", "b")
    assert compare("2", "<=", "2")
    assert compare("2", ">=", "2")
    assert not compare("abc", "<", "abb")


def test_if_branches_and_nesting(store: NodeStore) -> None:
    sub = find(store, "a/sub")
    assert render(store, sub, "{if{<%isfile%>}=={true}?{F}:{D}}") == "DF"
    assert render(store, sub, "{if{<%isfile%>}!={true}?{D}}") == "D"
    assert (
        render(store, sub, "{if{<%nestinglevel%>}<{1}?{root}:{if{<%fileext%>}=={MP4}?{video}:{other}}}")
        == "rootvideo"
    )
    for op, want in (("==", "0"), ("!=", "1"), ("<", ""), ("<=", "0"), (">", "1"), (">=", "01")):
        assert render(store, sub, f"{{if{{<%nestinglevel%>}}{op}{{0}}?{{<%nestinglevel%>}}}}") == want, op


def test_bad_if_is_reported_and_marked(store: NodeStore) -> None:
    sub = find(store, "a/sub")
    for text in (
        "{if{a}{b}?{c}}",
        "{if{a}=={b}{c}}",
        "{if{a}==?{c}}",
        "{if{a}=={b}?{c}:}",
        "{if{a}=={b}?{c}x}",
        "{if}",
    ):
        tpl = parse_template(text)
        assert [i.code for i in tpl.issues] == ["bad_if"], text
        assert render(store, sub, tpl) == "*BAD IF*" * 2, text
    assert (
        render(store, sub, "{if{a}=={b}x}<%file%>") == "*BAD IF*sub*BAD IF*movie.mp4"
    )  # skips to the call's end


def test_padding(store: NodeStore) -> None:
    sub = find(store, "a/sub")
    detail = "{if{<%isfile%>}=={true}?{[{leftpad{<%file%>}{.}{12}}][{rightpad{<%file%>}{-}{12}}]}}"
    assert render(store, sub, detail) == "[...movie.mp4][movie.mp4---]"
    assert (
        render(store, sub, "{if{<%isfile%>}=={true}?{{leftpad{ab}{xy}{7}}}}") == "xyxyxab"
    )  # cut to the length
    assert (
        render(store, sub, "{if{<%isfile%>}=={true}?{{leftpad{<%file%>}{ }{3}}}}") == "movie.mp4"
    )  # never cut
    assert render(store, sub, "{if{<%isfile%>}=={true}?{{leftpad{x}{ }{ 4 }}}}") == "   x"
    for bad in (
        "{leftpad{x}{}{4}}",
        "{leftpad{x}{ }{four}}",
        "{rightpad{x}{ }{-1}}",
        "{leftpad{x}{ }{99999999}}",
    ):
        tpl = parse_template(bad)
        assert tpl.ok, bad  # a run-time problem: the arguments are only known per element
        assert render(store, sub, tpl) == "*BAD PAD*" * 2, bad
    tpl = parse_template("{leftpad{x}{ }}")
    assert [i.code for i in tpl.issues] == ["bad_call"]
    assert render(store, sub, tpl) == "*BAD PAD*" * 2


def test_nest_repeats_by_level(store: NodeStore) -> None:
    out = render(store, find(store, "a"), "{nest{--}}<%file%>{&br}")
    assert out.splitlines() == ["a", "--sub", "----movie.mp4", "--Two.TXT", "--one.bin"]


def test_escape_functions(store: NodeStore) -> None:
    sub = find(store, "a/sub")
    detail = (
        '{if{<%isfile%>}=={true}?{{html{<a href="x">&</a>}}|{csv{a,"b"}}|{csv{plain}}|{json{<%file%>"}}}}'
    )
    assert (
        render(store, sub, detail)
        == '&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;|"a,""b"""|plain|"movie.mp4\\""'
    )


def test_groups_and_unbalanced_braces(store: NodeStore) -> None:
    sub = find(store, "a/sub")
    assert render(store, sub, "{x{y}z}") == "xyz" * 2  # a plain group is its content
    assert render(store, sub, "{{}if not a call}") == "if not a call" * 2  # an empty group hides a keyword
    tpl = parse_template("a}b")
    assert [(i.code, i.severity) for i in tpl.issues] == [("stray_close", "warning")]
    assert render(store, sub, tpl) == "a}b" * 2
    tpl = parse_template("{a")
    assert [i.code for i in tpl.issues] == ["unclosed_group"]
    assert render(store, sub, tpl) == "aa"


def test_script_mode_drops_line_breaks_and_tabs(store: NodeStore) -> None:
    sub = find(store, "a/sub")
    assert render(store, sub, "{script}\n<%file%>\n\t[ x ]\r\n") == "sub[ x ]movie.mp4[ x ]"
    assert render(store, sub, "<%file%>\nx") == "sub\nxmovie.mp4\nx"  # without it every character counts
    tpl = parse_template("{script}\n\n  <%nope%>")
    assert (tpl.issues[0].line, tpl.issues[0].column) == (3, 3)  # positions are in the original text


# ------------------------------------------------------------------------------------- template files


def test_template_file_sections_and_options(store: NodeStore) -> None:
    source = (
        "@name=Mine\r\n# a comment\r\n@file=out.txt\r\n@blocks=folders\r\n@sort=name\r\n@order=asc\r\n"
        "@header\r\nHEAD <%file%>\r\n\r\n@detail\r\n<%nestinglevel%>\r\n@footer\r\nEND\r\n"
    )
    tpl = parse_template("﻿" + source)
    assert tpl.ok and not tpl.issues
    assert (tpl.name, tpl.file_name) == ("Mine", "out.txt")
    assert tpl.ordering == Ordering(blocks="folders", sort="name", descending=False)
    assert (tpl.header, tpl.detail, tpl.footer) == ("HEAD <%file%>\n", "<%nestinglevel%>", "END")
    # folders first, then by name ascending: sub (and its file), then one.bin, Two.TXT
    assert render(store, find(store, "a"), tpl) == "HEAD a\n01211END"


def test_template_file_diagnostics() -> None:
    tpl = parse_template("@sort=sideways\n@colour=red\nnot an option\n@detail\nx\n@detail\n<%bad%>")
    assert [(i.code, i.line) for i in tpl.issues] == [
        ("bad_option", 1),
        ("unknown_option", 2),
        ("bad_line", 3),
        ("duplicate_section", 6),
        ("unknown_tag", 7),
    ]
    assert tpl.detail == "<%bad%>"
    assert tpl.ordering == Ordering()  # bad values keep the defaults
    assert check("<%file%>") == []


def test_bare_template_and_final_line_break(store: NodeStore) -> None:
    sub = find(store, "a/sub")
    assert render(store, sub, "<%file%>\n") == "submovie.mp4"  # the last line break is not content
    assert render(store, sub, "<%file%>\n\n") == "sub\nmovie.mp4\n"
    assert render(store, sub, "") == ""
    assert parse_template("# <%file%>").detail == "# <%file%>"  # only "@" starts a template file


def test_ordering(store: NodeStore) -> None:
    root = root_of(store)
    names = "{if{<%nestinglevel%>}=={1}?{<%file%> }}"
    assert (
        render(store, root, names) == "a c.txt .bashrc archive.tar.gz empty "
    )  # size, ties in listing order
    assert render(store, root, "@blocks=files\n@detail\n" + names) == "c.txt .bashrc archive.tar.gz a empty "
    assert (
        render(store, root, "@blocks=folders\n@detail\n" + names) == "a empty c.txt .bashrc archive.tar.gz "
    )
    assert (
        render(store, root, "@sort=name\n@order=asc\n@detail\n" + names)
        == ".bashrc a archive.tar.gz c.txt empty "
    )
    assert render(store, root, "@sort=name\n@detail\n" + names) == "empty c.txt archive.tar.gz a .bashrc "
    assert render(store, root, "@sort=none\n@detail\n" + names) == "a c.txt .bashrc archive.tar.gz empty "
    assert (
        render(store, root, "@sort=size\n@order=asc\n@detail\n" + names)
        == "empty c.txt .bashrc archive.tar.gz a "
    )


# ------------------------------------------------------------------------------------- built-ins


def test_builtin_templates_are_complete() -> None:
    assert list(BUILTIN_TEMPLATES) == [
        "Grouped by folder",
        "First level folders",
        "Simple file list",
        "Summary",
        "Folder tree",
    ]
    for name in BUILTIN_TEMPLATES:
        tpl = builtin_template(name)
        assert tpl is not None and tpl.ok and not tpl.issues, name
        assert tpl.name == name and tpl.file_name
        assert BUILTIN_INFO[name].title
    assert [n for n, b in BUILTIN_INFO.items() if not b.spacesniffer] == ["Folder tree"]
    assert builtin_template("simple FILE list") is not None  # SpaceSniffer: case sensitive
    assert builtin_template("nope") is None


def test_grouped_by_folder(store: NodeStore) -> None:
    out = render(store, root_of(store), BUILTIN_TEMPLATES["Grouped by folder"])
    kb4, kb8, kb124 = fmt.size(4096), fmt.size(8192), fmt.size(126976)
    assert out == (
        f"\n{R} [{fmt.size(151552)}]\n"
        f"  [{kb4:>8}] c.txt\n  [{kb4:>8}] .bashrc\n  [{kb4:>8}] archive.tar.gz\n"
        f"\n{J(R, 'a')} [{fmt.size(139264)}]\n"
        f"  [{kb8:>8}] Two.TXT\n  [{kb4:>8}] one.bin\n"
        f"\n{J(R, 'a', 'sub')} [{kb124}]\n"
        f"  [{kb124:>8}] movie.mp4\n"
    )


def test_first_level_folders(store: NodeStore) -> None:
    out = render(store, root_of(store), BUILTIN_TEMPLATES["First level folders"])
    assert out == f"\n{J(R, 'a')} [{fmt.size(139264)}]\n\n{J(R, 'empty')} [0 B]\n"


def test_simple_file_list(store: NodeStore) -> None:
    out = render(store, root_of(store), BUILTIN_TEMPLATES["Simple file list"])
    assert out.splitlines() == [
        J(R, "c.txt"),
        J(R, ".bashrc"),
        J(R, "archive.tar.gz"),
        J(R, "a", "Two.TXT"),
        J(R, "a", "one.bin"),
        J(R, "a", "sub", "movie.mp4"),
    ]


def test_summary_and_folder_tree(store: NodeStore) -> None:
    root = root_of(store)
    assert render(store, root, BUILTIN_TEMPLATES["Summary"]) == (
        f"Total size: {fmt.size(151552)}\nFolders   : 3\nFiles     : 6\n"
    )
    assert render(store, root, BUILTIN_TEMPLATES["Folder tree"]).splitlines() == [
        f"data  {fmt.size(151552)}",
        f"    a  {fmt.size(139264)}",
        f"        sub  {fmt.size(126976)}",
        "    empty  0 B",
    ]


# ------------------------------------------------------------------------------------- the tree


def test_filter_mask_hides_and_resizes(store: NodeStore) -> None:
    root = root_of(store)
    match = mask(store, "*.txt")
    assert render(store, root, BUILTIN_TEMPLATES["Simple file list"], match=match).splitlines() == [
        J(R, "c.txt"),
        J(R, "a", "Two.TXT"),
    ]
    assert render(store, root, BUILTIN_TEMPLATES["Summary"], match=match) == (
        f"Total size: {fmt.size(4096 + 8192)}\nFolders   : 1\nFiles     : 2\n"
    )
    a = element(store, "a", match=match)
    assert (TAGS["localcount"](a), TAGS["containsfolders"](a), TAGS["disksizebytes"](a)) == (
        "1",
        "false",
        "8192",
    )
    assert render(store, root, "<%file%> ", match=match) == "data a Two.TXT c.txt "  # a counts 8 KB now
    nothing = mask(store, "*.nothing")
    assert render(store, root, "<%file%> ", match=nothing) == "data "  # the root always stays
    assert render(store, root, BUILTIN_TEMPLATES["Grouped by folder"], match=nothing) == ""


def test_filter_mask_from_before_the_store_grew(store: NodeStore) -> None:
    root = root_of(store)
    short = mask(store, "*.mp4")[: find(store, "a/sub/movie.mp4")]  # rows past the mask: no match
    assert render(store, root, "<%file%> ", match=short) == "data "


def test_hardlink_duplicates_and_gone_rows(store: NodeStore) -> None:
    one = find(store, "a/one.bin")
    store.mark(one, Flag.HARDLINK_DUP)
    assert tag(store, "a/one.bin", "disksizebytes") == "0"  # counted at its other link
    masked = element(store, "a", match=mask(store, "*.bin"))
    assert TAGS["disksizebytes"](masked) == "0"
    store.mark(find(store, "c.txt"), Flag.GONE)
    assert "c.txt" not in render(store, root_of(store), "<%file%> ")
    assert tag(store, "", "filecount") == "5"


def test_multiple_roots_render_in_turn() -> None:
    store = NodeStore()
    roots = [store.add_root("/r1"), store.add_root("/r2")]
    for i, root in enumerate(roots):
        entries = {k: np.zeros(1, dtype=np.int64) for k in ENTRY_COLUMNS}
        entries["kind"][0] = Kind.FILE
        entries["size"][0] = entries["alloc"][0] = 100 * (i + 1)
        store.ingest_names(root, [f"f{i}.txt".encode()], entries)
    assert render(store, 0, "@footer\n<%file%>=<%sizebytes%>;") == "r1=100;r2=200;"
    with pytest.raises(ValueError, match="not an element"):
        ExportTree(store, 99)


def test_streaming_in_chunks() -> None:
    spec = {f"d{i:02d}": {f"file{j:04d}.dat": j + 1 for j in range(400)} for i in range(25)}
    store = build(spec, root="/big")
    root = root_of(store)
    template = BUILTIN_TEMPLATES["Simple file list"]
    chunks = list(render_chunks(store, root, template, chunk_chars=4096))
    assert len(chunks) > 50
    longest_line = max(len(line) + 1 for line in "".join(chunks).splitlines())
    assert all(4096 <= len(c) < 4096 + longest_line for c in chunks[:-1])
    assert "".join(chunks) == render(store, root, template)
    assert len("".join(chunks).splitlines()) == 25 * 400


# ------------------------------------------------------------------------------------- formats


def test_csv(store: NodeStore) -> None:
    text = export_csv(store, root_of(store))
    assert "\r\n" in text
    rows = list(csv.DictReader(io.StringIO(text, newline="")))
    assert [r["path"] for r in rows] == [
        R,
        J(R, "a"),
        J(R, "a", "sub"),
        J(R, "a", "sub", "movie.mp4"),
        J(R, "a", "Two.TXT"),
        J(R, "a", "one.bin"),
        J(R, "c.txt"),
        J(R, ".bashrc"),
        J(R, "archive.tar.gz"),
        J(R, "empty"),
    ]
    two = rows[4]
    assert two["name"] == "Two.TXT" and two["type"] == "file" and two["depth"] == "2"
    assert (two["size"], two["disk_size"], two["files"], two["folders"]) == ("5000", "8192", "", "")
    assert two["modified"].startswith("2024-03-05T14:07:09") and two["created"] == ""
    assert (rows[0]["files"], rows[0]["folders"], rows[0]["disk_size"]) == ("6", "3", "151552")
    filtered = list(
        csv.DictReader(io.StringIO(export_csv(store, root_of(store), match=mask(store, "*.mp4"))))
    )
    assert [r["name"] for r in filtered] == ["data", "a", "sub", "movie.mp4"]


def test_csv_quotes_odd_names() -> None:
    store = build({'we,ird "name".txt': 1, "plain.txt": 2}, root="/q")
    rows = list(csv.reader(io.StringIO(export_csv(store, root_of(store)), newline="")))
    assert {r[1] for r in rows[1:]} == {"q", 'we,ird "name".txt', "plain.txt"}


def test_json(store: NodeStore) -> None:
    doc = json.loads(export_json(store, root_of(store), filter_text="*.txt"))
    assert (doc["format"], doc["version"], doc["filter"]) == ("spaceknife-export", 1, "*.txt")
    (tree,) = doc["roots"]
    assert tree["path"] == R and tree["name"] == "data" and tree["type"] == "folder"
    assert (tree["files"], tree["folders"], tree["disk_size"]) == (6, 3, 151552)
    assert [c["name"] for c in tree["children"]] == ["a", "c.txt", ".bashrc", "archive.tar.gz", "empty"]
    a = tree["children"][0]
    assert [c["name"] for c in a["children"]] == ["sub", "Two.TXT", "one.bin"]
    assert a["children"][0]["children"][0]["name"] == "movie.mp4"
    two = a["children"][1]
    assert "children" not in two and "files" not in two
    assert two["modified"].startswith("2024-03-05T14:07:09") and two["created"] is None
    assert tree["children"][4]["children"] == []
    filtered = json.loads(export_json(store, root_of(store), match=mask(store, "*.txt")))
    assert [c["name"] for c in filtered["roots"][0]["children"]] == ["a", "c.txt"]
    assert filtered["filter"] is None


def test_json_streams_large_trees() -> None:
    spec = {f"d{i}": {f"f{j}.x": j for j in range(300)} for i in range(10)}
    store = build(spec, root="/big")
    chunks = list(FORMATS["json"].chunks(store, root_of(store)))
    assert len(chunks) > 2
    doc = json.loads("".join(chunks))
    assert sum(len(d["children"]) for d in doc["roots"][0]["children"]) == 3000


class _Tags(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.stack: list[str] = []
        self.seen: set[str] = set()
        self.urls: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        self.seen.add(tag)
        self.urls += [v or "" for k, v in attrs if k in ("src", "href")]
        if tag not in ("meta", "br", "img", "link", "input", "hr"):  # void elements have no end tag
            self.stack.append(tag)

    def handle_endtag(self, tag: str) -> None:
        assert self.stack and self.stack[-1] == tag, (self.stack[-3:], tag)
        self.stack.pop()


def test_html_report_is_self_contained_and_escaped() -> None:
    spec = {"<b>&evil</b>": {"x.txt": 5000}, "dir": {f"f{i}.log": i * 10 for i in range(40)}}
    store = build(spec, root="/report")
    page = export_html(store, root_of(store), filter_text="*.log;<x>", top=10, depth=3)
    parser = _Tags()
    parser.feed(page)
    assert not parser.stack
    assert parser.urls == []  # no links, images or scripts: nothing external
    assert not {"script", "link", "img", "iframe"} & parser.seen
    assert "&lt;b&gt;&amp;evil&lt;/b&gt;" in page and "<b>&evil" not in page
    assert "*.log;&lt;x&gt;" in page
    assert "其余 30 项" in page  # the folder's 40 files beyond the top 10
    assert page.count("<details") == page.count("</details>")
    assert ".log" in page and "x.txt" in page  # extensions and largest files


def test_html_report_under_a_filter_and_for_every_root(store: NodeStore) -> None:
    page = export_html(store, root_of(store), match=mask(store, "*.mp4"))
    assert "匹配过滤的文件" in page and "movie.mp4" in page and "Two.TXT" not in page
    two = NodeStore()
    for r in ("/x", "/y"):
        two.add_root(r)
    page = export_html(two, 0)
    assert page.count('class="root"') == 2 and "2 个位置" in page


# ------------------------------------------------------------------------------------- resolving and files


def test_resolve_names_formats_and_files(tmp_path: Path) -> None:
    assert isinstance(resolve("Summary"), ExportTemplate)
    assert isinstance(resolve("summary"), ExportTemplate)
    fmt_csv = resolve("CSV")
    assert isinstance(fmt_csv, ExportFormat) and fmt_csv.key == "csv" and fmt_csv.bom
    mine = tmp_path / "mine.sktpl"
    mine.write_text("@detail\n<%file%>", encoding="utf-8")
    found = resolve(str(mine))
    assert isinstance(found, ExportTemplate) and found.name == "mine" and found.detail == "<%file%>"
    assert isinstance(resolve("mine.sktpl", cwd=tmp_path), ExportTemplate)
    with pytest.raises(ExportError, match="找不到"):
        resolve(str(tmp_path / "missing.sktpl"))
    bad = tmp_path / "ansi.sktpl"
    bad.write_bytes("<%file%> 文件".encode("gbk"))
    with pytest.raises(ExportError, match="UTF-8"):
        resolve(str(bad))


def test_write_export_is_atomic_and_utf8(tmp_path: Path, store: NodeStore) -> None:
    out = tmp_path / "out.csv"
    target = resolve("csv")
    assert isinstance(target, ExportFormat)
    size = write_export(out, export_chunks(target, store, root_of(store)), bom=target.bom)
    data = out.read_bytes()
    assert size == len(data) and data.startswith(b"\xef\xbb\xbfpath,name")
    assert [p.name for p in tmp_path.iterdir()] == ["out.csv"]  # no temporary file left

    def failing() -> Iterator[str]:
        yield "partial"
        raise RuntimeError("boom")

    with pytest.raises(RuntimeError):
        write_export(out, failing())
    assert out.read_bytes() == data  # the old file is untouched
    assert [p.name for p in tmp_path.iterdir()] == ["out.csv"]


def test_names_that_are_not_unicode_are_escaped(tmp_path: Path) -> None:
    store = NodeStore()
    root = store.add_root("/u")
    entries = {k: np.zeros(1, dtype=np.int64) for k in ENTRY_COLUMNS}
    store.ingest_names(root, [encode_name("bad\udcff.txt")], entries)  # as a scan stores such a name
    out = tmp_path / "names.txt"
    write_export(out, render_chunks(store, root, "<%file%>|"))
    assert out.read_bytes() == b"u|bad\\udcff.txt|"
    doc = tmp_path / "names.json"
    write_export(doc, FORMATS["json"].chunks(store, root))
    assert json.loads(doc.read_text(encoding="utf-8"))["roots"][0]["children"][0]["name"] == "bad\udcff.txt"


def test_batched_local_times_match_one_by_one(monkeypatch: pytest.MonkeyPatch) -> None:
    from spaceknife.export.formats import LocalTimes, iso_time

    def check() -> None:
        stamps = np.array(
            [
                0,
                1_711_846_799,
                1_711_846_800,
                1_711_850_400,
                1_729_990_000,
                1_730_000_000,
                STAMP,
                4_000_000_000,
            ],
            dtype=np.uint32,
        )
        spread = np.arange(1_600_000_000, 1_760_000_000, 3_600_007, dtype=np.int64).astype(np.uint32)
        for values in (stamps, spread):
            assert LocalTimes().many(values) == [iso_time(int(t)) for t in values.tolist()]

    check()  # the machine's own zone
    if hasattr(time, "tzset") and os.path.exists("/usr/share/zoneinfo/Europe/Berlin"):
        monkeypatch.setenv("TZ", "Europe/Berlin")  # daylight saving switches on 2024-03-31 and 2024-10-27
        time.tzset()
        try:
            check()
            assert iso_time(1_711_846_799).endswith("+01:00") and iso_time(1_711_846_800).endswith("+02:00")
        finally:
            monkeypatch.delenv("TZ")
            time.tzset()
