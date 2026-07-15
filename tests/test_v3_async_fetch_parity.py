"""AsyncBot sparse-pool fetch parity — async-bridge fetcher end-to-end.

``AsyncBot``-built sparse V3 pools now carry a tick-word fetcher built via
``make_tick_data_fetcher_from_async_io`` (a daemon-loop sync bridge over an
``AsyncPoolIO``), so the synchronous Rust ``TickWordFetcher`` seam can issue
async ``eth_call`` RPCs to backfill neighbouring tick words on a crossing swap.

This is the feature-parity gate: an async-built sparse pool (tick-data empty,
fetcher = async bridge over a fake async IO returning canned tickBitmap/ticks
RPC responses) must match the dense-Rust oracle (full tick_data, no miss) for a
swap that crosses into a neighbour tick-bitmap word — the exact failure mode
that previously raised ``LiquidityPoolError`` (fetcher was ``None``).
"""

from __future__ import annotations

import eth_abi.abi
import pytest
from hexbytes import HexBytes

from degenbot.builders.tick_data_fetcher import (
    TickDataTypes,
    make_tick_data_fetcher_from_async_io,
)
from degenbot._ffi import PyBot
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool

# 1:1 price (sqrt_price_x96 = 2**96), tick 0, 0.3% fee, tick_spacing 60.
_SQRT_PRICE_1TO1 = 1 << 96
_LIQUIDITY = 10_000_000_000_000
_FEE = 3000
_TICK_SPACING = 60
_FACTORY = "0x" + "22" * 20

_V3_TYPES = TickDataTypes(
    bitmap_at_word=BitmapAtWord,
    liquidity_at_tick=LiquidityAtTick,
    tick_struct_types=UniswapV3Pool.TICK_STRUCT_TYPES,
)


class _FakeAsyncPoolIO:
    """Async PoolIO double returning canned tickBitmap/ticks RPC responses.

    Matches on calldata exactly (mirrors the sync ``FakePoolIO`` in
    ``tests/builders/test_tick_data_fetcher.py``), so the async bridge is
    forced to run each ``io.call`` coroutine to completion synchronously.
    """

    def __init__(self) -> None:
        self._responses: dict[bytes, bytes] = {}
        self.bitmap_calls: list[int] = []

    def add_response(self, calldata: bytes, response: bytes) -> None:
        self._responses[calldata] = response

    async def call(self, to: str, data: bytes, block: int | None = None) -> HexBytes:
        # tickBitmap(int16, word) — record which words were fetched on-chain.
        if data[:4] == encode_function_calldata("tickBitmap(int16)", [0])[:4]:
            word = eth_abi.abi.decode(["int16"], data[4:])[0]
            self.bitmap_calls.append(word)
        return HexBytes(self._responses.get(data) or b"")


def _encode_bitmap_call(word: int) -> bytes:
    return encode_function_calldata("tickBitmap(int16)", [word])


def _encode_ticks_call(tick: int) -> bytes:
    return encode_function_calldata("ticks(int24)", [tick])


def _make_tokens(py_bot: PyBot, tag: str):
    token0 = make_erc20(
        py_bot,
        address=f"0x{(tag * 20)[:40]}".ljust(42, "0"),
        name="T0",
        symbol="T0",
        decimals=18,
    )
    token1 = make_erc20(
        py_bot,
        address=f"0x{(tag * 20 + '1' * 20)[:40]}".ljust(42, "2"),
        name="T1",
        symbol="T1",
        decimals=18,
    )
    return token0, token1


def _build_dense_oracle(py_bot: PyBot, address: str) -> UniswapV3Pool:
    """Dense pool: position [-60, +60] seeded → coverage=tracked (no miss)."""
    token0, token1 = _make_tokens(py_bot, tag="d")
    tick_data = {
        -60: LiquidityAtTick(liquidity_net=_LIQUIDITY, liquidity_gross=_LIQUIDITY, block=0),
        60: LiquidityAtTick(liquidity_net=-_LIQUIDITY, liquidity_gross=_LIQUIDITY, block=0),
    }
    return make_v3_pool(
        address=address,
        token0=token0,
        token1=token1,
        factory=_FACTORY,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        tick_data=tick_data,
        py_bot=py_bot,
    )


def _build_sparse_with_async_fetcher(
    py_bot: PyBot, address: str, io: _FakeAsyncPoolIO
) -> UniswapV3Pool:
    """Sparse pool + async-bridge fetcher returning canned RPC on miss."""
    token0, token1 = _make_tokens(py_bot, tag="e")
    holder: list[UniswapV3Pool | None] = [None]
    fetcher = make_tick_data_fetcher_from_async_io(
        pool_lookup=lambda _block: holder[0],
        async_io=io,
        types=_V3_TYPES,
    )
    pool = make_v3_pool(
        address=address,
        token0=token0,
        token1=token1,
        factory=_FACTORY,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        tick_data=None,  # sparse — fetcher backfills on miss
        py_bot=py_bot,
        tick_data_fetcher=fetcher,
    )
    holder[0] = pool
    return pool


def _seed_fake_async_io(io: _FakeAsyncPoolIO) -> None:
    """Canned RPC responses for the two neighbour words (0 and -1).

    word 0: tick +60 (bit 1) → bitmap = 1<<1; liquidity_net = -L (upper bound).
    word -1: tick -60 (bit 255) → bitmap = 1<<255; liquidity_net = +L (lower).
    """
    # word 0, tick 60 (compressed=1, bit=1)
    io.add_response(
        _encode_bitmap_call(0),
        eth_abi.abi.encode(["uint256"], [1 << 1]),
    )
    io.add_response(
        _encode_ticks_call(60),
        eth_abi.abi.encode(
            UniswapV3Pool.TICK_STRUCT_TYPES,
            [_LIQUIDITY, -_LIQUIDITY, 0, 0, 0, 0, 0, False],
        ),
    )
    # word -1, tick -60 (compressed=-1, bit=255)
    io.add_response(
        _encode_bitmap_call(-1),
        eth_abi.abi.encode(["uint256"], [1 << 255]),
    )
    io.add_response(
        _encode_ticks_call(-60),
        eth_abi.abi.encode(
            UniswapV3Pool.TICK_STRUCT_TYPES,
            [_LIQUIDITY, _LIQUIDITY, 0, 0, 0, 0, 0, False],
        ),
    )


def test_async_sparse_exact_output_crossing_matches_dense_oracle():
    """Async-built sparse pool's exact-output crossing swap == dense oracle.

    A zero-for-one exact-OUTPUT swap large enough to cross tick -60 (into word
    -1) previously raised ``LiquidityPoolError`` for an async-built pool
    (fetcher was None). With the async-bridge fetcher, the Rust miss-detection
    backfills word -1 via async RPC and the result matches the dense oracle.
    """
    py_bot = PyBot()
    dense = _build_dense_oracle(py_bot, address="0x" + "11" * 20)

    io = _FakeAsyncPoolIO()
    _seed_fake_async_io(io)
    sparse = _build_sparse_with_async_fetcher(py_bot, "0x" + "33" * 20, io)
    assert sparse.sparse_liquidity_map, "no tick_data ⇒ sparse companion"

    # An exact-output amount that crosses tick -60 (zero_for_one: token1 out).
    # The dense oracle returns the exact required token0-in; pick an amount that
    # drives the price below tick -60 by checking the dense final state.
    amount_out = 60_000_000_000  # ~6e10 token1 out — large enough to cross.
    dense_result = dense.simulate_exact_output_swap(
        token_out=dense.token1,
        token_out_quantity=amount_out,
    )
    assert dense_result.final_state.tick < 0, "amount_out must cross tick 0 → -60"

    # Sparse pool with the async fetcher: must NOT raise and must match dense.
    sparse_result = sparse.simulate_exact_output_swap(
        token_out=sparse.token1,
        token_out_quantity=amount_out,
    )

    assert sparse_result.amount0_delta == dense_result.amount0_delta
    assert sparse_result.amount1_delta == dense_result.amount1_delta
    assert sparse_result.final_state.sqrt_price_x96 == dense_result.final_state.sqrt_price_x96
    assert sparse_result.final_state.liquidity == dense_result.final_state.liquidity
    assert sparse_result.final_state.tick == dense_result.final_state.tick

    # The async fetcher actually fetched the neighbour word (the crossing).
    assert -1 in io.bitmap_calls, "crossing into word -1 must trigger an async fetch"
    assert 0 in io.bitmap_calls, "the starting word (0) must also be fetched on miss"
