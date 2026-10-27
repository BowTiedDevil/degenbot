"""Pool identity: a built pool must say what it is, immutably.

The concrete Python class is NOT the identity: `sushiswap-v3` deployments
share ``UniswapV3Pool`` with vanilla v3 by design (same math), and
``PancakeswapV3Pool`` exists only because its storage layout deviates. What
governs behavior is the resolved ``PoolTypeDescriptor`` (family/variant/kind/
factory) — a wrong identity means swap math runs with the wrong fee model,
which for a settlement bot is real money. These tests pin identity visibility,
immutability, and the family refusal at the ``from_handle`` seam.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import cast

import pytest

from degenbot.bot import Bot
from degenbot.exceptions import DegenbotValueError
from degenbot.pancakeswap.pools import PancakeswapV3Pool
from degenbot.types.pool_type import (
    PoolFamily,
    PoolTypeDescriptor,
    derive_kind,
)
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.pool_bot import new_pool_bot
from tests.helpers.seam import Pool
from tests.helpers.v3_pool_factory import make_v3_pool

_TOKEN_ZERO = "0x" + "11" * 20
_TOKEN_ONE = "0x" + "22" * 20
_VANILLA_FACTORY = "0x" + "44" * 20
_PANCAKE_FACTORY = "0x" + "55" * 20
_VANILLA_POOL = "0x" + "66" * 20
_PANCAKE_POOL = "0x" + "77" * 20
_AMOUNT_IN = 10**18
_LIQUIDITY = 10**21

# One full-range-ish position around tick 0, seeded in the Rust write shape
# ({tick: LiquidityAtTick}): the sparse pool then prices small swaps without
# touching the chain (the walk never leaves the seeded bitmap words).
_TICK_DATA = {
    -60: LiquidityAtTick(liquidity_net=_LIQUIDITY, liquidity_gross=_LIQUIDITY, block=0),
    60: LiquidityAtTick(liquidity_net=-_LIQUIDITY, liquidity_gross=_LIQUIDITY, block=0),
}


def _build(bot, address: str, factory: str, *, fee: int, pool_class=UniswapV3Pool):
    token0 = make_erc20(bot, _TOKEN_ZERO, name="Token Zero", symbol="T0", decimals=18)
    token1 = make_erc20(bot, _TOKEN_ONE, name="Token One", symbol="T1", decimals=18)
    return make_v3_pool(
        address,
        token0=token0,
        token1=token1,
        factory=factory,
        fee=fee,
        tick_spacing=60,
        sqrt_price_x96=2**96,
        tick=0,
        liquidity=_LIQUIDITY,
        tick_data=_TICK_DATA,
        py_bot=bot,
        pool_class=pool_class,
    )


def test_pool_type_is_bound_and_read_only() -> None:
    pool = _build(new_pool_bot(), _VANILLA_POOL, _VANILLA_FACTORY, fee=3000)
    assert pool.pool_type == PoolTypeDescriptor(
        family=PoolFamily.CONCENTRATED_LIQUIDITY,
        variant=None,
        kind="uniswap_v3",
        factory=pool.factory,
    )
    with pytest.raises(AttributeError):
        pool.pool_type = pool.pool_type  # type: ignore[misc]


def test_identity_records_the_deployment_variant() -> None:
    pool = _build(
        new_pool_bot(), _PANCAKE_POOL, _PANCAKE_FACTORY, fee=2500, pool_class=PancakeswapV3Pool
    )
    assert pool.pool_type.kind == "pancakeswap_v3"
    assert pool.pool_type.variant == "pancakeswap"
    assert pool.pool_type.family is PoolFamily.CONCENTRATED_LIQUIDITY
    assert pool.pool_type == PoolTypeDescriptor(
        family=PoolFamily.CONCENTRATED_LIQUIDITY,
        variant="pancakeswap",
        kind=derive_kind(PoolFamily.CONCENTRATED_LIQUIDITY, "pancakeswap"),
        factory=pool.pool_type.factory,
    )


def test_identity_is_bound_exactly_once() -> None:
    pool = _build(new_pool_bot(), _VANILLA_POOL, _VANILLA_FACTORY, fee=3000)
    bound = pool.pool_type
    pool.bind_pool_type(bound)  # idempotent re-bind of the identical value is harmless
    with pytest.raises(DegenbotValueError):
        pool.bind_pool_type(
            PoolTypeDescriptor(
                family=PoolFamily.CONCENTRATED_LIQUIDITY,
                variant="camelot",
                kind=derive_kind(PoolFamily.CONCENTRATED_LIQUIDITY, "camelot"),
                factory=bound.factory,
            )
        )
    assert pool.pool_type is bound


def test_swap_math_follows_the_pools_own_fee() -> None:
    """Same state and input, different deployment fee -> different output.

    This is the behavior identity exists for: each pool must price swaps
    under ITS OWN fee schedule (PancakeSwap v3 deployments carry different fee
    tiers than vanilla Uniswap v3). More fee taken from the input means
    strictly less output. At identical state AND identical fee the two
    variants must agree exactly — the variant is data, not a divergent
    formula. If that last equality ever fails, the core has gained
    variant-specific math and THIS assertion is the tripwire to invert.
    """
    bot = new_pool_bot()
    cheap = _build(bot, _VANILLA_POOL, _VANILLA_FACTORY, fee=2500)
    dear = _build(bot, _PANCAKE_POOL, _PANCAKE_FACTORY, fee=3000, pool_class=PancakeswapV3Pool)
    pancake_same_fee = _build(
        new_pool_bot(), "0x" + "88" * 20, _PANCAKE_FACTORY, fee=2500, pool_class=PancakeswapV3Pool
    )
    out_cheap = cheap.calculate_tokens_out_from_tokens_in(
        token_in=cheap.tokens[0], token_in_quantity=_AMOUNT_IN
    )
    out_dear = dear.calculate_tokens_out_from_tokens_in(
        token_in=dear.tokens[0], token_in_quantity=_AMOUNT_IN
    )
    out_pancake = pancake_same_fee.calculate_tokens_out_from_tokens_in(
        token_in=pancake_same_fee.tokens[0], token_in_quantity=_AMOUNT_IN
    )
    assert 0 < out_dear < out_cheap
    assert out_pancake == out_cheap


def test_from_handle_refuses_a_foreign_family() -> None:
    foreign_handle = SimpleNamespace(pool_family="v2")
    with pytest.raises(DegenbotValueError, match="not a V3-family pool"):
        UniswapV3Pool.from_handle(cast("Pool", foreign_handle))


def test_resolve_pool_type_contract_is_held_at_the_live_seam() -> None:
    """The public resolution probe is covered live, where a chain exists.

    A typed refusal for an undeterminable address (``Bot.resolve_pool_type``
    raising ``DegenbotValueError``) and the preview-equals-bind invariant both
    need a reachable chain to be meaningful; the offline suite stays hermetic
    and both are asserted in the fork-tier identity test instead of against the
    suite's deliberately inert ambient endpoints.
    """
    assert callable(getattr(Bot, "resolve_pool_type", None))
