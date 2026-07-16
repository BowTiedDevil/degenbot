"""Smoke tests for the simulation-seam pyclasses (A2 / TCZ47Z).

These are constructor-shape tests, not behavioral parity tests — A4
(``dispatch_profitable_py``) and A6 (parity) cover behavior. The goal here is
to pin the ``#[new]`` arg extraction + the getter shapes so a follow-up
session cannot silently break the cockpit's construction sites.

Note: ``PySimulateContext`` constructs an ``AsyncAlloyProvider`` against a
localhost URL that is never dialed (no RPC call is made at construction — only
the provider arc is cloned). ``PyDispatchOutcome`` has no ``#[new]``: it is
built internally by ``dispatch_profitable_py`` (A4) via ``from_join``; this
test only asserts it is registered + introspectable.
"""

from __future__ import annotations

import pytest

from degenbot._ffi.provider import AlloyProvider, AsyncAlloyProvider
from degenbot._ffi.simulation import PyDispatchCandidate, PyDispatchOutcome, PySimulateContext
from degenbot.arbitrage.hop_info import PathInfo, V2HopInfo

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


@pytest.fixture
def v2_path_info() -> PathInfo:
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


class TestPyDispatchCandidate:
    """The per-path builder — field extraction + the encode-options flags."""

    def test_constructs_from_engine_result_and_path_info(self, v2_path_info: PathInfo) -> None:
        candidate = PyDispatchCandidate(
            path_id=42,
            optimal_input=1_000_000_000_000_000_000,
            engine_profit=2_000_000_000_000_000_000,
            hop_outputs=[1_500_000_000_000_000_000],
            solve_block=19_000_000,
            path_info=v2_path_info,
        )
        # No public fields yet (A4 reads `inner`); construction-not-erroring
        # is the contract. The presence of the instance confirms the hop
        # extraction dispatched to the V2 branch without raising.
        assert isinstance(candidate, PyDispatchCandidate)

    def test_encode_options_default_to_false(self, v2_path_info: PathInfo) -> None:
        # Both flags default False; construction must accept no kwargs.
        candidate = PyDispatchCandidate(
            path_id=0,
            optimal_input=1,
            engine_profit=1,
            hop_outputs=[1],
            solve_block=0,
            path_info=v2_path_info,
        )
        assert isinstance(candidate, PyDispatchCandidate)

    def test_encode_options_kw_flags_accepted(self, v2_path_info: PathInfo) -> None:
        candidate = PyDispatchCandidate(
            path_id=0,
            optimal_input=1,
            engine_profit=1,
            hop_outputs=[1],
            solve_block=0,
            path_info=v2_path_info,
            erc6909_profit=True,
            use_v4_batch=True,
        )
        assert isinstance(candidate, PyDispatchCandidate)

    def test_non_path_info_raises(self) -> None:
        with pytest.raises((TypeError, AttributeError)):
            PyDispatchCandidate(
                path_id=0,
                optimal_input=1,
                engine_profit=1,
                hop_outputs=[1],
                solve_block=0,
                path_info=object(),  # type: ignore[arg-type]
            )


class TestPySimulateContext:
    """The session-static config bag."""

    def test_constructs_with_inject_code_false(self) -> None:
        provider = _make_async_provider()
        ctx = PySimulateContext(
            provider=provider,
            executor_owner=OWNER,
            executor_address=EXECUTOR,
            weth_address=WETH,
            pool_manager_address=POOL_MANAGER,
            multicall3_address=MULTICALL3,
            inject_code=False,
            executor_runtime_bytecode=b"\xde\xad\xbe\xef",
        )
        assert isinstance(ctx, PySimulateContext)
        assert ctx.rpc_url == _RPC_URL

    def test_inject_code_true_without_injected_address_raises(self) -> None:
        provider = _make_async_provider()
        with pytest.raises(ValueError, match="injected_address is None"):
            PySimulateContext(
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
        ctx = PySimulateContext(
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
        assert isinstance(ctx, PySimulateContext)


class TestPyDispatchOutcome:
    """``PyDispatchOutcome`` is registered but built only by A4's ``from_join``."""

    def test_class_is_registered(self) -> None:
        # No #[new]; the cockpit cannot construct one directly. Asserting the
        # class is reachable (the registration in add_simulation_module ran).
        assert PyDispatchOutcome is not None
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
            assert hasattr(PyDispatchOutcome, getter), f"missing getter {getter}"
