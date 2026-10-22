"""SpaceKnife WebSocket protocol v1.

Single source of truth for every message exchanged between the backend and the frontends
(Electron, Qt, WinUI3). See ``docs/protocol.md`` for the wire-level description.
"""

from __future__ import annotations

PROTOCOL_VERSION = "1.2"
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
# Plugins (docs/plugins.md): plugin.* and extensions.* methods, the extensions.changed / plugin.changed /
# plugin.toast events, command actions, report documents, node fields, cleanups and plugin.<id>.*.
CAP_PLUGINS = "plugins.v1"
# Colour modes are open ids (plugins add some); frames may carry the u1 ``pal`` column.
CAP_COLORS = "colors.v1"
# The frontend can host sandboxed web panels (Electron and the browser mode).
CAP_PANELS = "panels.v1"
# What the host offers by itself; a server adds the capabilities of its registered visualizations
# (``layout.registry``), so a plugin's layout needs no entry here. Plugins never get one each.
BASE_CAPABILITIES = (
    CAP_BINARY_FRAMES,
    CAP_SUBSCRIPTIONS,
    CAP_CANCEL,
    CAP_LOG_STREAM,
    CAP_VISUALS,
    CAP_UPDATES,
    CAP_PLUGINS,
    CAP_COLORS,
    CAP_PANELS,
)
# Every capability this version knows: what a client announces by default (packages/client).
KNOWN_CAPABILITIES = (
    CAP_BINARY_FRAMES,
    CAP_SUBSCRIPTIONS,
    CAP_CANCEL,
    CAP_LOG_STREAM,
    CAP_VISUALS,
    CAP_LAYOUT_FLAME,
    CAP_LAYOUT_POLAR,
    CAP_UPDATES,
    CAP_PLUGINS,
    CAP_COLORS,
    CAP_PANELS,
)
ALL_CAPABILITIES = KNOWN_CAPABILITIES  # the older name


def protocol_major(version: str) -> int:
    """Major component of a ``"<major>.<minor>"`` version string."""
    return int(version.split(".", 1)[0])


__all__ = [
    "ALL_CAPABILITIES",
    "BASE_CAPABILITIES",
    "CAP_BINARY_FRAMES",
    "CAP_CANCEL",
    "CAP_COLORS",
    "CAP_LAYOUT_FLAME",
    "CAP_LAYOUT_POLAR",
    "CAP_LOG_STREAM",
    "CAP_PANELS",
    "CAP_PLUGINS",
    "CAP_SUBSCRIPTIONS",
    "CAP_UPDATES",
    "CAP_VISUALS",
    "KNOWN_CAPABILITIES",
    "PROTOCOL_VERSION",
    "SUBPROTOCOL",
    "WS_PATH",
    "protocol_major",
]
