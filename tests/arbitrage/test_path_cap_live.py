"""Live validation: the registered-path cap lives in the
engine path registry and the refusal is a typed benign stop.

Runs on the mainnet full fork tier (``online_rpc``): two REAL Uniswap V2
pools are built through the Bot seam, registered as a path (created),
re-registered (dedup answers with the same id, not created), and a second
new path is refused with :class:`PathRegistryFullError` at cap=1 — the crawl
benign-stop contract, proven end to end through the FFI.
"""

from __future__ import annotations

import pytest

from degenbot import Bot
from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.exceptions import PathRegistryFullError

USDC_WETH_V2 = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
USDT_WETH_V2 = "0x0d4a11d5EEaaC28eC3f61d100DaF4d40471F1852"


@pytest.mark.online_rpc
def test_engine_path_cap_refuses_live(bot_mainnet_full: Bot) -> None:
    registry = EngineRegistry(bot_mainnet_full)

    pool_a = bot_mainnet_full.build_pool(USDC_WETH_V2, silent=True)
    pool_b = bot_mainnet_full.build_pool(USDT_WETH_V2, silent=True)
    # The crawl registers every pool with the registry before its path.
    registry.register_v2_pool(pool_a)
    registry.register_v2_pool(pool_b)

    # The engine owns the cap; the crawl sets it before discovery.
    registry.engine.set_path_cap(1)

    try:
        path_id, created = registry.register_path([(pool_a, True), (pool_b, True)])
        assert created, "the first registration of a new path is created"

        # Dedup is by construction core-side: same pools + directions answer
        # with the SAME path_id and are never created.
        dup_id, dup_created = registry.register_path([(pool_a, True), (pool_b, True)])
        assert dup_id == path_id, "duplicate registration returns the existing path_id"
        assert not dup_created, "the duplicate is deduped, not created"

        # A NEW path at the cap: the typed benign refusal.
        with pytest.raises(PathRegistryFullError) as exc_info:
            registry.register_path([(pool_b, True), (pool_a, True)])
        assert "cap reached" in str(exc_info.value)

        # The registry never grew past the cap.
        assert registry.engine.path_count() == 1
    finally:
        registry.engine.set_path_cap(None)
