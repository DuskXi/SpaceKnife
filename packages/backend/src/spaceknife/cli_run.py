"""``spaceknife run``: SpaceSniffer-compatible command sequences, without a window.

    spaceknife run scan "D:\\Photos" filter "*.jpg;>5mb" export "Simple file list" big.txt autoclose

The commands are SpaceSniffer 2.0.5.18's (docs/export-templates.md, section 6): ``scan``, ``load``,
``filter``, ``save``, ``export``, ``autoclose`` and ``help``, plus the meta commands ``@delim=`` and
``@parser=OS``. They run in order; ``filter``, ``save`` and ``export`` act on the view opened by the
preceding ``scan`` / ``load``. The whole sequence is checked before anything runs.

SpaceSniffer reads the raw command line with its own parser (parameters in delimiters, no escape
character), so that ``scan "C:\\" …`` is not mangled by the C runtime's backslash rules. On Windows
:func:`run` does the same with ``GetCommandLineW``; elsewhere the shell has already split the
arguments and the same rules apply to them (:func:`split_tokens`). ``@parser=OS`` switches to the
arguments as the system split them.

Scans run in this process (the native lister on a few threads when it is built, else one thread).
Progress and messages go to stderr; without ``autoclose`` a summary of each view goes to stdout.
"""

from __future__ import annotations

import asyncio
import os
import sys
from collections import deque
from collections.abc import Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Literal

import numpy as np
import typer
from loguru import logger
from rich.console import Console
from rich.table import Table
from rich.text import Text

from . import fmt
from .cli_scan import apply_language
from .i18n import Texts, plural, tr
from .scan.scanner import ScanError, Scanner, ScanProgress, ScanState, native_slicer, native_threads
from .store.nodes import NodeStore

# User-facing text of this module, translated when read (M7.5).
TEXT = Texts(
    {
        "unknown": "未知的命令「{word}」（可用：help scan load filter save export autoclose）",
        "incomplete": "命令 {name} 不完整，写法：{usage}",
        "no_view": "{name} 前面没有 scan 或 load：它要作用于前一个打开的视图",
        "unclosed": "参数缺少结尾的分隔符 {delim}",
        "bad_delim": "元命令 {word} 无效：@delim= 后面应是一个或两个字符",
        "meta_after_os": "@parser=OS 之后不能再用元命令：{word}",
        "errors": "命令行有误，没有执行任何命令：",
        "help_hint": "运行 spaceknife run help 查看用法。",
        "scanning": "扫描中",
        "scan_failed": "无法扫描 {paths}：{reason}",
        "scan_stopped": "扫描没有完成（{state}）：{paths}",
        "scanned": "已扫描 {paths}：{items} 项，占用 {size}，用时 {time}",
        "denied": "{count} 个文件夹无法访问，例如 {example}",
        "load_failed": "无法打开快照 {path}：{reason}",
        "load_sns": "{path} 看起来是 SpaceSniffer 快照（.sns）：这种格式没有公开，SpaceKnife 读不了",
        "load_many": "一个 load 只能打开一个快照；要打开多个，请写多个 load",
        "loaded": "已打开快照 {path}：{items} 项，占用 {size}",
        "filter_failed": "过滤表达式有误：{expr}",
        "filter_diag": "  第 {start} 个字符起「{text}」：{reason}",
        "filtered": "过滤 {expr}：{count} 个文件，占用 {size}",
        "saved": "已保存快照 {path}（{size}）",
        "save_failed": "无法保存快照 {path}：{reason}",
        "template_errors": "模板「{name}」有错误，没有导出：",
        "template_issue": "  第 {line} 行第 {column} 列（{section}）：{message}",
        "exported": "已导出（{what}）到 {path}（{size}）",
        "export_failed": "无法写入 {path}：{reason}",
        "interrupted": "已中断",
        "view_title": "视图 {index}：{label}",
        "col_name": "名称",
        "col_disk": "占用",
        "col_share": "占比",
        "view_total": "共 {files}、{folders}，占用 {size}",  # the counts: view_files, view_folders
        "view_files": "{files} 个文件",
        "view_folders": "{folders} 个文件夹",
        "view_filter": "过滤 {expr}",
        "rest": "…其余 {count} 项",
    }
)
# docs/filter-syntax.md, section 8 (the frontends localise by code)
FILTER_REASONS = Texts(
    {
        "incomplete": "条件缺少必需的部分",
        "unterminated_quote": "引号没有闭合",
        "text_after_quote": "闭合引号后还有内容",
        "bad_escape": "引号内末尾的 \\",
        "backslash_in_mask": "不带引号的掩码里有 \\",
        "bad_char": "不带引号的掩码里有 | < >",
        "unknown_keyword": "未知关键字",
        "unknown_class": "未知类别",
        "unknown_tag": "未知标签",
        "unknown_attr": "未知属性",
        "bad_list": "列表格式不对",
        "unknown_prefix": "未知前缀",
        "bad_operator": "运算符应为 < > <= >=",
        "bad_number": "数字不合法",
        "missing_unit": "缺少单位",
        "unknown_unit": "未知单位",
        "unit_mismatch": "前缀与单位不同类",
        "out_of_range": "数值超出上限",
        "unsupported": "当前平台不记录这项属性或时间（警告）",
    }
)
USAGE = Texts(  # how a command with parameters is written (the others: just the name)
    {
        "scan": 'scan "<路径>"',
        "load": 'load "<快照>"',
        "filter": 'filter "<表达式>"',
        "save": 'save "<文件>"',
        "export": 'export "<模板>" "<文件>"',
    }
)
ARITY: dict[str, int] = {"help": 0, "scan": 1, "load": 1, "filter": 1, "save": 1, "export": 2, "autoclose": 0}
VIEW_COMMANDS = ("scan", "load")
CONTEXT_SETTINGS: dict[str, Any] = {"allow_extra_args": True, "ignore_unknown_options": True}
MUTED, WARN, ACCENT = "#8F95A2", "#F5C542", "#8B9CFF"  # design tokens text.tertiary, warning, accent
SCAN_THREADS = 4  # native lister threads for a command-line scan
SUMMARY_ROWS = 10
EXIT_OK, EXIT_ERROR, EXIT_INTERRUPTED = 0, 2, 130


# ---------------------------------------------------------------------------------------- parsing


@dataclass(frozen=True, slots=True)
class Command:
    name: str  # lowercase command name
    args: tuple[str, ...]


@dataclass(slots=True)
class Parsed:
    commands: list[Command]
    errors: list[str]
    parser: Literal["internal", "os"]


def _meta(word: str, errors: list[str]) -> tuple[str, str] | Literal["os"] | None:
    """A meta command: new delimiters, the parser switch, or None (an ordinary word)."""
    low = word.lower()
    if low == "@parser=os":
        return "os"
    if low.startswith("@delim="):
        spec = word[len("@delim=") :]
        if len(spec) in (1, 2) and not any(ch.isspace() for ch in spec):
            return spec[0], spec[-1]
        errors.append(TEXT["bad_delim"].format(word=word))
    return None


def split_line(text: str) -> tuple[list[str], list[str], bool]:
    """SpaceSniffer's internal parser over a raw command line: (words, errors, switch to OS).

    Words are separated by white space; a word that starts with the opening delimiter runs to the
    closing one (no escape character). Meta commands are bare words and change how the rest of
    the line is read.
    """
    words: list[str] = []
    errors: list[str] = []
    opening = closing = '"'
    i, n = 0, len(text)
    while i < n:
        if text[i].isspace():
            i += 1
            continue
        if text[i] == opening:
            j = text.find(closing, i + 1)
            if j < 0:
                errors.append(TEXT["unclosed"].format(delim=closing))
                words.append(text[i + 1 :])
                break
            words.append(text[i + 1 : j])
            i = j + 1
            continue
        j = i
        while j < n and not text[j].isspace():
            j += 1
        word, i = text[i:j], j
        meta = _meta(word, errors)
        if meta == "os":
            return words, errors, True
        if isinstance(meta, tuple):
            opening, closing = meta
        elif not word.lower().startswith("@delim="):
            words.append(word)
    return words, errors, False


def split_tokens(tokens: Sequence[str]) -> tuple[list[str], list[str], bool]:
    """The internal parser over arguments the shell already split (POSIX, or no raw line).

    A token that starts with the opening delimiter runs to the token holding the closing one (the
    tokens joined by single spaces); whatever follows the closing delimiter is the next token.
    """
    words: list[str] = []
    errors: list[str] = []
    opening = closing = '"'
    queue = deque(tokens)
    while queue:
        token = queue.popleft()
        if token.startswith(opening):
            buf = token[len(opening) :]
            while True:
                k = buf.find(closing)
                if k >= 0:
                    words.append(buf[:k])
                    if buf[k + 1 :]:
                        queue.appendleft(buf[k + 1 :])
                    break
                if not queue:
                    errors.append(TEXT["unclosed"].format(delim=closing))
                    words.append(buf)
                    break
                buf = f"{buf} {queue.popleft()}"
            continue
        meta = _meta(token, errors)
        if meta == "os":
            return words, errors, True
        if isinstance(meta, tuple):
            opening, closing = meta
        elif not token.lower().startswith("@delim="):
            words.append(token)
    return words, errors, False


def _os_words(tokens: Sequence[str]) -> tuple[list[str], list[str]]:
    """``@parser=OS``: the arguments as the system split them; the switch itself has no further effect."""
    words, errors = [], []
    for token in tokens:
        low = token.lower()
        if low == "@parser=os":
            continue
        if low.startswith("@delim="):
            errors.append(TEXT["meta_after_os"].format(word=token))
            continue
        words.append(token)
    return words, errors


def group(words: Sequence[str]) -> tuple[list[Command], list[str]]:
    """Words → commands by arity, then the order check (``filter`` / ``save`` / ``export`` need a view)."""
    commands: list[Command] = []
    errors: list[str] = []
    i = 0
    while i < len(words):
        name = words[i].lower()
        arity = ARITY.get(name)
        if arity is None:
            errors.append(TEXT["unknown"].format(word=words[i]))
            i += 1
            continue
        if len(words) - i - 1 < arity:
            errors.append(TEXT["incomplete"].format(name=name, usage=USAGE.get(name, name)))
            break
        commands.append(Command(name, tuple(words[i + 1 : i + 1 + arity])))
        i += 1 + arity
    view = False
    for cmd in commands:
        if cmd.name in VIEW_COMMANDS:
            view = True
        elif cmd.name in ("filter", "save", "export") and not view:
            errors.append(TEXT["no_view"].format(name=cmd.name))
    return commands, errors


def parse_command_line(tokens: Sequence[str], raw: str | None = None) -> Parsed:
    """Commands from the arguments after ``run`` (and, when known, the raw text they came from)."""
    if raw is not None:
        words, errors, to_os = split_line(raw)
    else:
        words, errors, to_os = split_tokens(tokens)
    parser: Literal["internal", "os"] = "internal"
    if to_os:  # "reparses everything from scratch"
        words, errors = _os_words(tokens)
        parser = "os"
    commands, more = group(words)
    return Parsed(commands, errors + more, parser)


def command_tail(raw: str, command: str = "run") -> str | None:
    """The text after the first bare word ``command`` of a raw command line, or None."""
    i, n = 0, len(raw)
    while i < n:
        if raw[i].isspace():
            i += 1
            continue
        if raw[i] == '"':
            j = raw.find('"', i + 1)
            i = n if j < 0 else j + 1
            continue
        j = i
        while j < n and not raw[j].isspace():
            j += 1
        if raw[i:j].lower() == command:
            return raw[j:]
        i = j
    return None


def windows_command_tail(tokens: Sequence[str]) -> str | None:
    """This process's raw command line after ``run`` (Windows only; None elsewhere).

    Only trusted when it starts like ``tokens`` (the arguments click passed): the C runtime's
    backslash rules mangle parameters, never the first command word.
    """
    if sys.platform != "win32":
        return None
    import ctypes

    get = ctypes.windll.kernel32.GetCommandLineW
    get.restype = ctypes.c_wchar_p
    get.argtypes = []
    raw = get()
    tail = command_tail(raw) if raw else None
    return tail if tail is not None and tail_matches(tail, tokens) else None


def tail_matches(tail: str, tokens: Sequence[str]) -> bool:
    """The raw tail and the system-split arguments begin with the same word (or are both empty)."""
    words = tail.split(None, 1)
    if not tokens:
        return not words
    return bool(words) and words[0].strip('"').lower() == tokens[0].lower()


# ---------------------------------------------------------------------------------------- running


@dataclass(slots=True)
class View:
    label: str
    store: NodeStore
    roots: list[str]
    volumes: list[dict[str, Any]] = field(default_factory=list)
    stats: dict[str, Any] = field(default_factory=dict)
    checkpoints: dict[str, Any] = field(default_factory=dict)
    filter_text: str = ""
    match: np.ndarray | None = None


class CommandFailed(Exception):
    """A command could not run; the message is for the user (in the interface language)."""


def _split_paths(arg: str, *, folder: bool) -> list[str]:
    """``a;b`` → two paths, unless the whole text names an existing folder (or file) itself."""
    whole = os.path.expanduser(arg.strip())
    if (os.path.isdir(whole) if folder else os.path.isfile(whole)) or ";" not in arg:
        return [whole]
    return [os.path.expanduser(p.strip()) for p in arg.split(";") if p.strip()]


class Runner:
    def __init__(self, out: Console, err: Console) -> None:
        self.out = out
        self.err = err
        self.views: list[View] = []
        self.autoclose = False

    def say(self, key: str, style: str = "", *, n: int | None = None, one: str = "", **kw: object) -> None:
        """Print ``TEXT[key]`` filled in; with a count ``n``, English says ``one`` for a single thing."""
        text = TEXT[key].format(**kw) if n is None else plural(n, TEXT.raw(key), one, **kw)
        self.err.print(Text(text, style=style))

    def run(self, commands: Sequence[Command]) -> int:
        for cmd in commands:
            logger.debug("run: {} {}", cmd.name, list(cmd.args))
            try:
                getattr(self, f"cmd_{cmd.name}")(*cmd.args)
            except CommandFailed as exc:
                self.err.print(Text(str(exc), style="red"))
                return EXIT_ERROR
        if not self.autoclose:
            for index, view in enumerate(self.views, start=1):
                self.summary(index, view)
        return EXIT_OK

    @property
    def view(self) -> View:
        return self.views[-1]  # group() made sure a scan / load came first

    # ------------------------------------------------------------------ commands

    def cmd_help(self) -> None:
        self.out.print(help_text(), markup=False, highlight=False)

    def cmd_autoclose(self) -> None:
        self.autoclose = True

    def cmd_scan(self, arg: str) -> None:
        paths = _split_paths(arg, folder=True)
        try:
            scanner, result = asyncio.run(self._scan(paths))
        except ScanError as exc:
            raise CommandFailed(TEXT["scan_failed"].format(paths="; ".join(paths), reason=exc)) from exc
        roots = [r.path for r in scanner.roots]
        if result.state != ScanState.DONE:
            raise CommandFailed(TEXT["scan_stopped"].format(state=result.state.value, paths="; ".join(roots)))
        self.say(
            "scanned",
            MUTED,
            n=result.items,
            one="Scanned {paths}: 1 item, {size} on disk, in {time}",
            paths="; ".join(roots),
            items=fmt.count(result.items),
            size=fmt.size(result.bytes_alloc),
            time=fmt.duration(result.elapsed_s),
        )
        if result.errors:
            self.say(
                "denied",
                WARN,
                n=result.errors,
                one="1 folder couldn't be accessed, e.g. {example}",
                count=result.errors,
                example=result.denied[0] if result.denied else "",
            )
        stats = {
            "items": result.items,
            "files": result.files,
            "dirs": result.dirs,
            "bytes_alloc": result.bytes_alloc,
            "bytes_size": result.bytes_size,
            "errors": result.errors,
            "elapsed_s": result.elapsed_s,
            "started_at": result.started_at.isoformat() if result.started_at else None,
            "finished_at": result.finished_at.isoformat() if result.finished_at else None,
            "denied": result.denied,
            "state": result.state.value,
            "roots": [{"path": r.path, "whole_volume": r.whole_volume} for r in scanner.roots],
        }
        volumes = [r.volume.to_dict() if r.volume else {} for r in scanner.roots]
        scanner.store.compact()
        self.views.append(View(f"scan {arg}", scanner.store, roots, volumes, stats))

    async def _scan(self, paths: list[str]) -> tuple[Scanner, ScanProgress]:
        from rich.progress import BarColumn, Progress, SpinnerColumn, TextColumn, TimeElapsedColumn

        slicer = None if os.environ.get("SPACEKNIFE_NO_NATIVE") else native_slicer()
        threads = min(SCAN_THREADS, native_threads()) if slicer is not None else 1
        pool = (
            ThreadPoolExecutor(max_workers=threads, thread_name_prefix="spaceknife-run") if slicer else None
        )
        try:
            scanner = Scanner(paths, executor=pool, concurrency=threads, slicer=slicer)
            progress = Progress(
                SpinnerColumn(style=ACCENT),
                TextColumn(f"[bold]{TEXT['scanning']}[/bold]"),
                BarColumn(bar_width=24, complete_style=ACCENT, finished_style="#3FCB80"),
                TextColumn("{task.fields[items]} · {task.fields[size]}"),
                TimeElapsedColumn(),
                console=self.err,
                transient=True,
            )
            with progress:
                task = progress.add_task("scan", total=None, items="0", size="0 B")

                def update(p: ScanProgress) -> None:
                    progress.update(
                        task,
                        total=1.0 if p.progress is not None else None,
                        completed=p.progress or 0,
                        items=f"{p.items:,}",
                        size=fmt.size(p.bytes_alloc),
                    )

                try:
                    result = await scanner.run(on_progress=update, progress_interval=0.2)
                except asyncio.CancelledError:
                    scanner.cancel()
                    raise
            return scanner, result
        finally:
            if pool is not None:
                pool.shutdown(wait=False, cancel_futures=True)

    def cmd_load(self, arg: str) -> None:
        from .store import snapshot  # pydantic: loaded with the first command that needs it

        paths = _split_paths(arg, folder=False)
        if len(paths) > 1:
            raise CommandFailed(TEXT["load_many"])
        path = Path(paths[0])
        try:
            store, header = snapshot.load(path)
        except snapshot.SnapshotError as exc:
            if path.suffix.lower() == ".sns":
                raise CommandFailed(TEXT["load_sns"].format(path=path)) from exc
            raise CommandFailed(TEXT["load_failed"].format(path=path, reason=exc)) from exc
        except OSError as exc:
            raise CommandFailed(TEXT["load_failed"].format(path=path, reason=exc.strerror or exc)) from exc
        items = header.node_count - 1
        self.say(
            "loaded",
            MUTED,
            n=items,
            one="Opened snapshot {path}: 1 item, {size} on disk",
            path=path,
            items=fmt.count(items),
            size=fmt.size(int(store.cols["tot_alloc"][0])),
        )
        view = View(f"load {arg}", store, list(header.roots), list(header.volumes), dict(header.stats))
        view.checkpoints = dict(header.checkpoints)
        self.views.append(view)

    def cmd_filter(self, expr: str) -> None:
        from spaceknife_protocol.filter_syntax import parse

        from .filter.engine import FilterContext, evaluate
        from .filter.vocab import platform_vocab

        result = parse(expr, vocab=platform_vocab())
        lines = []
        for d in result.diagnostics:
            reason = FILTER_REASONS.get(d.code, d.code)
            lines.append(TEXT["filter_diag"].format(start=d.start + 1, text=d.text, reason=reason))
        if result.normal is None:
            raise CommandFailed("\n".join([TEXT["filter_failed"].format(expr=expr), *lines]))
        for line in lines:
            self.err.print(Text(line, style=WARN))
        view = self.view
        found = evaluate(view.store, result.normal, FilterContext.current())
        view.match, view.filter_text = found.match, expr
        count, alloc, _ = found.totals(0)
        self.say(
            "filtered",
            MUTED,
            n=count,
            one="Filter {expr}: 1 file, {size} on disk",
            expr=expr,
            count=fmt.count(count),
            size=fmt.size(alloc),
        )

    def cmd_save(self, arg: str) -> None:
        from .store import snapshot

        view = self.view
        path = Path(os.path.expanduser(arg))
        try:
            snapshot.save(
                view.store,
                path,
                roots=view.roots,
                volumes=view.volumes,
                stats=view.stats,
                checkpoints=view.checkpoints,
            )
            size = path.stat().st_size
        except OSError as exc:
            raise CommandFailed(TEXT["save_failed"].format(path=path, reason=exc.strerror or exc)) from exc
        self.say("saved", MUTED, path=path, size=fmt.size(size))

    def cmd_export(self, name: str, arg: str) -> None:
        from .export import ExportError, ExportFormat, export_chunks, resolve, write_export

        view = self.view
        try:
            target = resolve(name)
        except ExportError as exc:
            raise CommandFailed(str(exc)) from exc
        if isinstance(target, ExportFormat):
            what, bom = target.title, target.bom
        else:
            what, bom = target.name or name, False
            if not target.ok:
                lines = [TEXT["template_errors"].format(name=what)]
                lines += [
                    TEXT["template_issue"].format(
                        line=i.line, column=i.column, section=i.section, message=i.message
                    )
                    for i in target.issues
                ]
                raise CommandFailed("\n".join(lines))
            for i in target.issues:  # warnings only
                line = TEXT["template_issue"].format(
                    line=i.line, column=i.column, section=i.section, message=i.message
                )
                self.err.print(Text(line, style=WARN))
        path = Path(os.path.expanduser(arg))
        chunks = export_chunks(target, view.store, 0, match=view.match, filter_text=view.filter_text)
        try:
            written = write_export(path, chunks, bom=bom)
        except OSError as exc:
            raise CommandFailed(TEXT["export_failed"].format(path=path, reason=exc.strerror or exc)) from exc
        self.say("exported", MUTED, what=what, path=path, size=fmt.size(written))

    # ------------------------------------------------------------------ the headless "view"

    def summary(self, index: int, view: View) -> None:
        self.out.print(Text(TEXT["view_title"].format(index=index, label=view.label), style="bold"))
        if view.filter_text:
            self.out.print(Text(TEXT["view_filter"].format(expr=view.filter_text), style=MUTED))
        from .export.template import ExportTree, Ordering

        order = Ordering(blocks="mixed", sort="size", descending=True)
        for root in view.store.roots():
            tree = ExportTree(view.store, root, match=view.match, ordering=order)
            _, total = tree.sizes(0)
            files, folders = tree.counts(0)
            self.out.print(Text(tree.root_path))
            table = Table(box=None, padding=(0, 1), pad_edge=False)
            table.add_column(TEXT["col_name"], overflow="ellipsis", max_width=48, no_wrap=True)
            table.add_column(TEXT["col_disk"], justify="right", style="bold", no_wrap=True)
            table.add_column(TEXT["col_share"], justify="right", no_wrap=True)
            kids = tree.children(0)
            for pos in kids[:SUMMARY_ROWS].tolist():
                _, alloc = tree.sizes(pos)
                name = view.store.name(int(tree.ids[pos])) + (os.sep if tree.is_dir[pos] else "")
                share = alloc / total if total else 0.0
                table.add_row(Text(name), fmt.size(alloc), f"{share * 100:.1f}%")
            if len(kids) > SUMMARY_ROWS:
                rest = int(tree.alloc_key(kids[SUMMARY_ROWS:]).sum())
                share = rest / total if total else 0.0
                label = TEXT["rest"].format(count=f"{len(kids) - SUMMARY_ROWS:,}")
                table.add_row(Text(label, style=MUTED), fmt.size(rest), f"{share * 100:.1f}%")
            self.out.print(table)
            line = TEXT["view_total"].format(
                files=plural(files, TEXT.raw("view_files"), "1 file", files=fmt.count(files)),
                folders=plural(folders, TEXT.raw("view_folders"), "1 folder", folders=fmt.count(folders)),
                size=fmt.size(total),
            )
            self.out.print(Text(line, style=MUTED))
        self.out.print()


def help_text() -> str:
    from .export import BUILTIN_INFO, FORMATS  # loaded when used: not part of every command's start-up

    return tr(
        """spaceknife run <命令序列>

按顺序执行与 SpaceSniffer 相同的命令，不打开窗口。整个序列先检查一遍，有错就一条也不执行。

  help                       显示本帮助
  scan "<路径>"              扫描文件夹或磁盘，打开一个视图；多个路径用 ; 分隔（同一个视图）
  load "<快照>"              打开 SpaceKnife 快照（.sks），打开一个视图
  filter "<表达式>"          对前一个视图应用过滤（语法见 docs/filter-syntax.md）
  save "<文件>"              把前一个视图的完整结果保存为快照（不受过滤影响）
  export "<模板>" "<文件>"   把前一个视图导出到文件（过滤后的内容）；<模板> 是内置模板名、
                             csv / json / html，或模板文件的路径（语法见 docs/export-templates.md）
  autoclose                  结束时不显示视图摘要

元命令（必须是一个不带分隔符的词）：
  @delim=X  @delim=XY        之后的参数改用 X 作分隔符（或 X 开头、Y 结尾），默认是 "
  @parser=OS                 改用系统拆分的参数从头再解析一次，之后不再接受元命令

内置模板：{builtins}
导出格式：{formats}

例：spaceknife run scan "D:\\Photos" filter "*.jpg;>5mb" export "Simple file list" big.txt autoclose
""",
        builtins=tr("、").join(BUILTIN_INFO),
        formats=" / ".join(FORMATS),
    )


def main(
    tokens: Sequence[str],
    *,
    raw: str | None = None,
    out: Console | None = None,
    err: Console | None = None,
) -> int:
    """Run a command sequence; returns the exit code (0, 2 = a command failed, 130 = interrupted).

    ``tokens`` are the arguments after ``run`` as the system split them; ``raw`` the same part of
    the raw command line, when known (Windows), which the internal parser then reads instead.
    """
    out = out or Console()
    err = err or Console(stderr=True)
    if not tokens and not (raw or "").strip():
        out.print(help_text(), markup=False, highlight=False)
        return EXIT_OK
    parsed = parse_command_line(tokens, raw)
    if parsed.errors:
        err.print(Text(TEXT["errors"], style="red"))
        for message in parsed.errors:
            err.print(Text(f"  {message}", style="red"))
        err.print(Text(TEXT["help_hint"], style=MUTED))
        return EXIT_ERROR
    runner = Runner(out, err)
    try:
        return runner.run(parsed.commands)
    except KeyboardInterrupt:
        err.print(Text(TEXT["interrupted"], style="yellow"))
        return EXIT_INTERRUPTED


def run(ctx: typer.Context) -> None:
    """Run SpaceSniffer-style commands without a window: scan, load, filter, save, export, autoclose, help."""
    from .log import LogFormat, setup_logging

    setup_logging("WARNING", LogFormat.RICH)
    apply_language()
    from . import classes, pathclass

    classes.load_user()  # the user's file classes (W7) also apply to :class: here
    pathclass.load_user()  # and their folder choices
    tokens = list(ctx.args)
    raise typer.Exit(main(tokens, raw=windows_command_tail(tokens)))
