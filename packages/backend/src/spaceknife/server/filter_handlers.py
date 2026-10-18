"""Handlers for filter.* (stateless: parsing, colouring, completion, vocabulary)."""

from __future__ import annotations

from typing import TYPE_CHECKING

from spaceknife_protocol.base import Empty
from spaceknife_protocol.filter import FilterParse, FilterParseParams, FilterVocab
from spaceknife_protocol.filter_syntax import parse
from spaceknife_protocol.methods import FILTER_PARSE, FILTER_VOCAB

from ..filter.vocab import platform_vocab

if TYPE_CHECKING:
    from .app import Server
    from .session import Session


def install(server: Server) -> None:
    router = server.router

    @router.method(FILTER_PARSE)
    async def filter_parse(session: Session, params: FilterParseParams) -> FilterParse:
        return parse(params.text, params.cursor, platform_vocab())

    @router.method(FILTER_VOCAB)
    async def filter_vocab(session: Session, _: Empty) -> FilterVocab:
        return platform_vocab()
