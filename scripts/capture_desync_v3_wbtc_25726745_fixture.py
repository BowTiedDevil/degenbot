#!/usr/bin/env python3
"""Capture the block-25726745 solver-state desync (path 27) into a JSON fixture
for deterministic replay and hypothesis testing.

Context — the live bot aborted loudly (UO3JM4, rc=134) on a *verified* desync:

    DEGENBOT_ASSERT_SOLVER_STATE: verified desync — ABORT path_idx=27 block=25726745
    ... hop V3 update_block=25726741 ... stale_by=4 ... hop V4 update_block=25726694
        stale_by=51 ... hop V3 update_block=25726744 ... stale_by=1 ...
    hop 0: V3 pool 0xCBCdF9626bC03E24f779434178A73a0B4bad62eD STALE at solve block
        25726745 (solver update_block=25726741, behind by 4 blocks): solver snapshot
        (sqrt=46319237888222796546747854391003398, liq=29526429242910427, tick=265588)
        no longer matches on-chain at 25726745
        (sqrt=46316826426435054225821589887038887, liq=29526429242910427, tick=265586).

The capture records the FULL on-chain scalar trajectory of the hop-0 pool across
the frozen/solve window (25726741..25726745) so a replay can distinguish the two
competing root-cause classes:

  (A) "pump drain / snapshot-backfill stall" — the pool's on-chain price moved in
      some block *before* 25726745 (i.e. the gap 25726741..25726744) and the pump
      never applied that Swap -> genuine multi-block stale state.
  (B) "in-block swap not applied before solve" (header-promote-ahead-of-apply) —
      on-chain is *identical* across 25726741..25726744 (the pool really is quiet,
      and stale_by=4 is just "no events since 25726741"), and the only move happens
      *inside the solve block itself* 25726745, i.e. a Swap included in the very
      block being solved was not applied before the solve/verify fired.

The observed truth is (B): slot0 is bit-identical for blocks 25726741-25726744 and
only changes at 25726745 (tick 265588 -> 265586), matching the single Swap at
logIndex 75 / tx 0xfa4bc4a2cc063afc1766c74b9cbf63f86784e46e652d4fa267ea9d9231ca7a7d.
The pool is a quiet UniV3 WBTC/WETH 0.3% (only 2 Swaps in the whole 52-block
window 25726694..25726745).

Consumed by
`rust/crates/degenbot/examples/desync_v3_25726745_replay.rs`.

Usage (DB liquidity snapshot + on-chain scalars read via cast):

    python3 scripts/capture_desync_v3_wbtc_25726745_fixture.py
"""

import json
import os
import sqlite3
import subprocess

RPC = "http://host.containers.internal:8545"
SOLVER_UPDATE_BLOCK = 25726741  # pool's stored update_block (the frozen snapshot the solver used)
SOLVE_BLOCK = 25726745  # block being solved when the abort fired
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")

V3_POOL = {  # hop0 of failing path 27: UniV3 WBTC/WETH 0.3%
    "family": "uniswap_v3",
    "address": "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD",
    "db_id": 599507,
}

# From the live abort (solver-side stored snapshot at SOLVER_UPDATE_BLOCK, and the
# on-chain verification at SOLVE_BLOCK):
RECORDED = {
    "solver_tick": 265588,
    "solver_sqrt_price_x96": "46319237888222796546747854391003398",
    "solver_liquidity": "29526429242910427",
    "solver_update_block": SOLVER_UPDATE_BLOCK,
    "onchain_solve_tick": 265586,
    "onchain_solve_sqrt_price_x96": "46316826426435054225821589887038887",
    "onchain_solve_liquidity": "29526429242910427",
    "verified_desync": True,
}

# The single Swap included in solve block 25726745 that moved the pool's price
# (logIndex 75, tx 0xfa4bc4…). This Swap did NOT reach the solver's stored state
# before the block-25726745 solve+verify fired.
SOLVE_BLOCK_SWAP = {
    "block": SOLVE_BLOCK,
    "log_index": 75,
    "transaction_hash": "0xfa4bc4a2cc063afc1766c74b9cbf63f86784e46e652d4fa267ea9d9231ca7a7d",
    "transaction_index": 3,
    "tick_before": 265588,
    "tick_after": 265586,
}


def cast_call(addr, sig, block):
    cmd = ["cast", "call", addr, sig, "--rpc-url", RPC, "--block", str(block)]
    return [
        ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()
    ]


def v3_slot0(addr, block):
    """slot0()(uint160 sqrtPriceX96,int24 tick,uint16 fee,uint16 unlocked,...)"""
    out = cast_call(addr, "slot0()(uint160,int24,uint16,uint16,uint16,uint8)", block)
    return int(out[0], 0), int(out[1], 0)


def v3_liquidity(addr, block):
    return int(cast_call(addr, "liquidity()(uint128)", block)[0], 0)


def scalars_at(addr, block):
    sq, tick = v3_slot0(addr, block)
    liq = v3_liquidity(addr, block)
    return {"block": block, "sqrt_price_x96": str(sq), "tick": tick, "liquidity": str(liq)}


def main():
    cur = sqlite3.connect(DB)
    (t0_id, t1_id) = cur.execute(
        "SELECT token0_id,token1_id FROM pools WHERE id=?", (V3_POOL["db_id"],)
    ).fetchone()
    tokens = {
        r[0]: r[1]
        for r in cur.execute(
            "SELECT id,address FROM erc20_tokens WHERE id IN (?,?)", (t0_id, t1_id)
        )
    }
    (ts, f0, f1, fd) = cur.execute(
        "SELECT tick_spacing,fee_token0,fee_token1,fee_denominator "
        "FROM uniswap_v3_pools WHERE pool_id=?",
        (V3_POOL["db_id"],),
    ).fetchone()
    ticks = cur.execute(
        "SELECT tick,liquidity_net,liquidity_gross FROM liquidity_positions "
        "WHERE pool_id=? ORDER BY tick",
        (V3_POOL["db_id"],),
    ).fetchall()
    cur.close()

    # On-chain scalars at the SOLVER's stored update block == the state the solver
    # held (frozen snapshot). Bit-identical to on-chain through block SOLVE_BLOCK-1.
    snap = scalars_at(V3_POOL["address"], SOLVER_UPDATE_BLOCK)

    # Full per-block trajectory so a replay can re-derive root cause A vs B.
    trajectory = {
        str(b): scalars_at(V3_POOL["address"], b)
        for b in range(SOLVER_UPDATE_BLOCK, SOLVE_BLOCK + 1)
    }
    truth = scalars_at(V3_POOL["address"], SOLVE_BLOCK)

    pool = {
        "family": V3_POOL["family"],
        "address": V3_POOL["address"],
        "token0": tokens[t0_id],
        "token1": tokens[t1_id],
        "tick_spacing": ts,
        "fee_token0": f0,
        "fee_token1": f1,
        "fee_denominator": fd,
        "liquidity_update_block": SOLVER_UPDATE_BLOCK,
        "tick_data": {t: {"liquidity_net": str(n), "liquidity_gross": str(g)} for t, n, g in ticks},
        # The solver's frozen scalars at SOLVER_UPDATE_BLOCK:
        "sqrt_price_x96": snap["sqrt_price_x96"],
        "tick": snap["tick"],
        "liquidity": snap["liquidity"],
    }

    # Cross-check: the captured frozen snapshot must reproduce the abort's solver
    # numbers, and the on-chain truth must reproduce the abort's on-chain numbers.
    assert str(snap["tick"]) == str(RECORDED["solver_tick"]), snap
    assert snap["sqrt_price_x96"] == RECORDED["solver_sqrt_price_x96"], snap
    assert str(truth["tick"]) == str(RECORDED["onchain_solve_tick"]), truth
    assert truth["sqrt_price_x96"] == RECORDED["onchain_solve_sqrt_price_x96"], truth

    const_gap = all(
        trajectory[str(b)]["sqrt_price_x96"] == snap["sqrt_price_x96"]
        for b in range(SOLVER_UPDATE_BLOCK, SOLVE_BLOCK)
    )
    moved_at_solve = trajectory[str(SOLVE_BLOCK)]["sqrt_price_x96"] != snap["sqrt_price_x96"]

    fixture = {
        "_doc": (
            f"Block-{SOLVE_BLOCK} solver-state desync capture (path 27). Captures the "
            f"failing hop-0 UniV3 WBTC/WETH 0.3% pool {V3_POOL['address']}: the frozen "
            "solver snapshot at SOLVER_UPDATE_BLOCK, the full per-block on-chain slot0 "
            "trajectory, and the on-chain truth at SOLVE_BLOCK. Observed: on-chain is "
            "bit-identical across 25726741..25726744 and only moves at the solve block "
            "itself (the single Swap at logIndex 75) -> root-cause class (B), the "
            "in-block-swap-not-applied-before-solve race, NOT a multi-block backfill "
            "stall. See the replay example for the deterministic reproduction."
        ),
        "target_block": SOLVE_BLOCK,
        "solver_update_block": SOLVER_UPDATE_BLOCK,
        "per_block_onchain": trajectory,
        "solve_block_swap": SOLVE_BLOCK_SWAP,
        "observed": {
            "constant_across_gap": const_gap,
            "moved_at_solve_block": moved_at_solve,
        },
        "pools": {"v3_wbtc": pool},
        "recorded_solve": RECORDED,
    }

    out = (
        f"/workspaces/degenbot/tests/fixtures/desync_v3_wbtc_{SOLVE_BLOCK}_block{SOLVE_BLOCK}.json"
    )
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)

    print("wrote", out)
    print(
        f"  frozen snapshot (solver, update_block={SOLVER_UPDATE_BLOCK}): "
        f"tick={snap['tick']} sqrt={snap['sqrt_price_x96']}"
    )
    print(
        f"  on-chain truth  (solve_block={SOLVE_BLOCK}): "
        f"tick={truth['tick']} sqrt={truth['sqrt_price_x96']}"
    )
    print(
        f"  constant across gap {SOLVER_UPDATE_BLOCK}..{SOLVE_BLOCK - 1}: {const_gap}; "
        f"moved at solve block: {moved_at_solve}"
    )
    print(f"  recorded_solve: {fixture['recorded_solve']}")


if __name__ == "__main__":
    main()
