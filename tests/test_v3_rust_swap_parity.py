"""ADR-005 sparse-map parity — Rust sparse+fetch vs dense-Rust oracle.

After the §4.3 oracle retirement (the Python ``_v3_swap`` simulator +
``_calculate_swap`` are deleted), the durable regression set for the Rust
V3 sim is: the Rust ``#[cfg(test)]`` corpus (``tick_bitmap.rs`` gen_ticks
clamping incl. the MIN/MAX strand-prevention assertions) + the on-chain-quoter
fork gate ``test_cached_calculations``. These offline gates cover the
sparse+fetch path against a DENSE-Rust oracle (full tick_data, no miss) —
proving the sparse fetch+retry walk matches the dense reference, including
the word-boundary walk down to MIN_TICK (the strand fix).
"""

from __future__ import annotations

import pytest

from degenbot._ffi import Bot
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v3_libraries import MIN_SQRT_RATIO
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool

# 1:1 price (sqrt_price_x96 = 2**96), tick 0, 0.3% fee, tick_spacing 60.
_SQRT_PRICE_1TO1 = 1 << 96
_LIQUIDITY = 10_000_000_000_000
_FEE = 3000
_TICK_SPACING = 60
_ADDRESS = "0x" + "11" * 20
_FACTORY = "0x" + "22" * 20


def _make_tokens(py_bot: Bot, tag: str):
    """Build a fresh token pair in the given Bot (distinct hex addresses)."""
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


def _build_v3_pool(
    py_bot: Bot,
    *,
    dense: bool,
    address: str,
    fetcher: object = None,
):
    """Build a V3 companion (1:1 price, position [-60, +60]).

    ``dense=True`` seeds the position's ticks into Rust (non-sparse); ``dense=False``
    leaves tick_data empty (sparse — the fetcher backfills on demand).
    """
    token0, token1 = _make_tokens(py_bot, tag="a" if dense else "b")
    tick_data = None
    if dense:
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
        tick_data_fetcher=fetcher,
    )


def test_sparse_mainline_swap_routed_to_rust_matches_dense_oracle():
    """Sparse pool's mainline swap → Rust fetch+retry == dense-Rust oracle.

    A sparse pool (empty tick_data) misses on the starting word; the return-data
    fetcher supplies the word's ticks; the Rust loop merges + retries; the
    result matches a dense pool (same position) computed via the dense Rust
    seam — i.e. the routed sparse path is correct end-to-end.
    """
    py_bot = Bot()

    # Dense oracle: same position, dense Rust seam (full tick_data → no miss).
    dense = _build_v3_pool(py_bot, dense=True, address="0x" + "11" * 20)
    oracle = dense.simulate_exact_input_swap(
        token_in=dense.token0,
        token_in_quantity=1_000,
    )

    # Sparse pool: identical scalars + position, but NO tick_data seeded.
    # The pool's sparse-word fetcher is the RUST-stored callback the routing
    # invokes on a miss (T2 FBJTUM: the companion-side fetcher is retired).
    # The starting tick 0 lives in word 0; the zfo walk descends to tick -60
    # which lives in word -1 (tick_spacing 60).
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

    sparse = _build_v3_pool(
        py_bot,
        dense=False,
        address="0x" + "22" * 20,
        fetcher=fetcher,
    )
    assert sparse.sparse_liquidity_map, "no tick_data ⇒ sparse companion"

    # Mainline (no override, no custom limit) → routed to the Rust seam.
    result = sparse.simulate_exact_input_swap(
        token_in=sparse.token0,
        token_in_quantity=1_000,
    )

    # The Rust path fetched the starting word (0) and the descending word (-1).
    assert 0 in fetched_words, "the sparse swap must fetch the missing starting word"

    # The routed result matches the dense-map oracle (amounts + final state).
    assert result.amount1_delta == oracle.amount1_delta
    assert result.amount0_delta == oracle.amount0_delta
    assert result.final_state.sqrt_price_x96 == oracle.final_state.sqrt_price_x96
    assert result.final_state.liquidity == oracle.final_state.liquidity
    assert result.final_state.tick == oracle.final_state.tick


def test_sparse_fetch_reaches_min_tick_via_empty_words():
    """Sparse zfo swap with no initialized ticks must reach MIN_TICK, not strand.

    Rust's ``gen_ticks`` 2nd-phase boundary walk previously BROKE past
    MIN_TICK/MAX_TICK, exhausting the tick list while
    ``amount_specified_remaining`` was still positive + the price-limit
    unreached — the walk stranded in the last fully-fitting word. The clamp
    fix yields the MIN/MAX boundary tick (matched by the Rust
    ``#[cfg(test)]`` gen_ticks clamping assertions in ``tick_bitmap.rs``).
    This gate is the end-to-end sparse+fetch coverage: a no-init-tick zfo
    swap through empty words must drive the price down to MIN, fetching each
    empty word, matching the dense-Rust oracle (full tick_data, no miss).
    """
    py_bot = Bot()
    token0, token1 = _make_tokens(py_bot, tag="e")
    # Dense oracle: a single dummy init tick ABOVE the start (opposite the zfo
    # walk direction) makes tick_data non-empty → coverage=tracked, but the zfo
    # path (tick ≤ start) has NO initialized ticks — liq stays constant all the
    # way to MIN.
    tick_data_dense = {
        60: LiquidityAtTick(liquidity_net=_LIQUIDITY, liquidity_gross=_LIQUIDITY, block=0),
    }
    dense = make_v3_pool(
        address="0x" + "66" * 20,
        token0=token0,
        token1=token1,
        factory=_FACTORY,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        tick_data=tick_data_dense,
        py_bot=py_bot,
    )
    # Huge amount: drives the zfo price from 2**96 down to the MIN limit.
    amount_in = 10**34
    # Dense-Rust oracle: the dense pool (full tick_data → no miss) computed
    # via the Rust seam. gen_ticks (now clamped at MIN_TICK) drives the walk
    # to the price limit; the dense path never exercises sparse+fetch, so it
    # is a valid reference for the sparse fetch+retry path below.
    oracle_outcome = dense._py_pool.simulate_swap_with_fetch(
        zero_for_one=True,  # zfo (token0 in → token1 out, descending → MIN)
        amount_in=amount_in,
        block=0,
    )
    assert oracle_outcome is not None, "dense oracle must not miss"
    py_a0, py_a1, py_sp, py_liq, py_tick = (int(x) for x in oracle_outcome)
    assert py_sp == MIN_SQRT_RATIO + 1, (
        f"dense oracle did not reach the MIN price limit (got sqrt={py_sp}); bump amount_in"
    )

    # Sparse pool — identical scalars, NO tick_data seeded. The fetcher returns
    # {} for every word (no initialized ticks anywhere); Rust marks each
    # fetched-empty word known + continues, but must reach MIN (not strand at
    # the last word above MIN_TICK).
    fetched_words: list[int] = []

    def fetcher(word: int, block: int):
        fetched_words.append(word)
        return {}

    sparse = make_v3_pool(
        address="0x" + "77" * 20,
        token0=token0,
        token1=token1,
        factory=_FACTORY,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        py_bot=py_bot,
        tick_data_fetcher=fetcher,
    )
    assert sparse.sparse_liquidity_map, "no tick_data ⇒ sparse companion"

    result = sparse._py_pool.simulate_swap_with_fetch(
        zero_for_one=True,
        amount_in=amount_in,
        block=0,
    )
    assert result is not None, "sparse swap returned None"
    rust_a0, rust_a1, rust_sp, rust_liq, rust_tick = (int(x) for x in result)
    assert rust_sp == MIN_SQRT_RATIO + 1, (
        f"rust sparse did not reach the MIN price limit (got sqrt={rust_sp}); "
        f"the walk stranded before MIN_TICK"
    )
    # Full parity with the dense-Rust oracle (amounts + final state). Both
    # paths return UNSIGNED absolute amounts (zfo: amount0 = input consumed,
    # amount1 = output sent).
    assert rust_a0 == py_a0, f"amount0: rust={rust_a0} oracle={py_a0}"
    assert rust_a1 == py_a1, f"amount1: rust={rust_a1} oracle={py_a1}"
    assert rust_tick == py_tick, f"final tick: rust={rust_tick} oracle={py_tick}"
    assert rust_liq == py_liq, f"final liq: rust={rust_liq} oracle={py_liq}"
    assert rust_sp == py_sp, f"final sqrt: rust={rust_sp} oracle={py_sp}"


if __name__ == "__main__":
    pytest.main([__file__, "-vv"])
