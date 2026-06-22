"""I/O-free `UniswapV4Pool` construction helper for tests (ADR-005 slice 9b).

Mirror of `make_v3_pool` — registers the V4 pool in Rust (PyBot) and wraps the
returned `PyLiquidityPool` handle in the V4 companion. The companion owns NO
mutable state (Rust is the source of truth); it carries the V4 identity
(pool_id, pool_manager, pool_key, hook_address, fees) + the `PyLiquidityPool`
handle. Construct via this helper; do NOT call `UniswapV4Pool(...)` directly in
tests (the constructor takes a handle, not scalars).
"""

from __future__ import annotations

from typing import Any

from degenbot.constants import ZERO_ADDRESS
from degenbot.degenbot_rs import PyBot
from degenbot.erc20 import Erc20Token
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

# No shared PyBot — V4 tests frequently reuse the same pool_id (V4 pool_id is
# derived from the pool_key, so tests sharing a pool_key collide on a shared
# bot's `(pool_manager, pool_id)` registry). Each `make_v4_pool` call gets a
# fresh bot (the token objects are independent Python wrappers; their Rust bot
# membership doesn't matter for pool registration, which takes addresses).


def make_v4_pool(
    *,
    pool_id: str,
    pool_manager_address: str,
    token0: Erc20Token,
    token1: Erc20Token,
    fee: int,
    tick_spacing: int,
    hook_address: str | None = None,
    state_view_address: str | None = None,
    chain_id: int | None = None,
    sqrt_price_x96: int,
    tick: int,
    liquidity: int,
    protocol_fee_zero_for_one: int,
    protocol_fee_one_for_zero: int,
    lp_fee: int,
    tick_data: dict[int, Any] | None = None,
    tick_bitmap: dict[int, Any] | None = None,
    state_block: int | None = None,
    py_bot: PyBot | None = None,
) -> UniswapV4Pool:
    """Construct an I/O-free `UniswapV4Pool` registered in Rust.

    Returns the V4 companion over the `PyLiquidityPool` handle.
    """
    bot = py_bot or PyBot()
    hook_flags = int(hook_address, 16) if hook_address else 0
    blk = state_block if state_block is not None else 0

    pool_id_int = bot.register_v4_pool(
        pool_manager=pool_manager_address,
        pool_id_hex=pool_id,
        currency0=token0.address,
        currency1=token1.address,
        fee=fee,
        tick_spacing=tick_spacing,
        hook_flags=hook_flags,
        sqrt_price_x96=sqrt_price_x96,
        liquidity=liquidity,
        tick=tick,
        block=blk,
    )
    handle = bot.get_pool(pool_id_int)

    # Seed the initial tick snapshot (non-empty tick data) into Rust so the
    # companion starts non-empty (mirrors `make_v3_pool`).
    if tick_data is not None and len(tick_data) > 0:
        rows: dict[int, tuple[int, int, int]] = {}
        for t, info in tick_data.items():
            if isinstance(info, LiquidityAtTick):
                rows[int(t)] = (int(info.liquidity_gross), int(info.liquidity_net), int(info.block))
            else:
                rows[int(t)] = (int(info[0]), int(info[1]), int(info[2]) if len(info) > 2 else blk)
        handle.update_tick_data(tick_bitmap or {}, rows, blk)

    sparse = tick_data is None or len(tick_data) == 0
    pool = UniswapV4Pool(
        handle,
        pool_id=pool_id,
        pool_manager_address=pool_manager_address,
        token0=token0,
        token1=token1,
        fee=fee,
        tick_spacing=tick_spacing,
        hook_address=hook_address or ZERO_ADDRESS,
        state_view_address=state_view_address,
        chain_id=chain_id,
        protocol_fee_zero_for_one=protocol_fee_zero_for_one,
        protocol_fee_one_for_zero=protocol_fee_one_for_zero,
        lp_fee=lp_fee,
        tick_bitmap=tick_bitmap,
        state_block=state_block,
        sparse_liquidity_map=sparse,
    )
    return pool


__all__ = ["make_v4_pool"]
