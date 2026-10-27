"""Registration fixtures for the structural ``Pool`` handle surface.

``tests/pool_handle/`` asserts on the Rust-owned ``Pool`` handle itself: the
atomic view a family answers (``reserve_pair``/``concentrated_liquidity``/
``balance_vector``), the identity it reports, and the swap math it dispatches
to. Those tests never build a companion, which is why they are not served by
``*_pool_factory`` — those helpers wrap a handle in a Python pool class, seed
token metadata, and pin a pool-type descriptor, all of which the handle surface
deliberately does not have.

What the handle tests do need is the same thing every other test does: a
registered pool, expressed once. Hand-writing the ``register_*`` pyclass kwargs
at each call site made a Rust-side signature change a mechanical edit in a
dozen test bodies, so each family's registration lives here with the defaults
the Rust fixtures use. A test overrides only the argument it is about.

Every registrar returns the ``Pool`` handle (or ``None`` would fail loudly) and
mints its own ``Bot`` unless one is passed, so a re-registration in another test
cannot answer this one's handle.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from tests.helpers.seam import Bot

if TYPE_CHECKING:
    from degenbot.types import Pool

ONE = 10**18

#: Equal-shaped synthetic token pair shared by the family fixtures.
TOKEN0 = "0x" + "a" * 40
TOKEN1 = "0x" + "b" * 40

#: The tick rows the concentrated-liquidity fixtures seed: one word either side
#: of tick 0, gross == |net| so the liquidity curve is symmetric.
CONCENTRATED_TICK_DATA = {
    -60: (1_000_000, 1_000_000, 0),
    60: (1_000_000, -1_000_000, 0),
}


def _handle(bot: Bot, pool_id: int) -> Pool:
    handle = bot.get_pool(pool_id)
    assert handle is not None, "a registration returned a pool_id with no handle"
    return handle


def make_v2_handle(
    *,
    bot: Bot | None = None,
    address: str = "0x" + "1" * 40,
    factory: str = "0x" + "2" * 40,
    reserve0: int = 1_000_000_000,
    reserve1: int = 2_000_000_000,
    update_block: int = 100,
) -> Pool:
    """Register a V2-style pool (0.3% fee both sides, ``uniswap-v2``)."""
    bot = bot if bot is not None else Bot(1)
    return _handle(
        bot,
        bot.register_v2_pool_test_only(
            address=address,
            token0=TOKEN0,
            token1=TOKEN1,
            reserve0=reserve0,
            reserve1=reserve1,
            gamma_numer0=997,
            fee_denom0=1000,
            gamma_numer1=997,
            fee_denom1=1000,
            factory=factory,
            update_block=update_block,
            variant="uniswap-v2",
            stable_swap=False,
            fee_denominator=None,
        ),
    )


def make_v3_handle(
    *,
    bot: Bot | None = None,
    address: str = "0x" + "1" * 40,
    factory: str = "0x" + "2" * 40,
    fee: int = 3000,
    tick_spacing: int = 60,
    sqrt_price_x96: int = 1 << 96,
    liquidity: int = 1_000_000,
    tick: int = 0,
    tick_data: dict[int, tuple[int, int, int]] | None = None,
    update_block: int = 100,
) -> Pool:
    """Register a concentrated-liquidity pool (``uniswap-v3``, tracked coverage)."""
    bot = bot if bot is not None else Bot(1)
    return _handle(
        bot,
        bot.register_v3_pool(
            address=address,
            token0=TOKEN0,
            token1=TOKEN1,
            fee=fee,
            tick_spacing=tick_spacing,
            factory=factory,
            sqrt_price_x96=sqrt_price_x96,
            liquidity=liquidity,
            tick=tick,
            tick_data=CONCENTRATED_TICK_DATA if tick_data is None else tick_data,
            update_block=update_block,
            coverage="tracked",
        ),
    )


def make_v4_handle(
    *,
    bot: Bot | None = None,
    pool_manager: str = "0x" + "1" * 40,
    pool_id_hex: str = "0x" + "00" * 32,
    fee: int = 3000,
    tick_spacing: int = 60,
    hook_address: str | None = None,
    sqrt_price_x96: int = 1 << 96,
    liquidity: int = 1_000_000,
    tick: int = 0,
    tick_data: dict[int, tuple[int, int, int]] | None = None,
    update_block: int = 100,
) -> Pool:
    """Register a V4 concentrated-liquidity pool keyed on ``(manager, pool_id)``.

    V4 identity is the pool key, not an address, so the manager and
    ``pool_id_hex`` are the arguments a test varies.
    """
    bot = bot if bot is not None else Bot(1)
    return _handle(
        bot,
        bot.register_v4_pool(
            pool_manager=pool_manager,
            pool_id_hex=pool_id_hex,
            currency0=TOKEN0,
            currency1=TOKEN1,
            fee=fee,
            tick_spacing=tick_spacing,
            hook_address=hook_address,
            sqrt_price_x96=sqrt_price_x96,
            liquidity=liquidity,
            tick=tick,
            block=update_block,
            tick_data=CONCENTRATED_TICK_DATA if tick_data is None else tick_data,
            coverage="tracked",
        ),
    )


def make_aerodrome_handle(
    *,
    bot: Bot | None = None,
    address: str = "0x" + "1" * 40,
    token0: str = TOKEN0,
    token1: str = TOKEN1,
    factory: str = "0x" + "2" * 40,
    stable: bool = False,
    fee_numer: int = 3,
    fee_denom: int = 10_000,
    token0_decimals: int = 18,
    token1_decimals: int = 18,
    reserve0: int = 1_000_000_000,
    reserve1: int = 2_000_000_000,
    update_block: int = 100,
) -> Pool:
    """Register an Aerodrome V2 pool; ``stable`` selects the Solidly invariant."""
    bot = bot if bot is not None else Bot(1)
    return _handle(
        bot,
        bot.register_aerodrome_pool(
            address=address,
            token0=token0,
            token1=token1,
            factory=factory,
            variant="aerodrome-v2-stable" if stable else "aerodrome-v2-volatile",
            stable=stable,
            fee_numer=fee_numer,
            fee_denom=fee_denom,
            token0_decimals=token0_decimals,
            token1_decimals=token1_decimals,
            reserve0=reserve0,
            reserve1=reserve1,
            update_block=update_block,
        ),
    )


def make_curve_handle(
    *,
    bot: Bot | None = None,
    address: str = "0x" + "1" * 40,
    tokens: tuple[str, str] = (TOKEN0, TOKEN1),
    a_coefficient: int = 100,
    a_precision: int = 100,
    fee: int = 0,
    admin_fee: int = 0,
    rate_multipliers: tuple[int, int] = (ONE, ONE),
    balances: tuple[int, int] = (ONE, ONE),
    update_block: int = 100,
) -> Pool:
    """Register a Curve stableswap pool (STANDARD variants, zero fee)."""
    bot = bot if bot is not None else Bot(1)
    return _handle(
        bot,
        bot.register_curve_pool(
            address=address,
            tokens=list(tokens),
            a_coefficient=a_coefficient,
            a_precision=a_precision,
            fee=fee,
            admin_fee=admin_fee,
            rate_multipliers=list(rate_multipliers),
            balances=list(balances),
            update_block=update_block,
            swap_style=1,  # STANDARD
            d_variant=1,  # STANDARD
            y_variant=1,  # STANDARD
            yd_variant=1,
        ),
    )


def make_balancer_weighted_handle(
    *,
    bot: Bot | None = None,
    address: str = "0x" + "2" * 40,
    vault: str = "0x" + "3" * 40,
    pool_id_hex: str = "0x" + "00" * 32,
    tokens: tuple[str, str] = (TOKEN0, TOKEN1),
    weights: tuple[int, int] = (500_000_000_000_000_000, 500_000_000_000_000_000),
    scaling_factors: tuple[int, int] = (ONE, ONE),
    swap_fee: int = 0,
    pow_version: int = 2,
    balances: tuple[int, int] = (1_000_000, 1_000_000),
    update_block: int = 100,
) -> Pool:
    """Register a Balancer weighted pool (equal weights, zero fee)."""
    bot = bot if bot is not None else Bot(1)
    return _handle(
        bot,
        bot.register_balancer_weighted_pool(
            address=address,
            vault=vault,
            pool_id_hex=pool_id_hex,
            tokens=list(tokens),
            weights=list(weights),
            scaling_factors=list(scaling_factors),
            swap_fee=swap_fee,
            pow_version=pow_version,
            balances=list(balances),
            update_block=update_block,
        ),
    )


def make_balancer_stable_handle(
    *,
    bot: Bot | None = None,
    chain_id: int = 1,
    address: str = "0x" + "4" * 40,
    vault: str = "0x" + "5" * 40,
    pool_id_hex: str = "0x44" + "00" * 31,
    tokens: tuple[str, str] = (TOKEN0, TOKEN1),
    amp: int = 100 * 1000,
    scaling_factors: tuple[int, int] = (ONE, ONE),
    swap_fee: int = 0,
    bpt_idx: int | None = None,
    invariant_version: int = 2,
    balances: tuple[int, int] = (1_000_000, 1_000_000),
    update_block: int = 100,
) -> Pool:
    """Register a Balancer stable pool.

    Stable-math registration reads token decimals through the Bot's token
    registry, so this registrar registers the pool's tokens first (the handle
    surface has no companion to supply them).
    """
    bot = bot if bot is not None else Bot(chain_id)
    for token in tokens:
        if bot.get_token(token) is None:
            bot.register_token(token, "T", "T", 18, chain_id)
    return _handle(
        bot,
        bot.register_balancer_stable_pool(
            address=address,
            vault=vault,
            pool_id_hex=pool_id_hex,
            tokens=list(tokens),
            amp=amp,
            scaling_factors=list(scaling_factors),
            swap_fee=swap_fee,
            bpt_idx=bpt_idx,
            invariant_version=invariant_version,
            balances=list(balances),
            update_block=update_block,
        ),
    )


__all__ = [
    "CONCENTRATED_TICK_DATA",
    "ONE",
    "TOKEN0",
    "TOKEN1",
    "make_aerodrome_handle",
    "make_balancer_stable_handle",
    "make_balancer_weighted_handle",
    "make_curve_handle",
    "make_v2_handle",
    "make_v3_handle",
    "make_v4_handle",
]
