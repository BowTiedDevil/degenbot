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
from types import SimpleNamespace

import pytest

from degenbot.operator.operator_channel import (
    PROBE_SUBMISSION_EVIDENCE,
    OperatorServer,
    execution_probe_response,
    send_command,
    step_from_wire,
    wrap_handler,
)
from degenbot.pathfinding import PoolKind
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


#: The executor's own ``InsufficientProfit(uint256,uint256)`` revert: its selector
#: plus the two ABI words the release declares.
INSUFFICIENT_PROFIT_SELECTOR = "0x4e88422a"
TERMINAL_REVERT = INSUFFICIENT_PROFIT_SELECTOR + "00" * 64


def _probe_record(**overrides: object) -> object:
    """One probe record as the typed report exposes it."""
    fields: dict[str, object] = {
        "candidate_id": SimpleNamespace(path_id=7, deployment_id=1),
        "path_type": "V2-V3-V4",
        "assembly_label": "assembled",
        "submit": None,
        "simulate": None,
    }
    fields.update(overrides)
    return SimpleNamespace(**fields)


def test_execution_probe_response_preserves_economic_failure_without_broadcast() -> None:
    """A typed report projects to the wire's failure shape, with its posture + bytes."""
    detail = SimpleNamespace(
        bucket="InsufficientProfit",
        record=lambda: {"bucket": "InsufficientProfit", "revert_data": TERMINAL_REVERT},
    )
    record = _probe_record(simulate=SimpleNamespace(kind=lambda: "failed", detail=detail))
    report = SimpleNamespace(capital="self_fund", records=[record], encoded=[])

    response = execution_probe_response(7, report, current_block=21_000_000)

    assert response == {
        "path_id": 7,
        "permutation": "V2-V3-V4",
        "capital": "self_fund",
        "current_block": 21_000_000,
        "assembly": "assembled",
        "submission": PROBE_SUBMISSION_EVIDENCE,
        "outcome": "failed",
        "bucket": "InsufficientProfit",
        "revert": TERMINAL_REVERT,
    }, "the raw revert rides the wire so a client checks the selector itself"
    assert response["revert"][:10] == INSUFFICIENT_PROFIT_SELECTOR


def test_execution_probe_response_carries_a_typed_zero_capital_refusal() -> None:
    """A capital-policy refusal crosses the wire as data, not as a sentence.

    The bucket says which answer this is (`insolvent-zero-capital`); the
    `zero_capital` object says what the stream could not fund. An ordinary
    `encode-failed` refusal carries no such object, so the two stay distinct.
    """
    detail = SimpleNamespace(
        bucket="insolvent-zero-capital",
        record=lambda: {"bucket": "insolvent-zero-capital", "revert_data": "0x"},
        zero_capital=SimpleNamespace(
            asset="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            required=1_000_000,
            available=250_000,
        ),
    )
    record = _probe_record(
        candidate_id=SimpleNamespace(path_id=11, deployment_id=1),
        path_type="V4-V4-V4",
        simulate=SimpleNamespace(kind=lambda: "failed", detail=detail),
    )
    report = SimpleNamespace(capital="zero_balance", records=[record], encoded=[])

    response = execution_probe_response(11, report, current_block=21_000_005)

    assert response == {
        "path_id": 11,
        "permutation": "V4-V4-V4",
        "capital": "zero_balance",
        "current_block": 21_000_005,
        "assembly": "assembled",
        "submission": PROBE_SUBMISSION_EVIDENCE,
        "outcome": "failed",
        "bucket": "insolvent-zero-capital",
        "revert": "0x",
        "zero_capital": {
            "asset": "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            "required": 1_000_000,
            "available": 250_000,
        },
    }, "the deficit's three fields ride the wire unchanged"
    assert response["zero_capital"]["required"] > response["zero_capital"]["available"]


def test_a_gas_unprofitable_receipt_is_its_own_typed_outcome() -> None:
    """A below-floor receipt executed — the wire must not call it `sim_ok`.

    `profitable` and `gas-unprofitable` both carry a receipt, and a report that
    collapsed them would let a losing receipt read as a successful arbitrage.
    """
    receipt = SimpleNamespace(gross_profit=10**15, net_profit=10**13, gas_used=120_000)
    record = _probe_record(
        candidate_id=SimpleNamespace(path_id=9, deployment_id=1),
        path_type="V4-V4-V4",
        simulate=SimpleNamespace(kind=lambda: "gas-unprofitable", receipt=receipt),
    )
    report = SimpleNamespace(capital="production", records=[record], encoded=[])

    assert execution_probe_response(9, report, current_block=7) == {
        "path_id": 9,
        "permutation": "V4-V4-V4",
        "capital": "production",
        "current_block": 7,
        "assembly": "assembled",
        "submission": PROBE_SUBMISSION_EVIDENCE,
        "outcome": "gas-unprofitable",
        "receipt": {"gross_profit": 10**15, "net_profit": 10**13, "gas_used": 120_000},
    }


def test_a_profitable_probe_projects_its_receipt_and_plan() -> None:
    """A winning probe states what it earned and what it would have sent."""
    receipt = SimpleNamespace(gross_profit=10**15, net_profit=10**13, gas_used=120_000)
    record = _probe_record(
        candidate_id=SimpleNamespace(path_id=9, deployment_id=1),
        simulate=SimpleNamespace(kind=lambda: "profitable", receipt=receipt),
    )
    encoded = SimpleNamespace(
        candidate_id=SimpleNamespace(path_id=9, deployment_id=1),
        execution_target="0x00000000000000000000000000000000000000C0",
        value=0,
        execute_calldata=b"\xde\xad",
    )
    report = SimpleNamespace(capital="production", records=[record], encoded=[encoded])

    response = execution_probe_response(9, report, current_block=21_000_000)

    assert response == {
        "path_id": 9,
        "permutation": "V2-V3-V4",
        "capital": "production",
        "current_block": 21_000_000,
        "assembly": "assembled",
        "submission": PROBE_SUBMISSION_EVIDENCE,
        "outcome": "profitable",
        "receipt": {"gross_profit": 10**15, "net_profit": 10**13, "gas_used": 120_000},
        "planning": {
            "execution_target": "0x00000000000000000000000000000000000000C0",
            "value": 0,
            "calldata_len": 2,
        },
    }


def test_an_assembly_skip_projects_its_skip_label() -> None:
    """A row that never reached the sim reports its stage-1 label and no plan."""
    record = _probe_record(
        candidate_id=SimpleNamespace(path_id=4, deployment_id=1),
        path_type="V2",
        assembly_label="skip-thin-margin",
    )
    report = SimpleNamespace(capital="zero_balance", records=[record], encoded=[])

    assert execution_probe_response(4, report, current_block=99) == {
        "path_id": 4,
        "permutation": "V2",
        "capital": "zero_balance",
        "current_block": 99,
        "assembly": "skip-thin-margin",
        "submission": PROBE_SUBMISSION_EVIDENCE,
        "outcome": "skip-thin-margin",
    }


def test_a_report_carrying_a_submit_verdict_is_refused() -> None:
    """The detective check beside the structural guarantee.

    A probe executor has no submit lane, so a report carrying a submit verdict
    means the surface was short-circuited — refused loudly rather than rendered
    as a dry-run success.
    """
    record = _probe_record(
        submit=SimpleNamespace(kind=lambda: "submitted"),
        simulate=SimpleNamespace(kind=lambda: "profitable"),
    )
    report = SimpleNamespace(capital="self_fund", records=[record], encoded=[])

    with pytest.raises(RuntimeError, match="submit verdict"):
        execution_probe_response(7, report, current_block=1)


def test_a_report_with_no_outcome_is_refused() -> None:
    """An empty report is a bug, not an "unknown" probe."""
    report = SimpleNamespace(capital="self_fund", records=[], encoded=[])

    with pytest.raises(RuntimeError, match="expected one outcome, got 0"):
        execution_probe_response(7, report, current_block=1)


def test_the_wire_carries_every_key_the_projection_states() -> None:
    """The wrapper forwards only the keys it names, so the projection is pinned.

    A key the projection states but the wrapper drops is evidence a client cannot
    audit, which is why the two are checked against each other rather than trusted.
    """
    deficit_detail = SimpleNamespace(
        bucket="insolvent-zero-capital",
        record=lambda: {"bucket": "insolvent-zero-capital", "revert_data": "0x"},
        zero_capital=SimpleNamespace(
            asset="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", required=9, available=1
        ),
    )
    encoded = SimpleNamespace(
        candidate_id=SimpleNamespace(path_id=7, deployment_id=1),
        execution_target="0x00000000000000000000000000000000000000C0",
        value=0,
        execute_calldata=b"\xde\xad",
    )
    receipt = SimpleNamespace(gross_profit=10**15, net_profit=10**13, gas_used=120_000)
    projections = [
        execution_probe_response(
            7,
            SimpleNamespace(
                capital="production",
                records=[
                    _probe_record(
                        simulate=SimpleNamespace(kind=lambda: "profitable", receipt=receipt)
                    )
                ],
                encoded=[encoded],
            ),
            current_block=21_000_000,
        ),
        execution_probe_response(
            7,
            SimpleNamespace(
                capital="zero_balance",
                records=[
                    _probe_record(
                        simulate=SimpleNamespace(
                            kind=lambda: "failed",
                            detail=deficit_detail,
                        )
                    )
                ],
                encoded=[encoded],
            ),
            current_block=21_000_000,
        ),
    ]

    stated = [set(projection) for projection in projections]

    async def handler(_op: str, _payload: dict[str, object]) -> dict[str, object]:
        await asyncio.sleep(0)
        return projections.pop(0)

    for keys in stated:
        wrapped = asyncio.run(wrap_handler(handler)("probe_path", {}))

        assert wrapped["ok"] is True
        assert not wrapped["detail"]
        assert set(wrapped) == {"ok", "detail"} | keys


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
