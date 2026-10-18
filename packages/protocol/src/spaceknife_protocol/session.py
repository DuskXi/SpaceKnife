"""Session handshake models (``session.hello``)."""

from __future__ import annotations

from typing import Literal

from pydantic import Field

from .base import Params, Payload

ClientKind = Literal["electron", "qt", "winui", "cli", "test", "other"]


class ClientInfo(Params):
    name: str = Field(min_length=1, max_length=64)
    version: str = Field(min_length=1, max_length=32)
    kind: ClientKind


class HelloParams(Params):
    """First request on every connection; anything else before it closes the socket."""

    token: str = Field(
        min_length=16, max_length=256, description="Per-launch secret handed to the client out of band"
    )
    protocol: str = Field(pattern=r"^\d+\.\d+$", description="Protocol version the client speaks, e.g. '1.0'")
    client: ClientInfo
    capabilities: list[str] = Field(default_factory=list, max_length=64)


class ServerInfo(Payload):
    name: str
    version: str
    python: str
    platform: str


class Limits(Payload):
    max_inbound_message_bytes: int
    max_outbound_chunk_bytes: int
    hello_timeout_s: float


class HelloResult(Payload):
    protocol: str
    server: ServerInfo
    session_id: str
    capabilities: list[str] = Field(description="Intersection of client and server capabilities")
    schema_hash: str
    limits: Limits
