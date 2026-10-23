"""SpaceKnife WebSocket protocol v1.

Single source of truth for every message exchanged between the backend and the frontends
(Electron, Qt, WinUI3). See ``docs/protocol.md`` for the wire-level description.
"""

from __future__ import annotations

PROTOCOL_VERSION = "1.1"
SUBPROTOCOL = "spaceknife.v1"
WS_PATH = "/v1"

# Capabilities a server may advertise; the handshake result carries the intersection.
CAP_BINARY_FRAMES = "binary-frames.v1"
CAP_SUBSCRIPTIONS = "subscriptions"
CAP_CANCEL = "cancel"
CAP_LOG_STREAM = "log-stream"
# Visualizations: the ``visual`` view setting, ``visual.list``, FrameInfo.visual/layout/geometry/root,
# the ``branch`` colour mode and column. Each non-treemap layout is negotiated on its own.
CAP_VISUALS = "visuals.v1"
CAP_LAYOUT_FLAME = "layout.flame.v1"
CAP_LAYOUT_POLAR = "layout.polar.v1"
# Update checks against the GitHub releases: update.* and the update.changed event.
CAP_UPDATES = "updates.v1"
# Duplicate files: dupes.*, the dupes.* events, the ``dupes`` colour mode with the ``dup`` / ``dupg``
# frame columns, TreeNodeResult.dupe and the basket's ``dupes`` items.
CAP_DUPES = "dupes.v1"
ALL_CAPABILITIES = (
    CAP_BINARY_FRAMES,
    CAP_SUBSCRIPTIONS,
    CAP_CANCEL,
    CAP_LOG_STREAM,
    CAP_VISUALS,
    CAP_LAYOUT_FLAME,
    CAP_LAYOUT_POLAR,
    CAP_UPDATES,
    CAP_DUPES,
)


def protocol_major(version: str) -> int:
    """Major component of a ``"<major>.<minor>"`` version string."""
    return int(version.split(".", 1)[0])


__all__ = [
    "ALL_CAPABILITIES",
    "CAP_BINARY_FRAMES",
    "CAP_CANCEL",
    "CAP_LAYOUT_FLAME",
    "CAP_LAYOUT_POLAR",
    "CAP_LOG_STREAM",
    "CAP_SUBSCRIPTIONS",
    "CAP_UPDATES",
    "CAP_VISUALS",
    "PROTOCOL_VERSION",
    "SUBPROTOCOL",
    "WS_PATH",
    "protocol_major",
]
