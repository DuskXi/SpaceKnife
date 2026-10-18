"""Error codes (JSON-RPC 2.0 + SpaceKnife extensions) and WebSocket close codes."""

from __future__ import annotations

from enum import IntEnum


class ErrorCode(IntEnum):
    # JSON-RPC 2.0 standard codes
    PARSE_ERROR = -32700
    INVALID_REQUEST = -32600
    METHOD_NOT_FOUND = -32601
    INVALID_PARAMS = -32602
    INTERNAL_ERROR = -32603

    # SpaceKnife server errors (-32000 .. -32099)
    UNAUTHORIZED = -32001
    NOT_HANDSHAKEN = -32002
    PROTOCOL_MISMATCH = -32003
    UNKNOWN_TOPIC = -32004
    UNKNOWN_SUBSCRIPTION = -32005
    SHUTTING_DOWN = -32006
    UNKNOWN_SCAN = -32007
    SCAN_FAILED = -32008
    SNAPSHOT_ERROR = -32009
    UNKNOWN_NODE = -32010
    SCAN_BUSY = -32011
    UNKNOWN_VIEW = -32012
    SETTINGS_CONFLICT = -32013

    # Same value LSP uses for a request cancelled by the client
    REQUEST_CANCELLED = -32800


class CloseCode(IntEnum):
    """Application close codes (4000-4999 are reserved for applications by RFC 6455)."""

    NORMAL = 1000
    GOING_AWAY = 1001
    PROTOCOL_ERROR = 4400
    UNAUTHORIZED = 4401
    HANDSHAKE_TIMEOUT = 4408
    VERSION_MISMATCH = 4426


class ProtocolError(Exception):
    """Raised by codecs when a message or frame does not follow the protocol."""
