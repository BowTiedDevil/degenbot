"""Smoke tests for ``dispatch_profitable_py`` (A4 / QQFTB4).

These are orchestrations-shape tests, not parity tests — A6
(``[sim] A6 — Sim-seam parity tests``) covers behavioral parity against the
Python oracle over the anvil mock corpus. The goal here is to pin the
``#[pyfunction]`` registration + the GIL-release + ``future_into_py`` async
wiring + the empty-candidate short-circuit + the ``PyDispatchOutcome`` join
shape (the join itself, ``SimResult → PySubmitCandidate``, is exercised by
the simulation-core mock-transport tests in
``rust/crates/degenbot-simulation/src/dispatch_profitable.rs`` — A4 only wires
it through the seam).

The empty-candidate case is RPC-free: the core short-circuits before any
``simulate_one`` is spawned (``dispatch_returns_empty_outcome_for_empty_input``
core test), so the offline dead-URL provider never dials.
"""

from __future__ import annotations

import pytest

from degenbot.arbitrage.hop_info import PathInfo, V2HopInfo
from degenbot._ffi import (
    AlloyProvider,
    AsyncAlloyProvider,
    PyDispatchCandidate,
    PyDispatcher,
    PyDispatchOutcome,
    PySimulateContext,
    PySubmitCandidate,
    dispatch_profitable_py,
)

# Canonical mainnet addresses (parity corpus constants — match the A2 test
# scaffolding in tests/rust/test_simulation_seam_classes.py).
OWNER = "0x9c56a29c7231974c269e24f9fb3c29203039089e"
EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"
MULTICALL3 = "0xcA11bde05977b3631167028862bE2a173976CA11"

# A dead-port URL — alloy's HTTP transport is lazy (no dial at construction);
# the empty-candidate case never dispatches an RPC, so the provider never dials.
_RPC_URL = "http://127.0.0.1:1"


def _make_async_provider() -> AsyncAlloyProvider:
    sync = AlloyProvider(_RPC_URL)
    return AsyncAlloyProvider(sync)


def _make_ctx(*, inject_code: bool = False) -> PySimulateContext:
    return PySimulateContext(
        provider=_make_async_provider(),
        executor_owner=OWNER,
        executor_address=EXECUTOR,
        weth_address=WETH,
        pool_manager_address=POOL_MANAGER,
        multicall3_address=MULTICALL3,
        inject_code=inject_code,
        executor_runtime_bytecode=b"\xde\xad\xbe\xef",
        injected_address=EXECUTOR if inject_code else None,
    )


def _v2_path_info() -> PathInfo:
    return PathInfo(
        hops=[
            V2HopInfo(
                pool_address="0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
                token0_address=WETH,
                token1_address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
                fee=30,
                zfo=False,
            )
        ]
    )


class TestDispatchProfitablePyRegistration:
    """The pyfunction is registered on the module surface."""

    def test_callable_is_registered(self) -> None:
        # `dispatch_profitable_py` is a module-level callable (the
        # `wrap_pyfunction!` registration in add_simulation_module).
        assert callable(dispatch_profitable_py)


class TestDispatchEmptyInput:
    """The empty-candidate short-circuit — proves the GIL release +
    ``future_into_py`` async wiring + the ``PyDispatchOutcome`` join shape
    end-to-end (no RPC dispatched; the dead-URL provider never dials)."""

    async def test_empty_candidates_returns_empty_outcome(self) -> None:
        ctx = _make_ctx()
        dispatcher = PyDispatcher.for_block(100)
        outcome = await dispatch_profitable_py(
            candidates=[],
            context=ctx,
            dispatcher=dispatcher,
            base_fee_next=1_000_000_000,
            current_block=100,
            min_profit_net=1,
            min_profit_margin_bps=0,
        )
        assert isinstance(outcome, PyDispatchOutcome)
        # The join ran (over zero survivors) → empty gas_profitable.
        assert outcome.gas_profitable == []
        # The core's empty-input short-circuit (the
        # `dispatch_returns_empty_outcome_for_empty_input` core test):
        # all counters zero.
        assert outcome.gas_unprofitable_count == 0
        assert outcome.exception_count == 0
        assert outcome.fail_count == 0
        assert outcome.candidate_count == 0
        assert outcome.suppressed_count == 0
        assert outcome.thin_dropped == 0
        assert outcome.fail_buckets == {}

    async def test_empty_candidates_releases_gil_across_await(self) -> None:
        # The function is `async` (returns a coroutine) — `future_into_py`
        # drives the core future on the tokio runtime the Python event loop
        # owns. The bare fact that the await resolves (rather than raising
        # `RuntimeError: no running event loop` or a tokio-init failure) proves
        # the GIL-release + async-runtime wiring.
        ctx = _make_ctx()
        dispatcher = PyDispatcher.for_block(100)
        coro = dispatch_profitable_py(
            candidates=[],
            context=ctx,
            dispatcher=dispatcher,
            base_fee_next=1_000_000_000,
            current_block=100,
            min_profit_net=1,
            min_profit_margin_bps=0,
        )
        assert hasattr(coro, "__await__")
        outcome = await coro
        assert isinstance(outcome, PyDispatchOutcome)


class TestDispatchArgumentValidation:
    """GIL-held arg extraction — the candidate-list must hold
    ``PyDispatchCandidate`` instances (the rejection raises synchronously
    BEFORE ``future_into_py``, proving the validation lives in the GIL-held
    arg-extraction phase, not the async block)."""

    async def test_non_candidate_list_element_raises_value_error(self) -> None:
        ctx = _make_ctx()
        dispatcher = PyDispatcher.for_block(100)
        # A bare int is not a PyDispatchCandidate → the extract fails →
        # ValueError (synchronous, before the future is created).
        with pytest.raises(ValueError, match="PyDispatchCandidate instances"):
            await dispatch_profitable_py(
                candidates=[42],  # type: ignore[list-item]
                context=ctx,
                dispatcher=dispatcher,
                base_fee_next=1_000_000_000,
                current_block=100,
                min_profit_net=1,
                min_profit_margin_bps=0,
            )

    async def test_non_list_candidates_raises_type_error(self) -> None:
        ctx = _make_ctx()
        dispatcher = PyDispatcher.for_block(100)
        # PyO3's `&Bound<PyList>` extraction rejects a non-list at the FFI
        # boundary (before the body runs) — a TypeError, not our ValueError.
        with pytest.raises(TypeError):
            await dispatch_profitable_py(
                candidates=42,  # type: ignore[arg-type]
                context=ctx,
                dispatcher=dispatcher,
                base_fee_next=1_000_000_000,
                current_block=100,
                min_profit_net=1,
                min_profit_margin_bps=0,
            )


class TestDispatchJoinShape:
    """The ``PyDispatchOutcome.gas_profitable`` getter returns
    ``list[PySubmitCandidate]`` (the submission seam's input shape): the
    cockpit chains ``dispatch_profitable_py → dispatch_and_submit_py``
    straight through that list. The empty case exercises the getter's list
    construction (it returns an empty list, not None / a dict /
    something else)."""

    async def test_gas_profitable_is_a_list(self) -> None:
        ctx = _make_ctx()
        dispatcher = PyDispatcher.for_block(100)
        outcome = await dispatch_profitable_py(
            candidates=[],
            context=ctx,
            dispatcher=dispatcher,
            base_fee_next=1_000_000_000,
            current_block=100,
            min_profit_net=1,
            min_profit_margin_bps=0,
        )
        assert isinstance(outcome.gas_profitable, list)


class TestPySubmitCandidateGetters:
    """Read-only getters on ``PySubmitCandidate`` — the rewired ``[dispatch]``
    per-path log reads ``path_id``/``gross_profit``/``net_profit``/
    ``gas_used``/``priority_fee`` from each survivor, so the pyclass must
    expose them (it previously exposed only ``#[new]``)."""

    def test_money_and_gas_getters_round_trip(self) -> None:
        cand = PySubmitCandidate(
            path_id=42,
            gross_profit=2_000_000_000_000_000_000,
            net_profit=1_500_000_000_000_000_000,
            gas_used=200_000,
            priority_fee=2_000_000_000,
            base_fee_next=1_000_000_000,
            execute_calldata=b"\xde\xad",
            executor_address=EXECUTOR,
        )
        assert cand.path_id == 42
        assert cand.gross_profit == 2_000_000_000_000_000_000
        assert cand.net_profit == 1_500_000_000_000_000_000
        assert cand.gas_used == 200_000
        assert cand.priority_fee == 2_000_000_000


class TestDispatchWithCandidateButNoRpc:
    """A candidate that's pre-filtered (suppressed) never dispatches an RPC —
    the dead-URL provider never dials. Exercises the suppression pre-filter
    path through the seam: by suppressing a path past the threshold, the
    candidate is dropped pre-sim (the asserter queue stays untouched — the
    core's `dispatch_predilters_suppressed_paths` test proves this at the core
    level; this test proves the seam propagates `suppressed_count` through to
    the outcome)."""

    async def test_suppressed_candidate_is_dropped_pre_sim(self) -> None:
        ctx = _make_ctx()
        dispatcher = PyDispatcher.for_block(100)
        # Manually suppress path 7 past the threshold (10 failures).
        for _ in range(10):
            dispatcher.record_failure(7)
        candidate = PyDispatchCandidate(
            path_id=7,
            optimal_input=1_000_000_000_000_000_000,
            engine_profit=2_000_000_000_000_000_000,
            hop_outputs=[1_500_000_000_000_000_000],
            solve_block=100,
            path_info=_v2_path_info(),
        )
        outcome = await dispatch_profitable_py(
            candidates=[candidate],
            context=ctx,
            dispatcher=dispatcher,
            base_fee_next=1_000_000_000,
            current_block=50,  # < PATH_SUPPRESS_RETRY_INTERVAL (100) → still suppressed
            min_profit_net=1,
            min_profit_margin_bps=0,
        )
        assert isinstance(outcome, PyDispatchOutcome)
        # The suppressed candidate is dropped pre-sim — no RPC, no survivors.
        assert outcome.suppressed_count == 1
        assert outcome.candidate_count == 0
        assert outcome.gas_profitable == []
        assert outcome.gas_unprofitable_count == 0
        # The path_info join map carries the INPUT candidate's PathInfo (keyed
        # by path_id) even though the path was suppressed pre-sim — the
        # `[profit]` hop-detail log looks up `path_infos[cand.path_id]` per
        # survivor, so the map is populated from the input batch, not filtered
        # to survivors. Exercises the Rust->Python PathInfo converter (Rust
        # V2HopInfo -> Python V2HopInfo) end-to-end WITHOUT an RPC.
        assert isinstance(outcome.path_infos, dict)
        assert set(outcome.path_infos.keys()) == {7}
        pi = outcome.path_infos[7]
        assert len(pi.hops) == 1
        hop = pi.hops[0]
        assert isinstance(hop, V2HopInfo)
        # EIP-55 checksummed (alloy Address Display) — matches the Python
        # cockpit's `h.pool_address` form.
        assert hop.pool_address == "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
        assert hop.zfo is False
        assert hop.fee == 30
