"""Logging setup: loguru everywhere, rich for humans, JSON lines for machines.

Logs always go to **stderr**; stdout is reserved for the machine-readable ``ready`` line the
Electron main process waits for. Standard-library logging (websockets, asyncio, …) is routed
into loguru so there is a single pipeline.
"""

from __future__ import annotations

import contextlib
import logging
import sys
from collections.abc import Callable
from enum import StrEnum
from pathlib import Path
from types import FrameType
from typing import TYPE_CHECKING

from loguru import logger
from rich.console import Console
from rich.text import Text

if TYPE_CHECKING:
    from loguru import Message

    from .logbus import LogHub


class LogFormat(StrEnum):
    RICH = "rich"
    PLAIN = "plain"
    JSON = "json"


_LEVEL_STYLES = {
    "TRACE": "dim",
    "DEBUG": "cyan",
    "INFO": "bold blue",
    "SUCCESS": "bold green",
    "WARNING": "bold yellow",
    "ERROR": "bold red",
    "CRITICAL": "bold white on red",
}


class InterceptHandler(logging.Handler):
    """Forward standard-library log records to loguru (the recipe from the loguru docs)."""

    def emit(self, record: logging.LogRecord) -> None:
        try:
            level: str | int = logger.level(record.levelname).name
        except ValueError:
            level = record.levelno
        frame: FrameType | None = logging.currentframe()
        depth = 2
        while frame is not None and frame.f_code.co_filename == logging.__file__:
            frame = frame.f_back
            depth += 1
        logger.opt(depth=depth, exception=record.exc_info).log(level, record.getMessage())


def _rich_sink(console: Console) -> Callable[[Message], None]:
    def sink(message: Message) -> None:
        record = message.record
        line = Text()
        line.append(record["time"].strftime("%H:%M:%S.%f")[:-3], style="dim")
        line.append(" ")
        level = record["level"].name
        line.append(f"{level:<8}", style=_LEVEL_STYLES.get(level, ""))
        line.append(f"{record['name']}", style="magenta")
        line.append("  ")
        line.append(record["message"])
        console.print(line, soft_wrap=True, highlight=False)
        if record["exception"] is not None:
            console.print_exception(show_locals=False)

    return sink


_file_sink: int | None = None


def set_file_logging(path: Path | None, level: str = "INFO") -> None:
    """(Re)open the rotating log file (W8 待定 8, W7 高级 · 日志文件级别); None closes it."""
    global _file_sink
    if _file_sink is not None:
        with contextlib.suppress(ValueError):
            logger.remove(_file_sink)
        _file_sink = None
    if path is None:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    _file_sink = logger.add(
        path,
        level=level,
        rotation="10 MB",
        retention=5,
        encoding="utf-8",
        enqueue=True,
        format="{time:YYYY-MM-DD HH:mm:ss.SSS} | {level:<8} | {name}:{line} - {message}",
    )


def setup_logging(level: str = "INFO", fmt: LogFormat = LogFormat.RICH, hub: LogHub | None = None) -> None:
    """Configure loguru sinks. Safe to call more than once (sinks are replaced)."""
    logger.remove()
    if fmt is LogFormat.RICH:
        logger.add(_rich_sink(Console(stderr=True)), level=level, format="{message}", colorize=False)
    elif fmt is LogFormat.JSON:
        logger.add(sys.stderr, level=level, serialize=True)
    else:
        logger.add(
            sys.stderr,
            level=level,
            colorize=False,
            format="{time:HH:mm:ss.SSS} | {level:<8} | {name}:{line} - {message}",
        )
    if hub is not None:
        logger.add(hub.sink, level="DEBUG", format="{message}", colorize=False, catch=True)
    logging.basicConfig(handlers=[InterceptHandler()], level=logging.INFO, force=True)
    logging.getLogger("websockets").setLevel(logging.WARNING)
