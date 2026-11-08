"""The session-consumer seam's executor product (the cut-over).

Pins the ``PyO3`` surface over ``degenbot-batch-executor`` offline, through the
two doors that survive the session cut-over:

* the **construction boundary** — a session executor built from resolved policy
  values, exposing only its counters and its drain (the enqueue door is gone:
  the consumer owns the input side);
* the **scripted session** — the engine's own stream shape fed through
  :meth:`SessionConsumer.scripted`, whose consume loop derives each batch's work
  core-side. What the drain then yields IS the product pinned here: the batch's
  outcome records, its raw submit-lane records, and the render fold
  (:meth:`MergedOutcome.from_records`) over those drained records — plus the
  failure dump's own detail for a served payload that failed.

The loop's own facts are NOT re-pinned here: its counts, the per-batch ordering,
the loud abort (the leaf's own detail, surfaced at the consumer) and the typed
end verdict belong to the shared event-sequence oracle
(``tests/standalone_parity/test_session_consumption_gate.py`` and its Rust half).

The engine fixture is REAL (offline-registered pools); the provider is a
dead-port URL (alloy's transport is lazy) and the session runs dry-run, so no
live RPC leaves the process. Every scripted payload row is served by its own
payload, so no candidate reaches the sim fan-out either.
"""

from __future__ import annotations

from typing import Any

import pytest

from degenbot.dispatch import (
    AssemblyVerdict,
    BatchExecutor,
    Dispatcher,
    SessionConsumer,
    SimulateContext,
    SimulateVerdict,
    SubmitSkipReason,
    TxSigner,
    typed_submit_record,
)
from degenbot.runner._render import MergedOutcome
from tests.helpers.seam import AlloyProvider, AsyncAlloyProvider, build_batch_executor_py
from tests.helpers.sim_records import inline_sim_payload

# Canonical mainnet addresses (the seam-suite constants).
# The operator account the fixture signer holds (key ``0x11..11``): the batch
# composition refuses a deployment whose owner is not the signing account.
OWNER = "0x19E7E376E7C213B7E7e7e46cc70A5dD086DAff2A"
EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"

_RPC_URL = "http://127.0.0.1:1"  # a dead port — the transport never dials


def _ctx() -> SimulateContext:
    return SimulateContext(
        provider=AsyncAlloyProvider(AlloyProvider(_RPC_URL)),
        executor_owner=OWNER,
        executor_address=EXECUTOR,
        weth_address=WETH,
        pool_manager_address=POOL_MANAGER,
        inject_code=False,
    )


def _executor(
    engine: Any,
    *,
    dry_run: bool = True,
) -> BatchExecutor:
    """Build the session executor over the fixture engine, offline.

    The dispatcher starts at block 0: the session consumer's block clock is the
    stream's ``block`` events, so construction never seeds it.
    """
    return build_batch_executor_py(
        context=_ctx(),
        dispatcher=Dispatcher.for_block(0),
        engine=engine,
        signer=TxSigner(key="0x" + "11" * 32, chain_id=1),
        submit_provider=AsyncAlloyProvider(AlloyProvider(_RPC_URL)),
        operator_nonce=5,
        dry_run=dry_run,
        broadcast_providers=None,
    )


def _block_event(
    number: int,
    *,
    timestamp: int = 1_700_000_000,
    base_fee_per_gas: int = 1_000_000_000,
    gas_used: int = 15_000_000,
    gas_limit: int = 30_000_000,
) -> dict[str, Any]:
    """One stream header in the engine's Python shape (the ``block`` arm)."""
    return {
        "kind": "block",
        "number": number,
        "timestamp": timestamp,
        "base_fee_per_gas": base_fee_per_gas,
        "gas_used": gas_used,
        "gas_limit": gas_limit,
    }


def _solver_row(
    path_id: int,
    *,
    optimal_input: int,
    hop_outputs: tuple[int, ...],
    consumed_inputs: tuple[int, ...],
    profit: int = 1,
    state_nonces: tuple[int, ...] = (0, 0),
) -> tuple[Any, ...]:
    """One engine solve row — the stream's six-tuple."""
    return (path_id, optimal_input, profit, hop_outputs, consumed_inputs, state_nonces)


def _batch_event(
    solve_block: int,
    *,
    rows: tuple[tuple[Any, ...], ...] = (),
    payloads: dict[int, dict[str, Any]] | None = None,
) -> dict[str, Any]:
    """One stream result batch: the solver rows plus the inline-sim payloads."""
    return {
        "kind": "batch",
        "solve_block": solve_block,
        "timestamp": 0,
        "base_fee_per_gas": None,
        "gas_used": 0,
        "gas_limit": 0,
        "fresh": list(rows),
        "updated": [],
        "expired": [],
        "removed": [],
        "payloads": payloads or {},
    }


async def _drain(executor: BatchExecutor) -> list[Any]:
    """Every outcome set the lane yields, in arrival order, to its own end."""
    drained: list[Any] = []
    while True:
        batch = await executor.next_outcome()
        if batch is None:
            return drained
        drained.append(batch)


class TestConstruction:
    """The construction boundary: policy values in, the drain handles out."""

    def test_construction_is_the_typed_executor(self, nxm2bf_v2_engine_and_path) -> None:
        engine, _path_id = nxm2bf_v2_engine_and_path
        executor = _executor(engine)
        assert isinstance(executor, BatchExecutor)
        assert executor.enqueued == 0
        assert executor.submitted == 0


class TestSessionDrainProduct:
    """A scripted session's work, drained: record, submit record, render fold."""

    async def test_a_scripted_batch_drains_its_record_and_folds_into_the_render(
        self, nxm2bf_v2_engine_and_path
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        executor = _executor(engine)
        consumer = SessionConsumer.scripted(
            executor,
            [
                _block_event(100),
                _batch_event(100, payloads={path_id: inline_sim_payload(path_id)}),
            ],
        )

        report = await consumer.run()
        assert report["batches"] == 1, "the scripted session consumed its one batch"
        assert executor.submitted == 1

        batch = await executor.next_outcome()
        assert batch is not None
        # The drained records: one payload row through the payload arm
        # (profitable — its submit rows join the ordered lane).
        assert [rec.candidate_id.path_id for rec in batch.records] == [path_id]
        assert batch.records[0].assembly is AssemblyVerdict.Assembled
        assert isinstance(batch.records[0].simulate, SimulateVerdict.Profitable)

        # The raw submit-lane records: the dry-run skip, typed decode intact.
        records = [typed_submit_record(raw) for raw in batch.submit_records]
        assert records[0].candidate.path_id == path_id
        assert records[0].reason is SubmitSkipReason.DRY_RUN

        # The render fold: counters arrive as folds over the records.
        outcome = MergedOutcome.from_records(batch.records)
        assert len(outcome.gas_profitable) == 1
        assert outcome.gas_profitable[0].net_profit == 500_000_000_000
        assert outcome.gas_unprofitable_count == 0
        assert outcome.candidate_count == 1
        assert outcome.path_infos[path_id]["path_type"] == "V2-V2"
        assert bool(outcome)

        # The lane is drained, and one batch was the session's whole work.
        assert await executor.next_outcome() is None
        assert executor.enqueued == 1


class TestSimFailureDetail:
    """A served payload's FAILURE dump carries the real figures.

    The payload arm builds its failure record from the payload, which carries
    only the bucket + revert bytes. The request's ``optimal_input`` / expected
    ``hop_outputs`` are joined from the batch's own solver row at the failure
    site, and the sim's captured swaps ride the payload — so the trap's
    ``[sim-fixture]`` dump is evidence, not ``optimal_input=0 hop_outputs=[]``.

    Such a batch also carries an observable of the hand-off itself: the solver
    row whose payload served it is skipped at assembly (the payload row is the
    record), so a batch that pins a failure dump pins that skip beside it.
    """

    async def test_a_sim_failure_dump_threads_the_request_and_the_capture(
        self, nxm2bf_v2_engine_and_path
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        executor = _executor(engine)
        swap = {
            "family": "v2",
            "emitter": "0x" + "cc" * 20,
            "amount0": -1_000,
            "amount1": 3_000,
            "sqrt_price_x96": 0,
            "liquidity": 0,
            "tick": 0,
        }
        payload = inline_sim_payload(
            path_id,
            net=0,
            failure={"bucket": "no-profit", "fail_index": None, "revert_data": "0x"},
        )
        payload["captured_swaps"] = [swap]
        consumer = SessionConsumer.scripted(
            executor,
            [
                _block_event(100),
                _batch_event(
                    100,
                    # The batch's own solver row for the path — the request the
                    # inline sim was issued from (payload-served rows are skipped
                    # from the sim lane, not from the batch).
                    rows=[
                        _solver_row(
                            path_id,
                            optimal_input=1_000,
                            hop_outputs=(1_100, 1_000),
                            consumed_inputs=(1_000, 1_100),
                        )
                    ],
                    payloads={path_id: payload},
                ),
            ],
        )

        await consumer.run()

        drained = await _drain(executor)
        assert len(drained) == 1
        batch = drained[0]
        assert sorted(rec.assembly_label for rec in batch.records) == [
            "assembled",
            "skip-payload-served",
        ]
        failed = [rec for rec in batch.records if isinstance(rec.simulate, SimulateVerdict.Failed)]
        assert len(failed) == 1
        match failed[0].simulate:
            case SimulateVerdict.Failed(detail=detail):
                record = detail.record()
            case _:  # pragma: no cover - the filter above pins the variant
                pytest.fail("expected a failed record")

        assert record["bucket"] == "no-profit"
        assert record["optimal_input"] == 1_000
        assert record["hop_outputs"] == [1_100, 1_000]
        assert len(record["captured_swaps"]) == 1
        assert record["captured_swaps"][0]["amount1"] == 3_000
        assert record["captured_swaps"][0]["emitter"] == "0x" + "cc" * 20

    async def test_a_revert_dump_carries_its_revert_data_and_failing_call(
        self, nxm2bf_v2_engine_and_path
    ) -> None:
        """A revert's dump keeps the revert bytes + the failing call index (the
        payload's own failure fields), beside the request's figures."""
        engine, path_id = nxm2bf_v2_engine_and_path
        executor = _executor(engine)
        payload = inline_sim_payload(
            path_id,
            net=0,
            failure={
                "bucket": "unknown:0xcafebabe",
                "fail_index": 3,
                "revert_data": "0xcafebabe",
            },
        )
        consumer = SessionConsumer.scripted(
            executor,
            [
                _block_event(100),
                _batch_event(
                    100,
                    rows=[
                        _solver_row(
                            path_id,
                            optimal_input=2_000,
                            hop_outputs=(2_100, 2_000),
                            consumed_inputs=(2_000, 2_100),
                        )
                    ],
                    payloads={path_id: payload},
                ),
            ],
        )

        await consumer.run()

        drained = await _drain(executor)
        assert len(drained) == 1
        record = next(
            rec.simulate.detail.record()
            for rec in drained[0].records
            if isinstance(rec.simulate, SimulateVerdict.Failed)
        )
        assert record["bucket"] == "unknown:0xcafebabe"
        assert record["revert_data"] == "0xcafebabe"
        assert record["fail_index"] == 3
        assert record["optimal_input"] == 2_000
        assert record["hop_outputs"] == [2_100, 2_000]
        assert record["captured_swaps"] == []
