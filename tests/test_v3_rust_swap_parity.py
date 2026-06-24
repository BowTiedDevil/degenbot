"""ADR-005 sparse-map parity, slice 3b — companion routing parity gate.

Establishes that the Rust ``simulate_swap_with_fetch`` seam produces the SAME
outcome as the Python ``_v3_swap`` simulator on a DENSE pool (where the Rust
path never misses, so the fetcher is unused). This pins the sign convention
(Rust returns unsigned absolute amounts; the Python simulator returns signed
deltas) AND proves the full-outcome (final sqrt_price / liquidity / tick)
matches — the precondition for routing the companion's mainline swap to Rust.

The companion's mainline ``simulate_exact_input_swap`` (no override, no custom
``sqrt_price_limit_x96``) is then routed to the Rust seam on a SPARSE pool
(slice-3b deliverable): the missing starting word is fetched via a return-data
fetcher, merged, retried, and the result matches the dense-map oracle.
"""

from __future__ import annotations

import pytest

from degenbot.degenbot_rs import PyBot
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v3_pool_factory import make_v3_pool

# 1:1 price (sqrt_price_x96 = 2**96), tick 0, 0.3% fee, tick_spacing 60.
_SQRT_PRICE_1TO1 = 1 << 96
_LIQUIDITY = 10_000_000_000_000
_FEE = 3000
_TICK_SPACING = 60
_ADDRESS = "0x" + "11" * 20
_FACTORY = "0x" + "22" * 20


def _make_tokens(py_bot: PyBot, tag: str):
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
    py_bot: PyBot,
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


def test_rust_seam_matches_python_simulator_on_dense_pool():
    """Rust simulate_swap_with_fetch == Python _v3_swap (dense, no miss)."""
    py_bot = PyBot()
    pool = _build_v3_pool(py_bot, dense=True, address="0x" + "11" * 20)

    amount_in = 1_000

    # Python simulator path (dense else-branch of _calculate_swap).
    py_result = pool.simulate_exact_input_swap(
        token_in=pool.token0,
        token_in_quantity=amount_in,
    )

    # Rust seam path (dense → no miss → fetcher unused).
    fetcher_calls: list[tuple[int, int]] = []

    def fetcher(word: int, block: int):
        fetcher_calls.append((word, block))
        return {}

    rust_outcome = pool._py_pool.simulate_swap_with_fetch(
        zero_for_one=True,
        amount_in=amount_in,
        block=0,
        fetcher=fetcher,
    )

    assert rust_outcome is not None, "dense pool must not miss on the Rust path"
    assert fetcher_calls == [], "dense pool must not invoke the fetcher"

    rust_amount0, rust_amount1, rust_sqrt, rust_liq, rust_tick = (
        int(rust_outcome[0]),
        int(rust_outcome[1]),
        int(rust_outcome[2]),
        int(rust_outcome[3]),
        int(rust_outcome[4]),
    )

    # Amounts: Rust returns UNSIGNED absolute values; Python returns signed
    # deltas. For zfo exact-in: amount0 is deposited (+), amount1 is sent (-).
    assert py_result.amount0_delta > 0, "zfo exact-in deposits token0"
    assert py_result.amount1_delta < 0, "zfo exact-in sends token1"
    assert rust_amount0 == py_result.amount0_delta
    assert rust_amount1 == -py_result.amount1_delta

    # Final state must match exactly (the companion builds final_state from
    # this, so any divergence would corrupt arbitrage state propagation).
    assert rust_sqrt == py_result.final_state.sqrt_price_x96
    assert rust_liq == py_result.final_state.liquidity
    assert rust_tick == py_result.final_state.tick


@pytest.mark.parametrize("zero_for_one", [True, False])
def test_rust_seam_sign_mapping_dense(*, zero_for_one: bool):
    """Pin the Rust→Python sign mapping for both swap directions (dense)."""
    py_bot = PyBot()
    pool = _build_v3_pool(py_bot, dense=True, address="0x" + "11" * 20)
    amount_in = 1_000
    token_in = pool.token0 if zero_for_one else pool.token1

    py_result = pool.simulate_exact_input_swap(
        token_in=token_in,
        token_in_quantity=amount_in,
    )
    rust_outcome = pool._py_pool.simulate_swap_with_fetch(
        zero_for_one=zero_for_one,
        amount_in=amount_in,
        block=0,
        fetcher=lambda *_: {},
    )
    assert rust_outcome is not None
    rust_amount0, rust_amount1 = int(rust_outcome[0]), int(rust_outcome[1])

    if zero_for_one:
        # zfo exact-in: token0 deposited (+), token1 sent (-).
        assert py_result.amount0_delta > 0
        assert py_result.amount1_delta < 0
        assert rust_amount0 == py_result.amount0_delta
        assert rust_amount1 == -py_result.amount1_delta
    else:
        # ofz exact-in: token0 sent (-), token1 deposited (+).
        assert py_result.amount0_delta < 0
        assert py_result.amount1_delta > 0
        assert rust_amount0 == -py_result.amount0_delta
        assert rust_amount1 == py_result.amount1_delta


def test_sparse_mainline_swap_routed_to_rust_matches_dense_oracle():
    """Sparse pool's mainline swap → Rust fetch+retry == dense oracle.

    A sparse pool (empty tick_data) misses on the starting word; the return-data
    fetcher supplies the word's ticks; the Rust loop merges + retries; the
    result matches a dense pool (same position) computed via the Python
    simulator — i.e. the routed sparse path is correct end-to-end.
    """
    py_bot = PyBot()

    # Dense oracle: same position, Python simulator.
    dense = _build_v3_pool(py_bot, dense=True, address="0x" + "11" * 20)
    oracle = dense.simulate_exact_input_swap(
        token_in=dense.token0,
        token_in_quantity=1_000,
    )

    # Sparse pool: identical scalars + position, but NO tick_data seeded.
    # The pool's ``_tick_data_fetcher`` is the Rust-side callback the routing
    # invokes on a miss. The starting tick 0 lives in word 0; the zfo walk
    # descends to tick -60 which lives in word -1 (tick_spacing 60).
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


if __name__ == "__main__":
    pytest.main([__file__, "-vv"])
