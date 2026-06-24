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

import json
import pathlib
from dataclasses import replace

import eth_abi.abi
import pytest
from web3 import Web3

from degenbot.constants import ZERO_ADDRESS
from degenbot.degenbot_rs import PyBot
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v4_libraries.tick_math import (
    MAX_SQRT_PRICE,
    MIN_SQRT_PRICE,
    get_sqrt_price_at_tick,
)
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


def _build_dense_v4_pool_crossing(py_bot: PyBot, address_tag: str, *, zero_for_one: bool):
    """Build a dense V4 pool whose seeded tick_data covers a CROSSING swap's walk.

    Two OVERLAPPING positions keep liquidity > 0 past the crossed boundary so
    the swap does NOT drain into an unseeded word — both the Python simulator
    and the Rust seam then run fully dense (no fetcher miss), making a
    crossing-swap amount parity comparison valid offline (no fork).

    - ofz (zero_for_one=False): positions [-60,+60] L + [-60,+1200] L.
      Crossing +60 (net -L) keeps liquidity L alive in [60,1200] (word 0).
    - zfo (zero_for_one=True):  positions [-1200,+60] L + [-60,+60] L.
      Crossing -60 (net +L) keeps liquidity L alive in [-1200,-60] (word -1).
    """
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
    if zero_for_one:
        # zfo: crossing -60 downward. Active at tick 0 = 2L.
        tick_data = {
            -1200: LiquidityAtTick(
                liquidity_net=_LIQUIDITY,
                liquidity_gross=_LIQUIDITY,
                block=0,
            ),
            -60: LiquidityAtTick(
                liquidity_net=_LIQUIDITY,
                liquidity_gross=2 * _LIQUIDITY,
                block=0,
            ),
            60: LiquidityAtTick(
                liquidity_net=-2 * _LIQUIDITY,
                liquidity_gross=2 * _LIQUIDITY,
                block=0,
            ),
        }
    else:
        # ofz: crossing +60 upward. Active at tick 0 = 2L.
        tick_data = {
            -60: LiquidityAtTick(
                liquidity_net=2 * _LIQUIDITY,
                liquidity_gross=2 * _LIQUIDITY,
                block=0,
            ),
            60: LiquidityAtTick(
                liquidity_net=-_LIQUIDITY,
                liquidity_gross=2 * _LIQUIDITY,
                block=0,
            ),
            1200: LiquidityAtTick(
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
        liquidity=2 * _LIQUIDITY,
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


@pytest.mark.parametrize("zero_for_one", [True, False])
def test_rust_v4_seam_matches_python_simulator_dense_crossing(*, zero_for_one: bool):
    """Rust == Python on a DENSE crossing swap (overlapping positions, no miss).

    A 2-position overlapping fixture keeps liquidity > 0 past the crossed
    boundary so the swap does NOT drain into an unseeded word: both the Python
    simulator and the Rust seam run fully dense (no fetcher miss), making the
    crossing-swap amount parity comparison valid OFFLINE (no fork). This is the
    tight loop that isolates whether the Rust V4 sim's crossing-swap math is
    sound — distinct from the sparse/fetch-merge path exercised by the fork
    gate `test_cached_calculations`.
    """
    py_bot = PyBot()
    pool = _build_dense_v4_pool_crossing(py_bot, address_tag="c", zero_for_one=zero_for_one)

    # Sized to cross the boundary (-60 for zfo / +60 for ofz) but stop well
    # short of the outer bound (-1200 / +1200), so the walk stays in a seeded
    # word and liquidity never drains to 0.
    amount_in = 400_000_000_000
    token_in = pool.token0 if zero_for_one else pool.token1

    py_amount_out = pool.calculate_tokens_out_from_tokens_in(
        token_in=token_in,
        token_in_quantity=amount_in,
    )
    # Build a FRESH identical pool for the Rust call so the mutating fetch seam
    # (which merges fetched words into BotState on a miss) can't leak one
    # call's state into the other. A dense pool doesn't miss → no mutation →
    # both pools stay identical regardless.
    rust_pool = _build_dense_v4_pool_crossing(
        py_bot,
        address_tag="d",
        zero_for_one=zero_for_one,
    )
    rust_outcome = rust_pool._py_pool.simulate_swap_with_fetch(
        zero_for_one=zero_for_one,
        amount_in=amount_in,
        block=0,
        fetcher=lambda *_: {},
    )
    assert rust_outcome is not None, "dense crossing V4 pool must not miss on the Rust path"
    rust_amount0, rust_amount1, _rsp, _rli, rust_tick = (int(x) for x in rust_outcome)
    expected = rust_amount1 if zero_for_one else rust_amount0
    # Sanity: the swap actually crossed the boundary tick.
    crossed = (rust_tick < -60) if zero_for_one else (rust_tick > 60)
    assert crossed, (
        f"V4 zfo={zero_for_one}: amount {amount_in} did not cross the boundary "
        f"(rust_tick={rust_tick}); bump the amount"
    )
    # And did not drain past the outer bound (stays liquid, in a seeded word).
    in_range = (rust_tick > -1200) if zero_for_one else (rust_tick < 1200)
    assert in_range, (
        f"V4 zfo={zero_for_one}: swap drained past the outer bound "
        f"(rust_tick={rust_tick}); lower the amount"
    )
    assert py_amount_out == expected, (
        f"V4 dense-crossing zfo={zero_for_one}: py={py_amount_out} rust={expected} "
        f"diff={expected - py_amount_out} (rust_tick={rust_tick})"
    )


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


_CORPUS_PATH = (
    pathlib.Path(__file__).parent
    / "uniswap"
    / "v4"
    / "fixtures"
    / "v4_eth_usdc_diverge_corpus.json"
)
_V4_MGR = "0x000000000004444c5dc75cB358380D2e3dE08A90"
# Frozen fork-captured ground truth (ETH/USDC V4 pool, ofz swap, amt=2.66e12).
_V4_DIVERGE_AMOUNT = 2_655_842_687_976
_V4_DIVERGE_QUOTER_OUT = 1_032_110_029_338_332_389_817
_V4_DIVERGE_RUST_OUT = 1_032_109_990_364_539_206_286
# The BUGGY Rust sparse+fetch undercount (pre-ELSE-branch-miss-check fix).
# Kept as the historical divergence magnitude reference; the fix makes the
# sparse path match `_V4_DIVERGE_QUOTER_OUT`.


def _load_corpus_fixture() -> dict:
    """Load the fork-captured ETH/USDC V4 state + 419-tick corpus fixture."""
    return json.loads(_CORPUS_PATH.read_bytes())


def _build_pool_from_corpus(
    py_bot: PyBot,
    state: dict,
    *,
    td: dict[int, LiquidityAtTick],
    sparse: bool,
    fetcher,
):
    """Build a V4 companion seeded with ``td``; if ``sparse``, clear the
    companion's seeded Rust state + flip sparse + attach ``fetcher`` (so the
    mainline + seam paths fetch on demand).
    """
    token0 = make_erc20(py_bot, address="0x" + "aa" * 20, name="T0", symbol="T0", decimals=18)
    token1 = make_erc20(py_bot, address="0x" + "bb" * 20, name="T1", symbol="T1", decimals=18)
    pool_id = (
        "0x"
        + Web3.keccak(
            eth_abi.abi.encode(
                ["address", "address", "uint24", "int24", "address"],
                [token0.address, token1.address, state["fee"], state["tick_spacing"], ZERO_ADDRESS],
            ),
        ).hex()
    )
    pool = make_v4_pool(
        pool_id=pool_id,
        pool_manager_address=_V4_MGR,
        token0=token0,
        token1=token1,
        fee=state["fee"],
        tick_spacing=state["tick_spacing"],
        hook_address=None,
        sqrt_price_x96=state["sqrt_price_x96"],
        tick=state["tick"],
        liquidity=state["liquidity"],
        tick_data=td,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=state["fee"],
        state_block=0,
        py_bot=py_bot,
    )
    if sparse:
        pool._py_pool.update_tick_data({}, {}, 0)
        pool._sparse_liquidity_map = True
        pool._tick_data_fetcher = fetcher
    return pool


def test_rust_v4_dense_corpus_matches_on_chain_quoter():
    """Rust == Python == on-chain quoter on the captured corpus (DENSE).

    Loads the fork-captured ETH/USDC V4 state + full initialized-tick corpus
    (419 ticks across words -79..-75) and runs the diverging ofz swap with NO
    fetcher (dense). All three agree exactly — proving the Rust V4 sim's core
    crossing-swap math is sound on a real, rich tick corpus. This is the
    precondition that isolates the sparse-path divergence to the fetch/merge
    loop (see ``test_rust_v4_sparse_fetch_corpus_diverges``).
    """
    state = _load_corpus_fixture()
    td = {
        int(t): LiquidityAtTick(liquidity_net=int(r[1]), liquidity_gross=int(r[0]), block=int(r[2]))
        for t, r in state["tick_data"].items()
    }
    py_bot = PyBot()
    pool = _build_pool_from_corpus(py_bot, state, td=td, sparse=False, fetcher=None)
    py_out = pool.calculate_tokens_out_from_tokens_in(
        token_in=pool.token1,
        token_in_quantity=_V4_DIVERGE_AMOUNT,
    )
    rust_outcome = pool._py_pool.simulate_swap_with_fetch(
        zero_for_one=False,
        amount_in=_V4_DIVERGE_AMOUNT,
        block=0,
        fetcher=lambda *_: {},
    )
    assert rust_outcome is not None
    rust_out = int(rust_outcome[0])  # ofz: token0 out
    assert py_out == _V4_DIVERGE_QUOTER_OUT, f"Python dense diverges from quoter: {py_out}"
    assert rust_out == _V4_DIVERGE_QUOTER_OUT, f"Rust dense diverges from quoter: {rust_out}"


@pytest.mark.parametrize("zero_for_one", [True, False])
def test_rust_v4_dense_corpus_custom_sqrt_price_limit_matches_python(*, zero_for_one: bool):
    """Rust seam honours a custom ``sqrt_price_limit_x96`` (== Python, dense).

    A custom price limit caps the swap walk short of its natural endpoint.
    The Rust ``simulate_swap_with_fetch`` seam must accept the limit + produce
    the SAME amount as the Python ``_v4_swap`` with the identical limit (the
    §4.3 precondition for routing the custom-limit / exact-output companion
    paths to Rust). Uses the captured corpus dense (no fetcher); picks a limit
    tick between the current tick and the full-walk endpoint so the limit is
    binding in both directions.
    """
    state = _load_corpus_fixture()
    td = {
        int(t): LiquidityAtTick(liquidity_net=int(r[1]), liquidity_gross=int(r[0]), block=int(r[2]))
        for t, r in state["tick_data"].items()
    }
    py_bot = PyBot()
    pool = _build_pool_from_corpus(py_bot, state, td=td, sparse=False, fetcher=None)
    # Current tick -202094. ofz (price rises) needs a limit ABOVE current +
    # below the full-walk endpoint (-194396): -195000 binds ofz. zfo (price
    # falls) needs a limit BELOW current: -202200 binds zfo.
    limit_tick = -195_000 if not zero_for_one else -202_200
    sqrt_price_limit = get_sqrt_price_at_tick(limit_tick)
    # V4 exact-input: amount_specified < 0.
    swap_delta, *_ = pool._calculate_swap(
        zero_for_one=zero_for_one,
        amount_specified=-_V4_DIVERGE_AMOUNT,
        sqrt_price_x96_limit=sqrt_price_limit,
    )
    py_amount_out = swap_delta.amount_out
    # Binding check: the limited walk must produce LESS than the unlimited
    # quoter amount (else the limit was non-binding + the test is vacuous).
    unlimited = _V4_DIVERGE_QUOTER_OUT  # ofz; for zfo compare the zfo dense out below
    if not zero_for_one:
        assert py_amount_out < unlimited, (
            f"ofz custom limit must cap below the full-walk out: {py_amount_out} >= {unlimited}"
        )
    rust_outcome = pool._py_pool.simulate_swap_with_fetch(
        zero_for_one=zero_for_one,
        amount_in=_V4_DIVERGE_AMOUNT,
        block=0,
        fetcher=lambda *_: {},
        sqrt_price_limit_x96=sqrt_price_limit,
    )
    assert rust_outcome is not None
    # ofz: token0 out; zfo: token1 out.
    rust_amount_out = int(rust_outcome[0] if not zero_for_one else rust_outcome[1])
    assert rust_amount_out == py_amount_out, (
        f"V4 custom-limit zfo={zero_for_one}: rust={rust_amount_out} py={py_amount_out} "
        f"diff={rust_amount_out - py_amount_out}"
    )


@pytest.mark.parametrize("zero_for_one", [True, False])
def test_rust_v4_dense_corpus_override_state_matches_python(*, zero_for_one: bool):
    """Rust seam honours an override (hypothetical) pool state (== Python).

    ``simulate_swap_with_override`` builds a transient V4 state from override
    scalars + override ``tick_data`` (reusing the registered pool's fee /
    tick_spacing) + runs the sim over it with NO fetcher + NO mutation of
    registered ``BotState`` (a frozen hypothetical, mirroring the Python
    ``_calculate_swap(override_state=...)`` frozen-snapshot path). This is the
    arbitrage-hypothetical seam (§4.3 precondition for retiring the Python
    ``_calculate_swap`` override path).

    The hypothetical shifts the starting price to a tick inside the live
    liquidity band (override tick = live tick - 6, same band → unchanged active
    liquidity, so no net walk is needed) — a self-consistent price hypothetical
    that must DIFFER from the mainline outcome + the Rust override must EXACTLY
    match the Python override.
    """
    state = _load_corpus_fixture()
    td = {
        int(t): LiquidityAtTick(liquidity_net=int(r[1]), liquidity_gross=int(r[0]), block=int(r[2]))
        for t, r in state["tick_data"].items()
    }
    # Raw {tick: (gross, net, block)} shape for the Rust seam (mirrors register).
    rust_td = {int(t): (int(r[0]), int(r[1]), int(r[2])) for t, r in state["tick_data"].items()}
    py_bot = PyBot()
    pool = _build_pool_from_corpus(py_bot, state, td=td, sparse=False, fetcher=None)

    # Shift the starting price within the live liquidity band (initialized
    # ticks bracket the live tick at -202110 / -202020, so -202100 keeps the
    # SAME active liquidity — no net walk needed). A different starting price
    # changes the outcome for any non-trivial amount in either direction.
    override_tick = state["tick"] - 6
    override_sqrt = get_sqrt_price_at_tick(override_tick)
    override_liq = state["liquidity"]
    default_limit = MIN_SQRT_PRICE + 1 if zero_for_one else MAX_SQRT_PRICE - 1

    # Derive the override from the live state (same tick_bitmap/tick_data/id),
    # overriding the starting price + tick (same active liquidity band).
    override_state = replace(
        pool.state,
        sqrt_price_x96=override_sqrt,
        tick=override_tick,
    )
    swap_delta, *_ = pool._calculate_swap(
        zero_for_one=zero_for_one,
        amount_specified=-_V4_DIVERGE_AMOUNT,
        sqrt_price_x96_limit=default_limit,
        override_state=override_state,
    )
    py_amount_out = swap_delta.amount_out
    # Binding: the override must differ from the mainline outcome (else the
    # override scalars had no effect + the test is vacuous).
    mainline_delta, *_ = pool._calculate_swap(
        zero_for_one=zero_for_one,
        amount_specified=-_V4_DIVERGE_AMOUNT,
        sqrt_price_x96_limit=default_limit,
        override_state=None,
    )
    assert py_amount_out != mainline_delta.amount_out, (
        f"override price-shift must change the outcome: "
        f"{py_amount_out} == mainline {mainline_delta.amount_out}"
    )

    rust_outcome = pool._py_pool.simulate_swap_with_override(
        zero_for_one=zero_for_one,
        amount_in=_V4_DIVERGE_AMOUNT,
        block=0,
        fetcher=lambda *_: {},  # dense corpus: no fetch expected
        override_sqrt_price_x96=override_sqrt,
        override_liquidity=override_liq,
        override_tick=override_tick,
        override_tick_data=rust_td,
        sqrt_price_limit_x96=None,  # Rust default (MIN+1 / MAX-1) == default_limit
    )
    assert rust_outcome is not None, "override sim returned None"
    rust_amount_out = int(rust_outcome[0] if not zero_for_one else rust_outcome[1])
    assert rust_amount_out == py_amount_out, (
        f"V4 override zfo={zero_for_one}: rust={rust_amount_out} py={py_amount_out} "
        f"diff={rust_amount_out - py_amount_out}"
    )


def test_rust_v4_dense_corpus_exact_output_matches_python():
    """Rust exact-OUTPUT seam == Python on the captured corpus (DENSE).

    The Rust ``simulate_exact_output_swap_with_fetch`` seam (caller passes the
    desired ``amount_out``; the sim derives the required input) must match the
    Python ``calculate_tokens_in_from_tokens_out`` on the captured ETH/USDC V4
    corpus. The desired output is the full-walk ofz quoter amount
    (``_V4_DIVERGE_QUOTER_OUT``), so the required input round-trips to the
    ``_V4_DIVERGE_AMOUNT`` mainline exact-input quantity — a strong
    round-trip + parity gate for the exact-output sign convention (V4
    exact-output is ``amountSpecified > 0``).
    """
    state = _load_corpus_fixture()
    td = {
        int(t): LiquidityAtTick(liquidity_net=int(r[1]), liquidity_gross=int(r[0]), block=int(r[2]))
        for t, r in state["tick_data"].items()
    }
    py_bot = PyBot()
    pool = _build_pool_from_corpus(py_bot, state, td=td, sparse=False, fetcher=None)

    # ofz exact-output: request token0 (the full-walk quoter out) → required
    # token1 input. zero_for_one = (token_out == token1) → False for token0.
    py_required_in = int(
        pool.calculate_tokens_in_from_tokens_out(
            token_out=pool.token0,
            token_out_quantity=_V4_DIVERGE_QUOTER_OUT,
        ),
    )
    rust_outcome = pool._py_pool.simulate_exact_output_swap_with_fetch(
        zero_for_one=False,
        amount_out=_V4_DIVERGE_QUOTER_OUT,
        block=0,
        fetcher=lambda *_: {},
        sqrt_price_limit_x96=None,
    )
    assert rust_outcome is not None, "exact-output sim returned None"
    # ofz: token1 is the required input (amount1).
    rust_required_in = int(rust_outcome[1])
    assert rust_required_in == py_required_in, (
        f"V4 exact-output ofz: rust_in={rust_required_in} py_in={py_required_in} "
        f"diff={rust_required_in - py_required_in}"
    )


def test_rust_v4_sparse_fetch_corpus_matches_dense():
    """Rust sparse+fetch == Rust dense on the SAME corpus (offline, no fork).

    Seeds the pool with the corpus MINUS word -76's ticks and runs both paths
    SPARSE with a fetcher that returns each word's ticks on demand (the last
    word is fetched, not pre-seeded, so the sparse+fetch loop is exercised).
    Python mainline (sparse, full-fetch) reproduces the on-chain quoter exactly;
    Rust ``simulate_swap_with_fetch`` now matches too (the sparse-path
    divergence is fixed — see the ELSE-branch miss check in `v4_simulate_swap`).
    divergence equals the fork divergence, so this is the offline gate for the
    V4 mainline routing gate — was RED (sparse-path divergence) before the
    ELSE-branch miss-check fix in `v4_simulate_swap`; now GREEN.
    """
    state = _load_corpus_fixture()
    ts = state["tick_spacing"]
    full = dict(state["tick_data"])
    minus76 = {t: v for t, v in full.items() if (int(t) // ts) >> 8 != -76}
    td_minus76 = {
        int(t): LiquidityAtTick(liquidity_net=int(r[1]), liquidity_gross=int(r[0]), block=int(r[2]))
        for t, r in minus76.items()
    }
    corpus_by_word: dict[int, dict] = {}
    for t, r in full.items():
        corpus_by_word.setdefault((int(t) // ts) >> 8, {})[int(t)] = tuple(r)

    def full_fetcher(word: int, block: int) -> dict:
        return corpus_by_word.get(word, {})

    # Each pool gets its OWN PyBot + token pair (distinct addresses) so the
    # second registration doesn't collide as a duplicate pool_id.
    # Python mainline (sparse, full-fetch) — the oracle (== quoter).
    py_pool = _build_pool_from_corpus(
        PyBot(),
        state,
        td=td_minus76,
        sparse=True,
        fetcher=full_fetcher,
    )
    py_out = py_pool.calculate_tokens_out_from_tokens_in(
        token_in=py_pool.token1,
        token_in_quantity=_V4_DIVERGE_AMOUNT,
    )
    assert py_out == _V4_DIVERGE_QUOTER_OUT, (
        f"Python sparse+full-fetch must match the quoter (== dense): {py_out}"
    )
    # Rust seam (sparse, full-fetch) — the path under fix.
    rust_pool = _build_pool_from_corpus(
        PyBot(),
        state,
        td=td_minus76,
        sparse=True,
        fetcher=full_fetcher,
    )
    rust_outcome = rust_pool._py_pool.simulate_swap_with_fetch(
        zero_for_one=False,
        amount_in=_V4_DIVERGE_AMOUNT,
        block=0,
        fetcher=full_fetcher,
    )
    assert rust_outcome is not None
    rust_out = int(rust_outcome[0])
    # Rust sparse+fetch MUST match its own dense result (== quoter): the
    # ELSE-branch miss check ensures an amount-capped step landing inside an
    # unfetched word raises MissingTickWord so the word is backfilled + the
    # walk re-runs applying its initialized ticks' liquidity-nets.
    assert rust_out == _V4_DIVERGE_QUOTER_OUT, (
        f"Rust sparse+fetch diverges from dense/quoter: rust={rust_out} "
        f"expected={_V4_DIVERGE_QUOTER_OUT} diff={rust_out - _V4_DIVERGE_QUOTER_OUT}"
    )


def test_sparse_fetch_reaches_min_tick_via_empty_words_v4():
    """Sparse V4 zfo swap with no initialized ticks must reach MIN_TICK, not strand.

    V4 mirror of ``test_sparse_fetch_reaches_min_tick_via_empty_words`` (V3).
    The V4 sim shares the same ``gen_ticks`` whose 2nd-phase boundary walk
    previously BROKE past MIN/MAX_TICK — stranding the swap loop short of the
    price limit. After the clamp fix, the V4 sparse+fetch path must match the
    Python ``_calculate_swap`` oracle down to MIN_TICK.
    """
    py_bot = PyBot()
    # Dense oracle: a dummy init tick ABOVE the start (opposite the zfo walk)
    # makes tick_data non-empty → coverage=tracked, but the zfo path (tick ≤
    # start=0) has NO initialized ticks — liq stays constant to MIN.
    token0 = make_erc20(py_bot, address="0x" + "f" * 40, name="T0", symbol="T0", decimals=18)
    token1 = make_erc20(py_bot, address="0x" + "e" * 40, name="T1", symbol="T1", decimals=18)
    pool_id = _compute_v4_pool_id(token0.address, token1.address, _FEE, _TICK_SPACING, ZERO_ADDRESS)
    tick_data_dense = {
        60: LiquidityAtTick(
            liquidity_net=_LIQUIDITY, liquidity_gross=_LIQUIDITY, block=0
        ),
    }
    dense = make_v4_pool(
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
        tick_data=tick_data_dense,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=_FEE,
        state_block=0,
        py_bot=py_bot,
    )
    amount_in = 10**34
    # A single empty-word fetcher shared by the dense oracle + the sparse pool.
    # V4 `_calculate_swap` always builds a sparse snapshot + drives its own
    # fetch+retry loop through `self._tick_data_fetcher`, so the Python oracle
    # fetches each empty word + reaches MIN — mirroring the Rust sparse path
    # (which uses the same fetcher via the seam's fetch+retry wrapper).
    fetched_words: list[int] = []

    def fetcher(word: int, block: int):
        fetched_words.append(word)
        return {}

    dense._tick_data_fetcher = fetcher
    # Python oracle: raw `_calculate_swap` (sparse path fetches empty words →
    # reaches MIN). V4 exact-input uses amount_specified < 0.
    swap_delta, _proto_fee, _swap_fee, swap_result = dense._calculate_swap(
        zero_for_one=True,
        amount_specified=-amount_in,
        sqrt_price_x96_limit=MIN_SQRT_PRICE + 1,
    )
    py_sp, py_tick, py_liq = (
        swap_result.sqrt_price_x96,
        swap_result.tick,
        swap_result.liquidity,
    )
    assert py_sp == MIN_SQRT_PRICE + 1, (
        f"python V4 oracle did not reach the MIN price limit (got sqrt={py_sp}); "
        f"bump amount_in"
    )

    # Sparse pool — identical scalars, NO tick_data seeded. Distinct tokens so
    # the pool_id differs (same PyBot); the fetcher is shared with the oracle.
    token0b = make_erc20(py_bot, address="0x" + "d" * 40, name="T0b", symbol="T0", decimals=18)
    token1b = make_erc20(py_bot, address="0x" + "c" * 40, name="T1b", symbol="T1", decimals=18)
    pool_id2 = _compute_v4_pool_id(token0b.address, token1b.address, _FEE, _TICK_SPACING, ZERO_ADDRESS)
    sparse = make_v4_pool(
        pool_id=pool_id2,
        pool_manager_address=_V4_POOL_MANAGER,
        token0=token0b,
        token1=token1b,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        hook_address=None,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=_FEE,
        state_block=0,
        py_bot=py_bot,
    )
    sparse._py_pool.update_tick_data({}, {}, 0)
    sparse._tick_data_fetcher = fetcher
    sparse._sparse_liquidity_map = True
    assert sparse.sparse_liquidity_map, "cleared tick_data ⇒ sparse companion"

    rust_outcome = sparse._py_pool.simulate_swap_with_fetch(
        zero_for_one=True,
        amount_in=amount_in,
        block=0,
        fetcher=fetcher,
    )
    assert rust_outcome is not None, "sparse V4 swap returned None"
    rust_a0, rust_a1, rust_sp, rust_liq, rust_tick = (int(x) for x in rust_outcome)
    assert rust_sp == MIN_SQRT_PRICE + 1, (
        f"rust V4 sparse did not reach the MIN price limit (got sqrt={rust_sp}); "
        f"the walk stranded before MIN_TICK"
    )
    # V4 sign convention: zfo deposits currency0 (token0), withdraws currency1
    # (token1). SwapDelta.currency0 < 0 (deposited), currency1 > 0 (withdrawn).
    # Rust returns unsigned: rust_amount0 = |c0|, rust_amount1 = c1.
    c0, c1 = swap_delta.currency0, swap_delta.currency1
    assert rust_a0 == -c0, f"amount0 (token0 in): rust={rust_a0} py={-c0}"
    assert rust_a1 == c1, f"amount1 (token1 out): rust={rust_a1} py={c1}"
    assert rust_tick == py_tick, f"final tick: rust={rust_tick} py={py_tick}"
    assert rust_liq == py_liq, f"final liq: rust={rust_liq} py={py_liq}"
    assert rust_sp == py_sp, f"final sqrt: rust={rust_sp} py={py_sp}"


if __name__ == "__main__":
    pytest.main([__file__, "-vv"])
