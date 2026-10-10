"""Factory helper for I/O-free Aerodrome V2 pool construction in tests.

Every direct ``AerodromeV2Pool(...)`` construction in the test suite routes
through ``make_aerodrome_v2_pool`` so the ``Pool`` handle is wired
through ``Bot.register_aerodrome_pool`` → ``get_pool`` → companion, matching
the ``Bot.build_pool()`` flow (ADR-005 Aerodrome state port).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.checksum_cache import get_checksum_address
from tests.helpers.pool_bot import new_pool_bot

if TYPE_CHECKING:
    from fractions import Fraction

    from degenbot._ffi import Bot
    from degenbot.erc20 import Erc20Token


def make_aerodrome_v2_pool(
    address: str,
    *,
    token0: Erc20Token,
    token1: Erc20Token,
    factory: str,
    fee: Fraction,
    stable: bool,
    reserves_token0: int,
    reserves_token1: int,
    deployer_address: str | None = None,
    state_block: int = 0,
    py_bot: Bot | None = None,
    pool_class: type[AerodromeV2Pool] = AerodromeV2Pool,
) -> AerodromeV2Pool:
    """Construct an I/O-free Aerodrome V2 companion over a fresh Pool handle.

    Mints a fresh ``Bot`` unless ``py_bot`` is injected, so each pool is
    isolated; registration is get-or-register (a same-identity re-build
    answers the EXISTING handle). A caller that wants to share one bot passes
    ``py_bot=``. Tokens must be registered in the same ``Bot`` (ADR-006).
    """
    bot = py_bot if py_bot is not None else new_pool_bot()
    address = get_checksum_address(address)

    # ADR-006: tokens must be in the same Bot as the pool.
    for tok in (token0, token1):
        if bot.get_token(tok.address) is None:
            bot.register_token(tok.address, tok.name, tok.symbol, tok.decimals, tok.chain_id)

    variant = "aerodrome-v2-stable" if stable else "aerodrome-v2-volatile"
    pool_id = bot.register_aerodrome_pool(
        address=address,
        token0=token0.address,
        token1=token1.address,
        factory=factory,
        variant=variant,
        stable=stable,
        fee_numer=fee.numerator,
        fee_denom=fee.denominator,
        token0_decimals=token0.decimals,
        token1_decimals=token1.decimals,
        reserve0=reserves_token0,
        reserve1=reserves_token1,
        update_block=state_block,
    )
    handle = bot.get_pool(pool_id)
    assert handle is not None, "register_aerodrome_pool returned no handle"

    pool = pool_class.from_handle(handle)
    if deployer_address is not None:
        pool.deployer_address = get_checksum_address(deployer_address)
    return pool


__all__ = ["make_aerodrome_v2_pool"]
