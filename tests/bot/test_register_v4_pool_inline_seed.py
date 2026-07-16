"""V4 `register_v4_pool` must seed tick_data INLINE (rolling-start race closure).

The V3 path was fixed by folding the snapshot seed into ``register_v3_pool``
(one ``BotState`` write lock) so the pool is never visible to the live pump
(resumed before ``build_paths``) in an unseeded state. The V4 pyo3
``register_v4_pool`` did NOT take ``tick_data`` — it registered with an empty
map (Sparse), then the builder seeded via a separate ``update_tick_data``
call (a wholesale ``state.tick_data = …`` REPLACE). A pump ``ModifyLiquidity``
landing between register and seed was applied to the empty map then CLOBBERED
by the seed overwrite — a lost update.

This is the V4 tick-map desync: the engine freezes at the exact snapshot-seed
value (verified against on-chain at the snapshot block) while on-chain moves,
because every live event between register and seed (and any event a late
``update_tick_data`` re-seed overwrites) is lost. ``verify_liquidity_maps``
fails at startup with ``liquidityGross mismatch``.
"""

from __future__ import annotations

from typing import TYPE_CHECKING
from unittest.mock import MagicMock

from degenbot.bot import Bot
from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.bot import PyBot
from degenbot.provider import AlloyProvider
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v4_pool_factory import make_v4_pool

if TYPE_CHECKING:
    import pathlib

V4_POOL_MANAGER = "0x000000000004444c5DC75cB358380D2e3dE08A90"


def _make_test_config(tmp_path: pathlib.Path, chain_id: int = 1) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=tmp_path / "test.db"),
        rpc={1: "http://localhost:8545/"},
        default_chain_id=chain_id,
    )


def _fake_provider(chain_id: int = 1) -> AlloyProvider:
    provider = MagicMock(spec=AlloyProvider)
    provider.chain_id = chain_id
    return provider


def _make_tokens(py_bot: object) -> tuple[object, object]:
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
    return weth, usdc


def test_register_v4_pool_seeds_tick_data_inline_without_update_tick_data(
    tmp_path: pathlib.Path,
) -> None:
    """``register_v4_pool`` accepts ``tick_data`` + ``coverage`` inline; the
    builder no longer needs a separate ``update_tick_data`` clobber.

    Pre-fix: ``register_v4_pool`` registered with an empty map (Sparse) and the
    builder's separate ``update_tick_data`` was the only seed — a pump
    ModifyLiquidity in the register→seed window was lost. The inline seed
    (one write lock) makes the pool visible to the pump only after seeding,
    closing the window.
    """

    py_bot = PyBot()
    weth, usdc = _make_tokens(py_bot)

    pool = make_v4_pool(
        pool_id="0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7",
        pool_manager_address=V4_POOL_MANAGER,
        token0=usdc,
        token1=weth,
        fee=500,
        tick_spacing=10,
        hook_address=None,
        sqrt_price_x96=2_198_666_895_605_149_686_863,
        tick=-76020,
        liquidity=9876543210,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=500000,
        state_block=18_000_000,
        py_bot=py_bot,
        tick_data={-201000: (100, 1000, 18_000_000)},
    )

    td = pool._py_pool.tick_data_snapshot()
    assert td is not None, "V4 pool must be registered with tick data inline"
    assert -201000 in td, "the seeded tick must be present without a separate update_tick_data"
    assert td[-201000][0] == 100, "inline seed gross matches"
    assert td[-201000][1] == 1000, "inline seed net matches"


def test_release_python_state_keeps_rust_v4_pool_registered(tmp_path: pathlib.Path) -> None:
    """release_python_state must NOT unregister V4 pools from Rust BotState.

    V4 unregister is deferred in ``PoolRegistry._reset`` (engine-side), so this
    is the symmetric guard to the V3 release test: the mid-lifecycle handoff
    keeps the pump writing through the shared core, so V4 pools must survive
    release (the engine still solves against them).
    """

    config = _make_test_config(tmp_path)
    bot = Bot(config, provider=_fake_provider(1))
    py_bot = bot._py_bot
    weth, usdc = _make_tokens(py_bot)

    pool = make_v4_pool(
        pool_id="0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7",
        pool_manager_address=V4_POOL_MANAGER,
        token0=usdc,
        token1=weth,
        fee=500,
        tick_spacing=10,
        hook_address=None,
        sqrt_price_x96=2_198_666_895_605_149_686_863,
        tick=-76020,
        liquidity=9876543210,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=500000,
        state_block=18_000_000,
        py_bot=py_bot,
        tick_data={-201000: (100, 1000, 18_000_000)},
    )
    # V4 pools aren't in the Python pool registry (managed-pool sub-registry is
    # separate), so ``_reset`` doesn't touch them anyway — but the Rust count
    # must survive the release regardless.
    assert py_bot.pool_count() == 1

    bot.release_python_state()

    assert py_bot.pool_count() == 1, "V4 pool must stay registered in Rust after release"
    td = pool._py_pool.tick_data_snapshot()
    assert -201000 in td, "V4 tick data survives release"
