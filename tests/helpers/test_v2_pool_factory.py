"""Tests for make_v2_pool's bot seam (ADR-006 D1 shared-core).

When `py_bot` is supplied, the pool registers against that bot's shared
BotState (mirrors `Bot.build_pool`), rather than a fresh per-call Bot. This
lets synthetic V2 pools register against the SAME core the engine adopts —
enabling offline integration tests of EngineRegistry.register_path.

Registration answers per bot: two builds of one pool address on two bots stay
independent, each keeping the reserves it was built with.
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

from tests.helpers.erc20_factory import make_erc20
from tests.helpers.pool_bot import new_pool_bot
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    from tests.helpers.seam import Bot


def _make_tokens(py_bot: Bot):
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


def test_make_v2_pool_with_py_bot_registers_against_that_bot(py_bot) -> None:
    """A pool built with py_bot=supplied registers against that bot's BotState,
    so its pool_id resolves there. A fresh Bot does NOT see it — proving the
    supplied bot (not a per-call one) owns the registration."""
    weth, usdc = _make_tokens(py_bot)

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
        py_bot=py_bot,
    )

    # The supplied bot holds the pool — pool_id resolves there.
    assert py_bot.get_pool(pool._py_pool.engine_pool_id) is not None

    # A different, fresh Bot does NOT — proving the registration is scoped to
    # the supplied bot, mirroring Bot.build_pool's shared-core wiring.
    other = new_pool_bot()
    assert other.get_pool(pool._py_pool.engine_pool_id) is None


def test_same_pool_address_on_two_bots_keeps_each_its_own_state(py_bot) -> None:
    """Two builds of one pool address carry their own reserves on their own bot.

    A build must never answer another bot's registration: each keeps the
    reserves it was built with, and the handles are distinct. The assertion
    holds whatever any other test built or mutated before this one.
    """
    weth, usdc = _make_tokens(py_bot)
    other = new_pool_bot()
    other_weth, other_usdc = _make_tokens(other)

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
        py_bot=py_bot,
    )
    twin = make_v2_pool(
        address="0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc",
        token0=other_usdc,
        token1=other_weth,
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=900_000 * 10**6,
        reserves_token1=450 * 10**18,
        state_block=18_000_000,
        py_bot=other,
    )

    assert pool._py_pool is not twin._py_pool
    assert (pool.reserves_token0, pool.reserves_token1) == (1_000_000 * 10**6, 500 * 10**18)
    assert (twin.reserves_token0, twin.reserves_token1) == (900_000 * 10**6, 450 * 10**18)
