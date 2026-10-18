"""``python -m spaceknife.helper``: the read-only scan helper, started by the backend only.

Exit codes: 0 normal (shutdown, backend gone, idle), 2 bad arguments or token file,
3 cannot reach or trust the backend, 130 interrupted.
"""

from __future__ import annotations

import argparse
import os
import sys
from collections.abc import Sequence

from loguru import logger

from .. import i18n
from ..i18n import _, tr
from .auth import is_elevated, read_token_file
from .messages import PROTOCOL_VERSION, Hello
from .service import IDLE_TIMEOUT, IO_TIMEOUT, ChannelClosed, Service
from .transport import connect

CONNECT_TIMEOUT = 10.0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m spaceknife.helper", description=_("SpaceKnife 只读扫描助手（由后端启动）")
    )
    parser.add_argument("--pipe", required=True, help=_("后端的命名管道（Windows）或 unix 套接字路径"))
    parser.add_argument("--token-file", required=True, help=_("一次性令牌文件，读取后删除"))
    parser.add_argument("--server-pid", type=int, required=True, help=_("后端进程号；管道另一端必须是它"))
    parser.add_argument("--idle-timeout", type=float, default=IDLE_TIMEOUT, help=_("空闲多少秒后退出"))
    parser.add_argument("--log-level", default="INFO", choices=["TRACE", "DEBUG", "INFO", "WARNING", "ERROR"])
    return parser


def _setup_logging(level: str) -> None:
    logger.remove()
    stream = sys.stderr
    if stream is None:  # no console (CREATE_NO_WINDOW, pythonw)
        return
    reconfigure = getattr(stream, "reconfigure", None)
    if reconfigure is not None:
        reconfigure(errors="backslashreplace")
    logger.add(
        stream,
        level=level,
        colorize=False,
        format="{time:HH:mm:ss.SSS} | {level:<8} | helper[{process}] - {message}",
    )


def main(argv: Sequence[str] | None = None) -> int:
    # Its own process, often elevated: the user's settings may not be readable, so the system's
    # language (M7.5); the backend shows the helper's error messages as they come.
    i18n.set_language(i18n.system_language())
    args = _parser().parse_args(argv)
    _setup_logging(args.log_level)
    try:
        token = read_token_file(args.token_file)
    except (OSError, ValueError) as exc:
        logger.error(tr("无法读取令牌文件：{reason}", reason=exc))
        return 2
    if not (0 < args.idle_timeout <= 24 * 3600):
        logger.error(_("--idle-timeout 超出范围"))
        return 2
    try:
        channel = connect(args.pipe, CONNECT_TIMEOUT)
    except (OSError, ValueError) as exc:
        logger.error(tr("无法连接后端：{reason}", reason=exc))
        return 3
    try:
        if channel.peer_pid != args.server_pid:
            logger.error(
                tr(
                    "连接的另一端是进程 {peer}，不是后端 {server}；不发送令牌",
                    peer=channel.peer_pid,
                    server=args.server_pid,
                )
            )
            return 3
        elevated = is_elevated()
        service = Service(channel, elevated=elevated, io_timeout=IO_TIMEOUT)
        try:
            service.send(Hello(version=PROTOCOL_VERSION, token=token, pid=os.getpid(), elevated=elevated))
        except ChannelClosed as exc:
            logger.error(tr("发送 hello 失败：{reason}", reason=exc))
            return 3
        logger.debug("connected to backend {} (elevated: {})", args.server_pid, elevated)
        service.serve(args.idle_timeout)
        return 0
    except KeyboardInterrupt:
        return 130
    finally:
        channel.close()


if __name__ == "__main__":
    sys.exit(main())
