"""Smoke tests for the simulation-seam pyclasses (A2 / TCZ47Z).

These are constructor-shape tests, not behavioral parity tests — A4
(``dispatch_profitable_py``) and A6 (parity) cover behavior. The goal here is
to pin the ``#[new]`` arg extraction + the getter shapes so a follow-up
session cannot silently break the cockpit's construction sites.

Note: ``SimulateContext`` constructs an ``AsyncAlloyProvider`` against a
localhost URL that is never dialed (no RPC call is made at construction — only
the provider arc is cloned). ``DispatchOutcome`` has no ``#[new]``: it is
built internally by ``dispatch_profitable_py`` (A4) via ``from_join``; this
test only asserts it is registered + introspectable.
"""

from __future__ import annotations

import pytest

from degenbot._ffi.provider import AlloyProvider, AsyncAlloyProvider
from degenbot._ffi.simulation import (
    DispatchCandidate,
    DispatchOutcome,
    SimulateContext,
)
from degenbot.arbitrage.engine_registry import ArbitrageEngine

# Canonical mainnet addresses (parity corpus constants).
OWNER = "0x9c56a29c7231974c269e24f9fb3c29203039089e"
EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
WETH = "0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"
MULTICALL3 = "0xcA11bde05977b3631167028862bE2a173976CA11"

# An HTTP URL against a dead port. alloy's HTTP transport is lazy (it does
# not dial at construction), so this constructs offline. No RPC is made
# anywhere in this test: the seam only clones the provider arc.
_RPC_URL = "http://127.0.0.1:1"


def _make_async_provider() -> AsyncAlloyProvider:
    # Sync `AlloyProvider(url)` (the factory's lazy construction path) + the
    # `AsyncAlloyProvider(sync_provider)` wrapper — both offline.
    sync = AlloyProvider(_RPC_URL)
    return AsyncAlloyProvider(sync)


class TestPyDispatchCandidate:
    """The per-path builder — `path_id` resolution + the encode-options flags.

    NXM2BF: the candidate resolves its `composers::PathInfo` from a registered
    `path_id` via `PyArbitrageEngine::path_info_for_core` — no Python `PathInfo`
    dataclass is threaded. The fixture builds a 2-hop V2 cycle (the engine's
    `register_and_solve_path` requires a ≥2-hop cycle); `hop_outputs` must match
    the path's hop count (2) or `__new__` raises `ValueError`.
    """

    def test_constructs_from_engine_and_path_id(
        self, nxm2bf_v2_engine_and_path: tuple[PyArbitrageEngine, int]
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        candidate = DispatchCandidate(
            engine=engine,
            path_id=path_id,
            optimal_input=1_000_000_000_000_000_000,
            engine_profit=2_000_000_000_000_000_000,
            hop_outputs=[1_500_000_000_000_000_000, 1_400_000_000_000_000_000],
            consumed_inputs=[1_000_000_000_000_000_000, 1_500_000_000_000_000_000],
            solve_block=19_000_000,
            state_nonces=[0, 0],
        )
        # No public fields yet (A4 reads `inner`); construction-not-erroring
        # is the contract. The instance confirms the projection resolved the
        # 2-hop V2 path's `PathInfo` without raising.
        assert isinstance(candidate, DispatchCandidate)

    def test_encode_options_default_to_false(
        self, nxm2bf_v2_engine_and_path: tuple[PyArbitrageEngine, int]
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        # Both flags default False; construction must accept no kwargs.
        candidate = DispatchCandidate(
            engine=engine,
            path_id=path_id,
            optimal_input=1,
            engine_profit=1,
            hop_outputs=[1, 1],
            consumed_inputs=[1, 1],
            solve_block=0,
            state_nonces=[0, 0],
        )
        assert isinstance(candidate, DispatchCandidate)

    def test_encode_options_kw_flags_accepted(
        self, nxm2bf_v2_engine_and_path: tuple[PyArbitrageEngine, int]
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        candidate = DispatchCandidate(
            engine=engine,
            path_id=path_id,
            optimal_input=1,
            engine_profit=1,
            hop_outputs=[1, 1],
            consumed_inputs=[1, 1],
            solve_block=0,
            state_nonces=[0, 0],
            erc6909_profit=True,
            use_v4_batch=True,
        )
        assert isinstance(candidate, DispatchCandidate)

    def test_unregistered_path_id_raises_value_error(
        self, nxm2bf_v2_engine_and_path: tuple[PyArbitrageEngine, int]
    ) -> None:
        engine, _path_id = nxm2bf_v2_engine_and_path
        with pytest.raises(ValueError, match="not registered"):
            DispatchCandidate(
                engine=engine,
                path_id=999_999,  # never registered
                optimal_input=1,
                engine_profit=1,
                hop_outputs=[1, 1],
                consumed_inputs=[1, 1],
                solve_block=0,
                state_nonces=[0, 0],
            )

    def test_hop_outputs_length_mismatch_raises_value_error(
        self, nxm2bf_v2_engine_and_path: tuple[PyArbitrageEngine, int]
    ) -> None:
        engine, path_id = nxm2bf_v2_engine_and_path
        # The path is 2-hop; passing 1 hop_output is a solver/stale-path_id defect.
        with pytest.raises(ValueError, match="hop_outputs length"):
            DispatchCandidate(
                engine=engine,
                path_id=path_id,
                optimal_input=1,
                engine_profit=1,
                hop_outputs=[1],
                consumed_inputs=[1],
                solve_block=0,
                state_nonces=[0],
            )


class TestPySimulateContext:
    """The session-static config bag."""

    def test_constructs_with_inject_code_false(self) -> None:
        provider = _make_async_provider()
        ctx = SimulateContext(
            provider=provider,
            executor_owner=OWNER,
            executor_address=EXECUTOR,
            weth_address=WETH,
            pool_manager_address=POOL_MANAGER,
            multicall3_address=MULTICALL3,
            inject_code=False,
            executor_runtime_bytecode=b"\xde\xad\xbe\xef",
        )
        assert isinstance(ctx, SimulateContext)
        assert ctx.rpc_url == _RPC_URL

    def test_inject_code_true_without_injected_address_raises(self) -> None:
        provider = _make_async_provider()
        with pytest.raises(ValueError, match="injected_address is None"):
            SimulateContext(
                provider=provider,
                executor_owner=OWNER,
                executor_address=EXECUTOR,
                weth_address=WETH,
                pool_manager_address=POOL_MANAGER,
                multicall3_address=MULTICALL3,
                inject_code=True,
                executor_runtime_bytecode=b"\xde\xad\xbe\xef",
            )

    def test_inject_code_true_with_injected_address_constructs(self) -> None:
        provider = _make_async_provider()
        ctx = SimulateContext(
            provider=provider,
            executor_owner=OWNER,
            executor_address=EXECUTOR,
            weth_address=WETH,
            pool_manager_address=POOL_MANAGER,
            multicall3_address=MULTICALL3,
            inject_code=True,
            executor_runtime_bytecode=b"\xde\xad\xbe\xef",
            injected_address=EXECUTOR,
        )
        assert isinstance(ctx, SimulateContext)


class TestPyDispatchOutcome:
    """``DispatchOutcome`` is registered but built only by A4's ``from_join``."""

    def test_class_is_registered(self) -> None:
        # No #[new]; the cockpit cannot construct one directly. Asserting the
        # class is reachable (the registration in add_simulation_module ran).
        assert DispatchOutcome is not None
        # The getters exist as descriptors on the type.
        for getter in (
            "gas_profitable",
            "gas_unprofitable_count",
            "exception_count",
            "fail_count",
            "candidate_count",
            "suppressed_count",
            "thin_dropped",
            "fail_buckets",
        ):
            assert hasattr(DispatchOutcome, getter), f"missing getter {getter}"
