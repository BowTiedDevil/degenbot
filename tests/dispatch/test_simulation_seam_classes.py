"""Smoke tests for the simulation-seam pyclasses.

These are constructor-shape tests, not behavioral parity tests — the batch
executor's drain suites and the parity corpus cover behavior. The goal here is
to pin the ``#[new]`` arg extraction + the getter shapes so a follow-up
session cannot silently break the cockpit's construction sites.

Note: ``SimulateContext`` constructs an ``AsyncAlloyProvider`` against a
localhost URL that is never dialed (no RPC call is made at construction — only
the provider arc is cloned).
"""

from __future__ import annotations

import pytest

from tests.helpers.seam import (
    AlloyProvider,
    AsyncAlloyProvider,
    SimulateContext,
)

# Canonical mainnet addresses (parity corpus constants).
OWNER = "0x9c56a29c7231974c269e24f9fb3c29203039089e"
EXECUTOR = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
WETH = "0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2"
POOL_MANAGER = "0x000000000004444C5DC75cB358380d2e3dE08a90"
# The Multicall3 identity an earlier constructor took. The built-in deployment
# reads no Multicall3 balances (it assesses the executor's own receipt), so the
# keyword is gone and a stale construction site must fail at the boundary.
RETIRED_MULTICALL3 = "0xcA11bde05977b3631167028862bE2a173976CA11"

# An HTTP URL against a dead port. alloy's HTTP transport is lazy (it does
# not dial at construction), so this constructs offline. No RPC is made
# anywhere in this test: the seam only clones the provider arc.
_RPC_URL = "http://127.0.0.1:1"


def _make_async_provider() -> AsyncAlloyProvider:
    # Sync `AlloyProvider(url)` (the factory's lazy construction path) + the
    # `AsyncAlloyProvider(sync_provider)` wrapper — both offline.
    sync = AlloyProvider(_RPC_URL)
    return AsyncAlloyProvider(sync)


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
            inject_code=False,
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
                inject_code=True,
            )

    def test_retired_multicall3_keyword_raises_type_error(self) -> None:
        """A stale construction site passes the retired keyword: the boundary
        refuses it as an unexpected keyword instead of quietly simulating a
        deployment whose Multicall3 argument no longer means anything."""
        provider = _make_async_provider()
        with pytest.raises(TypeError, match="unexpected keyword argument"):
            SimulateContext(
                provider=provider,
                executor_owner=OWNER,
                executor_address=EXECUTOR,
                weth_address=WETH,
                pool_manager_address=POOL_MANAGER,
                multicall3_address=RETIRED_MULTICALL3,
                inject_code=False,
            )

    def test_inject_code_true_with_mismatched_injected_address_raises(self) -> None:
        """The override installs code at the executor the adapter encodes to, so
        a declared address anywhere else is refused instead of simulated."""
        provider = _make_async_provider()
        with pytest.raises(ValueError, match="is not executor_address"):
            SimulateContext(
                provider=provider,
                executor_owner=OWNER,
                executor_address=EXECUTOR,
                weth_address=WETH,
                pool_manager_address=POOL_MANAGER,
                inject_code=True,
                injected_address=POOL_MANAGER,
            )

    def test_inject_code_true_with_injected_address_constructs(self) -> None:
        provider = _make_async_provider()
        ctx = SimulateContext(
            provider=provider,
            executor_owner=OWNER,
            executor_address=EXECUTOR,
            weth_address=WETH,
            pool_manager_address=POOL_MANAGER,
            inject_code=True,
            injected_address=EXECUTOR,
        )
        assert isinstance(ctx, SimulateContext)
