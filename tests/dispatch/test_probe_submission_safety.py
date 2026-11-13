"""Execution probes are non-submitting: a PROFITABLE probe still cannot reach a
submission, and the FFI probe entry refuses a submission-capable executor.

Two halves carry the guarantee, and both are pinned here over a REAL executor
built by the FFI:

* the CONSTRUCTION half (``_sim_submit.SubmissionStance.PROBE``) builds the
  session's probe pipeline with submission disabled and no relay fan-out —
  pinned in ``tests/arbitrage/test_execution_probe.py``;
* the ENTRY half is the core's own guard (``BatchExecutor::enqueue_probe``):
  a submission-capable executor cannot run a probe AT ALL, so no probe row can
  reach the nonce lease, the signer, or the broadcast arm even when a caller
  holds a live executor.

The positive evidence is a POWERED probe, not an empty lane: the profitable
case below drives the certified adapter through the compiled Vyper runtime
(the deterministic profitable fixture the dispatch seam suite already owns, over
a loopback JSON-RPC stub), so the probe row really reaches a profitable
simulation and really reaches the submit leaf — which records the dry-run skip
and nothing else. A probe whose simulation failed would produce no submit
candidate at all, and an ``all(...)``-over-an-empty-list assertion would prove
nothing; that is why this file drives one candidate all the way to the skip.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

import pytest

from degenbot.dispatch import (
    BatchExecutor,
    Dispatcher,
    SimulateContext,
    SimulateVerdict,
    SkippedRecord,
    SubmitSkipReason,
    TxSigner,
    typed_submit_record,
)
from tests.dispatch.test_custom_executor_contract import (
    # The certified profitable fixture, reused rather than re-minted: the
    # compiled Vyper runtime, the adapter builders that encode one declared
    # call, the loopback JSON-RPC stub that answers the sim's cold-state reads,
    # and the executor construction the dispatch seam already certifies as
    # profitable. Only this file's subject is new — the probe entry on top.
    CONTRACT_PATH,
    DEPLOYMENT_ID,
    LAST_VALUE_SIGNATURE,
    MARKER,
    OWNER,
    RECORD_SIGNATURE,
    _adapter,
    _CannedChainNode,
    _compile,
    _declaration,
    _executor,
    _getter_read,
)
from tests.helpers.seam import (
    AlloyProvider,
    AsyncAlloyProvider,
    abi_encode_call,
    build_batch_executor_py,
)

if TYPE_CHECKING:
    from collections.abc import Iterator

    from degenbot.arbitrage.engine_registry import ArbitrageEngine


# Canonical mainnet addresses (the seam-suite constants).
EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"

_RPC_URL = "http://127.0.0.1:1"  # a dead port — the transport never dials

BLOCK = 100
BASE_FEE = 1_000_000_000


@pytest.fixture
def canned_chain() -> Iterator[_CannedChainNode]:
    """The certified fixture's loopback JSON-RPC stub (see ``_CannedChainNode``).

    The engine executes in-process against the fixture's own state and only calls
    out for a cold miss, so this stub is the whole wire the probe can touch —
    which is what makes its recorded method list a broadcast/access-list spy.
    """
    node = _CannedChainNode()
    try:
        yield node
    finally:
        node.close()


@pytest.fixture(scope="module")
def certified_probe_compiled() -> dict[str, Any]:
    """The certified profitable fixture's compiled artifact (one compile per module)."""
    return _compile(CONTRACT_PATH)


def _dead_rpc_executor(engine: ArbitrageEngine, *, dry_run: bool) -> BatchExecutor:
    """The session executor over the fixture engine, offline.

    ``dry_run=False`` is the SUBMISSION-CAPABLE shape a live session builds;
    ``dry_run=True`` is the non-submitting lane a probe is allowed to use.
    """
    provider = AsyncAlloyProvider(AlloyProvider(_RPC_URL))
    return build_batch_executor_py(
        context=SimulateContext(
            provider=provider,
            executor_owner=OWNER,
            executor_address=EXECUTOR,
            weth_address=WETH,
            pool_manager_address=POOL_MANAGER,
            inject_code=False,
        ),
        dispatcher=Dispatcher.for_block(0),
        engine=engine,
        signer=TxSigner(key="0x" + "11" * 32, chain_id=1),
        submit_provider=provider,
        operator_nonce=5,
        dry_run=dry_run,
        broadcast_providers=None,
    )


async def _probe(executor: BatchExecutor, path_id: int) -> Any:
    """Probe one registered 2-hop path with an explicit amount vector."""
    return await executor.probe_path(
        path_id=path_id,
        optimal_input=10**15,
        hop_outputs=[10**15, 10**15],
        consumed_inputs=[10**15, 10**15],
        current_block=BLOCK,
        base_fee_next=BASE_FEE,
        block_timestamp=1_700_000_000,
    )


async def test_a_probe_is_refused_on_a_submission_capable_executor(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
) -> None:
    """A live executor refuses the probe entry before the row enters the lane.

    The refusal is UNCONDITIONAL — it reads the executor's submission policy, not
    the amount vector — so no probe, a profitable one included, can reach
    ``dispatch_and_submit``, the signer, a broadcast provider, or the nonce lane.
    """
    engine, path_id = nxm2bf_v2_engine_and_path
    executor = _dead_rpc_executor(engine, dry_run=False)

    with pytest.raises(RuntimeError, match="probe_path refused"):
        await _probe(executor, path_id)

    assert executor.enqueued == 0, "the refusal fires before the lane sees the row"


async def test_a_profitable_probe_reaches_the_dry_run_guard_and_stops(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
    canned_chain: _CannedChainNode,
    certified_probe_compiled: dict[str, Any],
) -> None:
    """A POWERED probe: profitable simulation in, dry-run skip out, no broadcast.

    This is the non-vacuous half of the guarantee. The probe row runs through the
    certified adapter's compiled runtime, so the sim answers
    ``SimulateVerdict.Profitable`` with the declared read's own delta — a real
    submit candidate — and the submit leaf then refuses it as
    ``SubmitSkipReason.DRY_RUN``. The skip is therefore evidence about the
    dry-run guard, not about an empty lane, and the counted submit batch proves
    the leaf ran rather than never being reached.

    The nonce-and-broadcast seam rides along as a wire-level spy: the guard sits
    before the access-list recompute (which needs the leased nonce) and before
    the broadcast, so the stub endpoint sees neither call. The direct
    authority-state assertion lives at the Rust seam
    (``a_dry_run_submission_leaves_the_nonce_authority_untouched``, with its live
    control ``a_live_submission_stamps_the_nonce_the_dry_run_left_free``).
    """
    engine, path_id = nxm2bf_v2_engine_and_path
    runtime = bytes.fromhex(certified_probe_compiled["bytecode_runtime"].removeprefix("0x"))
    assert runtime, "the pinned Vyper compiler produced runtime bytecode"

    payload = abi_encode_call(RECORD_SIGNATURE, [MARKER])
    adapter = _adapter(
        lambda _result: payload,
        probes=[_getter_read("LAST_VALUE", LAST_VALUE_SIGNATURE)],
        declaration=_declaration(certified_probe_compiled["abi"]),
    )
    # The certified construction: the loopback stub's URL, the declared runtime,
    # and the adapter that encodes one ``record(MARKER)`` call.
    executor = _executor(
        engine,
        rpc_url=canned_chain.url,
        runtime=runtime,
        adapter=adapter,
    )

    batch = await _probe(executor, path_id)

    # 1. The probe's own record: the simulation was PROFITABLE, which is what
    #    puts a candidate in front of the submit lane at all. The batch fans the
    #    ONE probe row out to every composed deployment (the built-in settlement
    #    one, whose target holds no code here, beside the certified one), so the
    #    record of interest is the certified deployment's own.
    (record,) = [rec for rec in batch.records if rec.candidate_id.deployment_id == DEPLOYMENT_ID]
    assert sorted(rec.candidate_id.deployment_id for rec in batch.records) == [
        1,
        DEPLOYMENT_ID,
    ], "the one probe row ran against both composed deployments"
    assert record.candidate_id.path_id == path_id
    assert record.assembly_label == "assembled"
    simulate = record.simulate
    assert isinstance(simulate, SimulateVerdict.Profitable), (
        "the probe row must reach a profitable simulation for the skip below to "
        f"mean anything: {simulate!r}"
    )
    assert simulate.receipt.gross_profit == MARKER, (
        "the gross figure is the declared read's post-minus-pre delta"
    )
    assert simulate.receipt.net_profit > 0

    # 2. The submit lane RAN (the row was accepted by the ordered lane) and its
    #    only record for that candidate is the dry-run skip.
    assert executor.submitted == 1, "the probe row reached the ordered submit lane"
    assert [raw["kind"] for raw in batch.submit_records] == ["skipped"], (
        f"one skip, no submission: {batch.submit_records!r}"
    )
    (submit,) = [typed_submit_record(raw) for raw in batch.submit_records]
    assert isinstance(submit, SkippedRecord)
    assert submit.candidate.path_id == path_id
    assert submit.reason is SubmitSkipReason.DRY_RUN, (
        "the refusal is the dry-run guard, not an economic refusal upstream of it"
    )

    # 3. The wire-level spy: the access-list pass runs AFTER the nonce lease and
    #    the broadcast runs after the signature, so neither reached the endpoint.
    assert "eth_createAccessList" not in canned_chain.methods, (
        f"the nonce-lease tail never ran: {canned_chain.methods!r}"
    )
    assert "eth_sendRawTransaction" not in canned_chain.methods, (
        f"nothing was broadcast: {canned_chain.methods!r}"
    )


async def test_a_probe_still_answers_its_sim_stage(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
) -> None:
    """A non-submitting executor still executes the probe (the probe still works).

    The built-in settlement deployment is code-less here (a dead-port provider,
    no injected runtime), so this probe's sim fails and produces no submit
    candidate — the probe still answers its assembly record rather than refusing
    or hanging. The profitable case above is the one that pins the submit half.
    """
    engine, path_id = nxm2bf_v2_engine_and_path
    executor = _dead_rpc_executor(engine, dry_run=True)

    batch = await _probe(executor, path_id)

    record = batch.records[0]
    assert record.assembly_label == "assembled"
    assert record.path_type == "V2-V2"
    assert isinstance(record.simulate, SimulateVerdict.Failed), (
        "the code-less built-in target cannot execute: the sim fails offline"
    )
    assert batch.submit_records == [], (
        "a failed simulation produces NO submit candidate, so this fixture cannot "
        "pin the dry-run guard -- an all()/no-submission assertion over this empty "
        "list would be vacuous. The profitable case above is the test that carries "
        "that evidence."
    )
