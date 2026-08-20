"""T2 (FBJTUM, epic OU4SYZ) — the companion's sparse-word backfill gate.

A Mint/Burn on boundary ticks that live in an un-fetched Sparse word must
backfill the word via the RUST-stored fetcher at ``state_block - 1`` BEFORE
applying — and a failed fetch must RAISE (never silently apply over an
unknown word; the reorg journal's priors for those ticks would be wrong).
The pre-T2 gate in both twins was dead (``_tick_data_fetcher`` is always
``None`` once the fetcher moved Rust-side), so the gate never fired.
Tracked pools: the gate is inert (the bitmap is complete).
"""

from __future__ import annotations

import pytest

from degenbot._ffi import Bot
from degenbot.exceptions import LiquidityMapWordMissing
from degenbot.uniswap.v3_types import UniswapV3PoolLiquidityMappingUpdate
from degenbot.uniswap.v4_types import UniswapV4PoolLiquidityMappingUpdate
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool
from tests.helpers.v4_pool_factory import make_v4_pool

_C0 = "0xA0b86991c6218b36c1D19D4a2e9Eb0cE3606eB48"
_C1 = "0xC02aaA39b223FE8D0A0e5C4f27eAD9083C756Cc2"
_POOL_ADDR = "0x2222222222222222222222222222222222222222"
_FACTORY = "0x1111111111111111111111111111111111111111"
_MGR = "0x3333333333333333333333333333333333333333"
_RPC_DOWN = "rpc down"  # adapter maps fetcher exceptions to fetch-failure


def _tokens(bot: Bot):
    return (
        make_erc20(bot, _C0, name="c0", symbol="C0", decimals=18, chain_id=1),
        make_erc20(bot, _C1, name="c1", symbol="C1", decimals=6, chain_id=1),
    )


# ── V3 ─────────────────────────────────────────────────────────────────────


def test_v3_sparse_gate_backfills_boundary_word_before_applying() -> None:
    bot = Bot()
    t0, t1 = _tokens(bot)
    calls: list[tuple[int, int]] = []

    def fetcher(word: int, block: int) -> dict | None:
        calls.append((word, block))
        # word 0 (spacing 60) holds ticks 0..119; boundary 60 is in it.
        return {60: (1000, 0, 0)} if word == 0 else None

    pool = make_v3_pool(
        _POOL_ADDR,
        token0=t0,
        token1=t1,
        factory=_FACTORY,
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=1 << 96,
        tick=0,
        liquidity=10_000_000_000_000,
        tick_data=None,  # sparse
        tick_data_fetcher=fetcher,
        py_bot=bot,
    )

    update = UniswapV3PoolLiquidityMappingUpdate(
        block_number=10, liquidity=50, tick_lower=0, tick_upper=60
    )
    pool.update_liquidity_map(update)

    assert calls == [(0, 9)], "the gate must fetch word 0 at state_block - 1"
    assert 60 in pool.tick_data, "the backfilled word's tick must be merged"
    assert 0 in pool.tick_data, "the applied event's boundary tick must be present"


def test_v3_sparse_gate_fetch_failure_raises_and_does_not_apply() -> None:
    bot = Bot()
    t0, t1 = _tokens(bot)

    def failing_fetcher(word: int, block: int):
        raise RuntimeError(_RPC_DOWN)

    pool = make_v3_pool(
        _POOL_ADDR,
        token0=t0,
        token1=t1,
        factory=_FACTORY,
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=1 << 96,
        tick=0,
        liquidity=10_000_000_000_000,
        tick_data=None,
        tick_data_fetcher=failing_fetcher,
        py_bot=bot,
    )

    update = UniswapV3PoolLiquidityMappingUpdate(
        block_number=10, liquidity=50, tick_lower=0, tick_upper=60
    )
    with pytest.raises(LiquidityMapWordMissing):
        pool.update_liquidity_map(update)
    assert 60 not in pool.tick_data, "a failed fetch must NOT apply the event"


def test_v3_sparse_gate_known_word_is_not_refetched() -> None:
    bot = Bot()
    t0, t1 = _tokens(bot)
    calls: list[int] = []

    def fetcher(word: int, block: int) -> dict | None:
        calls.append(word)
        return {60: (1000, 0, 0)}

    pool = make_v3_pool(
        _POOL_ADDR,
        token0=t0,
        token1=t1,
        factory=_FACTORY,
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=1 << 96,
        tick=0,
        liquidity=10_000_000_000_000,
        tick_data=None,
        tick_data_fetcher=fetcher,
        py_bot=bot,
    )

    mk = lambda lb, ub: UniswapV3PoolLiquidityMappingUpdate(  # ruff: ignore[lambda-assignment]
        block_number=10, liquidity=50, tick_lower=lb, tick_upper=ub
    )
    pool.update_liquidity_map(mk(0, 60))
    pool.update_liquidity_map(mk(60, 120))  # word 0 already known — no re-fetch
    assert calls == [0], "a known word must not be refetched by a later event"


def test_v3_tracked_pool_gate_inert_without_fetcher() -> None:
    bot = Bot()
    t0, t1 = _tokens(bot)
    pool = make_v3_pool(
        _POOL_ADDR,
        token0=t0,
        token1=t1,
        factory=_FACTORY,
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=1 << 96,
        tick=0,
        liquidity=10_000_000_000_000,
        tick_data={0: (1000, 0, 0), 60: (1000, 0, 0)},  # tracked (no fetcher)
        py_bot=bot,
    )
    # A Tracked pool may touch ticks in words absent from the derived bitmap
    # (absent = known-empty) — the gate must NOT fetch or raise.
    update = UniswapV3PoolLiquidityMappingUpdate(
        block_number=10, liquidity=50, tick_lower=120, tick_upper=180
    )
    pool.update_liquidity_map(update)  # no exception
    assert 120 in pool.tick_data
    assert 180 in pool.tick_data


# ── V4 (the twin) ──────────────────────────────────────────────────────────


def _v4_pool_id(token0, token1, fee: int, spacing: int) -> str:
    from degenbot.abi import encode
    from degenbot.crypto import keccak256

    zero_hook = "0x0000000000000000000000000000000000000000"
    return (
        "0x"
        + keccak256(
            encode(
                types=["address", "address", "uint24", "int24", "address"],
                args=[token0.address, token1.address, fee, spacing, zero_hook],
            )
        ).hex()
    )


def test_v4_sparse_gate_backfills_and_raises_like_v3() -> None:
    bot = Bot()
    t0, t1 = _tokens(bot)
    calls: list[tuple[int, int]] = []

    def fetcher(word: int, block: int) -> dict | None:
        calls.append((word, block))
        return {60: (1000, 0, 0)} if word == 0 else None

    pool = make_v4_pool(
        pool_id=_v4_pool_id(t0, t1, fee=3000, spacing=60),
        pool_manager_address=_MGR,
        token0=t0,
        token1=t1,
        fee=3000,
        tick_spacing=60,
        sqrt_price_x96=1 << 96,
        tick=0,
        liquidity=10_000_000_000_000,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=0,
        tick_data=None,
        tick_data_fetcher=fetcher,
        state_block=0,
        coverage="sparse",
        py_bot=bot,
    )

    update = UniswapV4PoolLiquidityMappingUpdate(
        block_number=10, liquidity=50, tick_lower=0, tick_upper=60
    )
    pool.update_liquidity_map(update)
    assert calls == [(0, 9)], "the V4 twin gate backfills word 0 at state_block - 1"
    assert 60 in pool.tick_data

    def failing_fetcher(word: int, block: int):
        raise RuntimeError(_RPC_DOWN)

    pool2 = make_v4_pool(
        pool_id=_v4_pool_id(t0, t1, fee=500, spacing=60),
        pool_manager_address=_MGR,
        token0=t0,
        token1=t1,
        fee=500,
        tick_spacing=60,
        sqrt_price_x96=1 << 96,
        tick=0,
        liquidity=10_000_000_000_000,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=0,
        tick_data=None,
        tick_data_fetcher=failing_fetcher,
        state_block=0,
        coverage="sparse",
        py_bot=bot,
    )
    with pytest.raises(LiquidityMapWordMissing):
        pool2.update_liquidity_map(
            UniswapV4PoolLiquidityMappingUpdate(
                block_number=10, liquidity=50, tick_lower=0, tick_upper=60
            )
        )
