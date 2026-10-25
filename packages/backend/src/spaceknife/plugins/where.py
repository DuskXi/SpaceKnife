"""Where a plugin call comes from: the context every call carries, built in this one place.

Every call the host makes into a plugin (``PluginHost.call``) takes a :class:`Where`, and only
:class:`Places` makes them, one method per place a call comes from: a frontend request (a command's
``call`` action, a report, a cleanup, a panel's ``sk.call``, a stream), the palette's context (a
command's ``check``), the details panel and the hover card (fields, insights), a frame being drawn
(node sets, colours, code visualizations, layouts), an export, a hook, a background task, a scan
source and its watcher, a comparison (change annotators). A :class:`Where` says

- what ``ctx.tree`` is (:data:`TreeOf`): the call's scan, the scan in front for a call about no
  scan, or why there is none (a closed scan, a comparison's change tree, a tree still being listed);
- the epoch of the node ids the call carries (``ctx.tree`` is that tree, else ``STALE_TREE``);
- the view, node, selection, zoom root, filter, args and comparison it is about (``ctx.where``, an
  ``ActionContext``) and its language.

docs/plugin-api.md 「调用的上下文」 lists what each kind of call gets; tests/test_plugin_where.py
calls every kind with two scans open.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass, replace
from typing import TYPE_CHECKING, Any, Literal

from .. import i18n

if TYPE_CHECKING:
    from ..store.nodes import NodeStore
    from .hostapi import HostApi

TreeOf = Literal["scan", "front", "closed", "compare", "listing"]
"""What ``ctx.tree`` of a call is: ``scan`` the scan ``scan_id``; ``front`` the scan in front (a
call about no scan: settings, language, basket, start, snapshots hooks, a legend); none, and why:
``closed`` the scan ``scan_id`` was closed (``scan_closed``, a frame of a scan closed meanwhile),
``compare`` the call is about a comparison's change tree (``compare_id``; its node ids are not a
scan's, ``scan_id`` is the scan the comparison was opened from), ``listing`` a scan source is
listing the tree of ``scan_id``."""

NOW: Literal["now"] = "now"  # an epoch taken when the call starts (ids the frontend took of the tree now)
MAX_SELECTION = 1000  # ActionContext.selection
MAX_FILTER = 4096
MAX_ID = 64
CONTEXT_KEYS = ("scan_id", "view_id", "node", "selection", "zoom_node", "filter", "args", "compare_id")
WHERE_KEY = "$where"  # a request's own context, apart from its params (a panel's sk.call, sk.subscribe)


def _str(v: object, limit: int = MAX_ID) -> str | None:
    return v[:limit] if isinstance(v, str) and v else None


def _int(v: object) -> int | None:
    return int(v) if isinstance(v, int) and not isinstance(v, bool) else None


def _ints(v: object) -> tuple[int, ...]:
    if not isinstance(v, list | tuple):
        return ()
    return tuple(int(x) for x in v[:MAX_SELECTION] if isinstance(x, int) and not isinstance(x, bool))


@dataclass(frozen=True, slots=True)
class Where:
    """The context of one call (see the module's docstring); made by :class:`Places` only."""

    tree: TreeOf = "front"
    scan_id: str | None = None
    epoch: int | Literal["now"] | None = None  # of the node ids the call carries; None: not pinned
    view_id: str | None = None
    node: int | None = None
    selection: tuple[int, ...] = ()
    zoom_node: int | None = None
    filter: str = ""
    args: Mapping[str, Any] | None = None
    compare_id: str | None = None
    lang: str | None = None  # None: the interface language when the call starts
    slot: str | None = None  # a node field: "inspector" or "hover"

    def at(self, epoch: int | None) -> Where:
        """The same place, its node ids of ``epoch`` (a hook called again with the new ids)."""
        return replace(self, epoch=epoch)

    def action(self) -> dict[str, Any]:
        """As an ``ActionContext`` (``ctx.where``; the params of a command's ``check``)."""
        return {
            "scan_id": self.scan_id,
            "view_id": self.view_id,
            "node": self.node,
            "selection": list(self.selection),
            "compare_id": self.compare_id,
            "args": dict(self.args or {}),
            "zoom_node": self.zoom_node,
            "filter": self.filter,
        }

    def wire(self, api: HostApi) -> dict[str, Any]:
        """What the plugin's runtime is told (``CallInfo``): the epoch ``now`` taken here."""
        out: dict[str, Any] = {"lang": self.lang or i18n.language()}
        if self.tree != "front":
            out["tree"] = self.tree
        epoch = self.epoch
        if epoch == NOW:
            store = api.store_of(self.scan_id) if self.tree == "scan" and self.scan_id else None
            epoch = int(getattr(store, "epoch", 0)) if store is not None else None
        if epoch is not None:
            out["epoch"] = int(epoch)
        for key in ("scan_id", "view_id", "node", "zoom_node", "compare_id", "slot"):
            value = getattr(self, key)
            if value is not None:
                out[key] = value
        if self.selection:
            out["selection"] = list(self.selection)
        if self.filter:
            out["filter"] = self.filter
        if self.args:
            out["args"] = dict(self.args)
        return out


FRONT = Where()


class Places:
    """Makes the :class:`Where` of each kind of call, from where it comes from."""

    def __init__(self, api: HostApi) -> None:
        self.api = api

    # ------------------------------------------------------------- what a scan id names

    def _compared(self, compare_id: str) -> tuple[TreeOf, str | None, str | None]:
        """A comparison's change tree: no scan tree (its node ids are not a scan's); the scan
        it was opened from goes along (``ctx.scan(ctx.where.scan_id)`` reads that one)."""
        h = self.api.compare_of(compare_id)
        return "compare", (str(h.scan_id) if h is not None else None), compare_id

    def _named(
        self, scan_id: str | None, view_id: str | None = None
    ) -> tuple[TreeOf, str | None, str | None]:
        """(tree, scan id, comparison id) of what a frontend's request is about: a scan, or a
        comparison (views open those like scans; the comparison panel names the tab's scan and
        the compare view: either way its node ids are the change tree's)."""
        if (
            scan_id is not None
            and self.api.store_of(scan_id) is None
            and self.api.compare_of(scan_id) is not None
        ):
            return self._compared(scan_id)
        shown = self.api.view_store(view_id) if view_id else None
        cid = self.api.compare_of_store(shown) if shown is not None else None
        if cid is not None:
            return self._compared(cid)
        if scan_id is None:
            sid = self.api.scan_of_store(shown) if shown is not None else None
            return ("scan", sid, None) if sid is not None else ("front", None, None)
        return "scan", scan_id, None  # (an unknown one: ctx.tree says so when it is read)

    def _in_view(
        self, view_id: str | None, tree: TreeOf, scan: str | None, compare: str | None
    ) -> dict[str, Any]:
        """Where the user is in the view a request names: its zoom root, filter (when one is
        applied) and selection as they are now, for what the request leaves out (a method, a
        report or a stream called with only ``view_id``). Only a view of the tree the request is
        about (another tree's node ids would name other items)."""
        view = self.api.view_of(view_id) if view_id else None
        if view is None:
            return {}
        if tree == "scan":
            if scan is None or self.api.store_of(scan) is not view.store:
                return {}
        elif tree != "compare" or compare is None or self.api.compare_of_store(view.store) != compare:
            return {}
        filt = view.filter
        out: dict[str, Any] = {"zoom_node": int(view.zoom.node), "filter": filt.text if filt.active else ""}
        picked = view.selected
        out["selection"] = [int(picked)] if picked is not None and 0 < picked < len(view.store) else []
        return out

    def _of_store(self, store: NodeStore, scan_id: str | None = None) -> Where:
        """The tree a frame, an export or a node set is about, by its store."""
        if scan_id is not None and self.api.store_of(scan_id) is store:
            sid: str | None = scan_id
        else:
            sid = self.api.scan_of_store(store)
        if sid is not None:
            return Where("scan", sid, epoch=int(getattr(store, "epoch", 0)))
        cid = self.api.compare_of_store(store)
        if cid is not None:
            tree, scan, compare = self._compared(cid)
            return Where(tree, scan, compare_id=compare)
        return Where("closed", scan_id)  # neither: the scan was closed meanwhile

    # ------------------------------------------------------------- the places

    def request(self, body: Mapping[str, Any] | None, *, long: bool = False) -> Where:
        """A frontend's request: a command's ``call`` action (its ``ActionContext``), a report or
        a cleanup (``ReportContext``), a panel's call or stream (its context in ``$where``, under
        what the params name themselves), a stream's params. A view it names without saying
        where the user is in it adds its zoom root, filter and selection; a view without a scan
        is about the view's tree. Node ids are of the tree as it is when the call starts; a
        stream's tree is not pinned."""
        given = dict(body or {})
        own = given.get(WHERE_KEY)
        where = given
        if isinstance(own, dict):  # what the params name themselves wins
            where = {**own, **{k: given[k] for k in CONTEXT_KEYS if k in given}}
        view_id = _str(where.get("view_id"))
        tree, scan, compare = self._named(_str(where.get("scan_id")), view_id)
        where = {**self._in_view(view_id, tree, scan, compare), **where}
        args = where.get("args")
        return Where(
            tree,
            scan,
            epoch=NOW if tree == "scan" and not long else None,
            view_id=view_id,
            node=_int(where.get("node")),
            selection=_ints(where.get("selection")),
            zoom_node=_int(where.get("zoom_node")),
            filter=_str(where.get("filter"), MAX_FILTER) or "",
            args=dict(args) if isinstance(args, dict) else None,
            compare_id=compare or _str(where.get("compare_id")),
        )

    def palette(self, c: Any) -> Where:
        """The palette's context (the palette, a context menu, the details panel's actions): a
        command's ``check``. The view's scan (whose node ids these are) and where the user is in
        it; without a view, the tab's scan and the node the commands are about (the details
        panel's)."""
        view = getattr(c, "view", None)
        if view is None:
            sid = getattr(c, "scan_id", None)
            store = self.api.store_of(sid) if sid else None
            node = _int(getattr(c, "node", None))
            if store is None or node is None or not 0 < node < len(store):
                node = None
            return self.request({"scan_id": sid, "node": node, "selection": [node] if node else []})
        node = getattr(c, "selected_node", None)
        filt = view.filter
        return replace(
            self._of_store(view.store, getattr(c, "scan_id", None)),
            view_id=view.id,
            node=node,
            selection=(node,) if node is not None else (),
            zoom_node=int(view.zoom.node),
            filter=filt.text if filt.active else "",
        )

    def node(self, scan_id: str, node: int, *, slot: str | None = None) -> Where:
        """A node of a scan: a field (``slot``: the details panel or the hover card), an insight."""
        tree, scan, compare = self._named(scan_id)
        return Where(
            tree,
            scan,
            epoch=NOW if tree == "scan" else None,
            node=node,
            selection=(node,),
            compare_id=compare,
            slot=slot,
        )

    def view(self, view: Any) -> Where:
        """A frame of a view being drawn: a code colour mode (its rows' node ids)."""
        filt = view.filter
        return replace(
            self._of_store(view.store),
            view_id=view.id,
            zoom_node=int(view.zoom.node),
            filter=filt.text if filt.active else "",
        )

    def store(self, store: NodeStore, scan_id: str | None = None) -> Where:
        """A tree as a whole: a node set, a code visualization's weights, a layout's blocks."""
        return self._of_store(store, scan_id)

    def export(
        self, store: NodeStore, root: int, *, selection: Sequence[int] = (), filter_text: str = ""
    ) -> Where:
        """An export: the tree below ``root``, the selection and the filter it was made with."""
        picked = tuple(int(n) for n in selection[:MAX_SELECTION])
        return replace(
            self._of_store(store),
            node=int(root),
            selection=picked,
            filter=filter_text[:MAX_FILTER],
        )

    def hook(self, event: str, args: Mapping[str, Any]) -> Where:
        """A hook: ``scan_done`` about its scan; ``scan_closed`` about a scan whose tree is gone;
        ``tree_changed`` see :meth:`tree_change`; the others are about no scan."""
        scan = args.get("scan")
        sid = _str(scan.get("scan_id")) if isinstance(scan, Mapping) else None
        if event == "scan_done" and sid:
            return Where("scan", sid, epoch=NOW)
        if event == "scan_closed" and sid:
            return Where("closed", sid)
        if event == "tree_changed":
            change = args.get("change")
            if isinstance(change, Mapping) and _str(change.get("scan_id")):
                return self.tree_change(str(change["scan_id"]), _int(change.get("epoch")))
        return FRONT

    def tree_change(self, scan_id: str, epoch: int | None) -> Where:
        """``tree_changed``: the scan, its tree of the epoch of ``change.folders``."""
        return Where("scan", scan_id, epoch=epoch)

    def task(self, scan_id: str | None) -> Where:
        """A background task: the scan of the call that started it (none: the scan in front; a
        scan closed since, started again after a crash: no tree), not pinned (it runs long; the
        tree follows the newest epoch)."""
        if not scan_id:
            return FRONT
        return Where("scan" if self.api.store_of(scan_id) is not None else "closed", scan_id)

    def watch(self, scan_id: str) -> Where:
        """A scan source's watcher: the scan it watches, not pinned."""
        return Where("scan", scan_id)

    def listing(self, scan_id: str | None) -> Where:
        """A scan source listing a tree: there is none to read yet (``scan_id``: the scan it fills)."""
        return Where("listing", scan_id)

    def comparison(self, compare_id: str | None, scan_id: str | None = None) -> Where:
        """A change annotator: the scan the comparison was opened from (its items carry paths, no
        node ids: not pinned), the comparison."""
        h = self.api.compare_of(compare_id) if compare_id else None
        sid = str(h.scan_id) if h is not None else scan_id
        if sid is None:
            return replace(FRONT, compare_id=compare_id)
        tree: TreeOf = "scan" if self.api.store_of(sid) is not None else "closed"
        return Where(tree, sid, compare_id=compare_id)

    def nowhere(self, *, lang: str | None = None) -> Where:
        """A call about no scan (a legend in ``lang``; the start, stop, settings, language, basket
        and snapshots hooks): ``ctx.tree`` is the scan in front."""
        return FRONT if lang is None else Where(lang=lang)


__all__ = ["CONTEXT_KEYS", "FRONT", "NOW", "WHERE_KEY", "Places", "TreeOf", "Where"]
