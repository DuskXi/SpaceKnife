"""Command-line entry point (``spaceknife``)."""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path
from typing import Annotated

import typer
from rich.console import Console

from . import __version__
from .log import LogFormat, setup_logging
from .logbus import LogHub

app = typer.Typer(
    name="spaceknife",
    help="SpaceKnife backend: disk space scanning, analysis and the WebSocket server for the UIs.",
    add_completion=False,
    no_args_is_help=True,
    rich_markup_mode="rich",
)


@app.command()
def serve(
    host: Annotated[
        str, typer.Option(help="Interface to bind; keep loopback unless you know why.")
    ] = "127.0.0.1",
    port: Annotated[
        int, typer.Option(help="TCP port; 0 picks a free port (reported in the ready line).")
    ] = 0,
    token_stdin: Annotated[
        bool,
        typer.Option(
            "--token-stdin", help="Read the session token from the first stdin line; exit on stdin EOF."
        ),
    ] = False,
    allow_origin: Annotated[
        list[str] | None,
        typer.Option(help="Extra allowed WebSocket Origin (repeatable), e.g. a dev server URL."),
    ] = None,
    hello_timeout: Annotated[float, typer.Option(help="Seconds a client has to send session.hello.")] = 5.0,
    log_level: Annotated[str, typer.Option(help="TRACE, DEBUG, INFO, WARNING, ERROR.")] = "INFO",
    log_format: Annotated[LogFormat, typer.Option(help="Log format on stderr.")] = LogFormat.RICH,
    ready_file: Annotated[
        Path | None,
        typer.Option(
            help="Also write the ready info, including the token, to this file (owner-only; removed on exit)."
        ),
    ] = None,
) -> None:
    """Run the WebSocket backend. Prints one JSON 'ready' line on stdout once listening."""
    from .server.app import DEFAULT_ORIGINS, ServerConfig, new_token
    from .server.runner import read_token_line, run_server

    hub = LogHub()
    setup_logging(log_level.upper(), log_format, hub=hub)
    if token_stdin:
        token, generated = read_token_line(sys.stdin.buffer), False
    else:
        token, generated = new_token(), True
    origins = DEFAULT_ORIGINS + tuple(allow_origin or ())
    config = ServerConfig(
        token=token,
        host=host,
        port=port,
        allowed_origins=origins,
        hello_timeout=hello_timeout,
        log_file=True,
    )
    try:
        code = asyncio.run(
            run_server(
                config,
                log_hub=hub,
                ready_out=sys.stdout,
                stdin=sys.stdin.buffer if token_stdin else None,
                token_generated=generated,
                ready_file=ready_file,
            )
        )
    except KeyboardInterrupt:
        code = 0
    raise typer.Exit(code)


@app.command()
def version() -> None:
    """Print the backend, protocol and native-extension versions."""
    from spaceknife_protocol import PROTOCOL_VERSION
    from spaceknife_protocol.schema import schema_hash

    from . import native

    Console().print(
        f"spaceknife {__version__} · protocol {PROTOCOL_VERSION} · schema {schema_hash()}"
        f" · native {native.version() or 'none'}"
    )


@app.command()
def schema(
    output: Annotated[
        Path | None, typer.Option(help="Write the protocol JSON Schema here (default: stdout).")
    ] = None,
    check: Annotated[
        Path | None,
        typer.Option(help="Exit 1 if this file differs from the current schema (CI drift check)."),
    ] = None,
) -> None:
    """Export the protocol JSON Schema (source for TypeScript codegen)."""
    from spaceknife_protocol.schema import export_schema

    text = json.dumps(export_schema(), indent=2, ensure_ascii=False, sort_keys=True) + "\n"
    if check is not None:
        current = check.read_text(encoding="utf-8") if check.exists() else ""
        if current != text:
            Console(stderr=True).print(
                f"[red]{check} is out of date; run `spaceknife schema --output {check}`[/red]"
            )
            raise typer.Exit(1)
        return
    if output is None:
        sys.stdout.write(text)
    else:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(text, encoding="utf-8", newline="\n")


from . import cli_cleanup, cli_dupes, cli_run, cli_scan  # noqa: E402

app.command()(cli_scan.scan)
app.command()(cli_dupes.dupes)
app.command(name="dupes-cleanup")(cli_cleanup.dupes_cleanup)
app.command("run", context_settings=cli_run.CONTEXT_SETTINGS)(cli_run.run)
app.add_typer(cli_scan.snapshot_app, name="snapshot")


def _force_utf8_pipes() -> None:
    """Piped stdout/stderr default to the ANSI code page on Windows (e.g. GBK); parents expect UTF-8."""
    for stream in (sys.stdout, sys.stderr):
        if not stream.isatty() and hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")


def main() -> None:
    _force_utf8_pipes()
    from .io.priority import no_cloud_downloads

    no_cloud_downloads()  # macOS: never fetch iCloud / File Provider contents (every command)
    app()
