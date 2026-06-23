"""EngineRegistry.register_v2_pool over a shared BotState (ADR-006 D1).

Canonical shared-state contract — the V2 path is the pattern the V3 and V4
companions (`test_engine_registry_register_vN_shared_state.py`) adopt. The
engine adopts the bot's shared ``BotState``, V2 pools are pre-registered there
by ``bot.build_pool`` / ``make_v2_pool`` (``py_bot.register_v2_pool`` in the V2
builder / helper), so the registry must NOT re-register with the engine (which
would panic ``BotCore::register_v2_pool`` on the duplicate address, same as V3).
"""

from __future__ import annotations

import dataclasses
from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.degenbot_rs import PyBot
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token


@dataclasses.dataclass
class _FakeBot:
    """Minimal Bot double exposing ``_py_bot`` for the production path."""

    _py_bot: PyBot


def _build_shared_v2_pool(py_bot: PyBot) -> tuple[Erc20Token, Erc20Token, object]:
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
    pool = make_v2_pool(
        "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",  # USDC/WETH-0.3% mainnet
        token0=usdc,
        token1=weth,
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=10_000_000_000,
        reserves_token1=3_000 * 10**18,
        state_block=18_000_000,
        py_bot=py_bot,
    )
    return weth, usdc, pool


def test_register_v2_pool_resolves_shared_state_key_without_re_registering() -> None:
    """A V2 pool pre-registered in the shared BotState resolves to its key.

    ``register_v2_pool`` must NOT call ``engine.register_v2_pool`` (which
    panics ``BotCore::register_v2_pool`` on the duplicate address); it reads
    ``pool._py_pool.pool_id`` — the canonical shared-state pattern.
    """
    py_bot = PyBot()
    _weth, _usdc, pool = _build_shared_v2_pool(py_bot)
    bot = _FakeBot(py_bot)
    registry = EngineRegistry(bot=bot)

    key = registry.register_v2_pool(pool)  # type: ignore[arg-type]

    assert key == pool._py_pool.pool_id
    assert registry._v2_keys[pool.address] == key


def test_register_v2_pool_idempotent_across_paths() -> None:
    """The same V2 pool registered twice (two discovered paths share a hop)
    returns the same key both times and never re-enters the engine."""
    py_bot = PyBot()
    _weth, _usdc, pool = _build_shared_v2_pool(py_bot)
    bot = _FakeBot(py_bot)
    registry = EngineRegistry(bot=bot)

    first = registry.register_v2_pool(pool)  # type: ignore[arg-type]
    second = registry.register_v2_pool(pool)  # type: ignore[arg-type]

    assert first == second == pool._py_pool.pool_id
