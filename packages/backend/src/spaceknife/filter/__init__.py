"""Filter engine: SKF 1 expressions (``spaceknife_protocol.filter_syntax``) evaluated on node columns."""

from __future__ import annotations

from .engine import FilterContext, FilterResult, evaluate

__all__ = ["FilterContext", "FilterResult", "evaluate"]
