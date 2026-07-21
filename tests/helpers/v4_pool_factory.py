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

from degenbot.bot import PyBot
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20 import Erc20Token
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v4_liquidity_pool import ProtocolFee, UniswapV4Pool

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
    coverage: str | None = None,
    tick_data_fetcher: Any | None = None,
) -> UniswapV4Pool:
    """Construct an I/O-free `UniswapV4Pool` registered in Rust.

    ``coverage``: override the coverage flag. Defaults to ``"tracked"`` when
    ``tick_data`` is non-empty and ``"sparse"`` otherwise — BUT a sparse pool
    seeded with PARTIAL tick_data (a fetch+retry regression gate that must miss
    + fetch the absent words) must register ``coverage="sparse"`` so the Rust
    miss-detection in ``v4_simulate_swap`` fires on the unseeded words. Passing
    ``coverage="sparse"`` explicitly keeps the pool sparse even with seeded
    data; clearing tick_data via ``update_tick_data({})`` does NOT flip the
    Rust coverage flag (it is set at registration), so the sparse contract
    must be established at construction.

    Returns the V4 companion over the `PyLiquidityPool` handle.
    """
    bot = py_bot or PyBot()
    hook_flags = int(hook_address, 16) if hook_address else 0
    blk = state_block if state_block is not None else 0

    # ADR-006 rolling-start race closure: seed tick_data INLINE in
    # ``register_v4_pool`` (mirrors ``make_v3_pool`` + the production V4
    # builders) so the pool is never visible to a live pump unseeded.
    register_rows: dict[int, tuple[int, int, int]] | None = None
    has_tick_data = tick_data is not None and len(tick_data) > 0
    if has_tick_data:
        register_rows = {}
        for t, info in tick_data.items():
            if isinstance(info, LiquidityAtTick):
                register_rows[int(t)] = (
                    int(info.liquidity_gross),
                    int(info.liquidity_net),
                    int(info.block),
                )
            else:
                register_rows[int(t)] = (
                    int(info[0]),
                    int(info[1]),
                    int(info[2]) if len(info) > 2 else blk,
                )
    if coverage is None:
        coverage = "tracked" if has_tick_data else "sparse"

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
        tick_data=register_rows,
        coverage=coverage,
        tick_data_fetcher=tick_data_fetcher,
    )
    handle = bot.get_pool(pool_id_int)

    # No separate ``update_tick_data`` — the inline seed is complete (tick
    # map + known bitmap words, atomically with registration). A separate
    # REPLACE would clobber live pump events (the pre-fix V4 desync).
    # ADR-006: tokens must be in the same Bot as the pool.
    for tok in (token0, token1):
        if bot.get_token(tok.address) is None:
            bot.register_token(tok.address, tok.name, tok.symbol, tok.decimals, tok.chain_id)
    sparse = tick_data is None or len(tick_data) == 0
    pool = UniswapV4Pool._from_py_pool(handle)
    # Builder-supplied values the seam defaults; override from test args.
    pool._state_view_address = (
        get_checksum_address(state_view_address) if state_view_address else ZERO_ADDRESS
    )
    pool.protocol_fee = ProtocolFee(
        zero_for_one=protocol_fee_zero_for_one,
        one_for_zero=protocol_fee_one_for_zero,
    )
    pool.lp_fee = lp_fee
    if tick_bitmap is not None:
        for word, bitmap_at_word in tick_bitmap.items():
            if isinstance(bitmap_at_word, BitmapAtWord):
                pool._bitmap_override[int(word)] = bitmap_at_word
            elif isinstance(bitmap_at_word, dict):
                pool._bitmap_override[int(word)] = BitmapAtWord(**bitmap_at_word)
            else:
                pool._bitmap_override[int(word)] = bitmap_at_word
    pool._sparse_liquidity_map = sparse
    return pool


__all__ = ["make_v4_pool"]
