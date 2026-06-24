"""ADR-005 sparse-map parity, slice 3b — V4 companion routing parity gate.

Establishes whether the Rust ``simulate_swap_with_fetch`` seam matches the
Python ``_v4_swap`` simulator on a DENSE V4 pool (no hooks, default fees). This
pins the V4 sign convention (Rust unsigned-absolute amounts vs the Python
simulator's signed ``amount0``/``amount1`` → ``SwapDelta`` amount_in/out) AND
verifies the Rust V4 sim honours the LP fee — the precondition for routing the
V4 companion's mainline swap to Rust.

V4 exact-input uses ``amount_specified < 0``; the simulator returns signed
deltas where the deposited currency is negative and the withdrawn currency is
positive, and ``SwapDelta`` exposes ``amount_in = -min(c0,c1)`` (deposited
abs) / ``amount_out = max(c0,c1)`` (withdrawn abs).
"""

from __future__ import annotations

# Lazily import Web3/eth_abi only when computing a pool_id.
import eth_abi.abi
import pytest
from web3 import Web3

from degenbot.constants import ZERO_ADDRESS
from degenbot.degenbot_rs import PyBot
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v4_pool_factory import make_v4_pool

# 1:1 price (sqrt_price_x96 = 2**96), tick 0, 0.3% fee, tick_spacing 60.
_SQRT_PRICE_1TO1 = 1 << 96
_LIQUIDITY = 10_000_000_000_000
_FEE = 3000
_TICK_SPACING = 60
_V4_POOL_MANAGER = "0x000000000004444c5dc75cB358380D2e3dE08A90"


def _compute_v4_pool_id(
    currency0: str,
    currency1: str,
    fee: int,
    tick_spacing: int,
    hooks: str,
) -> str:
    """Mirror UniswapV4Pool's pool_id derivation so the test pool validates."""
    return (
        "0x"
        + Web3.keccak(
            eth_abi.abi.encode(
                types=["address", "address", "uint24", "int24", "address"],
                args=[currency0, currency1, fee, tick_spacing, hooks],
            ),
        ).hex()
    )


def _build_dense_v4_pool(py_bot: PyBot, address_tag: str):
    """Build a dense V4 companion (1:1 price, position [-60, +60], no hooks)."""
    token0 = make_erc20(
        py_bot,
        address=f"0x{(address_tag * 40)[:40]}",
        name="T0",
        symbol="T0",
        decimals=18,
    )
    token1 = make_erc20(
        py_bot,
        address=f"0x{(address_tag * 39 + 'e')[:40]}",
        name="T1",
        symbol="T1",
        decimals=18,
    )
    pool_id = _compute_v4_pool_id(
        token0.address,
        token1.address,
        _FEE,
        _TICK_SPACING,
        ZERO_ADDRESS,
    )
    tick_data = {
        -60: LiquidityAtTick(
            liquidity_net=_LIQUIDITY,
            liquidity_gross=_LIQUIDITY,
            block=0,
        ),
        60: LiquidityAtTick(
            liquidity_net=-_LIQUIDITY,
            liquidity_gross=_LIQUIDITY,
            block=0,
        ),
    }
    return make_v4_pool(
        pool_id=pool_id,
        pool_manager_address=_V4_POOL_MANAGER,
        token0=token0,
        token1=token1,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        hook_address=None,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        tick_data=tick_data,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=_FEE,
        state_block=0,
        py_bot=py_bot,
    )


@pytest.mark.parametrize("zero_for_one", [True, False])
def test_rust_v4_seam_matches_python_simulator_dense(*, zero_for_one: bool):
    """Rust simulate_swap_with_fetch == Python _v4_swap (dense, no miss, no hooks).

    Pins the V4 sign mapping: ``calculate_tokens_out_from_tokens_in`` returns
    ``swap_delta.amount_out`` = ``max(currency0, currency1)`` = the withdrawn
    currency's absolute value, which is ``rust_amount1`` for zfo / ``rust_amount0``
    for ofz.
    """
    py_bot = PyBot()
    pool = _build_dense_v4_pool(py_bot, address_tag="a")

    amount_in = 1_000
    token_in = pool.token0 if zero_for_one else pool.token1

    # Python simulator path (dense else-branch of _calculate_swap).
    py_amount_out = pool.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )

    # Rust seam path. V4 exact-input is amount_specified < 0; the PyO3 seam
    # takes the (positive) amount_in and the Rust core negates it internally
    # (V4 sign convention) — mirror that here is unnecessary: the seam sign is
    # handled by the Rust v4_simulate_swap caller.
    rust_outcome = pool._py_pool.simulate_swap_with_fetch(
        zero_for_one=zero_for_one,
        amount_in=amount_in,
        block=0,
        fetcher=lambda *_: {},
    )
    assert rust_outcome is not None, "dense V4 pool must not miss on the Rust path"
    rust_amount0, rust_amount1 = int(rust_outcome[0]), int(rust_outcome[1])

    # swap_delta.amount_out = max(c0, c1) = the withdrawn currency's abs.
    # zfo: token1 withdrawn → rust_amount1. ofz: token0 withdrawn → rust_amount0.
    expected = rust_amount1 if zero_for_one else rust_amount0
    assert py_amount_out == expected, (
        f"V4 zfo={zero_for_one}: py_amount_out={py_amount_out} expected={expected}"
    )


# NOTE: a dense CROSSING-swap parity gate (large amount crossing an
# initialized tick) is NOT included because an offline 2-tick synthetic pool
# can't represent a partial multi-tick cross — any swap large enough to cross
# the position's boundary drains liquidity to 0 and walks into the next
# (un-seeded) word, raising MissingLiquidityData on the Python side (no
# fetcher). The Rust fetch-seam marks unknown words known-empty and completes,
# so the two paths aren't comparable on that fixture. The authoritative V4
# crossing-swap gate is the fork test `test_cached_calculations` (green with
# routing reverted; goes RED when V4 mainline routing is re-enabled — that
# flip is the signal to investigate the Rust V4 sim's multi-tick behavior
# before re-enabling routing in slice 4).


def test_sparse_mainline_v4_swap_fetch_merge_matches_dense_oracle():
    """Sparse V4 pool's mainline swap → fetch+merge == dense oracle.

    A sparse V4 pool (empty tick_data) misses on the starting word; the
    return-data fetcher supplies the word's ticks; the Python sparse loop's
    ``_apply_fetched_tick_word`` merges them and retries; the result matches a
    dense pool (same position) computed via the Python simulator. (The Rust
    mainline routing is deferred to slice 4 pending fork-validated crossing-swap
    parity — see `test_cached_calculations`; the fetch-callback return-data
    contract + V4 sparse-loop helper stay wired + validated here.)
    """
    py_bot = PyBot()

    # Dense oracle (Python simulator).
    dense = _build_dense_v4_pool(py_bot, address_tag="a")
    oracle = dense.calculate_tokens_out_from_tokens_in(
        token_in=dense.token0,
        token_in_quantity=1_000,
    )

    # Sparse pool: identical scalars + position, but NO tick_data seeded.
    fetched_words: list[int] = []

    def fetcher(word: int, block: int):
        fetched_words.append(word)
        if word == 0:
            # tick +60 lives in word 0; its liquidity_net is -L (upper bound).
            return {60: (_LIQUIDITY, -_LIQUIDITY, 0)}
        if word == -1:
            # tick -60 lives in word -1; its liquidity_net is +L (lower bound).
            return {-60: (_LIQUIDITY, _LIQUIDITY, 0)}
        return {}

    sparse = _build_dense_v4_pool(py_bot, address_tag="b")
    # Force sparseness: clear the seeded tick_data + attach the fetcher.
    sparse._py_pool.update_tick_data({}, {}, 0)
    sparse._tick_data_fetcher = fetcher
    sparse._sparse_liquidity_map = True
    assert sparse.sparse_liquidity_map, "cleared tick_data ⇒ sparse companion"

    result = sparse.calculate_tokens_out_from_tokens_in(
        token_in=sparse.token0,
        token_in_quantity=1_000,
    )

    # The fetch+merge path fetched the starting word (0).
    assert 0 in fetched_words, "the sparse swap must fetch the missing starting word"
    assert result == oracle, f"sparse fetch+merge result={result} != dense oracle={oracle}"


if __name__ == "__main__":
    pytest.main([__file__, "-vv"])
