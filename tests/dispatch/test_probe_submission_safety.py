"""Execution probes are non-submitting BY CONSTRUCTION.

The probe now runs on the core's dedicated `ProbeExecutor`, whose construction
takes only sim-side handles: no `SubmitLeaf`, no signer, no nonce lane, no relay
fan-out, no receipt probe, and no broadcast target. Non-submission is therefore a
MISSING FIELD set rather than a dry-run flag, a policy check, or a refused
enqueue, which changes what this file can honestly pin:

* there is no submit lane to reach, so a profitable probe's evidence is the
  profitable record + the encoder's planning row, never a dry-run skip record;
* non-submission is pinned on the report's own surface: the probe record carries
  no submit verdict to read;
* the ordinary production lane stays buildable beside it — `build_batch_executor_py`
  still takes the submitting handles for ordinary candidates.

The positive evidence is still a POWERED probe: the certified adapter runs the
compiled Vyper runtime (the deterministic profitable fixture the dispatch seam
suite owns, over a loopback JSON-RPC stub), so the row really reaches a
profitable simulation, really produces an encoded planning row, and still
reaches no broadcast — the stub's recorded method list is the wire-level spy.
"""

from __future__ import annotations

import asyncio
import contextlib
from typing import TYPE_CHECKING, Any

import pytest

from degenbot.dispatch import (
    BatchExecutor,
    Dispatcher,
    ExecutionProbeRequest,
    ProbeEncodedCall,
    ProbeExecutor,
    ProbeReport,
    SimulateContext,
    SimulationDeployment,
    SimulateVerdict,
    TxSigner,
    ZeroCapitalDeficit,
)
from tests.dispatch.test_custom_executor_contract import (
    # The certified profitable fixture, reused rather than re-minted: the
    # compiled Vyper runtime, the adapter builders that encode one declared
    # call, the loopback JSON-RPC stub that answers the sim's cold-state reads,
    # and the executor construction the dispatch seam already certifies as
    # profitable. Only this file's subject is new — the probe construction on
    # top.
    CONTRACT_PATH,
    DEPLOYMENT_ID,
    GAS_LIMIT,
    LAST_VALUE_SIGNATURE,
    MARKER,
    OWNER,
    RECORD_SIGNATURE,
    TARGET,
    _adapter,
    _CannedChainNode,
    _compile,
    _declaration,
    _getter_read,
)
from tests.helpers.seam import (
    AlloyProvider,
    AsyncAlloyProvider,
    abi_encode_call,
    build_batch_executor_py,
    build_probe_executor_py,
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


def _ctx(rpc_url: str = _RPC_URL) -> SimulateContext:
    return SimulateContext(
        provider=AsyncAlloyProvider(AlloyProvider(rpc_url)),
        executor_owner=OWNER,
        executor_address=EXECUTOR,
        weth_address=WETH,
        pool_manager_address=POOL_MANAGER,
        inject_code=False,
    )


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


def _probe_executor(
    engine: ArbitrageEngine,
    *,
    rpc_url: str = _RPC_URL,
    compiled: dict[str, Any] | None = None,
    capital: str | None = None,
) -> ProbeExecutor:
    """Build a probe executor over the fixture engine.

    ``compiled`` registers the certified profitable deployment (its runtime and
    its own ABI); omitting it leaves only the code-less built-in settlement
    deployment, which cannot execute offline — the shape the failed-sim case
    pins.
    """
    deployments = None
    if compiled is not None:
        runtime = bytes.fromhex(compiled["bytecode_runtime"].removeprefix("0x"))
        payload = abi_encode_call(RECORD_SIGNATURE, [MARKER])
        adapter = _adapter(
            lambda _result: payload,
            probes=[_getter_read("LAST_VALUE", LAST_VALUE_SIGNATURE)],
            declaration=_declaration(compiled["abi"]),
        )
        deployments = [_deployment(adapter, runtime)]
    return build_probe_executor_py(
        context=_ctx(rpc_url),
        dispatcher=Dispatcher.for_block(0),
        engine=engine,
        provider=AsyncAlloyProvider(AlloyProvider(rpc_url)),
        capital=capital,
        deployments=deployments,
        target_selection="all_targets",
    )


def _deployment(adapter: Any, runtime: bytes) -> SimulationDeployment:
    """The certified foreign deployment: the compiled runtime at its own target."""
    return SimulationDeployment(DEPLOYMENT_ID, adapter, OWNER, TARGET, GAS_LIMIT, code=runtime)


async def _probe(executor: ProbeExecutor, request: ExecutionProbeRequest) -> ProbeReport:
    """Await one probe call.

    The FFI hands back a future rather than a coroutine, so a helper is what makes
    the call schedulable as a task (and cancellable) from a test.
    """
    return await executor.probe(request)


def _request(path_id: int, *, optimal_input: int = 10**15) -> ExecutionProbeRequest:
    """One typed probe request over the certified 2-hop fixture path."""
    # The vector is CONSISTENT by construction: the certified fixture's encoder
    # validates the plan it derives, so a per-hop output below the input would be
    # a fixture bug (the encoder panics on an inconsistent plan), not a probe one.
    return ExecutionProbeRequest(
        path_id=path_id,
        optimal_input=optimal_input,
        hop_outputs=[optimal_input, optimal_input],
        consumed_inputs=[optimal_input, optimal_input],
        current_block=BLOCK,
        base_fee_next=BASE_FEE,
        block_timestamp=1_700_000_000,
    )


async def test_a_profitable_probe_reports_its_plan_and_never_submits(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
    canned_chain: _CannedChainNode,
    certified_probe_compiled: dict[str, Any],
) -> None:
    """A POWERED probe: profitable simulation in, planning row out, no broadcast.

    This is the no-SubmitLane end state. The probe row runs through the certified
    adapter's compiled runtime, so the sim answers `Profitable` with the declared
    read's own delta and the production encoder composes a real row — evidence
    about the pipeline, not about an empty lane. What it canNOT produce is a
    submission: the report has no submit-record list, the record's `submit` slot
    is `None`, and the stub endpoint never sees an access-list or broadcast call.
    """
    engine, path_id = nxm2bf_v2_engine_and_path
    assert certified_probe_compiled["bytecode_runtime"], "the pinned compiler produced runtime"

    executor = _probe_executor(
        engine,
        rpc_url=canned_chain.url,
        compiled=certified_probe_compiled,
        capital="self_fund",
    )
    assert executor.capital == "self_fund"

    report = await executor.probe(_request(path_id))

    assert isinstance(report, ProbeReport)
    assert report.capital == "self_fund", "the report states the posture it ran under"

    # 1. The profitable record, from the certified deployment's own lane.
    (record,) = [rec for rec in report.records if rec.candidate_id.deployment_id == DEPLOYMENT_ID]
    assert sorted(rec.candidate_id.deployment_id for rec in report.records) == [
        1,
        DEPLOYMENT_ID,
    ], "the one probe row ran against both composed deployments"
    assert record.candidate_id.path_id == path_id
    assert record.assembly_label == "assembled"
    simulate = record.simulate
    assert isinstance(simulate, SimulateVerdict.Profitable), (
        f"the probe row must reach a profitable simulation: {simulate!r}"
    )
    assert simulate.receipt.gross_profit == MARKER
    assert simulate.receipt.net_profit > 0

    # 2. Non-submission is STRUCTURAL: there is no submit verdict to read.
    assert record.submit is None, "the probe executor has no submit stage to fill it"

    # 3. The PLANNING evidence: the production encoder's own row for that
    #    candidate — calldata, native value, and the deployment's target.
    (encoded,) = [row for row in report.encoded if row.candidate_id.deployment_id == DEPLOYMENT_ID]
    assert isinstance(encoded, ProbeEncodedCall)
    assert encoded.execute_calldata == abi_encode_call(RECORD_SIGNATURE, [MARKER])
    assert encoded.value == 0
    assert encoded.execution_target.lower() == TARGET.lower()
    assert encoded.net_profit > 0

    # 4. The wire-level spy: nothing that belongs to the submit choreography ran.
    assert "eth_createAccessList" not in canned_chain.methods, (
        f"no nonce-lease tail: {canned_chain.methods!r}"
    )
    assert "eth_sendRawTransaction" not in canned_chain.methods, (
        f"nothing was broadcast: {canned_chain.methods!r}"
    )


async def test_a_failed_probe_record_carries_no_typed_deficit(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
) -> None:
    """An ordinary failure is NOT a capital-policy refusal.

    The typed `zero_capital` slot is a refusal's DATA half: a sim/encoder failure
    carries `None`, which is what keeps a plain failure distinct from
    `insolvent-zero-capital` on the typed surface (and in the render dict) without
    a caller matching on the bucket string.
    """
    engine, path_id = nxm2bf_v2_engine_and_path
    executor = _probe_executor(engine)

    report = await executor.probe(_request(path_id))

    (record,) = report.records
    assert isinstance(record.simulate, SimulateVerdict.Failed)
    detail = record.simulate.detail
    assert detail.zero_capital is None, "no deficit is fabricated for a plain failure"
    assert detail.record()["zero_capital"] is None
    assert isinstance(ZeroCapitalDeficit, type), "the typed refusal class crosses the FFI"


async def test_the_ordinary_production_lane_remains_available(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
) -> None:
    """A production executor is still buildable for ordinary candidates.

    It takes the submitting handles — signer, nonce seed, submit provider, run
    mode, and the relay fan-out — and no capital posture, because a posture
    belongs to a probe.
    """
    engine, _path_id = nxm2bf_v2_engine_and_path
    provider = AsyncAlloyProvider(AlloyProvider(_RPC_URL))
    executor = build_batch_executor_py(
        context=_ctx(),
        dispatcher=Dispatcher.for_block(0),
        engine=engine,
        signer=TxSigner(key="0x" + "11" * 32, chain_id=1),
        submit_provider=provider,
        operator_nonce=5,
        dry_run=True,
        broadcast_providers=None,
    )
    assert isinstance(executor, BatchExecutor), "the ordinary lane is untouched"


async def test_a_probe_still_answers_its_sim_stage_without_a_plan(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
) -> None:
    """A probe whose simulation fails reports the failure and composes no row.

    The built-in settlement deployment is code-less here (a dead-port provider,
    no injected runtime), so the sim fails and the encoder produces nothing —
    which is exactly what "no planning row" must mean, and why the profitable
    case above is the one that carries the encoding evidence.
    """
    engine, path_id = nxm2bf_v2_engine_and_path
    executor = _probe_executor(engine)

    report = await executor.probe(_request(path_id))

    record = report.records[0]
    assert record.assembly_label == "assembled"
    assert record.path_type == "V2-V2"
    assert isinstance(record.simulate, SimulateVerdict.Failed), (
        "the code-less built-in target cannot execute: the sim fails offline"
    )
    assert record.submit is None
    assert report.encoded == [], "a failed simulation produces no encoder row"


@pytest.mark.parametrize(
    ("kwargs", "match"),
    [
        ({"optimal_input": 0}, "optimal_input must be positive"),
        ({"hop_outputs": [], "consumed_inputs": []}, "at least one hop"),
        ({"hop_outputs": [1, 2], "consumed_inputs": [1]}, "one entry per hop"),
    ],
)
def test_a_malformed_request_never_reaches_the_stages(kwargs: dict[str, Any], match: str) -> None:
    """The request validates itself at construction, so a bad vector is refused."""
    fields: dict[str, Any] = {
        "path_id": 7,
        "optimal_input": 10**15,
        "hop_outputs": [10**15, 10**15],
        "consumed_inputs": [10**15, 10**15],
        "current_block": BLOCK,
        "base_fee_next": BASE_FEE,
        "block_timestamp": 1_700_000_000,
    }
    fields.update(kwargs)
    with pytest.raises(ValueError, match=match):
        ExecutionProbeRequest(**fields)


async def test_a_request_for_an_unregistered_path_is_refused(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
) -> None:
    """The path registry is the one thing the request cannot carry: it is checked here."""
    engine, path_id = nxm2bf_v2_engine_and_path
    executor = _probe_executor(engine)

    with pytest.raises(ValueError, match="is not registered"):
        await executor.probe(_request(path_id + 999))

    with pytest.raises(ValueError, match="hops but the request carries"):
        await executor.probe(
            ExecutionProbeRequest(
                path_id=path_id,
                optimal_input=10**15,
                hop_outputs=[10**15],
                consumed_inputs=[10**15],
                current_block=BLOCK,
                base_fee_next=BASE_FEE,
                block_timestamp=1_700_000_000,
            )
        )


@pytest.mark.parametrize("capital", [None, "production", "self_fund", "zero_balance"])
async def test_every_capital_posture_builds(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int], capital: str | None
) -> None:
    """All three postures are reachable through ONE value (and the default is production)."""
    engine, _path_id = nxm2bf_v2_engine_and_path
    executor = _probe_executor(engine, capital=capital)
    assert executor.capital == (capital or "production")


@pytest.mark.parametrize("misspelling", ["self-funded", "zero-balance", "SELF_FUND", ""])
def test_an_unknown_capital_is_refused_at_the_probe_construction(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int], misspelling: str
) -> None:
    """A posture the core does not know is refused, never defaulted."""
    engine, _path_id = nxm2bf_v2_engine_and_path
    with pytest.raises(ValueError, match="unknown capital"):
        _probe_executor(engine, capital=misspelling)


async def test_concurrent_and_cancelled_probes_never_take_one_another_s_outcome(
    nxm2bf_v2_engine_and_path: tuple[ArbitrageEngine, int],
    certified_probe_compiled: dict[str, Any],
    canned_chain: _CannedChainNode,
) -> None:
    """Two concurrent calls and one cancelled call: each survivor reads its own row.

    There is no shared drain to interleave and no queue to strand: each call owns
    its channel, so the pair settles independently and the cancelled call leaves
    nothing for the call that follows.
    """
    engine, path_id = nxm2bf_v2_engine_and_path
    executor = _probe_executor(engine, rpc_url=canned_chain.url, compiled=certified_probe_compiled)

    # A cancelled call: its stage is aborted, and its outcome is not forwarded.
    # Whether it was cancelled mid-flight or had already settled, NOTHING is
    # queued for a successor — there is no queue. The abort-on-drop proof for a
    # call that really is mid-stage lives at the core seam
    # (`probe::tests::a_dropped_probe_aborts_its_stage_future_instead_of_detaching_it`)
    # and at the session seam (a cancelled probe cancels its own call).
    cancelled = asyncio.create_task(_probe(executor, _request(path_id, optimal_input=10**14)))
    await asyncio.sleep(0)
    cancelled.cancel()
    with contextlib.suppress(asyncio.CancelledError):
        await cancelled

    first, second = await asyncio.gather(
        executor.probe(_request(path_id, optimal_input=10**15)),
        executor.probe(_request(path_id, optimal_input=10**15)),
    )

    for report in (first, second):
        (record,) = [
            rec for rec in report.records if rec.candidate_id.deployment_id == DEPLOYMENT_ID
        ]
        assert record.candidate_id.path_id == path_id
        assert record.submit is None
        assert isinstance(record.simulate, SimulateVerdict.Profitable)
        # Each call's report carries the row ITS OWN pass composed, exactly once.
        own_rows = [
            row for row in report.encoded if row.candidate_id.deployment_id == DEPLOYMENT_ID
        ]
        assert len(own_rows) == 1

    # And the executor is still answerable after both the cancellation and the
    # concurrent pair: nothing was consumed out of turn.
    third = await executor.probe(_request(path_id))
    assert third.records, "a later call still gets its own outcome"
    assert "eth_sendRawTransaction" not in canned_chain.methods
