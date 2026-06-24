"""Factory helpers for I/O-free V3 pool construction in tests.

Mirrors ``tests/helpers/v2_pool_factory.py`` (ADR-005 slice 4): every direct
``UniswapV3Pool(...)`` construction in the test suite routes through
``make_v3_pool`` so the ``PyLiquidityPool`` handle is wired through
``Bot::register_v3_pool`` → ``get_pool`` → companion, matching the
``Bot.build_pool()`` flow (ADR-005 slice 8b).

Each call creates its own short-lived ``PyBot`` (the returned handle holds an
``Arc`` clone of the underlying ``Bot``, so it outlives the ``PyBot``) — so
each test pool is fully isolated (no shared mutable state across tests, which
matters because pools are mutable, unlike slice-3's tokens).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyBot
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from degenbot.degenbot_rs import PyLiquidityPool
    from degenbot.types.aliases import BlockNumber, ChainId


def _tick_data_to_rust_rows(
    tick_data: dict[int, Any] | None,
) -> dict[int, tuple[int, int, int]]:
    """Convert ``{tick: LiquidityAtTick | tuple}`` to the Rust write shape.

    Rust's ``PyLiquidityPool.update_tick_data`` expects
    ``{tick: (liquidity_gross, liquidity_net, block)}`` (symmetric with
    ``tick_data_snapshot``'s read shape). The Python companion carries
    ``LiquidityAtTick(liquidity_net, liquidity_gross, block)`` — net before
    gross, in the pydantic field order — so this helper does the field
    reordering + int narrowing at the boundary (mirrors how the V2 helper
    converts its Python ``Fraction`` fees to the Rust ``gamma_numer`` at the
    boundary).
    """
    rows: dict[int, tuple[int, int, int]] = {}
    if tick_data is None:
        return rows
    for tick, info in tick_data.items():
        if isinstance(info, LiquidityAtTick):
            rows[int(tick)] = (
                int(info.liquidity_gross),
                int(info.liquidity_net),
                int(info.block),
            )
        elif isinstance(info, dict):
            rows[int(tick)] = (
                int(info["liquidity_gross"]),
                int(info["liquidity_net"]),
                int(info.get("block", 0)),
            )
        else:
            # tuple/list ordering: (liquidity_gross, liquidity_net, block) —
            # matches the snapshot read shape + the original V3 test fixtures.
            gross, net = int(info[0]), int(info[1])
            block = int(info[2]) if len(info) > 2 else 0
            rows[int(tick)] = (gross, net, block)
    return rows


def make_v3_pool(
    address: str,
    *,
    token0: Erc20Token,
    token1: Erc20Token,
    factory: str,
    fee: int,
    tick_spacing: int,
    sqrt_price_x96: int,
    tick: int,
    liquidity: int,
    chain_id: ChainId | None = None,
    deployer_address: str | None = None,
    init_hash: str | None = None,
    state_block: BlockNumber | None = None,
    tick_bitmap: dict[int, Any] | None = None,
    tick_data: dict[int, Any] | None = None,
    tick_data_fetcher: Any = None,
    py_bot: PyBot | None = None,
    pool_class: type[UniswapV3Pool] = UniswapV3Pool,
) -> UniswapV3Pool:
    """Construct an I/O-free V3 companion over a fresh ``PyLiquidityPool`` handle.

    Registers the pool in a short-lived ``PyBot`` (the returned handle holds an
    ``Arc`` clone of the underlying ``Bot``, so it outlives the ``PyBot``) —
    so each test pool is fully isolated (no shared mutable state across tests,
    which matters because pools are mutable, unlike slice-3's tokens).
    The token companions passed in may live in a different ``Bot`` (their own
    ``make_erc20`` ``PyBot``); that's fine — the pool reads scalars from its
    own ``Bot`` and token metadata from the token's handle, independently.

    ``Bot.build_pool()`` is the production path (registers in the session's
    shared ``_py_bot``); this helper is the test-only equivalent.

    Sparseness: when both ``tick_bitmap`` and ``tick_data`` are ``None`` (the
    common test case for the I/O-free scalar-only construction), the companion
    is built sparse (the fetcher backfills on demand, mirroring the engine's
    authority over tick data per plan-101). When ``tick_data`` is provided it
    is seeded into Rust via ``PyLiquidityPool.update_tick_data`` and the
    companion is built non-sparse.
    """
    address_checksum = get_checksum_address(address)
    resolved_chain_id = chain_id if chain_id is not None else token0.chain_id

    bot = py_bot if py_bot is not None else PyBot()
    # ADR-006 rolling-start race closure: seed tick_data INLINE in
    # ``register_v3_pool`` (one BotState write lock) so a pump Mint/Burn
    # landing after registration applies on top of the seed and is never
    # clobbered by a later ``update_tick_data`` overwrite.
    rust_rows = _tick_data_to_rust_rows(tick_data)
    state_block_int = state_block if state_block is not None else 0
    coverage = "sparse" if (tick_data is None or len(tick_data) == 0) else "tracked"
    pool_id = bot.register_v3_pool(
        address=address_checksum,
        token0=token0.address,
        token1=token1.address,
        fee=fee,
        tick_spacing=tick_spacing,
        factory=factory,
        sqrt_price_x96=sqrt_price_x96,
        liquidity=liquidity,
        tick=tick,
        tick_data=rust_rows or None,
        update_block=state_block_int,
        coverage=coverage,
    )
    handle: PyLiquidityPool | None = bot.get_pool(pool_id)
    assert handle is not None, "register_v3_pool returned a pool_id with no handle"

    # ``apply_swap`` anchors the reorg genesis delta at state_block (mirrors
    # the V2 builder writing reserves with update_block=state_block). The
    # tick_data seed landed inline in ``register_v3_pool`` — no separate
    # ``update_tick_data`` call (that would clobber pump-applied events).
    if state_block is not None and state_block_int > 0:
        handle.apply_swap(
            sqrt_price_x96=sqrt_price_x96,
            liquidity=liquidity,
            tick=tick,
            block_number=state_block_int,
        )

    sparse = tick_data is None or len(tick_data) == 0
    pool = pool_class(
        handle,
        address=address_checksum,
        token0=token0,
        token1=token1,
        factory=factory,
        fee=fee,
        tick_spacing=tick_spacing,
        chain_id=resolved_chain_id,
        deployer_address=deployer_address,
        init_hash=init_hash,
        tick_data_fetcher=tick_data_fetcher,
        state_block=state_block_int,
        sparse_liquidity_map=sparse,
        tick_bitmap_override=tick_bitmap,
    )
    return pool


__all__ = [
    "BitmapAtWord",
    "LiquidityAtTick",
    "UniswapV3Pool",
    "make_v3_pool",
]
