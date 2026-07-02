"""Parity test: the `cl_apply_liquidity_mapping_update` PyO3 seam vs the pure-Python oracle.

The Rust seam (``degenbot.degenbot_rs.cl_apply_liquidity_mapping_update``) wraps
the pure-Rust core in ``degenbot-cl-math``. It must produce state byte-identical
to the pure-Python ``apply_liquidity_mapping_update`` for the same input
tick-event sequences — covering in-range liquidity adjustment, out-of-range
events, tick init (flip on), gross-to-zero deletion (flip off), net-sign handling
at the upper tick, large deltas, and the ``update_block == initial_state_block``
no-op path.
"""

from __future__ import annotations

import pytest

from degenbot.calculations.concentrated_liquidity import apply_liquidity_mapping_update
from degenbot.degenbot_rs import cl_apply_liquidity_mapping_update
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick


def _rs(
    bitmap: dict[int, BitmapAtWord],
    data: dict[int, LiquidityAtTick],
    *,
    spacing: int,
    tick: int,
    liquidity: int,
    init_block: int,
    update_block: int,
    tick_lower: int,
    tick_upper: int,
    delta: int,
) -> tuple[dict, dict, int]:
    """Call the Rust seam, returning (bitmap, data, liquidity) as plain dicts."""
    out = cl_apply_liquidity_mapping_update(
        None if bitmap is None else dict(bitmap),
        None if data is None else dict(data),
        spacing,
        tick,
        liquidity,
        init_block,
        update_block,
        tick_lower,
        tick_upper,
        delta,
    )
    return out["tick_bitmap"], out["tick_data"], out["liquidity"]


def _py(
    bitmap: dict[int, BitmapAtWord],
    data: dict[int, LiquidityAtTick],
    *,
    spacing: int,
    tick: int,
    liquidity: int,
    init_block: int,
    update_block: int,
    tick_lower: int,
    tick_upper: int,
    delta: int,
) -> tuple[dict, dict, int]:
    """Call the Python oracle, returning (bitmap, data, liquidity) as plain dicts."""
    r = apply_liquidity_mapping_update(
        tick_bitmap=dict(bitmap),
        tick_data=dict(data),
        tick_spacing=spacing,
        tick=tick,
        liquidity=liquidity,
        initial_state_block=init_block,
        update_block=update_block,
        tick_lower=tick_lower,
        tick_upper=tick_upper,
        liquidity_delta=delta,
    )
    bm = {w: {"bitmap": v.bitmap, "block": v.block} for w, v in r.tick_bitmap.items()}
    td = {
        t: {
            "liquidity_net": v.liquidity_net,
            "liquidity_gross": v.liquidity_gross,
            "block": v.block,
        }
        for t, v in r.tick_data.items()
    }
    return bm, td, r.liquidity


def _wrap(
    bm: dict[int, dict[str, int]], td: dict[int, dict[str, int]],
) -> tuple[dict[int, BitmapAtWord], dict[int, LiquidityAtTick]]:
    """Re-wrap plain-dict state into pydantic models for the Python oracle."""
    wbm = {w: BitmapAtWord(bitmap=v["bitmap"], block=v["block"]) for w, v in bm.items()}
    wtd = {t: LiquidityAtTick(**d) for t, d in td.items()}
    return wbm, wtd


def _run_chain(steps, *, spacing, tick, init_block, tick_lower, tick_upper, start_liquidity):
    """Replay a step chain through BOTH implementations from identical seeds."""
    py_bm, py_td, py_liq = {}, {}, start_liquidity
    rs_bm, rs_td, rs_liq = {}, {}, start_liquidity
    for delta, upd_block in steps:
        # The previous step's output is plain dicts; wrap into models for the oracle
        # (the Rust seam accepts plain dicts directly).
        wbm, wtd = _wrap(py_bm, py_td)
        py_bm, py_td, py_liq = _py(
            wbm, wtd,
            spacing=spacing, tick=tick, liquidity=py_liq, init_block=init_block,
            update_block=upd_block, tick_lower=tick_lower, tick_upper=tick_upper, delta=delta,
        )
        rs_bm, rs_td, rs_liq = _rs(
            rs_bm, rs_td,
            spacing=spacing, tick=tick, liquidity=rs_liq, init_block=init_block,
            update_block=upd_block, tick_lower=tick_lower, tick_upper=tick_upper, delta=delta,
        )
        # Compare after EACH step (catches drift early).
        assert rs_liq == py_liq, f"liquidity mismatch after delta={delta} upd={upd_block}"
        assert rs_bm == py_bm, f"bitmap mismatch after delta={delta} upd={upd_block}"
        assert rs_td == py_td, f"tick_data mismatch after delta={delta} upd={upd_block}"
    return rs_bm, rs_td, rs_liq


def test_chain_a_in_range_burn_to_zero():
    # tick=0 active within [-1000,1000], spacing 10, init_block 10.
    # Steps: +1M, +250K, -750K, -500K → burns both ticks to zero, active → 0.
    _run_chain(
        [(1_000_000, 11), (250_000, 12), (-750_000, 13), (-500_000, 14)],
        spacing=10, tick=0, init_block=10, tick_lower=-1000, tick_upper=1000,
        start_liquidity=0,
    )


def test_chain_b_large_delta_out_of_range():
    # active tick 5000 never in range → liquidity stays 5_000_000.
    # 2^100 mint then full burn (delete + flip off) at [-120,-60], then re-init,
    # then a +999 out-of-range mint at [300,600].
    big = 1 << 100
    # First chain at [-120, -60]
    rs_bm1, rs_td1, rs_liq1 = _run_chain(
        [(big, 101), (-big, 102), (123_456_789, 103)],
        spacing=60, tick=5000, init_block=100, tick_lower=-120, tick_upper=-60,
        start_liquidity=5_000_000,
    )
    assert rs_liq1 == 5_000_000
    # Continue with a new position [300,600] (out of range, active unchanged).
    # Wrap dicts→models for the oracle (the Rust seam takes plain dicts).
    wbm1, wtd1 = _wrap(rs_bm1, rs_td1)
    rs_bm2, rs_td2, rs_liq2 = _rs(
        rs_bm1, rs_td1,
        spacing=60, tick=5000, liquidity=rs_liq1, init_block=100,
        update_block=104, tick_lower=300, tick_upper=600, delta=999,
    )
    py_bm2, py_td2, py_liq2 = _py(
        wbm1, wtd1,
        spacing=60, tick=5000, liquidity=rs_liq1, init_block=100,
        update_block=104, tick_lower=300, tick_upper=600, delta=999,
    )
    assert rs_liq2 == py_liq2 == 5_000_000
    assert rs_bm2 == py_bm2
    assert rs_td2 == py_td2


def test_chain_c_net_signs_and_reinit_delete():
    # ticks [10,20], tick=0 out of range → active stays 0.
    # +1000, +1000, -1000, +500 (new tick 5), -500 (tick 5 deleted + flipped off).
    _run_chain(
        [(1000, 201), (1000, 202), (-1000, 203), (500, 204), (-500, 205)],
        spacing=1, tick=0, init_block=200, tick_lower=10, tick_upper=20,
        start_liquidity=0,
    )


def test_chain_d_block_equals_init_no_inrange_change():
    # update_block (300) == initial_state_block (300) → in-range branch skipped,
    # active liquidity stays 0 even though tick=0 is in range [-100,100].
    rs_bm, rs_td, rs_liq = _run_chain(
        [(1000, 300)],
        spacing=10, tick=0, init_block=300, tick_lower=-100, tick_upper=100,
        start_liquidity=0,
    )
    assert rs_liq == 0


def test_chain_e_in_range_nonzero_residual():
    # tick=0 active within [-100,100], spacing 10, init_block 400.
    # +1M (→1M), +500K (→1.5M), -200K (→1.3M).
    rs_bm, rs_td, rs_liq = _run_chain(
        [(1_000_000, 401), (500_000, 402), (-200_000, 403)],
        spacing=10, tick=0, init_block=400, tick_lower=-100, tick_upper=100,
        start_liquidity=0,
    )
    assert rs_liq == 1_300_000


def test_initial_state_block_max_uint256_disables_inrange():
    """cli/pool.py passes initial_state_block=MAX_UINT256 to skip the in-range
    adjustment. The seam clamps that sentinel to u64::MAX, preserving the skip
    (update_block < MAX → no active-liquidity change), matching the oracle."""
    MAX = (1 << 256) - 1
    # In range, but with MAX initial_state_block → active liquidity unchanged.
    py_bm, py_td, py_liq = _py(
        {}, {}, spacing=10, tick=0, liquidity=1_000_000, init_block=MAX,
        update_block=999_999, tick_lower=-100, tick_upper=100, delta=500_000,
    )
    rs_bm, rs_td, rs_liq = _rs(
        {}, {}, spacing=10, tick=0, liquidity=1_000_000, init_block=MAX,
        update_block=999_999, tick_lower=-100, tick_upper=100, delta=500_000,
    )
    assert rs_liq == py_liq == 1_000_000  # unchanged
    assert rs_bm == py_bm
    assert rs_td == py_td


def test_seat_empty_inputs():
    # Empty bitmap/data, out-of-range, single mint → identical to oracle.
    py_bm, py_td, py_liq = _py(
        {}, {}, spacing=10, tick=0, liquidity=0, init_block=0,
        update_block=1, tick_lower=-100, tick_upper=100, delta=42,
    )
    rs_bm, rs_td, rs_liq = _rs(
        {}, {}, spacing=10, tick=0, liquidity=0, init_block=0,
        update_block=1, tick_lower=-100, tick_upper=100, delta=42,
    )
    assert rs_liq == py_liq == 42
    assert rs_bm == py_bm
    assert rs_td == py_td


def test_seat_accepts_pydantic_models_directly():
    """The seam reads fields by attr OR key — pydantic BitmapAtWord/LiquidityAtTick
    models (attr access) must work, not just plain dicts."""
    seed_bm = {0: BitmapAtWord(bitmap=0, block=5)}
    seed_td = {10: LiquidityAtTick(liquidity_net=0, liquidity_gross=0, block=5)}
    rs_bm, rs_td, rs_liq = _rs(
        seed_bm, seed_td, spacing=1, tick=-10, liquidity=0, init_block=5,
        update_block=6, tick_lower=10, tick_upper=20, delta=100,
    )
    py_bm, py_td, py_liq = _py(
        seed_bm, seed_td, spacing=1, tick=-10, liquidity=0, init_block=5,
        update_block=6, tick_lower=10, tick_upper=20, delta=100,
    )
    assert rs_liq == py_liq
    assert rs_bm == py_bm
    assert rs_td == py_td


def test_seat_preserves_none_inputs():
    """Passing None for the bitmap/data (instead of empty dicts) must be tolerated."""
    rs_bm, rs_td, rs_liq = _rs(
        None, None, spacing=10, tick=0, liquidity=0, init_block=0,
        update_block=1, tick_lower=-100, tick_upper=100, delta=42,
    )
    py_bm, py_td, py_liq = _py(
        {}, {}, spacing=10, tick=0, liquidity=0, init_block=0,
        update_block=1, tick_lower=-100, tick_upper=100, delta=42,
    )
    assert rs_liq == py_liq == 42
    assert rs_bm == py_bm
    assert rs_td == py_td