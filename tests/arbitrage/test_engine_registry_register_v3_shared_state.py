"""EngineRegistry.register_v3_pool over a shared BotState (ADR-006 D1).

Same shared-state contract as the V4 companion test — the engine adopts the
bot's shared BotState, V3 pools are pre-registered there by ``bot.build_pool``
/ ``make_v3_pool`` (``py_bot.register_v3_pool`` in the V3 builder / helper),
so the registry must NOT re-register the pool with the engine. Unlike V4
(which raises a catchable ``ValueError`` on duplicate), V3 re-registration
**panics** the Rust core (``BotCore::register_v3_pool`` panics on duplicate
address) — taking the whole process down. This is the identical bug class the
V2 path already handles (``register_v2_pool`` reads ``pool._py_pool.pool_id``
instead of calling ``engine.register_v2_pool``).
"""

from __future__ import annotations

import asyncio
import dataclasses

import pytest

from degenbot.arbitrage.engine_registry import EngineRegistry, ArbitrageEngine
from degenbot.exceptions import VerificationRpcError
from degenbot.bot import RustBot
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool


@dataclasses.dataclass
class _FakeBot:
    """Minimal Bot double exposing ``_py_bot`` for the production path."""

    _py_bot: RustBot


def _build_shared_v3_pool(py_bot: RustBot) -> object:
    weth = make_erc20(
        py_bot,
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )
    usdc = make_erc20(
        py_bot,
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        chain_id=1,
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )
    return make_v3_pool(
        "0x11b815efB8f58119D17b5fc9880b1e1a29B7dC33",  # USDC/WETH-0.05% mainnet
        token0=usdc,
        token1=weth,
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        fee=500,
        tick_spacing=10,
        sqrt_price_x96=1_795_387_016_509_156_625_815_244_826,
        tick=-76020,
        liquidity=9876543210,
        state_block=18_000_000,
        py_bot=py_bot,
    )


def test_register_v3_pool_resolves_shared_state_key_without_re_registering() -> None:
    """A V3 pool pre-registered in the shared BotState resolves to its key.

    Without the fix, ``engine.register_v3_pool`` panics the Rust core on the
    duplicate address (``pool already registered: …``). The registry must read
    the shared ``pool._py_pool.pool_id`` like the V2 path.
    """
    py_bot = RustBot()
    pool = _build_shared_v3_pool(py_bot)
    bot = _FakeBot(py_bot)
    registry = EngineRegistry(bot=bot)

    key = asyncio.run(registry.register_v3_pool(pool))

    assert key == pool._py_pool.pool_id
    assert registry._v3_keys[pool.address] == key


def test_register_tracked_v3_pool_without_provider_fails_fast() -> None:
    """D-C (ADR-022): a TRACKED pool is always verified — there is no
    verify-disabled mode that would release it unverified. With no RPC provider
    configured (the bot's single provider is stashed by `start()`), registering
    a tracked pool now fails fast with `VerificationRpcError` instead of
    releasing it Live.

    The drain invariant this test historically pinned (buffered backfill events
    drained onto the snapshot seed — the perm-V2-V2-V3 class, incl. the tick's
    removal when a Burn zeroes its liquidity) moved core-side with the
    registration-lifecycle (IKGQ6F / ADR-022 D1) and is now asserted by the
    Rust unit test `bot_core::registration_lifecycle::tests::
    tracked_v3_lifecycle_drains_buffered_backfill`.
    """
    py_bot = RustBot()
    engine = ArbitrageEngine(py_bot=py_bot)
    bot = _FakeBot(py_bot)
    registry = EngineRegistry(bot=bot, engine=engine)

    address = "0x11b815efB8f58119D17b5fc9880b1e1a29B7dC33"
    weth = make_erc20(
        py_bot,
        "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )
    usdc = make_erc20(
        py_bot,
        "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        chain_id=1,
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )
    pool = make_v3_pool(
        address,
        token0=usdc,
        token1=weth,
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        fee=500,
        tick_spacing=10,
        sqrt_price_x96=1_795_387_016_509_156_625_815_244_826,
        tick=-76020,
        liquidity=9876543210,
        state_block=18_000_000,
        py_bot=py_bot,
        tick_data={-201000: (100, 1000, 18_000_000)},
    )
    # Tracked (complete tick_data seed) → registration must verify; no provider
    # configured (start() never ran) → D-C fail-fast, NOT a silent release.
    with pytest.raises(VerificationRpcError, match="RPC provider"):
        asyncio.run(registry.register_v3_pool(pool))
