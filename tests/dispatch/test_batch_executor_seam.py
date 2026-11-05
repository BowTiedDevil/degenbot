"""The BatchExecutor construction + drain seam (the cut-over).

Pins the PyO3 surface over ``degenbot-batch-executor`` offline: construction
with resolved policy values, the loud payload-arm resolve miss (the pinned
``ValueError`` string), enqueue → drain of the Batch outcome records, and the
render fold (:meth:`MergedOutcome.from_records`) over real drained records.
The engine fixture is REAL (offline-registered pools); the provider is a
dead-port URL (alloy's transport is lazy) and the session runs dry-run, so no
live RPC leaves the process.
"""

from __future__ import annotations

from typing import Any

import pytest

from degenbot.dispatch import (
    AssemblyVerdict,
    BatchExecutor,
    Dispatcher,
    SimulateContext,
    SimulateVerdict,
    SubmitSkipReason,
    TxSigner,
    typed_submit_record,
)
from degenbot.runner._dispatch import RawEngineResult
from degenbot.runner._render import MergedOutcome
from degenbot.runner._sim_submit import BatchWork
from tests.helpers.seam import AlloyProvider, AsyncAlloyProvider, build_batch_executor_py
from tests.helpers.sim_records import inline_sim_payload

# Canonical mainnet addresses (the seam-suite constants).
OWNER = "0x9c56a29c7231974c269e24f9fb3c29203039089e"
EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"
MULTICALL3 = "0xcA11bde05977b3631167028862bE2a173976CA11"

_RPC_URL = "http://127.0.0.1:1"  # a dead port — the transport never dials


def _ctx() -> SimulateContext:
    return SimulateContext(
        provider=AsyncAlloyProvider(AlloyProvider(_RPC_URL)),
        executor_owner=OWNER,
        executor_address=EXECUTOR,
        weth_address=WETH,
        pool_manager_address=POOL_MANAGER,
        multicall3_address=MULTICALL3,
        inject_code=False,
    )


def _executor(
    engine: Any,
    *,
    dry_run: bool = True,
) -> BatchExecutor:
    """Build the session executor over the fixture engine, offline."""
    return build_batch_executor_py(
        context=_ctx(),
        dispatcher=Dispatcher.for_block(100),
        engine=engine,
        signer=TxSigner(key="0x" + "11" * 32, chain_id=1),
        submit_provider=AsyncAlloyProvider(AlloyProvider(_RPC_URL)),
        operator_nonce=5,
        dry_run=dry_run,
        broadcast_providers=None,
    )


class TestConstruction:
    """The construction boundary: policy values in, executor out."""

    def test_construction_is_the_typed_executor(self, nxm2bf_v2_engine_and_path) -> None:
        engine, _path_id = nxm2bf_v2_engine_and_path
        executor = _executor(engine)
        assert isinstance(executor, BatchExecutor)
        assert executor.enqueued == 0
        assert executor.submitted == 0


class TestPayloadResolveMiss:
    """The loud payload-arm resolve miss (the pinned contract)."""

    def test_unregistered_payload_path_fails_loud(self, nxm2bf_v2_engine_and_path) -> None:
        engine, _path_id = nxm2bf_v2_engine_and_path
        executor = _executor(engine)
        with pytest.raises(ValueError, match="not registered in this engine"):
            executor.enqueue(
                BatchWork(
                    results=[],
                    block_timestamp=1_700_000_000,
                    base_fee_next=30,
                    current_block=100,
                    payloads={999: inline_sim_payload(999)},
                )
            )


class TestEnqueueDrainFold:
    """Enqueue → shutdown → drain → the render fold over real records."""

    async def test_payload_batch_drains_records_and_folds(self, nxm2bf_v2_engine_and_path) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        executor = _executor(engine)
        executor.enqueue(
            BatchWork(
                results=[],
                block_timestamp=1_700_000_000,
                base_fee_next=30,
                current_block=100,
                payloads={path_id: inline_sim_payload(path_id)},
            )
        )
        await executor.shutdown()
        assert executor.submitted == 1

        batch = await executor.next_outcome()
        assert batch is not None
        # The drained records: one payload row through the sim seam
        # (profitable — its submit rows join the ordered lane).
        assert [rec.path_id for rec in batch.records] == [path_id]
        assert batch.records[0].assembly is AssemblyVerdict.Assembled
        assert isinstance(batch.records[0].simulate, SimulateVerdict.Profitable)

        # The raw submit-lane records: the dry-run skip, typed decode intact.
        records = [typed_submit_record(raw) for raw in batch.submit_records]
        assert records[0].path_id == path_id
        assert records[0].reason is SubmitSkipReason.DRY_RUN

        # The render fold: counters arrive as folds over the records.
        outcome = MergedOutcome.from_records(batch.records)
        assert len(outcome.gas_profitable) == 1
        assert outcome.gas_profitable[0].net_profit == 500_000_000_000
        assert outcome.gas_unprofitable_count == 0
        assert outcome.candidate_count == 1
        assert outcome.path_infos[path_id]["path_type"] == "V2-V2"
        assert bool(outcome)
        # The lane is drained.
        assert await executor.next_outcome() is None


class TestPayloadFailureDetail:
    """A payload FAILURE's dump record carries the real figures.

    The inline-sim arm builds its failure record from the payload, which carries
    only the bucket + revert bytes. The request's ``optimal_input`` / expected
    ``hop_outputs`` are joined from the batch's own solver row at the failure
    site, and the sim's captured swaps ride the payload — so the trap's
    ``[sim-fixture]`` dump is evidence, not ``optimal_input=0 hop_outputs=[]``.
    """

    async def test_the_failure_detail_threads_the_request_and_the_capture(
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
        executor.enqueue(
            BatchWork(
                # The batch's own solver row for the path — the request the
                # inline sim was issued from (payload-served rows are skipped
                # from the sim lane, not from the batch).
                results=[
                    RawEngineResult(
                        path_id=path_id,
                        optimal_input=1_000,
                        engine_profit=1,
                        hop_outputs=(1_100, 1_000),
                        consumed_inputs=(1_000, 1_100),
                        solve_block=100,
                        state_nonces=(0, 0),
                    )
                ],
                block_timestamp=1_700_000_000,
                base_fee_next=30,
                current_block=100,
                payloads={path_id: payload},
            )
        )
        await executor.shutdown()

        batch = await executor.next_outcome()
        assert batch is not None
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
        executor.enqueue(
            BatchWork(
                results=[
                    RawEngineResult(
                        path_id=path_id,
                        optimal_input=2_000,
                        engine_profit=1,
                        hop_outputs=(2_100, 2_000),
                        consumed_inputs=(2_000, 2_100),
                        solve_block=100,
                        state_nonces=(0, 0),
                    )
                ],
                block_timestamp=1_700_000_000,
                base_fee_next=30,
                current_block=100,
                payloads={path_id: payload},
            )
        )
        await executor.shutdown()

        batch = await executor.next_outcome()
        assert batch is not None
        record = next(
            rec.simulate.detail.record()
            for rec in batch.records
            if isinstance(rec.simulate, SimulateVerdict.Failed)
        )
        assert record["bucket"] == "unknown:0xcafebabe"
        assert record["revert_data"] == "0xcafebabe"
        assert record["fail_index"] == 3
        assert record["optimal_input"] == 2_000
        assert record["hop_outputs"] == [2_100, 2_000]
        assert record["captured_swaps"] == []
