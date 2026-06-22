"""Factory helpers for I/O-free Curve StableSwap pool construction in tests.

Mirrors ``tests/helpers/v3_pool_factory.py`` and ``v4_pool_factory.py``
(ADR-005 slice 11b): every direct ``CurveStableswapPool(...)`` construction in
the test suite routes through ``make_curve_pool`` so the ``PyLiquidityPool``
handle is wired through ``PyBot::register_curve_pool`` → ``get_pool`` →
companion, matching the ``Bot.build_pool()`` flow (ADR-005).

Each call creates its own short-lived ``PyBot`` (the returned handle holds an
``Arc`` clone of the underlying ``Bot``, so it outlives the ``PyBot``) — so
each test pool is fully isolated (no shared mutable state across tests).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.curve_stableswap_liquidity_pool import (
    CurveStableswapPool,
    _compute_rate_and_precision_multipliers,
)
from degenbot.curve.strategies import PoolStrategies
from degenbot.degenbot_rs import PyBot

if TYPE_CHECKING:
    from degenbot.curve.curve_stableswap_liquidity_pool import CurveDataProvider
    from degenbot.degenbot_rs import PyLiquidityPool
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types.aliases import BlockNumber, ChainId


def _strategies_to_rust_enums(strategies: PoolStrategies) -> tuple[int, int, int, int, int]:
    """Map ``PoolStrategies`` enums → the ``u8`` discriminants Rust stores.

    The Rust core (``register_curve_pool``) carries the variant enums as
    opaque ``u8`` for the future Rust ``get_dy`` (ADR-005 slice 11c). `auto()`
    enums are 1-based; ``.value`` is forwarded verbatim. Not consumed in
    slice 11b (the companion keeps its own ``PoolStrategies`` for the Python
    calc path).
    """
    return (
        strategies.swap_style.value,
        strategies.lending_rate_style.value,
        strategies.d_variant.value,
        strategies.y_variant.value,
        strategies.yd_variant.value,
    )


def make_curve_pool(
    address: str,
    *,
    tokens: list[Erc20Token] | tuple[Erc20Token, ...],
    a_coefficient: int,
    fee: int,
    admin_fee: int,
    balances: list[int] | tuple[int, ...],
    chain_id: ChainId | None = None,
    state_block: BlockNumber | None = None,
    state_cache_depth: int = 8,
    data_provider: CurveDataProvider | None = None,
    base_pool: CurveStableswapPool | None = None,
    tokens_underlying: list[Erc20Token] | tuple[Erc20Token, ...] | None = None,
    lp_token: Erc20Token | None = None,
    use_lending: list[bool] | tuple[bool, ...] | None = None,
    precision_multipliers: list[int] | tuple[int, ...] | None = None,
    initial_a_coefficient: int | None = None,
    future_a_coefficient: int | None = None,
    initial_a_coefficient_time: int | None = None,
    future_a_coefficient_time: int | None = None,
    create_timestamp: int | None = None,
    fee_gamma: int | None = None,
    mid_fee: int | None = None,
    out_fee: int | None = None,
    gamma: int | None = None,
    offpeg_fee_multiplier: int | None = None,
    strategies: PoolStrategies | None = None,
    py_bot: PyBot | None = None,
    pool_class: type[CurveStableswapPool] = CurveStableswapPool,
) -> CurveStableswapPool:
    """Construct an I/O-free Curve companion over a fresh ``PyLiquidityPool`` handle.

    Registers the pool in a short-lived ``PyBot`` (the returned handle holds an
    ``Arc`` clone of the underlying ``Bot``, so it outlives the ``PyBot``) —
    so each test pool is fully isolated (no shared mutable state across tests).
    The token companions passed in may live in a different ``Bot`` (their own
    ``make_erc20`` ``PyBot``); that's fine — the pool reads balances from its
    own ``Bot`` and token metadata from the token handle, independently.

    ``Bot.build_pool()`` is the production path (registers in the session's
    shared ``_py_bot``); this helper is the test-only equivalent.

    The immutable config (tokens, A, fee, strategies, rate_multipliers) stays
    Python-side on the companion; the mutable ``balances`` + ``update_block`` +
    the reorg journal live in Rust. The factory derives ``rate_multipliers``
    via the shared ``_compute_rate_and_precision_multipliers`` helper and
    forwards them to ``register_curve_pool`` so the Rust core stores the
    same values the companion keeps (consumed by the future Rust ``get_dy``).
    """
    address_checksum = get_checksum_address(address)
    resolved_chain_id = chain_id if chain_id is not None else tokens[0].chain_id
    state_block_int = state_block if state_block is not None else 0

    bot = py_bot if py_bot is not None else PyBot()

    rate_multipliers, _ = _compute_rate_and_precision_multipliers(
        tokens, precision_multipliers, CurveStableswapPool.PRECISION_DECIMALS
    )

    resolved_strategies = strategies if strategies is not None else PoolStrategies()
    swap_style, lending_rate_style, d_variant, y_variant, yd_variant = _strategies_to_rust_enums(
        resolved_strategies
    )

    pool_id = bot.register_curve_pool(
        address=address_checksum,
        tokens=[t.address for t in tokens],
        a_coefficient=a_coefficient,
        fee=fee,
        admin_fee=admin_fee,
        rate_multipliers=list(rate_multipliers),
        balances=list(balances),
        update_block=state_block_int,
        swap_style=swap_style,
        lending_rate_style=lending_rate_style,
        d_variant=d_variant,
        y_variant=y_variant,
        yd_variant=yd_variant,
        base_pool=(base_pool.address if base_pool is not None else None),
    )
    handle: PyLiquidityPool | None = bot.get_pool(pool_id)
    assert handle is not None, "register_curve_pool returned a pool_id with no handle"

    return pool_class(
        handle,
        address=address_checksum,
        tokens=tokens,
        a_coefficient=a_coefficient,
        fee=fee,
        admin_fee=admin_fee,
        chain_id=resolved_chain_id,
        state_block=state_block_int,
        state_cache_depth=state_cache_depth,
        data_provider=data_provider,
        base_pool=base_pool,
        tokens_underlying=tokens_underlying,
        lp_token=lp_token,
        use_lending=use_lending,
        precision_multipliers=precision_multipliers,
        initial_a_coefficient=initial_a_coefficient,
        future_a_coefficient=future_a_coefficient,
        initial_a_coefficient_time=initial_a_coefficient_time,
        future_a_coefficient_time=future_a_coefficient_time,
        create_timestamp=create_timestamp,
        fee_gamma=fee_gamma,
        mid_fee=mid_fee,
        out_fee=out_fee,
        gamma=gamma,
        offpeg_fee_multiplier=offpeg_fee_multiplier,
        strategies=resolved_strategies,
    )


__all__ = [
    "CurveStableswapPool",
    "make_curve_pool",
]
