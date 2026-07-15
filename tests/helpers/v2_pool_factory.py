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
from degenbot._ffi import PyBot, PyDexIdentity, PyLiquidityPool
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.types.aliases import ChainId


def _gamma_complement(fee: Fraction) -> tuple[int, int]:
    """Return the ``(gamma_numer, fee_denom)`` Rust registration pair for a fee.

    Rust's ``Bot::register_v2_pool`` and the Möbius ``IntHopState`` interpret
    ``gamma_numer`` as the post-fee RETAINED fraction (e.g. 997 for a 0.3% fee,
    where ``fee_denom=1000`` and ``gamma_numer=1000-3=997`` — the slice-4
    parity tests confirm this). Source data from Python is the FEE ``Fraction``
    (e.g. ``Fraction(3, 1000)``), so we convert to the retained-fraction form.
    """
    return (fee.denominator - fee.numerator, fee.denominator)


def make_v2_pool(
    address: str,
    *,
    token0: Erc20Token,
    token1: Erc20Token,
    factory: str | None = None,
    fee_token0: Fraction | None = None,
    fee_token1: Fraction | None = None,
    reserves_token0: int,
    reserves_token1: int,
    chain_id: ChainId | None = None,
    deployer_address: str | None = None,
    init_hash: str | None = None,
    state_block: int = 0,
    pool_class: type[UniswapV2Pool] = UniswapV2Pool,
    dex: PyDexIdentity | None = None,
    py_bot: PyBot | None = None,
    variant: str | None = None,
    stable_swap: bool = False,
    fee_denominator: int | None = None,
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

    ``dex`` (ADR-005 slice 7 step 3): the canonical DexIdentity preset. When
    provided, fills in ``factory``/``fee_token0``/``fee_token1``/``init_hash``
    from the preset when those explicit params are ``None``. Also threaded
    through to ``UniswapV2Pool(dex=...)``. Explicit params always take
    precedence (non-breaking for callers that pass everything explicitly).
    """
    address = get_checksum_address(address)
    # chain_id is recovered off the handle by _from_py_pool (token0.chain_id).
    if dex is not None:
        if factory is None:
            factory = dex.factory
        if init_hash is None:
            init_hash = dex.init_hash
        if fee_token0 is None:
            gamma0, denom0 = dex.fee_token0
            fee_token0 = Fraction(denom0 - gamma0, denom0)
        if fee_token1 is None:
            gamma1, denom1 = dex.fee_token1
            fee_token1 = Fraction(denom1 - gamma1, denom1)

    if factory is None:
        msg = "factory must be provided explicitly or resolvable from dex"
        raise ValueError(msg)
    assert fee_token0 is not None
    assert fee_token1 is not None

    # Compute the Rust registration params (gamma_numer = retained fraction).
    # When dex provided the fees, ``dex.fee_tokenN`` is already
    # ``(gamma_numer, fee_denom)`` — read directly. When explicit fees were
    # given (FEE Fraction), convert via subtraction.
    if dex is not None and fee_token0 == Fraction(
        dex.fee_token0[1] - dex.fee_token0[0], dex.fee_token0[1]
    ):
        gamma_numer0, fee_denom0 = dex.fee_token0
        gamma_numer1, fee_denom1 = dex.fee_token1
    else:
        gamma_numer0, fee_denom0 = _gamma_complement(fee_token0)
        gamma_numer1, fee_denom1 = _gamma_complement(fee_token1)

    py_bot = py_bot if py_bot is not None else PyBot()

    # Descriptor params (ADR-005 / FMO2GE): ``variant`` defaults from the dex
    # preset (if provided) or "uniswap-v2". ``stable_swap``/``fee_denominator``
    # default to False/None — callers building a Camelot stable pool pass them
    # explicitly. These flow into Rust as a ``V2PoolDescriptor`` on the
    # ``PyLiquidityPool`` handle.
    resolved_variant = (
        variant if variant is not None else (dex.variant if dex is not None else "uniswap-v2")
    )

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
        variant=resolved_variant,
        stable_swap=stable_swap,
        fee_denominator=fee_denominator,
    )
    # ADR-006 (OGTTCS D1): the pool's tokens must live in the SAME Bot as
    # the pool — ``_from_py_pool`` recovers them via ``py_pool.get_token0``/
    # ``get_token1``, which look up ``token0_address``/``token1_address`` in
    # the pool's own ``BotState``. The token companions passed in may have been
    # built against a different ``PyBot``; re-register their metadata here so
    # the handle is self-describing. ``register_token`` asserts on duplicates,
    # so guard with ``get_token``.
    for tok in (token0, token1):
        if py_bot.get_token(tok.address) is None:
            py_bot.register_token(tok.address, tok.name, tok.symbol, tok.decimals, tok.chain_id)

    py_pool: PyLiquidityPool | None = py_bot.get_pool(pool_id)
    assert py_pool is not None, "register_v2_pool returned a pool_id with no handle"

    return pool_class._from_py_pool(py_pool)
