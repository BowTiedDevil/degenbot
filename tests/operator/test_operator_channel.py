"""Operator command channel (NWTUM3): wire round-trip, family mapping, errors.

Covers :mod:`degenbot.operator.operator_channel` — the Unix-domain-socket
JSON-lines channel an operator uses to add a path / trigger discovery on a live
bot, plus the CLI client helper (:func:`send_command`). The server side is
exercised with a stub handler; the real host handler (wiring into
`BackrunSession.enqueue_path` / `trigger_discovery`) is exercised at the
example/session layer in `tests/arbitrage/test_backrun_session.py`.
"""

import asyncio
import json

import pytest

from degenbot.database.models.pools import (
    UniswapV2PoolTableBase,
    UniswapV3PoolTableBase,
    UniswapV4PoolTableBase,
)
from degenbot.operator.operator_channel import (
    OperatorServer,
    send_command,
    step_from_wire,
    wrap_handler,
)


def _stub_handler(*, fail_on=None):
    """Return a stub host ``(op, payload) -> response`` handler."""
    seen = {"add_path": [], "discover": []}

    async def handler(op, payload):
        await asyncio.sleep(0)
        seen[op].append(payload)
        if fail_on and op == fail_on:
            msg = "boom"
            raise RuntimeError(msg)
        if op == "add_path":
            steps = [step_from_wire(s) for s in payload["steps"]]
            return {"detail": f"enqueued {len(steps)}-hop path"}
        if op == "discover":
            return {"detail": f"discovery processed {payload.get('bound', 0)}"}
        return {"error": f"unknown op {op}"}

    return handler, seen


def test_step_from_wire_maps_families_and_rejects_bad_input() -> None:
    """Family strings map to the right pool-table base classes; bad input raises."""
    assert step_from_wire({"family": "V2", "address": "0xaa"}).type is UniswapV2PoolTableBase
    assert step_from_wire({"family": "V3", "address": "0xaa"}).type is UniswapV3PoolTableBase
    v4 = step_from_wire({"family": "V4", "address": "0x00", "hash": "0x" + "a" * 64})
    assert v4.type is UniswapV4PoolTableBase
    assert v4.hash == "0x" + "a" * 64

    with pytest.raises(ValueError, match="unknown pool family"):
        step_from_wire({"family": "V5", "address": "0xaa"})
    with pytest.raises(ValueError, match="missing an address"):
        step_from_wire({"family": "V2"})


def test_decode_request_type_error_for_non_dict_payload() -> None:
    """A non-dict ``payload`` is rejected as a TypeError (not ValueError)."""
    from degenbot.operator.operator_channel import _decode_request

    with pytest.raises(TypeError, match="must be a dict"):
        _decode_request(b'{"op":"add_path","payload":[1,2,3]}')


async def test_add_path_and_discover_round_trip(tmp_path) -> None:
    """A full client->server round trip for both operator commands."""
    handler, seen = _stub_handler()
    socket_path = str(tmp_path / "bot.sock")
    server = OperatorServer(handler, socket_path=socket_path)
    task = asyncio.create_task(server.serve())
    try:
        # brief yield so the server binds the socket
        for _ in range(50):
            if _socket_bound(socket_path):
                break
            await asyncio.sleep(0.01)

        resp = await send_command(
            socket_path,
            "add_path",
            {
                "steps": [
                    {"family": "V3", "address": "0x" + "1" * 40},
                    {"family": "V4", "address": "0x" + "2" * 40, "hash": "0x" + "a" * 64},
                ],
                "directions": [True, False],
            },
        )
        assert resp == {"ok": True, "detail": "enqueued 2-hop path"}
        assert seen["add_path"] == [
            {
                "steps": [
                    {"family": "V3", "address": "0x" + "1" * 40},
                    {"family": "V4", "address": "0x" + "2" * 40, "hash": "0x" + "a" * 64},
                ],
                "directions": [True, False],
            }
        ]

        resp = await send_command(socket_path, "discover", {"bound": 5})
        assert resp == {"ok": True, "detail": "discovery processed 5"}
        assert seen["discover"] == [{"bound": 5}]
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


async def test_handler_error_becomes_ok_false_and_host_survives(tmp_path) -> None:
    """A failing command returns ok=false without crashing the host."""
    handler, _ = _stub_handler(fail_on="add_path")
    socket_path = str(tmp_path / "bot.sock")
    server = OperatorServer(handler, socket_path=socket_path)
    task = asyncio.create_task(server.serve())
    try:
        for _ in range(50):
            if _socket_bound(socket_path):
                break
            await asyncio.sleep(0.01)

        resp = await send_command(
            socket_path,
            "add_path",
            {"steps": [{"family": "V2", "address": "0x" + "1" * 40}]},
        )
        assert resp["ok"] is False
        assert "boom" in resp["error"]

        # The host keeps serving the next (healthy) command after a failure.
        resp = await send_command(socket_path, "discover", {"bound": 1})
        assert resp == {"ok": True, "detail": "discovery processed 1"}
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


async def test_malformed_request_returns_ok_false(tmp_path) -> None:
    """A malformed request line yields ok=false, never a host crash."""
    handler, _ = _stub_handler()
    socket_path = str(tmp_path / "bot.sock")
    server = OperatorServer(handler, socket_path=socket_path)
    task = asyncio.create_task(server.serve())
    try:
        for _ in range(50):
            if _socket_bound(socket_path):
                break
            await asyncio.sleep(0.01)

        reader, writer = await asyncio.open_unix_connection(socket_path)
        writer.write(b"not-json\n")
        await writer.drain()
        line = await reader.readline()
        writer.close()
        await writer.wait_closed()
        resp = json.loads(line.decode())
        assert resp["ok"] is False
        assert "invalid request" in resp["error"]
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


def _socket_bound(path: str) -> bool:
    """Return True once the server has bound its Unix socket file."""
    from pathlib import Path

    return Path(path).exists()


def test_wrap_handler_normalizes_detail_and_error() -> None:
    """The wrapper turns detail->ok and error->ok:false spellings into wire shape."""

    async def ok_handler(op, payload):
        await asyncio.sleep(0)
        return {"detail": "fine"}

    async def err_handler(op, payload):
        await asyncio.sleep(0)
        return {"error": "nope"}

    assert asyncio.run(wrap_handler(ok_handler)("x", {})) == {
        "ok": True,
        "detail": "fine",
    }
    assert asyncio.run(wrap_handler(err_handler)("x", {})) == {
        "ok": False,
        "error": "nope",
    }
