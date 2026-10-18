"""Base model classes for every protocol message.

Two flavours exist on purpose:

* :class:`Params` — payloads the *server* receives (request params). Unknown fields are
  rejected so typos and version skew surface immediately instead of being silently dropped.
* :class:`Payload` — payloads the *clients* receive (results and events). Unknown fields are
  ignored so an older client keeps working when a newer server adds optional fields
  (new behaviour itself is gated by capability negotiation, never by new fields alone).
"""

from __future__ import annotations

from pydantic import BaseModel, ConfigDict


class Params(BaseModel):
    """Inbound (client → server) payload: strict."""

    model_config = ConfigDict(extra="forbid", frozen=True)


class Payload(BaseModel):
    """Outbound (server → client) payload: tolerant of fields added by newer servers."""

    model_config = ConfigDict(extra="ignore", frozen=True)


class Empty(Params):
    """Params for methods that take no arguments."""


class EmptyResult(Payload):
    """Result for methods that return nothing but an acknowledgement."""
