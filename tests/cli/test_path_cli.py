"""CLI `degenbot path` client tests (NWTUM3).

The command functions are invoked directly (bypassing the `degenbot` root group,
which loads a bot config) against a live :class:`OperatorServer` running on a
background thread — proving the wire + exit/error behavior. The server-side
routing into the session is covered by `tests/operator/test_operator_channel.py`
+ the session tests.
"""

import asyncio
import contextlib
import threading
import time
from pathlib import Path

import click
import pytest

from degenbot.cli.path import _parse_hop, path_add, path_discover
from degenbot.operator.operator_channel import OperatorServer


def _socket_bound(path: str) -> bool:
    """Return True once the server has bound its Unix socket file."""
    return Path(path).exists()


def _start_server(handler, socket_path):
    """Start an :class:`OperatorServer` on a background thread + its event loop.

    The CLI command functions own their own event loop (`asyncio.run`), so the
    server must live on a separate thread's loop and be reached over the socket.

    Returns:
        A tuple of the server, its loop, the thread, and a state dict holding
        the serve task; pass them to `_stop_server` in a finally block.
    """
    loop = asyncio.new_event_loop()
    server = OperatorServer(handler, socket_path=socket_path)
    state: dict[str, object] = {}

    def _run() -> None:
        asyncio.set_event_loop(loop)
        state["task"] = loop.create_task(server.serve())
        loop.run_forever()

    thread = threading.Thread(target=_run, daemon=True)
    thread.start()
    for _ in range(500):
        if _socket_bound(socket_path):
            break
        time.sleep(0.01)
    return server, loop, thread, state


def _stop_server(server, loop, thread, state) -> None:
    """Cancel the serve task, stop the background loop, and unlink the socket.

    Cancels the serve task and awaits it (so the ``serve_forever`` cleanup
    runs), then stops the loop and removes the socket file. Deliberately does
    not await ``server.close()`` here: the background loop would die when
    ``serve()`` returns, stranding any in-flight close coroutine.
    """

    async def _shutdown() -> None:
        task = state["task"]
        if task is not None and not task.done():
            task.cancel()
        if task is not None:
            with contextlib.suppress(asyncio.CancelledError):
                await task
        loop.stop()

    loop.call_soon_threadsafe(lambda: loop.create_task(_shutdown()))
    thread.join(timeout=10)
    loop.close()
    Path(server._socket_path).unlink(missing_ok=True)


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        ("v2:0xaa", {"family": "V2", "address": "0xaa"}),
        ("V3:0xbb", {"family": "V3", "address": "0xbb"}),
        (
            "v4:0xcc:0x" + "a" * 64,
            {"family": "V4", "address": "0xcc", "hash": "0x" + "a" * 64},
        ),
    ],
)
def test_parse_hop_ok(raw, expected) -> None:
    """A well-formed --hop token (any family case) parses into a wire step."""
    assert _parse_hop(raw) == expected


def test_parse_hop_rejects_bad_family_and_missing_address() -> None:
    """Bad family and missing address are rejected as click.UsageError."""
    with pytest.raises(click.UsageError, match="family must be"):
        _parse_hop("V5:0xaa")
    with pytest.raises(click.UsageError, match="missing an address"):
        _parse_hop("V2")


def test_path_add_round_trip(tmp_path, capsys) -> None:
    """`path add` (no direction) enqueues the parsed hops and prints the detail."""
    seen = {}

    async def handler(op, payload):
        await asyncio.sleep(0)
        seen.update(payload)
        return {"detail": f"enqueued {len(payload['steps'])}-hop path"}

    socket_path = str(tmp_path / "bot.sock")
    server, loop, thread, state = _start_server(handler, socket_path)
    try:
        path_add.callback(
            socket_path,
            ("v3:0x" + "1" * 40, "v4:0x" + "2" * 40 + ":0x" + "a" * 64),
            None,
        )
    finally:
        _stop_server(server, loop, thread, state)

    assert seen["directions"] is None
    assert len(seen["steps"]) == 2
    assert seen["steps"][1] == {
        "family": "V4",
        "address": "0x" + "2" * 40,
        "hash": "0x" + "a" * 64,
    }
    assert "enqueued 2-hop path" in capsys.readouterr().out


def test_path_add_with_direction(tmp_path) -> None:
    """`--direction zfo` maps to a per-hop True list sent on the wire."""
    seen = {}

    async def handler(op, payload):
        await asyncio.sleep(0)
        seen.update(payload)
        return {"detail": "enqueued"}

    socket_path = str(tmp_path / "bot.sock")
    server, loop, thread, state = _start_server(handler, socket_path)
    try:
        path_add.callback(socket_path, ("v3:0x" + "1" * 40, "v2:0x" + "2" * 40), "zfo")
    finally:
        _stop_server(server, loop, thread, state)

    assert seen["directions"] == [True, True]


def test_path_add_failure_raises_click_exception(tmp_path) -> None:
    """A rejecting bot surfaces as a click.ClickException, not a silent exit."""
    seen = {}

    async def handler(op, payload):
        await asyncio.sleep(0)
        seen.update(payload)
        return {"error": "no live registration pipeline"}

    socket_path = str(tmp_path / "bot.sock")
    server, loop, thread, state = _start_server(handler, socket_path)
    try:
        with pytest.raises(click.ClickException, match="no live registration pipeline"):
            path_add.callback(socket_path, ("v3:0x" + "1" * 40,), None)
    finally:
        _stop_server(server, loop, thread, state)


def test_path_discover_round_trip(tmp_path, capsys) -> None:
    """`path discover --bound N` reports the sweep's processed count."""
    seen = {}

    async def handler(op, payload):
        await asyncio.sleep(0)
        seen.update(payload)
        return {"detail": "discovery processed 3"}

    socket_path = str(tmp_path / "bot.sock")
    server, loop, thread, state = _start_server(handler, socket_path)
    try:
        path_discover.callback(socket_path, 3)
    finally:
        _stop_server(server, loop, thread, state)

    assert seen["bound"] == 3
    assert "discovery processed 3" in capsys.readouterr().out
