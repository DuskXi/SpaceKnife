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
    # plugins (docs/plugins.md)
    PERMISSION_DENIED = -32014  # missing permission, or a method a plugin may not call (data.permission)
    PLUGIN_TIMEOUT = -32015
    PLUGIN_UNAVAILABLE = -32016  # its process could not start, crashed or hung
    PLUGIN_DISABLED = -32017  # not enabled, changed since confirmed, or incompatible
    STALE_TREE = -32018  # a node id from before the tree was reorganised (its epoch moved on)
    PLUGIN_ERROR = -32019  # the plugin reported an error; message is for the user
    PLUGIN_LIMIT = -32020  # too many calls in flight or queued, or an output past its limit

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
