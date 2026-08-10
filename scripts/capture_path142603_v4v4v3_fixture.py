#!/usr/bin/env python3
"""Capture the exact V4->V4->V3 pool states for the no-profit path-142603 crash
(block 25723658) into a JSON fixture.

Modeled 1:1 on `capture_fee1_v3v4v3_fixture.py` / `capture_path_13308_fixture.py`,
with the hop layout adjusted for the LIVE incident:

    path = V4(USDC/WETH 0x4f88…) -> V4(USDC/USDT 0x3ad2…) -> V3(WETH/USDT 0xc7bBeC68)
    solve_block = 25723658
    recorded_solve: optimal_input=351476045207054
                    hop_outputs=[676293, 676607, 351475872056229]
    sim-bucket = no-profit  (executed net -173150825 wei WETH -> gross 0)
    captured_swaps: three in-block swaps the bot backruns (see module docstrings)

The capture reads the DB liquidity snapshot (tick_data) for the three pools and
the live scalars via `cast` against the archive RPC at TARGET (the same
DB-snapshot + on-chain-scalar split path-13308 uses). The output JSON is the
input to `rust/crates/degenbot/examples/path142603_v4v4v3_solver_fixture.rs`,
which reconstructs the three pools, runs the production Möbius solver, and
reports optimal_input/hop_outputs against the RECORDED solve.

HEADLINE (verified 2026-08-10 from archive RPC at 25723658):
    Pool A (V4 USDC/WETH) scalar == solver  (sqrt & liq match)
    Pool C (V3 WETH/USDT) scalar == solver  (sqrt & liq match)
    Pool B (V4 USDC/USDT) sqrt/tick == solver (current), BUT
        solver liq = 1_018_741_430_873  vs  on-chain liq = 718_152_690_765
        (a ~3.05e11 ModifyLiquidity removal at ~block 25720300 was NOT applied;
         solver liq is frozen at the pre-removal value, ~3,300 blocks stale)

Usage — point FIX_TARGET (defaults are the incident), then:

    python3 scripts/capture_path142603_v4v4v3_fixture.py

Override most fields via FIX_* env vars to capture a fresh recurrence.
"""

import json
import os
import sqlite3
import subprocess

RPC = "http://host.containers.internal:8545"


def _env(name, default):
    v = os.environ.get(name)
    return default if v in (None, "") else v


def _env_int(name, default):
    return int(_env(name, default))


# --- Incident identity (path 142603, block 25723658) ---------------------------
TARGET = _env_int("FIX_TARGET", 25723658)
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")
SV = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"  # canonical V4 StateView

# hop0: V4 USDC/WETH  (pool A)
V4_A_PID = _env(
    "FIX_V4_A_PID",
    "0x4f88f7c99022eace4740c6898f59ce6a2e798a1e64ce54589720b7153eb224a7",
)
# hop1: V4 USDC/USDT (pool B)  <- the STALE-LIQUIDITY hop
V4_B_PID = _env(
    "FIX_V4_B_PID",
    "0x3ad280c97568a027da5e10bf3e757886fc4e2fa301959d7bb6c296d3e39f30b5",
)
# hop2: V3 WETH/USDT (pool C)
V3_C = _env("FIX_V3_C", "0xc7bBeC68d12a0d1830360F8Ec58fA599bA1b0e9b")

V4_MGR = "0x000000000004444c5dc75cb358380d2e3de08a90"  # canonical PoolManager

# DB pool ids (resolve by address/hash on first run if needed)
V4_A_MANAGED = _env_int("FIX_V4_A_MANAGED", 3)
V4_B_MANAGED = _env_int("FIX_V4_B_MANAGED", 11)
V3_C_POOLID = _env_int("FIX_V3_C_POOLID", 609252)

# Path direction flags (from the live sim-fixture for path 142603).
HOP0_ZFO = True if _env("FIX_HOP0_ZFO", "0") == "1" else False
HOP1_ZFO = True if _env("FIX_HOP1_ZFO", "1") == "1" else False
HOP2_ZFO = True if _env("FIX_HOP2_ZFO", "0") == "1" else False

# Recorded solve + sim outcome (from the live DEGENBOT_SIM_EXIT_ON_FAIL trap).
RECORDED_OPTIMAL = _env("FIX_OPTIMAL", "351476045207054")
RECORDED_HOPS = [int(x) for x in _env("FIX_HOPS", "676293,676607,351475872056229").split(",")]
RECORDED_BUCKET = _env("FIX_BUCKET", "no-profit")

# --- selectors ---------------------------------------------------------------
GET_SLOT0 = "0xc815641c"
GET_LIQ = "0xfa6793d5"
SLOT0 = "0x3850c7bd"
LIQ = "0x1a686502"


def v4_scalars(pool_id):
    out = [ln.split()[0] for ln in subprocess.check_output(
        ["cast", "call", SV, "getSlot0(bytes32)(uint160,int24,uint24,uint24)", pool_id,
         "--rpc-url", RPC, "--block", str(TARGET)], text=True).splitlines() if ln.strip()]
    sq, tick, protocol_fee, lp_fee = int(out[0], 0), int(out[1], 0), int(out[2], 0), int(out[3], 0)
    liq = int([ln.split()[0] for ln in subprocess.check_output(
        ["cast", "call", SV, "getLiquidity(bytes32)(uint128)", pool_id,
         "--rpc-url", RPC, "--block", str(TARGET)], text=True).splitlines() if ln.strip()][0], 0)
    return sq, tick, liq, protocol_fee, lp_fee


def v3_scalars(addr):
    vals = [int(p, 0) for p in [ln.split()[0] for ln in subprocess.check_output(
        ["cast", "call", addr, "slot0()(uint160,int24,uint16,uint16,uint16,uint8,bool)",
         "--rpc-url", RPC, "--block", str(TARGET)], text=True).splitlines() if ln.strip()][:6]]
    liq = int([ln.split()[0] for ln in subprocess.check_output(
        ["cast", "call", addr, "liquidity()(uint128)", "--rpc-url", RPC,
         "--block", str(TARGET)], text=True).splitlines() if ln.strip()][0], 0)
    return vals[0], vals[1], liq


def load_v4_pool(cur, managed_id, pool_hash, mgr):
    (t0, t1, f0, ts, ublk) = cur.execute(
        """SELECT uv4.currency0_id, uv4.currency1_id, uv4.fee_currency0, uv4.tick_spacing,
                  uv4.liquidity_update_block
           FROM uniswap_v4_pools uv4 WHERE uv4.managed_pool_id=?""", (managed_id,)).fetchone()
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id, address FROM erc20_tokens")}
    rows = cur.execute(
        "SELECT tick, liquidity_net, liquidity_gross FROM managed_pool_liquidity_positions "
        "WHERE managed_pool_id=?", (managed_id,)).fetchall()
    tick_data = {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in rows}
    return {"family": "uniswap_v4", "pool_manager": mgr, "pool_id": pool_hash,
            "currency0": tokens[t0], "currency1": tokens[t1], "fee_currency0": f0,
            "fee_currency1": f0, "fee_denominator": 1000000, "tick_spacing": ts,
            "liquidity_update_block": ublk, "tick_data": tick_data}


def load_v3_pool(cur, pool_id, addr):
    kind = cur.execute("SELECT kind FROM pools WHERE id=?", (pool_id,)).fetchone()[0]
    tbl = "uniswap_v3_pools"  # path 142603 hop2 is canonical Uniswap V3
    (t0, t1, ts, f0, f1, fd) = cur.execute(
        f"""SELECT p.token0_id, p.token1_id, {tbl}.tick_spacing,
                  {tbl}.fee_token0, {tbl}.fee_token1, {tbl}.fee_denominator
           FROM pools p JOIN {tbl} ON {tbl}.pool_id=p.id WHERE p.id=?""", (pool_id,)).fetchone()
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id, address FROM erc20_tokens")}
    rows = cur.execute(
        "SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions WHERE pool_id=?",
        (pool_id,)).fetchall()
    tick_data = {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in rows}
    return {"family": kind, "address": addr, "token0": tokens[t0], "token1": tokens[t1],
            "tick_spacing": ts, "fee_token0": f0, "fee_token1": f1, "fee_denominator": fd,
            "liquidity_update_block": TARGET, "tick_data": tick_data}


def main():
    cur = sqlite3.connect(DB)
    v4a = load_v4_pool(cur, V4_A_MANAGED, V4_A_PID, V4_MGR)
    v4b = load_v4_pool(cur, V4_B_MANAGED, V4_B_PID, V4_MGR)
    v3c = load_v3_pool(cur, V3_C_POOLID, V3_C)
    cur.close()

    # On-chain scalars at TARGET (the verified domain of truth).
    sa, ta, qa, pfa, lfa = v4_scalars(V4_A_PID)
    v4a.update(sqrt_price_x96=str(sa), tick=ta, liquidity=str(qa), protocol_fee=pfa, lp_fee=lfa)
    sb, tb, qb, pfb, lfb = v4_scalars(V4_B_PID)
    v4b.update(sqrt_price_x96=str(sb), tick=tb, liquidity=str(qb), protocol_fee=pfb, lp_fee=lfb)
    sc, tc, qc = v3_scalars(V3_C)
    v3c.update(sqrt_price_x96=str(sc), tick=tc, liquidity=str(qc))

    fixture = {
        "_doc": (f"Exact V4-V4-V3 path-142603 pool states at block {TARGET}. "
                 "DB liquidity snapshots + on-chain scalars read at TARGET; "
                 "populated by capture_path142603_v4v4v3_fixture.py. "
                 "NOTE: pool B (USDC/USDT) DB tick_data is tracked/sparse and its "
                 "on-chain liq (718152690765) differs from the solver's stale "
                 "1,018,741,430,873 — the FIXTURE_V4_B_LIQ override probes that gap."),
        "target_block": TARGET,
        "recorded_solve": {
            "optimal_input": RECORDED_OPTIMAL,
            "hop_outputs": [str(h) for h in RECORDED_HOPS],
            "sim_bucket": RECORDED_BUCKET,
        },
        "pools": {"v4_a": v4a, "v4_b": v4b, "v3_c": v3c},
        "path": [
            {"hop": 0, "pool": "v4_a", "zero_for_one": HOP0_ZFO},
            {"hop": 1, "pool": "v4_b", "zero_for_one": HOP1_ZFO},
            {"hop": 2, "pool": "v3_c", "zero_for_one": HOP2_ZFO},
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path142603_v4v4v3_block{TARGET}.json"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    for n, p in (("v4_a", v4a), ("v4_b", v4b), ("v3_c", v3c)):
        print("%s ticks=%d sqrt=%s liq=%s tick=%s" %
              (n, len(p["tick_data"]), p["sqrt_price_x96"], p["liquidity"], p["tick"]))
    print("v4_b NOTE: solver liq=1018741430873 vs on-chain", v4b["liquidity"])


if __name__ == "__main__":
    main()
