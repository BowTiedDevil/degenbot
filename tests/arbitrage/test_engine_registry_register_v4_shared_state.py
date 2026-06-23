"""EngineRegistry.register_v4_pool over a shared BotState (ADR-006 D1).

Background — the engine adopts the bot's shared BotState
(``UniswapArbEngine(py_bot=bot._py_bot)``). V4 pools built via
``bot.build_managed_pool`` / ``make_v4_pool`` are registered in that SAME
BotState at build time (``py_bot.register_v4_pool`` in the V4 builder /
``make_v4_pool``). So a subsequent ``register_v4_pool`` from the registry must
NOT re-register the pool with the engine — it must resolve the existing
shared-core key, mirroring the V2 path (``register_v2_pool`` reads
``pool._py_pool.pool_id``).

Regression (the production backrun bot, ``examples/eth_backrun_v2_v3_v4_rust.py``):
``register_v4_pool`` called ``self.engine.register_v4_pool`` on the pre-
registered pool, raising ``ValueError("V4 pool already registered: …")`` for
every V4 hop in every discovered path. Because the Python ``_v4_keys`` cache
was only set on success, the same pool tripped the error repeatedly.

These tests pin the shared-state contract against a real ``PyBot`` + real
``UniswapArbEngine`` (no RPC/anvil — same offline topology
``test_shared_state_topology`` proves).
"""

from __future__ import annotations

import dataclasses
from typing import TYPE_CHECKING

from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.constants import ZERO_ADDRESS
from degenbot.degenbot_rs import PyBot
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v4_pool_factory import make_v4_pool

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token

V4_POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"


@dataclasses.dataclass
class _FakeBot:
    """Minimal Bot double exposing ``_py_bot`` for the production path."""

    _py_bot: PyBot


def _build_shared_v4_pool(py_bot: PyBot) -> tuple[Erc20Token, Erc20Token, object]:
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
    pool = make_v4_pool(
        pool_id="0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7",
        pool_manager_address=V4_POOL_MANAGER,
        token0=usdc,
        token1=weth,
        fee=500,
        tick_spacing=10,
        hook_address=ZERO_ADDRESS,
        sqrt_price_x96=2_198_666_895_605_149_686_863,
        tick=-76020,
        liquidity=9876543210,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=500000,
        state_block=18_000_000,
        py_bot=py_bot,
    )
    return weth, usdc, pool


def test_register_v4_pool_resolves_shared_state_key_without_re_registering() -> None:
    """A V4 pool pre-registered in the shared BotState resolves to its key.

    ``register_v4_pool`` must NOT call ``engine.register_v4_pool`` (which would
    raise ``ValueError("V4 pool already registered")``); it reads the shared
    ``pool._py_pool.pool_id`` like the V2 path.
    """
    py_bot = PyBot()
    _weth, _usdc, pool = _build_shared_v4_pool(py_bot)
    bot = _FakeBot(py_bot)
    registry = EngineRegistry(bot=bot)

    # The pool is already in the shared BotState (built by make_v4_pool).
    # Registering with the registry must NOT raise and must return the
    # shared-core pool_id.
    key = registry.register_v4_pool(pool)  # type: ignore[arg-type]

    assert key == pool._py_pool.pool_id
    # Cache populated so a second call returns the same key (no re-entry).
    assert registry._v4_keys[pool.pool_id.to_0x_hex()] == key


def test_register_v4_pool_idempotent_across_paths() -> None:
    """The same V4 pool registered twice (two discovered paths share a hop)
    returns the same key both times and never re-enters the engine."""
    py_bot = PyBot()
    _weth, _usdc, pool = _build_shared_v4_pool(py_bot)
    bot = _FakeBot(py_bot)
    registry = EngineRegistry(bot=bot)

    first = registry.register_v4_pool(pool)  # type: ignore[arg-type]
    second = registry.register_v4_pool(pool)  # type: ignore[arg-type]

    assert first == second == pool._py_pool.pool_id
