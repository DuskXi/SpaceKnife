"""Insights: known application folders and files — what they are and how to reclaim their space.

The rules library behind the insight card (UI-MAIN-41/42) and the change annotations of the
snapshot comparison (UI-W6-30). Rule format: docs/insights.md; contract: docs/manage.md section 5.
"""

from __future__ import annotations

from .rules import (
    CLEANUPS,
    KINDS,
    Cleanup,
    Insight,
    InsightKind,
    NodeKind,
    Rule,
    RuleProblem,
    RuleSet,
    RuleText,
    active_rules,
    compile_rules,
    load_rules,
    match,
    match_many,
    match_node,
    user_rules_dir,
)

__all__ = [
    "CLEANUPS",
    "KINDS",
    "Cleanup",
    "Insight",
    "InsightKind",
    "NodeKind",
    "Rule",
    "RuleProblem",
    "RuleSet",
    "RuleText",
    "active_rules",
    "compile_rules",
    "load_rules",
    "match",
    "match_many",
    "match_node",
    "user_rules_dir",
]
