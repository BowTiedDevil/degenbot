"""Factory helpers for I/O-free V3 pool construction in tests.

Mirrors ``tests/helpers/v2_pool_factory.py``: every direct
``UniswapV3Pool(...)`` construction in the test suite routes through
``make_v3_pool`` so the ``Pool`` handle is wired through
``Bot::register_v3_pool`` → ``get_pool`` → companion, matching the
``Bot.build_pool()`` flow.

Each call mints a FRESH ``Bot`` (via ``new_pool_bot``) so every built pool is
isolated. The register entry is get-or-register — a same address + same
family + same identity build answers the EXISTING handle — while a genuine
conflict (a different family, or a mismatched identity = wire drift) still
refuses loudly. Pools are MUTABLE, so isolation is the default; a caller that
WANTS to share one bot passes ``py_bot=`` explicitly.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from degenbot.checksum_cache import get_checksum_address
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.types.pool_type import PoolFamily, PoolTypeDescriptor, derive_kind
from tests.helpers.pool_bot import new_pool_bot

if TYPE_CHECKING:
    from collections.abc import Iterable

    from degenbot._ffi import Bot
    from degenbot.erc20.erc20 import Erc20Token
    from degenbot.types import Pool
    from degenbot.types.aliases import BlockNumber


def _tick_data_to_rust_rows(
    tick_data: dict[int, Any] | None,
) -> dict[int, tuple[int, int, int]]:
    """Convert ``{tick: LiquidityAtTick | tuple}`` to the Rust seed shape.

    Rust's ``register_v3_pool`` tick-data wire shape is
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
    deployer_address: str | None = None,
    init_hash: str | None = None,
    state_block: BlockNumber | None = None,
    tick_data: dict[int, Any] | None = None,
    known_bitmap_words: Iterable[int] | None = None,
    tick_data_fetcher: Any = None,
    py_bot: Bot | None = None,
    pool_class: type[UniswapV3Pool] = UniswapV3Pool,
) -> UniswapV3Pool:
    """Construct an I/O-free V3 companion over a fresh ``Pool`` handle.

    Mints a fresh ``Bot`` unless ``py_bot`` is injected, so each pool is
    isolated. Registration is get-or-register (a same-identity re-build
    answers the EXISTING handle); a caller that wants to share one bot passes
    ``py_bot=``. The token companions passed in may live in a different
    ``Bot`` (their own ``make_erc20`` ``Bot``); that's fine — the pool reads
    scalars from its own ``Bot`` and token metadata from the token's handle,
    independently.

    ``Bot.build_pool()`` is the production path (registers in the session's
    shared ``_py_bot``); this helper is the test-only equivalent.

    Sparseness: when ``tick_data`` is ``None`` (the common test case for the
    I/O-free scalar-only construction), the companion is built sparse (the
    fetcher backfills on demand, mirroring the engine's authority over tick
    data). When ``tick_data`` is provided it is seeded into Rust
    inline at registration and the companion is built non-sparse. The retired
    ``tick_bitmap`` value arg is gone (Rust derives the words from the seeded
    tick rows).

    ``known_bitmap_words`` forwards to ``register_v3_pool``: the tick-bitmap
    WORD positions a caller already checked on-chain. A Sparse pool records
    them as checked (a checked word is never re-fetched); a Tracked pool
    ignores them (its bitmap is complete).
    """
    address_checksum = get_checksum_address(address)

    bot = py_bot if py_bot is not None else new_pool_bot()
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
        tick_data_fetcher=tick_data_fetcher,
        known_bitmap_words=(list(known_bitmap_words) if known_bitmap_words is not None else None),
    )
    handle: Pool | None = bot.get_pool(pool_id)
    assert handle is not None, "register_v3_pool returned a pool_id with no handle"

    # ``seed_genesis`` anchors the reorg journal at state_block — the same
    # production anchor ``Bot.build_pool`` uses (a ``before == after`` delta
    # that advances neither clock), so a mid-window reorg restore has a known
    # state boundary. The tick_data seed landed inline in ``register_v3_pool``.
    if state_block is not None and state_block_int > 0:
        handle.seed_genesis(block_number=state_block_int)

    # ADR-006: the pool's tokens must live in the SAME Bot as
    # the pool — ``from_handle`` recovers them via ``py_pool.get_token0``/
    # ``get_token1``, which look up ``token0_address``/``token1_address`` in
    # the pool's own ``BotState``. The token companions passed in may have been
    # built against a different ``Bot``; re-register their metadata here.
    for tok in (token0, token1):
        if bot.get_token(tok.address) is None:
            bot.register_token(tok.address, tok.name, tok.symbol, tok.decimals, tok.chain_id)
    pool = pool_class.from_handle(handle)
    # Identity is builder knowledge (Bot.build_pool binds the resolver's
    # descriptor the same way). The class label alone conflates variants.
    pool.bind_pool_type(
        PoolTypeDescriptor(
            family=PoolFamily.CONCENTRATED_LIQUIDITY,
            variant=pool_class.variant,
            kind=derive_kind(PoolFamily.CONCENTRATED_LIQUIDITY, pool_class.variant),
            factory=get_checksum_address(factory),
        )
    )
    return pool


__all__ = [
    "LiquidityAtTick",
    "UniswapV3Pool",
    "make_v3_pool",
]
