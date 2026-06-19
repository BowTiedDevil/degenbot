"""Tests for the shared-py_bot option on make_v2_pool (ADR-006 D1 shared-core).

When `py_bot` is supplied, the pool registers against that bot's shared
BotState (mirrors `Bot.build_pool`), rather than a fresh per-call PyBot. This
lets synthetic V2 pools register against the SAME core the engine adopts —
enabling offline integration tests of EngineRegistry.register_path.
"""

from __future__ import annotations

from fractions import Fraction

from degenbot.degenbot_rs import PyBot
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

_SHARED = PyBot()


def _make_tokens(py_bot: PyBot):
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


def test_make_v2_pool_with_py_bot_registers_against_that_bot() -> None:
    """A pool built with py_bot=shared registers against that bot's BotState,
    so its pool_id resolves there. A fresh PyBot does NOT see it — proving the
    shared bot (not a per-call one) owns the registration."""
    weth, usdc = _make_tokens(_SHARED)

    pool = make_v2_pool(
        address="0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
        token0=usdc,
        token1=weth,
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1_000_000 * 10**6,
        reserves_token1=500 * 10**18,
        state_block=18_000_000,
        py_bot=_SHARED,
    )

    # The shared bot holds the pool — pool_id resolves there.
    assert _SHARED.get_pool(pool._py_pool.pool_id) is not None

    # A different, fresh PyBot does NOT — proving the registration is scoped to
    # the supplied bot, mirroring Bot.build_pool's shared-core wiring.
    other = PyBot()
    assert other.get_pool(pool._py_pool.pool_id) is None
