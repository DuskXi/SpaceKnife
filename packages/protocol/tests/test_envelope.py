from __future__ import annotations

import json

import pytest

from spaceknife_protocol.envelope import (
    MAX_SAFE_INTEGER,
    ErrorObject,
    ErrorResponse,
    Notification,
    Request,
    SuccessResponse,
    dump_message,
    parse_message,
)
from spaceknife_protocol.errors import ProtocolError


def test_parses_each_kind() -> None:
    assert isinstance(parse_message('{"jsonrpc":"2.0","id":1,"method":"system.ping"}'), Request)
    assert isinstance(parse_message('{"jsonrpc":"2.0","method":"$/cancel","params":{"id":1}}'), Notification)
    assert isinstance(parse_message('{"jsonrpc":"2.0","id":"a","result":null}'), SuccessResponse)
    err = parse_message('{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad"}}')
    assert isinstance(err, ErrorResponse)
    assert err.id is None


@pytest.mark.parametrize(
    "raw",
    [
        "not json",
        "[]",
        '{"foo": 1}',
        '{"jsonrpc":"1.0","id":1,"method":"x"}',
        '{"jsonrpc":"2.0","id":1,"method":""}',
        '{"jsonrpc":"2.0","id":1,"method":"x","extra":true}',
        f'{{"jsonrpc":"2.0","id":{MAX_SAFE_INTEGER + 1},"method":"x"}}',
        '{"jsonrpc":"2.0","id":-1,"method":"x"}',
        '{"jsonrpc":"2.0","id":1,"method":"x","params":[1,2]}',
    ],
)
def test_rejects_invalid_frames(raw: str) -> None:
    with pytest.raises(ProtocolError):
        parse_message(raw)


def test_dump_omits_absent_params_but_keeps_required_nulls() -> None:
    assert json.loads(dump_message(Request(id=1, method="system.ping"))) == {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "system.ping",
    }
    assert json.loads(dump_message(SuccessResponse(id=1))) == {"jsonrpc": "2.0", "id": 1, "result": None}
    error = json.loads(dump_message(ErrorResponse(id=None, error=ErrorObject(code=-32600, message="x"))))
    assert error == {"jsonrpc": "2.0", "id": None, "error": {"code": -32600, "message": "x"}}


def test_roundtrip() -> None:
    msg = Notification(method="log.record", params={"sub": "s1", "message": "中文日志"})
    assert parse_message(dump_message(msg)) == msg
