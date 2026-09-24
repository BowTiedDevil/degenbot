"""Fleet-posture operator ops (JCI2FW Part B): wire round-trip over a live socket.

Covers the `set_fleet_posture` / `get_fleet_posture` ops through
:func:`handle_fleet_posture_op` — the helper the runner's operator handler
routes them through — over a REAL unix socket
(`OperatorServer` + :func:`send_command`): the effective-policy echo,
unknown-key rejection, empty-patch rejection, typed-refusal surfacing, and
the `effective` passthrough of :func:`wrap_handler`. The validation rules
themselves live ONCE in the Rust core (`PosturePolicyPatch::validate`,
unit-tested in `rust/crates/engine/degenbot-workers/src/posture.rs`) and are
exercised through the compiled verb in `tests/rust/test_fleet_posture_ffi.py`.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

import pytest

from degenbot.fleet import current_posture, set_posture_thresholds
from degenbot.operator.operator_channel import (
    FLEET_POSTURE_THRESHOLD_KEYS,
    OperatorServer,
    handle_fleet_posture_op,
    send_command,
    wrap_handler,
)

#: The six threshold keys of an effective-policy echo (everything but the
#: posture label).
_THRESHOLD_FIELDS = FLEET_POSTURE_THRESHOLD_KEYS


@pytest.fixture(autouse=True)
def _restore_live_posture_policy():
    """Snapshot + restore the process-global live policy around each test.

    The re-tune channel drives the ONE process-level posture owner, so a
    test's patch would otherwise leak into whichever test runs next
    (pytest-randomly shuffles; xdist groups). Snapshot all six fields and
    re-apply them on teardown — the same partial-patch op, so the restore
    exercises nothing new.
    """
    before = current_posture()
    yield
    patch = {key: before[key] for key in FLEET_POSTURE_THRESHOLD_KEYS}
    set_posture_thresholds(patch)


def _fleet_posture_handler() -> tuple[Any, dict[str, list[Any]]]:
    """Return a host handler routing ONLY through the fleet-posture helper.

    The seen dict records every (op, payload) the wire delivered, so the
    tests assert what crossed the socket, not what the helper returned.
    """
    seen: dict[str, list[Any]] = {"ops": []}

    async def handler(op: str, payload: dict[str, Any]) -> dict[str, Any]:
        await asyncio.sleep(0)
        seen["ops"].append((op, payload))
        return handle_fleet_posture_op(op, payload)

    return handler, seen


def _socket_bound(path: str) -> bool:
    """Return True once the server has bound its Unix socket file."""
    return Path(path).exists()


async def _serve(socket_path: str, handler) -> tuple[OperatorServer, asyncio.Task[None]]:
    """Start an OperatorServer and wait until its socket file is bound."""
    server = OperatorServer(handler, socket_path=socket_path)
    task = asyncio.create_task(server.serve())
    for _ in range(50):
        if _socket_bound(socket_path):
            break
        await asyncio.sleep(0.01)
    return server, task


async def test_get_fleet_posture_echoes_the_effective_policy(tmp_path) -> None:
    """The read op returns the six thresholds + the current posture."""
    handler, _ = _fleet_posture_handler()
    socket_path = str(tmp_path / "bot.sock")
    server, task = await _serve(socket_path, handler)
    try:
        resp = await send_command(socket_path, "get_fleet_posture", {})
        assert resp["ok"] is True
        effective = resp["effective"]
        assert set(_THRESHOLD_FIELDS) <= set(effective)
        assert effective["posture"] in ("Nominal", "Cordoned")
        # The six values are the typed schema defaults on a fresh owner.
        assert effective["cordon_enter_events"] >= 1
        assert effective["cordon_enter_window_ms"] > 0
        assert 0.0 < effective["cordon_duty_percent"] <= 100.0
        assert effective["cordon_duty_window_ms"] > 0
        assert effective["cordon_exit_clean_ms"] > 0
        assert effective["cordon_sim_intake_floor"] is None or (
            effective["cordon_sim_intake_floor"] >= 1
        )
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


async def test_set_fleet_posture_round_trips_a_partial_patch(tmp_path) -> None:
    """A subset patch echoes the MERGED effective policy over the wire."""
    handler, seen = _fleet_posture_handler()
    socket_path = str(tmp_path / "bot.sock")
    server, task = await _serve(socket_path, handler)
    try:
        before = (await send_command(socket_path, "get_fleet_posture", {}))["effective"]
        resp = await send_command(
            socket_path,
            "set_fleet_posture",
            {"cordon_enter_events": 9, "cordon_duty_percent": 3.5},
        )
        assert resp["ok"] is True
        effective = resp["effective"]
        assert effective["cordon_enter_events"] == 9
        assert effective["cordon_duty_percent"] == pytest.approx(3.5)
        # Absent keys keep the LIVE value (partial-patch semantics).
        assert effective["cordon_enter_window_ms"] == before["cordon_enter_window_ms"]
        assert effective["cordon_duty_window_ms"] == before["cordon_duty_window_ms"]
        assert effective["cordon_exit_clean_ms"] == before["cordon_exit_clean_ms"]
        assert effective["cordon_sim_intake_floor"] == before["cordon_sim_intake_floor"]
        assert effective["posture"] in ("Nominal", "Cordoned")
        # The wire delivered exactly the supplied subset.
        assert seen["ops"] == [
            ("get_fleet_posture", {}),
            ("set_fleet_posture", {"cordon_enter_events": 9, "cordon_duty_percent": 3.5}),
        ]
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


async def test_set_fleet_posture_rejects_unknown_keys(tmp_path) -> None:
    """An unknown key is refused at the wire (defense-in-depth over layer 2)."""
    handler, _ = _fleet_posture_handler()
    socket_path = str(tmp_path / "bot.sock")
    server, task = await _serve(socket_path, handler)
    try:
        resp = await send_command(
            socket_path,
            "set_fleet_posture",
            {"cordon_enter_events": 3, "cordon_bogus": 1},
        )
        assert resp["ok"] is False
        assert "unknown fleet-posture threshold key(s): cordon_bogus" in resp["error"]
        # No partial application: a refused patch never lands.
        after = (await send_command(socket_path, "get_fleet_posture", {}))["effective"]
        assert after["cordon_enter_events"] != 3
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


async def test_set_fleet_posture_rejects_an_empty_patch(tmp_path) -> None:
    """An empty patch is refused before it reaches the Rust validator."""
    handler, _ = _fleet_posture_handler()
    socket_path = str(tmp_path / "bot.sock")
    server, task = await _serve(socket_path, handler)
    try:
        resp = await send_command(socket_path, "set_fleet_posture", {})
        assert resp["ok"] is False
        assert "at least one threshold key" in resp["error"]
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


@pytest.mark.parametrize(
    "patch",
    [
        {"cordon_duty_percent": 0.0},
        {"cordon_duty_percent": 100.5},
        {"cordon_enter_window_ms": 0},
        {"cordon_enter_events": 0},
        {"cordon_sim_intake_floor": 0},
        {"cordon_exit_clean_ms": 0},
        {"cordon_enter_events": "many"},
    ],
)
async def test_set_fleet_posture_surfaces_typed_refusals(tmp_path, patch) -> None:
    """A value outside its typed range surfaces as the typed error text."""
    handler, _ = _fleet_posture_handler()
    socket_path = str(tmp_path / "bot.sock")
    server, task = await _serve(socket_path, handler)
    try:
        resp = await send_command(socket_path, "set_fleet_posture", patch)
        assert resp["ok"] is False, f"{patch} must be refused"
        assert "PostureRetuneError" in resp["error"]
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


async def test_set_fleet_posture_clears_the_sim_intake_floor(tmp_path) -> None:
    """An explicit null floor restores the half-slot-cap default (None)."""
    handler, _ = _fleet_posture_handler()
    socket_path = str(tmp_path / "bot.sock")
    server, task = await _serve(socket_path, handler)
    try:
        resp = await send_command(
            socket_path,
            "set_fleet_posture",
            {"cordon_sim_intake_floor": None},
        )
        assert resp["ok"] is True
        assert resp["effective"]["cordon_sim_intake_floor"] is None
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


async def test_fleet_posture_ops_keep_the_wire_ok_false_on_failures(tmp_path) -> None:
    """A failing fleet op never crashes the host; the next command still works."""
    handler, _ = _fleet_posture_handler()
    socket_path = str(tmp_path / "bot.sock")
    server, task = await _serve(socket_path, handler)
    try:
        bad = await send_command(socket_path, "set_fleet_posture", {"cordon_nope": 1})
        assert bad["ok"] is False
        good = await send_command(socket_path, "get_fleet_posture", {})
        assert good["ok"] is True
    finally:
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        await server.close()


def test_wrap_handler_passes_effective_through() -> None:
    """The wrapper carries an effective echo on ok and omits it otherwise."""

    async def effective_handler(op, payload):
        await asyncio.sleep(0)
        return {"effective": {"cordon_enter_events": 1, "posture": "Nominal"}}

    async def plain_handler(op, payload):
        await asyncio.sleep(0)
        return {"detail": "enqueued"}

    assert asyncio.run(wrap_handler(effective_handler)("x", {})) == {
        "ok": True,
        "detail": "",
        "effective": {"cordon_enter_events": 1, "posture": "Nominal"},
    }
    # Handlers without an echo keep the exact pre-fleet wire shape.
    assert asyncio.run(wrap_handler(plain_handler)("x", {})) == {
        "ok": True,
        "detail": "enqueued",
    }
