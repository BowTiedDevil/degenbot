"""Factory helpers for I/O-free V2 pool construction in tests.

Mirrors ``tests/helpers/erc20_factory.py`` (slice 3): every direct
``UniswapV2Pool(...)`` / V2-subclass construction in the test suite routes
through ``make_v2_pool`` so the ``PyLiquidityPool`` handle is wired through
``Bot::register_v2_pool`` → ``get_pool`` → companion, matching the
``Bot.build_pool()`` flow.

Each call creates its own short-lived ``PyBot`` (the returned handle holds an
``Arc`` clone of the underlying ``Bot``, so it outlives the ``PyBot``) — so
each test pool is fully isolated (no shared mutable state across tests, which
matters because pools are mutable, unlike slice-3's tokens).
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot, PyLiquidityPool
from degenbot.erc20 import Erc20Token
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

if TYPE_CHECKING:
    from degenbot.types.aliases import ChainId


def _fee_parts(fee: Fraction) -> tuple[int, int]:
    """Return the ``(numerator, denominator)`` of a fee ``Fraction``."""
    return (fee.numerator, fee.denominator)


def make_v2_pool(
    address: str,
    *,
    token0: Erc20Token,
    token1: Erc20Token,
    factory: str,
    fee_token0: Fraction,
    fee_token1: Fraction,
    reserves_token0: int,
    reserves_token1: int,
    chain_id: ChainId | None = None,
    deployer_address: str | None = None,
    init_hash: str | None = None,
    state_block: int = 0,
    pool_class: type[UniswapV2Pool] = UniswapV2Pool,
) -> UniswapV2Pool:
    """Construct an I/O-free V2-style pool companion over a fresh ``PyLiquidityPool`` handle.

    Each call creates its own short-lived ``PyBot`` (the returned handle holds
    an ``Arc`` clone of the underlying ``Bot``, so it outlives the ``PyBot``)
    — so each test pool is fully isolated (no shared mutable state across
    tests, which matters because pools are mutable, unlike slice-3's tokens).
    The token companions passed in may live in a different ``Bot`` (their own
    ``make_erc20`` ``PyBot``); that's fine — the pool reads reserves from its
    own ``Bot`` and token metadata from the token's handle, independently.

    ``Bot.build_pool()`` is the production path (registers in the session's
    shared ``_py_bot``); this helper is the test-only equivalent.

    ``pool_class`` defaults to ``UniswapV2Pool``; subclasses like
    ``SushiswapV2Pool`` inherit the companion ``__init__``.
    """
    address = get_checksum_address(address)
    resolved_chain_id = chain_id if chain_id is not None else token0.chain_id

    gamma_numer0, fee_denom0 = _fee_parts(fee_token0)
    gamma_numer1, fee_denom1 = _fee_parts(fee_token1)

    py_bot = PyBot()
    pool_id = py_bot.register_v2_pool(
        address=address,
        token0=token0.address,
        token1=token1.address,
        reserve0=reserves_token0,
        reserve1=reserves_token1,
        gamma_numer0=gamma_numer0,
        fee_denom0=fee_denom0,
        gamma_numer1=gamma_numer1,
        fee_denom1=fee_denom1,
        factory=factory,
        update_block=state_block,
    )
    py_pool: PyLiquidityPool | None = py_bot.get_pool(pool_id)
    assert py_pool is not None, "register_v2_pool returned a pool_id with no handle"

    return pool_class(
        py_pool,
        address=address,
        chain_id=resolved_chain_id,
        deployer_address=deployer_address,
        init_hash=init_hash,
        token0=token0,
        token1=token1,
        factory=factory,
        fee_token0=fee_token0,
        fee_token1=fee_token1,
    )
