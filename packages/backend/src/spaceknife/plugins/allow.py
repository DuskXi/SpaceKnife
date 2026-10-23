"""The built-in methods plugin code (``ctx.call``) and web panels (``sk.core``) may call.

Everything else is refused — in particular ``basket.execute`` and every other way to delete, the
settings, the scans' lifecycle, sessions and the app itself (the tests pin the refusals). Plugins
put files in the delete basket; only the user deletes.
"""

from __future__ import annotations

from spaceknife_protocol.plugins import Permission

# method -> the permission it needs (None: none)
ALLOWED: dict[str, Permission | None] = {
    "tree.node": "tree.read",
    "tree.children": "tree.read",
    "tree.summary": "tree.read",
    "tree.find": "tree.read",
    "scan.list": None,
    "scan.status": None,
    "volume.list": None,
    "view.list": None,
    "visual.list": None,
    "filter.parse": None,
    "filter.vocab": None,
    "classes.get": None,
    "export.targets": None,
    "snapshot.list": "snapshots.read",
    "compare.info": "tree.read",
    "compare.summary": "tree.read",
    "compare.node": "tree.read",
    "basket.list": None,
    "basket.add": "basket.propose",
    "insight.node": "tree.read",
    "insight.nodes": "tree.read",
    "extensions.get": None,
}

# never callable by a plugin, whatever it was granted (the tests check each one)
REFUSED = (
    "basket.execute",
    "basket.remove",
    "basket.clear",
    "ops.cancel",
    "ops.undo",
    "app.shutdown",
    "system.info",
    "system.maintenance",
    "system.helper_stop",
    "system.cache_clear",
    "settings.update",
    "settings.import",
    "settings.reset",
    "settings.export",
    "update.download",
    "update.check",
    "snapshot.load",
    "snapshot.save",
    "classes.update",
    "classes.reset",
    "classes.folder",
    "session.hello",
    "scan.start",
    "scan.cancel",
    "scan.close",
    "plugin.enable",
    "plugin.install",
    "plugin.uninstall",
    "plugin.link",
)


def permission_for(method: str) -> tuple[bool, Permission | None]:
    """(allowed at all, the permission it needs)."""
    if method not in ALLOWED:
        return False, None
    return True, ALLOWED[method]


__all__ = ["ALLOWED", "REFUSED", "permission_for"]
