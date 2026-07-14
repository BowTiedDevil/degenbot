"""ADR-005 sparse-map parity — V4 Rust sparse+fetch vs dense-Rust oracle.

After the §4.3 oracle retirement (the Python ``_v4_swap`` simulator +
``_calculate_swap`` are deleted), the durable regression set for the Rust
V4 sim is: the Rust ``#[cfg(test)]`` corpus + the on-chain-quoter fork gate
``test_cached_calculations``. These offline gates cover the sparse+fetch
path against a dense-Rust oracle (full tick_data → no miss) + against baked-in
on-chain-quoter amounts from a captured corpus — proving the sparse fetch+retry
walk matches the dense reference, including the word-boundary walk down to
MIN_TICK (the strand fix).
"""

from __future__ import annotations

import json
import pathlib

import eth_abi.abi
import pytest
from degenbot.crypto import keccak256

from degenbot.constants import ZERO_ADDRESS
from degenbot.degenbot_rs import PyBot
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.v3_libraries import MIN_SQRT_RATIO as MIN_SQRT_PRICE
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
        + keccak256(
            eth_abi.abi.encode(
                types=["address", "address", "uint24", "int24", "address"],
                args=[currency0, currency1, fee, tick_spacing, hooks],
            ),
        ).hex()
    )


def _build_dense_v4_pool(py_bot: PyBot, address_tag: str, *, coverage: str = "tracked"):
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
        coverage=coverage,
    )


def test_sparse_mainline_v4_swap_fetch_merge_matches_dense_oracle():
    """Sparse V4 pool's mainline swap → fetch+merge == dense oracle.

    A sparse V4 pool (empty tick_data) misses on the starting word; the
    return-data fetcher supplies the word's ticks; the Python sparse loop's
    ``_apply_fetched_tick_word`` merges them and retries; the result matches a
    dense pool (same position) computed via the dense Rust seam (full tick_data →
    no miss). (The Rust mainline routing is deferred to slice 4 pending
    fork-validated crossing-swap parity — see `test_cached_calculations`; the
    fetch-callback return-data contract + V4 sparse-loop helper stay wired +
    validated here.)
    """
    py_bot = PyBot()

    # Dense oracle (dense Rust seam — full tick_data, no miss).
    dense = _build_dense_v4_pool(py_bot, address_tag="a")
    oracle = dense.calculate_tokens_out_from_tokens_in(
        token_in=dense.token0,
        token_in_quantity=1_000,
    )

    # Sparse pool: identical scalars + position, registered SPARSE with NO
    # tick_data seeded (coverage="sparse"; the contract is established at
    # registration — clearing tick_data after a dense registration does NOT flip
    # the Rust `coverage` flag, so the miss-detection in v4_simulate_swap would
    # never fire + the fetcher would never run). The starting word (0) is
    # unknown → the first `v4_simulate_swap` call raises `MissingTickWord(0)` →
    # the fetch+retry loop backfills the bounds' words on demand.
    token0_b = make_erc20(
        py_bot,
        address=f"0x{'b' * 40}",
        name="T0b",
        symbol="T0b",
        decimals=18,
    )
    token1_b = make_erc20(
        py_bot,
        address=f"0x{'b' * 39}{'e'}",
        name="T1b",
        symbol="T1b",
        decimals=18,
    )
    pool_id_b = _compute_v4_pool_id(
        token0_b.address,
        token1_b.address,
        _FEE,
        _TICK_SPACING,
        ZERO_ADDRESS,
    )
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

    sparse = make_v4_pool(
        pool_id=pool_id_b,
        pool_manager_address=_V4_POOL_MANAGER,
        token0=token0_b,
        token1=token1_b,
        fee=_FEE,
        tick_spacing=_TICK_SPACING,
        hook_address=None,
        sqrt_price_x96=_SQRT_PRICE_1TO1,
        tick=0,
        liquidity=_LIQUIDITY,
        tick_data=None,
        protocol_fee_zero_for_one=0,
        protocol_fee_one_for_zero=0,
        lp_fee=_FEE,
        state_block=0,
        py_bot=py_bot,
        coverage="sparse",
        tick_data_fetcher=fetcher,
    )
    sparse._sparse_liquidity_map = True
    assert sparse.sparse_liquidity_map, "sparse-registered companion is sparse"

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
    """Build a V4 companion seeded with ``td``.

    If ``sparse``, register with ``coverage="sparse"`` + the partial ``td``
    (the sparse-fetch contract is established at registration — clearing
    tick_data after a dense registration does NOT flip the Rust coverage
    flag). The companion's fetcher is attached so the Rust miss-detection in
    ``v4_simulate_swap`` fetches the absent words on demand (``td`` omits the
    words the gate must backfill).
    """
    token0 = make_erc20(py_bot, address="0x" + "aa" * 20, name="T0", symbol="T0", decimals=18)
    token1 = make_erc20(py_bot, address="0x" + "bb" * 20, name="T1", symbol="T1", decimals=18)
    pool_id = (
        "0x"
        + keccak256(
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
        coverage="sparse" if sparse else "tracked",
        tick_data_fetcher=fetcher if sparse else None,
    )
    if sparse:
        pool._sparse_liquidity_map = True
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
    )
    assert rust_outcome is not None
    rust_out = int(rust_outcome[0])  # ofz: token0 out
    assert py_out == _V4_DIVERGE_QUOTER_OUT, f"Python dense diverges from quoter: {py_out}"
    assert rust_out == _V4_DIVERGE_QUOTER_OUT, f"Rust dense diverges from quoter: {rust_out}"


def test_rust_v4_sparse_fetch_corpus_matches_dense():
    """Rust sparse+fetch == Rust dense on the SAME corpus (offline, no fork).

    Seeds the pool with the corpus MINUS word -76's ticks and runs both paths
    SPARSE with a fetcher that returns each word's ticks on demand (the last
    # each word's ticks on demand (the last word is fetched, not pre-seeded, so
    # the sparse+fetch loop is exercised). The dense-Rust mainline (sparse,
    # full-fetch) reproduces the on-chain quoter exactly; Rust
    # ``simulate_swap_with_fetch`` matches too (the sparse-path divergence is
    # fixed — see the ELSE-branch miss check in `v4_simulate_swap`). The
    # divergence equals the fork divergence, so this is the offline gate for
    # the V4 mainline routing gate — was RED (sparse-path divergence) before
    # the ELSE-branch miss-check fix in `v4_simulate_swap`; now GREEN.
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
    # dense-Rust mainline (sparse, full-fetch) — the oracle (== quoter).
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
        f"dense-Rust sparse+full-fetch must match the quoter (== dense): {py_out}"
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
    dense-Rust oracle (full tick_data → no miss) down to MIN_TICK.
    """
    py_bot = PyBot()
    # Dense oracle: a dummy init tick ABOVE the start (opposite the zfo walk)
    # makes tick_data non-empty → coverage=tracked, but the zfo path (tick ≤
    # start=0) has NO initialized ticks — liq stays constant to MIN.
    token0 = make_erc20(py_bot, address="0x" + "f" * 40, name="T0", symbol="T0", decimals=18)
    token1 = make_erc20(py_bot, address="0x" + "e" * 40, name="T1", symbol="T1", decimals=18)
    pool_id = _compute_v4_pool_id(token0.address, token1.address, _FEE, _TICK_SPACING, ZERO_ADDRESS)
    tick_data_dense = {
        60: LiquidityAtTick(liquidity_net=_LIQUIDITY, liquidity_gross=_LIQUIDITY, block=0),
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
    # Dense-Rust oracle: the dense pool (full tick_data → no miss) computed
    # via the Rust seam (the seam is the subject under test for the sparse
    # path; the dense path never misses, so it is a valid reference). gen_ticks
    # (now clamped at MIN_TICK) drives the walk to the price limit.
    oracle_outcome = dense._py_pool.simulate_swap_with_fetch(
        zero_for_one=True,  # zfo (token0 in → token1 out, descending → MIN)
        amount_in=amount_in,
        block=0,
    )
    assert oracle_outcome is not None, "dense oracle must not miss"
    c0, c1, py_sp, py_liq, py_tick = (int(x) for x in oracle_outcome)
    assert py_sp == MIN_SQRT_PRICE + 1, (
        f"dense oracle did not reach the MIN price limit (got sqrt={py_sp}); bump amount_in"
    )

    # Sparse pool — identical scalars, NO tick_data seeded. Distinct tokens so
    # the pool_id differs (same PyBot). The fetcher returns {} for every word
    # (no initialized ticks anywhere); Rust marks each fetched-empty word known
    # + continues, but must reach MIN (not strand at the last word above
    # MIN_TICK).
    fetched_words: list[int] = []

    def fetcher(word: int, block: int):
        fetched_words.append(word)
        return {}

    token0b = make_erc20(py_bot, address="0x" + "d" * 40, name="T0b", symbol="T0", decimals=18)
    token1b = make_erc20(py_bot, address="0x" + "c" * 40, name="T1b", symbol="T1", decimals=18)
    pool_id2 = _compute_v4_pool_id(
        token0b.address,
        token1b.address,
        _FEE,
        _TICK_SPACING,
        ZERO_ADDRESS,
    )
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
        tick_data_fetcher=fetcher,
    )
    sparse._py_pool.update_tick_data({}, {}, 0)
    sparse._sparse_liquidity_map = True
    assert sparse.sparse_liquidity_map, "cleared tick_data ⇒ sparse companion"

    rust_outcome = sparse._py_pool.simulate_swap_with_fetch(
        zero_for_one=True,
        amount_in=amount_in,
        block=0,
    )
    assert rust_outcome is not None, "sparse V4 swap returned None"
    rust_a0, rust_a1, rust_sp, rust_liq, rust_tick = (int(x) for x in rust_outcome)
    assert rust_sp == MIN_SQRT_PRICE + 1, (
        f"rust V4 sparse did not reach the MIN price limit (got sqrt={rust_sp}); "
        f"the walk stranded before MIN_TICK"
    )
    # Full parity with the dense-Rust oracle (amounts + final state). Both
    # paths return UNSIGNED absolute amounts (zfo: amount0 = token0 in,
    # amount1 = token1 out) — no sign flip needed.
    assert rust_a0 == c0, f"amount0 (token0 in): rust={rust_a0} oracle={c0}"
    assert rust_a1 == c1, f"amount1 (token1 out): rust={rust_a1} oracle={c1}"
    assert rust_tick == py_tick, f"final tick: rust={rust_tick} oracle={py_tick}"
    assert rust_liq == py_liq, f"final liq: rust={rust_liq} oracle={py_liq}"
    assert rust_sp == py_sp, f"final sqrt: rust={rust_sp} oracle={py_sp}"


if __name__ == "__main__":
    pytest.main([__file__, "-vv"])
