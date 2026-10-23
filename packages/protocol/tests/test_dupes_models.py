"""dupes.v1 models (docs/protocol.md §dupes): round trips, the capability, what older peers see."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from spaceknife_protocol import ALL_CAPABILITIES, CAP_DUPES
from spaceknife_protocol import dupes as pd
from spaceknife_protocol import ops as po
from spaceknife_protocol import scan as ps
from spaceknife_protocol import view as pv
from spaceknife_protocol.methods import EVENTS, METHODS


def test_the_capability_methods_and_events_are_registered() -> None:
    assert CAP_DUPES == "dupes.v1" and CAP_DUPES in ALL_CAPABILITIES
    names = set(METHODS)
    for m in ("plan", "start", "pause", "resume", "cancel", "status", "set_load", "groups", "group"):
        assert f"dupes.{m}" in names
    for m in ("folder", "folders", "locate", "keep", "clear"):
        assert f"dupes.{m}" in names
    assert {"dupes.progress", "dupes.changed"} <= set(EVENTS)


def test_a_status_and_a_plan_round_trip() -> None:
    st = pd.DupesStatus(
        job_id="dupes-1",
        state="verifying",
        scan_ids=["a", "b"],
        mode="against",
        node=7,
        cross_volume=True,
        funnel=pd.DupesFunnel(candidates=10, confirmed_groups=2),
        ledger=[pd.DupesLedgerVolume(volume="D:", extra=3, reclaim=1 << 30)],
        skipped_by_reason={"cloud": 2},
    )
    again = pd.DupesStatus.model_validate_json(st.model_dump_json())
    assert again == st and again.min_size == 1 << 20
    plan = pd.DupesPlan(
        candidates=1,
        groups=1,
        bytes=1,
        probe_bytes=1,
        full_bytes_max=1,
        reclaim_max=1,
        excluded={},
        current=st,
    )
    assert pd.DupesPlan.model_validate_json(plan.model_dump_json()).current == st


def test_scopes_are_bounded() -> None:
    with pytest.raises(ValidationError):
        pd.DupesScope(scan_ids=[])
    with pytest.raises(ValidationError):
        pd.DupesScope(scan_ids=[f"s{i}" for i in range(17)])
    with pytest.raises(ValidationError):
        pd.DupesLocateParams(job_id="j", view_id="v", group=0, limit=2001)
    with pytest.raises(ValidationError):
        pd.DupesGroupsParams(job_id="j", limit=501)


def test_older_peers_see_nothing_new() -> None:
    # a view state, a tree node and a basket item without the new fields still parse
    st = pv.ViewState.model_json_schema()
    assert "has_dupes" in st["properties"] and "has_dupes" not in st.get("required", [])
    node = ps.TreeNodeResult.model_json_schema()
    assert "dupe" in node["properties"] and "dupe" not in node.get("required", [])
    # the new colour mode, status and skip reasons; no basket item of duplicate files (no one-click
    # deletion: cleaning up goes through the cleanup plan)
    assert "dupes" in pv.ColorModeName.__args__  # type: ignore[attr-defined]
    item_kind = po.BasketItem.model_json_schema()["properties"]["kind"]
    assert "dupes" not in str(item_kind)
    status = po.BasketItem.model_json_schema()["properties"]["status"]
    assert "link_target" in str(status) and "blocked" not in str(status)
    code = po.OpItemResult.model_json_schema()["properties"]["code"]
    for c in ("changed", "keeper_changed", "last_copy", "keeper_busy"):
        assert c in str(code)
