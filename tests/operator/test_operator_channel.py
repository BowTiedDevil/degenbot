"""Operator command channel: wire round-trip, family mapping, errors.

Covers :mod:`degenbot.operator.operator_channel` — the Unix-domain-socket
JSON-lines channel an operator uses to add a path / trigger discovery on a live
bot, plus the CLI client helper (:func:`send_command`). The server side is
exercised with a stub handler; the real host handler (wiring into
`BotRunner.enqueue_path` / `trigger_discovery`) is exercised at the
example/session layer in `tests/arbitrage/test_arbitrage_session.py`.
"""

import asyncio
import json

import pytest

from degenbot.pathfinding import PoolKind
from degenbot.operator.operator_channel import (
    OperatorServer,
    send_command,
    step_from_wire,
    wrap_handler,
)
from tests.deadlock_guard import DEADLOCK_GUARD_S


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
            return {
                "detail": f"enqueued {len(steps)}-hop path",
                "sequence": 7,
                "path_id": 42,
            }
        if op == "discover":
            return {"detail": f"discovery processed {payload.get('bound', 0)}"}
        return {"error": f"unknown op {op}"}

    return handler, seen


def test_step_from_wire_maps_families_and_rejects_bad_input() -> None:
    """Family strings map to typed pool kinds; identities remain byte-stable."""
    assert step_from_wire({"family": "V2", "address": "0xaa"}).type is PoolKind.V2
    assert step_from_wire({"family": "V3", "address": "0xaa"}).type is PoolKind.V3
    v4 = step_from_wire({"family": "V4", "address": "0x00", "hash": "0x" + "a" * 64})
    assert v4.type is PoolKind.V4
    assert v4.address == "0x00"
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
        await server.wait_ready()

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
        assert resp == {
            "ok": True,
            "detail": "enqueued 2-hop path",
            "sequence": 7,
            "path_id": 42,
        }
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


async def test_serve_parks_until_closed(tmp_path) -> None:
    """serve() itself must park: it runs forever (cancellable), so the serve
    task stays pending while the server is up and completes only once close()
    tears down serve_forever — a caller that ``create_task(serve())`` gets a
    task that lives for the bot's lifetime, not one that returns instantly.
    """
    handler, _ = _stub_handler()
    socket_path = str(tmp_path / "bot.sock")
    server = OperatorServer(handler, socket_path=socket_path)
    task = asyncio.create_task(server.serve())
    try:
        await server.wait_ready(timeout_s=5)
        # Parked on serve_forever — not completed, not errored out.
        assert task.done() is False

        # While serve() parks, the channel is live for real traffic.
        resp = await send_command(socket_path, "discover", {"bound": 1})
        assert resp == {"ok": True, "detail": "discovery processed 1"}
        assert task.done() is False
    finally:
        # close() cancels serve_forever, so the parked serve() task unwinds
        # (cancelled). Deadlock guard only — not an ordering assertion: the
        # assertion is the CancelledError below, and the budget only keeps a
        # regression to a non-parking serve() a fast, named failure instead of
        # a hang. The shield keeps the guard's own timeout from cancelling the
        # task under inspection.
        await server.close()
        with pytest.raises(asyncio.CancelledError):
            await asyncio.wait_for(asyncio.shield(task), timeout=DEADLOCK_GUARD_S)


async def test_handler_error_becomes_ok_false_and_host_survives(tmp_path) -> None:
    """A failing command returns ok=false without crashing the host."""
    handler, _ = _stub_handler(fail_on="add_path")
    socket_path = str(tmp_path / "bot.sock")
    server = OperatorServer(handler, socket_path=socket_path)
    task = asyncio.create_task(server.serve())
    try:
        await server.wait_ready()

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
        await server.wait_ready()

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


async def test_close_observes_serve_cancellation(tmp_path) -> None:
    """``close()`` asserts the serve_forever cancellation it requested: a
    serve future that absorbs the cancel fails loudly instead of being
    reaped silently by the ``contextlib.suppress`` reap."""
    handler, _seen = _stub_handler()
    server = OperatorServer(handler, socket_path=str(tmp_path / "obs.sock"))

    async def absorbing_serve_forever() -> None:
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            return  # the swallow the reap assertion must catch

    server._serving = asyncio.ensure_future(absorbing_serve_forever())
    server._server = None  # no real socket: close() only reaps the future
    await asyncio.sleep(0)  # start the body so the cancel lands mid-await

    with pytest.raises(AssertionError, match="cancellation"):
        await server.close()


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


def test_execution_probe_response_preserves_economic_failure_without_broadcast() -> None:
    """A typed report projects to the wire's failure shape, with its posture."""
    from types import SimpleNamespace

    from degenbot.operator.operator_channel import execution_probe_response

    detail = SimpleNamespace(bucket="InsufficientProfit")
    verdict = SimpleNamespace(kind=lambda: "failed", detail=detail)
    record = SimpleNamespace(
        candidate_id=SimpleNamespace(path_id=7, deployment_id=1),
        path_type="V2-V3-V4",
        assembly_label="assembled",
        submit=None,
        simulate=verdict,
    )
    report = SimpleNamespace(capital="self_fund", records=[record], encoded=[])

    response = execution_probe_response(7, report)

    assert response == {
        "path_id": 7,
        "permutation": "V2-V3-V4",
        "capital": "self_fund",
        "outcome": "failed",
        "bucket": "InsufficientProfit",
    }


def test_execution_probe_response_accepts_valid_below_threshold_receipt() -> None:
    """A gas-unprofitable receipt is `sim_ok` — and it carries its planning row."""
    from types import SimpleNamespace

    from degenbot.operator.operator_channel import execution_probe_response

    verdict = SimpleNamespace(kind=lambda: "gas_unprofitable")
    record = SimpleNamespace(
        candidate_id=SimpleNamespace(path_id=9, deployment_id=1),
        path_type="V4-V4-V4",
        assembly_label="assembled",
        submit=None,
        simulate=verdict,
    )
    encoded = SimpleNamespace(
        candidate_id=SimpleNamespace(path_id=9, deployment_id=1),
        execution_target="0x00000000000000000000000000000000000000C0",
        value=0,
        execute_calldata=b"\xde\xad",
    )
    report = SimpleNamespace(capital="production", records=[record], encoded=[encoded])

    assert execution_probe_response(9, report) == {
        "path_id": 9,
        "permutation": "V4-V4-V4",
        "capital": "production",
        "outcome": "sim_ok",
        "planning": {
            "execution_target": "0x00000000000000000000000000000000000000C0",
            "value": 0,
            "calldata_len": 2,
        },
    }


def test_an_assembly_skip_projects_its_skip_label() -> None:
    """A row that never reached the sim reports its stage-1 label and no plan."""
    from types import SimpleNamespace

    from degenbot.operator.operator_channel import execution_probe_response

    record = SimpleNamespace(
        candidate_id=SimpleNamespace(path_id=4, deployment_id=1),
        path_type="V2",
        assembly_label="skip-thin-margin",
        submit=None,
        simulate=None,
    )
    report = SimpleNamespace(capital="zero_balance", records=[record], encoded=[])

    assert execution_probe_response(4, report) == {
        "path_id": 4,
        "permutation": "V2",
        "capital": "zero_balance",
        "outcome": "skip-thin-margin",
    }


def test_a_report_carrying_a_submit_verdict_is_refused() -> None:
    """The detective check beside the structural guarantee.

    A probe executor has no submit lane, so a report carrying a submit verdict
    means the surface was short-circuited — refused loudly rather than rendered
    as a dry-run success.
    """
    from types import SimpleNamespace

    from degenbot.operator.operator_channel import execution_probe_response

    record = SimpleNamespace(
        candidate_id=SimpleNamespace(path_id=7, deployment_id=1),
        path_type="V2-V3-V4",
        assembly_label="assembled",
        submit=SimpleNamespace(kind=lambda: "submitted"),
        simulate=SimpleNamespace(kind=lambda: "profitable"),
    )
    report = SimpleNamespace(capital="self_fund", records=[record], encoded=[])

    with pytest.raises(RuntimeError, match="submit verdict"):
        execution_probe_response(7, report)


def test_a_report_with_no_outcome_is_refused() -> None:
    """An empty report is a bug, not an "unknown" probe."""
    from types import SimpleNamespace

    from degenbot.operator.operator_channel import execution_probe_response

    report = SimpleNamespace(capital="self_fund", records=[], encoded=[])

    with pytest.raises(RuntimeError, match="expected one outcome, got 0"):
        execution_probe_response(7, report)


def test_the_probe_payload_names_one_capital_mode() -> None:
    """The wire carries ONE capital spelling, and every mode is accepted.

    The retired `zero_balance` boolean is gone: a payload either names the mode
    it wants by its spelling, or omits the field and gets the self-funded probe
    the wire has always requested.
    """
    from degenbot.operator.operator_channel import capital_from_wire
    from degenbot.runner._sim_submit import ProbeCapital

    assert capital_from_wire({"path_id": 7}) is ProbeCapital.SELF_FUND, (
        "an absent capital field keeps the historical self-funded probe"
    )
    for mode in ProbeCapital:
        assert capital_from_wire({"path_id": 7, "capital": mode.value}) is mode


@pytest.mark.parametrize(
    "payload",
    [
        {"capital": "self-funded"},  # a near miss, not a mode
        {"capital": "zero-balance"},
        {"capital": ""},
        {"capital": None},
        {"capital": True},  # a boolean is not a mode spelling
    ],
)
def test_an_unknown_capital_spelling_is_refused(payload: dict[str, object]) -> None:
    """A spelling the core does not know is refused, never guessed at."""
    from degenbot.operator.operator_channel import capital_from_wire

    with pytest.raises(ValueError, match="unknown capital"):
        capital_from_wire({"path_id": 7, **payload})


@pytest.mark.parametrize("retired", ["self_fund", "zero_balance"])
def test_the_retired_boolean_payload_is_refused(retired: str) -> None:
    """A payload still sending a retired boolean is refused, not silently defaulted.

    Ignoring the old key would run the probe in the self-funded default, i.e. a
    posture the caller did not ask for — so the transition is loud.
    """
    from degenbot.operator.operator_channel import capital_from_wire

    with pytest.raises(ValueError, match=f"retired probe key.*{retired}"):
        capital_from_wire({"path_id": 7, retired: True})
