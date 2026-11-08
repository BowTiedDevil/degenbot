"""The session-consumption oracle — the Python half of the shared gate.

The fixture (``fixtures/session_consumption.json``) records the EXPECTED product
of the ONE session consumer over a scripted ordered event sequence. The pure-Rust
half (``rust/crates/engine/degenbot-batch-executor/tests/session_consumption.rs``)
replays it through ``degenbot_batch_executor::session::SessionConsumer``; this
half replays the SAME script into the SAME consumer through the ``PyO3`` seam
(``degenbot.dispatch.SessionConsumer``), so both halves pin one source of truth:

* **executor work** — the per-batch outcome records a script produces (path id,
  block, assembly verdict, simulate verdict);
* **outcome ordering** — one record set per enqueued batch, in arrival order;
* **loud abort** — the first leaf failure surfaces at the consumer with its batch
  index and the leaf's own detail, and the lane stops;
* **stall detection** — one heartbeat beat per ACTUAL stream event (a silent
  source feeds none);
* **close on termination** — the typed end verdict (``quiet-end`` /
  ``pump-stopped``).

Path ids are symbolic in the fixture; each half substitutes its own
(``path_a`` = the one registered 2-hop V2 path here). Every raw row in a script
skips pre-sim, so no scenario dials RPC.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from degenbot.dispatch import (
    BatchExecutor,
    ConsumerErrorKind,
    Dispatcher,
    SessionConsumer,
    SessionConsumerError,
    SimulateContext,
    TxSigner,
    build_batch_executor,
)
from tests.helpers.seam import AlloyProvider, AsyncAlloyProvider
from tests.helpers.v2_cycle_engine import build_two_hop_v2_engine

_ORACLE = Path(__file__).parent / "fixtures" / "session_consumption.json"

# A dead port: alloy's transport is lazy and every scripted row skips pre-sim,
# so the construction never dials and the run stays offline and deterministic.
_RPC_URL = "http://127.0.0.1:1"
# The operator account the fixture signer holds (key ``0x11..11``): the batch
# composition refuses a deployment whose owner is not the signing account.
_OWNER = "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A"
_EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
_WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
_POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"

#: An id no resolver answers (the fixture's symbolic ``path_missing``).
_PATH_MISSING = 4_242


def _oracle() -> dict[str, Any]:
    return json.loads(_ORACLE.read_text(encoding="utf-8"))


def _context() -> SimulateContext:
    return SimulateContext(
        provider=AsyncAlloyProvider(AlloyProvider(_RPC_URL)),
        executor_owner=_OWNER,
        executor_address=_EXECUTOR,
        weth_address=_WETH,
        pool_manager_address=_POOL_MANAGER,
        inject_code=False,
    )


def _executor(engine: Any) -> BatchExecutor:
    """Build the session executor over the fixture engine, offline."""
    return build_batch_executor(
        context=_context(),
        dispatcher=Dispatcher.for_block(0),
        engine=engine,
        signer=TxSigner(key="0x" + "11" * 32, chain_id=1),
        submit_provider=AsyncAlloyProvider(AlloyProvider(_RPC_URL)),
        operator_nonce=5,
        dry_run=True,
        broadcast_providers=None,
    )


def _payload(path_id: int, sim: dict[str, Any]) -> dict[str, Any]:
    """One inline-sim payload in the engine's result-channel field set."""
    return {
        "candidate": {"path_id": path_id, "deployment_id": 1},
        "gross_profit": sim["gross_profit"],
        "net_profit": sim["net_profit"],
        "gas_used": sim["gas_used"],
        "priority_fee": sim["priority_fee"],
        "base_fee_next": sim["base_fee_next"],
        "execute_calldata": b"",
        "access_list": None,
        "captured_swaps": [],
        "hop_count": 2,
        "failure": None,
    }


def _script_event(oracle: dict[str, Any], event: dict[str, Any], path_a: int) -> dict[str, Any]:
    """Translate one fixture event into the engine stream's Python shape."""

    def live(symbolic: str) -> int:
        return {"path_a": path_a, "path_missing": _PATH_MISSING}[symbolic]

    if event["kind"] == "block":
        return {"kind": "block", **oracle["blocks"][event["block"]]}
    rows = [
        (
            live(row["path"]),
            row["optimal_input"],
            row["profit"],
            row["hop_outputs"],
            row["consumed_inputs"],
            row["state_nonces"],
        )
        for row in event.get("rows") or ()
    ]
    payloads = {
        live(sim["path"]): _payload(live(sim["path"]), sim) for sim in event.get("payloads") or ()
    }
    return {
        "kind": "batch",
        "solve_block": event["solve_block"],
        "timestamp": 0,
        "base_fee_per_gas": None,
        "gas_used": 0,
        "gas_limit": 0,
        "fresh": rows,
        "updated": [],
        "expired": [],
        "removed": [],
        "payloads": payloads,
    }


async def _run_scenario(scenario: dict[str, Any], path_a: int) -> dict[str, Any]:
    """Replay one scenario through the consumer and collect its trace."""
    oracle = _oracle()
    engine, registered = build_two_hop_v2_engine()
    assert registered == path_a, "the fixture engine's path id is the oracle's path_a"
    executor = _executor(engine)
    events = [_script_event(oracle, event, path_a) for event in scenario["events"]]
    consumer = SessionConsumer.scripted(
        executor, events, allow_quiet_end=scenario["allow_quiet_end"]
    )

    outcome = "ok"
    detail: str | None = None
    aborted_at: int | None = None
    abort_kind: Any = None
    try:
        report = await consumer.run()
    except SessionConsumerError as exc:
        outcome = "aborted"
        detail = str(exc)
        aborted_at = exc.batch_index
        report = exc.report
        abort_kind = exc.kind
    if abort_kind is not None:
        assert abort_kind == ConsumerErrorKind.LeafFailure, (
            "the gate's aborts are stored leaf failures, core-stamped"
        )

    records: list[list[dict[str, Any]]] = []
    while True:
        batch = await executor.next_outcome()
        if batch is None:
            break
        records.append([
            {
                "path": "path_a" if record.candidate_id.path_id == path_a else "path_missing",
                "block": record.block,
                "assembly": record.assembly_label,
                "simulate": "none" if record.simulate is None else record.simulate.kind(),
            }
            for record in batch.records
        ])

    return {
        "outcome": outcome,
        "end": report["end"],
        "batches": report["batches"],
        "blocks": report["blocks"],
        "stale_batches": report["stale_batches"],
        "end_of_stream": report["end_of_stream"],
        "beats": consumer.beats,
        "aborted_at": aborted_at,
        "detail": detail,
        "records": records,
    }


@pytest.fixture(scope="module")
def oracle() -> dict[str, Any]:
    return _oracle()


@pytest.fixture(scope="module")
def path_a() -> int:
    return build_two_hop_v2_engine()[1]


async def test_the_shared_event_sequence_oracle_pins_the_consumer_product(
    oracle: dict[str, Any], path_a: int
) -> None:
    for scenario in oracle["scenarios"]:
        actual = await _run_scenario(scenario, path_a)
        expected = scenario["expected"]
        assert actual["outcome"] == expected["outcome"], scenario["name"]
        assert actual["end"] == expected["end"], scenario["name"]
        assert actual["batches"] == expected["batches"], scenario["name"]
        assert actual["blocks"] == expected["blocks"], scenario["name"]
        assert actual["stale_batches"] == expected["stale_batches"], scenario["name"]
        assert actual["end_of_stream"] == expected["end_of_stream"], scenario["name"]
        assert actual["beats"] == expected["beats"], scenario["name"]
        assert actual["aborted_at"] == expected["aborted_at"], scenario["name"]
        assert actual["records"] == expected["records"], scenario["name"]
        for fragment in expected["detail_contains"] or ():
            assert fragment in (actual["detail"] or ""), scenario["name"]


def test_the_oracle_vocabulary_is_closed(oracle: dict[str, Any]) -> None:
    """Every recorded status is a declared member of the fixture's vocabulary."""
    vocabulary = oracle["status_vocabulary"]
    for scenario in oracle["scenarios"]:
        expected = scenario["expected"]
        assert expected["outcome"] in vocabulary["outcome"], scenario["name"]
        assert expected["end"] in vocabulary["end"], scenario["name"]
        for record_set in expected["records"]:
            for row in record_set:
                assert row["assembly"] in vocabulary["assembly"], scenario["name"]
                assert row["simulate"] in vocabulary["simulate"], scenario["name"]
