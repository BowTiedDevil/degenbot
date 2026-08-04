#!/usr/bin/env python3
"""Capture the exact V3→V4→V3 pool states for the fee-1 V4 overdraw bug
(ergo UO3JM4, live paths 10234/10338) into a JSON fixture.

Modeled 1:1 on `capture_path_13308_fixture.py` (the multi-pool-state fixture
that surfaced the PancakeSwap-V3 bug). The bug it reproduces:
  V3(fee=30 zfo) → V4(fee=1 zfo=false) → V3(fee=25 zfo), where the fee-1 V4 hop
  is `[sim-revert-swap] actual_out=9585 predicted=9586 matched=false` — the
  solver's V4-hop output over-predicts on-chain by 1 wei → `V4_TAKE(predicted)`
  overdrafts USDC → path reverts in sim.

Usage — fill the identity + block + recorded solve, then run:

    python3 scripts/capture_fee1_v3v4v3_fixture.py

The capture reads the DB liquidity snapshot (tick_data) for the three pools and
the scalars via `cast` against the archive RPC at TARGET (the same
DB-snapshot + on-chain-scalar split path-13308 uses). The output JSON is the
input to `rust/crates/degenbot/examples/fee1_v3v4v3_solver_fixture.rs`, which
reconstructs the three pools, runs the production Möbius solver, and asserts the
fix target (solver V4 hop == v4_simulate_swap == recorded on-chain actual).

Before first run you MUST fill in:
  TARGET         = the solve block (read from the live [sim-revert-swap] log)
  V3_0 / V3_2    = the hop-0 / hop-2 V3 pool addresses
  V4_MGR / V4_PID= the V4 PoolManager + poolId (v4_pool_id for the fee-1 pool)
  RECORDED_*     = the live-observed solve + the V4 hop input/predicted/actual
"""

import json
import os
import sqlite3
import subprocess

RPC = "http://host.containers.internal:8545"


def _env(name, default):
    """Env-var override so one script captures any recurrence (see README)."""
    v = os.environ.get(name)
    if v is None or v == "":
        return default
    return v


def _env_int(name, default):
    return int(_env(name, default))


# Defaults capture the original fee-1 path (paths 10234/10338, block 25600000).
# Override any of these via FIX_* env vars to capture a fresh recurrence, e.g.:
#   FIX_TARGET=25672332 FIX_V3_0_POOLID=609252 FIX_V3_2_POOLID=616795 \
#   FIX_V4_PID=0x76f7596508... FIX_V4_INPUT=3184 FIX_V4_PREDICTED=3182 \
#   FIX_V4_ACTUAL=3179 FIX_V3_0=0xc7bBeC68... python3 scripts/capture_fee1_v3v4v3_fixture.py
TARGET = _env_int("FIX_TARGET", 0)  # FILL: solve block from the live [sim-revert-swap] log
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")
SV = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"  # V4 StateView

# The fee-1 V3-V4-V3 path identity recovered from bot_run.log (paths 10234/10338):
#   hop0 V3 = Uniswap USDT/WETH 0.30% (solver fee "30")
#   hop1 V4 = USDC/USDT pool_id 0x76f75965... (lp_fee=50/1e6, protocol_fee=53261, t.s.=1)
#   hop2 V3 = PancakeSwap USDC/WETH 0.25% (solver fee "25")
V3_0 = _env("FIX_V3_0", "0x4e68ccd3e89f51c3074ca5072bbac773960dfa36")  # hop0 V3 (Uniswap WETH/USDT 3000 default)
V3_2 = _env("FIX_V3_2", "0x19ac5f80ec17497d0e585b953100e6d18c330040")  # hop2 V3 (Pancake USDC/WETH 2500)
V4_MGR = "0x000000000004444c5dc75cb358380d2e3de08a90"  # canonical PoolManager
V4_PID = _env("FIX_V4_PID", "0x76f75965083b5bfcc0b96f9c5e77e9e00f80b377def5e7625866013fa3059080")
V3_0_POOLID = _env_int("FIX_V3_0_POOLID", 0)  # DB pools.id for hop0 V3 (resolve via address)
V3_2_POOLID = _env_int("FIX_V3_2_POOLID", 1)  # DB pools.id for hop2 V3

# Per-hop swap direction (zero_for_one) for the reconstructed path. The defaults
# match the original fee-1 repro (hop0 true, hop1 false, hop2 true); override via
# FIX_HOP{0,1,2}_ZFO to capture a recurrence with different directions (e.g. the
# path-9354 recurrence hops 0=false/1=false/2=true).
HOP0_ZFO = True if _env("FIX_HOP0_ZFO", "1") == "1" else False
HOP1_ZFO = True if _env("FIX_HOP1_ZFO", "0") == "1" else False
HOP2_ZFO = True if _env("FIX_HOP2_ZFO", "1") == "1" else False

# historical overdraw reproduction from the prior live session (paths 10234/10338)
RECORDED_V4_HOP_INDEX = 1
_in = _env("FIX_V4_INPUT", None)
RECORDED_V4_INPUT = _env_int("FIX_V4_INPUT", None) if _in is not None else None  # exact-in USDT fed to V4
RECORDED_V4_PREDICTED = _env_int("FIX_V4_PREDICTED", 9586)  # solver hop_outputs[1]
RECORDED_V4_ACTUAL = _env_int("FIX_V4_ACTUAL", 9585)  # on-chain actual_out

# selectors
SLOT0 = "0x3850c7bd"
LIQ = "0x1a686502"
GET_SLOT0 = "0xc815641c"
GET_LIQ = "0xfa6793d5"


def rpc(method, params):
    import urllib.request

    req = urllib.request.Request(
        RPC,
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        headers={"Content-Type": "application/json"},
    )
    resp = json.load(urllib.request.urlopen(req, timeout=60))
    if "error" in resp:
        raise RuntimeError(f"{method}: {resp['error']}")
    return resp["result"]


def v3_scalars(addr):
    def cc(sig, arg=None):
        cmd = ["cast", "call", addr, sig, "--rpc-url", RPC, "--block", str(TARGET)]
        out = subprocess.check_output(cmd, text=True)
        parts = [ln.split()[0] for ln in out.splitlines() if ln.strip()]
        return int(parts[arg], 0) if arg is not None else [int(p, 0) for p in parts[:6]]

    vals = cc("slot0()(uint160,int24,uint16,uint16,uint16,uint8,bool)")
    liquidity = cc("liquidity()(uint128)", 0)
    return vals[0], vals[1], liquidity


def v4_scalars():
    cmd = ["cast", "call", SV, "getSlot0(bytes32)(uint160,int24,uint24,uint24)", V4_PID,
           "--rpc-url", RPC, "--block", str(TARGET)]
    out = [ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    sq, tick, protocol_fee, lp_fee = int(out[0], 0), int(out[1], 0), int(out[2], 0), int(out[3], 0)
    liquidity = int([ln.split()[0] for ln in subprocess.check_output(
        ["cast", "call", SV, "getLiquidity(bytes32)(uint128)", V4_PID,
         "--rpc-url", RPC, "--block", str(TARGET)], text=True).splitlines() if ln.strip()][0], 0)
    return sq, tick, liquidity, protocol_fee, lp_fee


def load_v3_liquidity_pool(cur, pool_id, addr):
    kind = cur.execute("SELECT kind FROM pools WHERE id=?", (pool_id,)).fetchone()[0]
    tbl = {"uniswap_v3": "uniswap_v3_pools", "pancakeswap_v3": "pancakeswap_v3_pools",
           "sushiswap_v3": "sushiswap_v3_pools", "aerodrome_v3": "aerodrome_v3_pools"}[kind]
    (t0, t1, ts, f0, f1, fd) = cur.execute(
        f"""SELECT p.token0_id, p.token1_id, {tbl}.tick_spacing,
                  {tbl}.fee_token0, {tbl}.fee_token1, {tbl}.fee_denominator
           FROM pools p JOIN {tbl} ON {tbl}.pool_id=p.id WHERE p.id=?""", (pool_id,)).fetchone()
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id, address FROM erc20_tokens")}
    tick_rows = cur.execute(
        "SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions WHERE pool_id=?",
        (pool_id,)).fetchall()
    tick_data = {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in tick_rows}
    return {"family": kind, "address": addr, "token0": tokens[t0], "token1": tokens[t1],
            "tick_spacing": ts, "fee_token0": f0, "fee_token1": f1, "fee_denominator": fd,
            "liquidity_update_block": TARGET, "tick_data": tick_data}


def load_v4_pool(cur):
    (t0, t1, f0, ts, ublk) = cur.execute(
        """SELECT t0.address, t1.address, uv4.fee_currency0, uv4.tick_spacing,
                  uv4.liquidity_update_block
           FROM uniswap_v4_pools uv4
           JOIN erc20_tokens t0 ON t0.id=uv4.currency0_id
           JOIN erc20_tokens t1 ON t1.id=uv4.currency1_id
           WHERE uv4.pool_hash=?""", (V4_PID,)).fetchone()
    mp = cur.execute("SELECT managed_pool_id FROM uniswap_v4_pools WHERE pool_hash=?",
                     (V4_PID,)).fetchone()[0]
    tick_rows = cur.execute(
        "SELECT tick, liquidity_net, liquidity_gross FROM managed_pool_liquidity_positions "
        "WHERE managed_pool_id=?", (mp,)).fetchall()
    tick_data = {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in tick_rows}
    return {"family": "uniswap_v4", "pool_manager": V4_MGR, "pool_id": V4_PID,
            "currency0": t0, "currency1": t1, "fee_currency0": f0, "fee_currency1": f0,
            "fee_denominator": 1000000, "tick_spacing": ts, "liquidity_update_block": ublk,
            "tick_data": tick_data}


def main():
    if V4_PID is None or RECORDED_V4_INPUT is None:
        raise SystemExit("FILL the identity + recorded V4 hop at the top of this script first.")
    cur = sqlite3.connect(DB)
    # FILL pool DB ids for v3_0 / v3_2 (query pools by address) before use.
    v3a = load_v3_liquidity_pool(cur, V3_0_POOLID, V3_0)
    v3c = load_v3_liquidity_pool(cur, V3_2_POOLID, V3_2)
    v4 = load_v4_pool(cur)
    cur.close()

    sa, ta, lqa = v3_scalars(V3_0)
    v3a.update(sqrt_price_x96=str(sa), tick=ta, liquidity=str(lqa))
    sw, tw, lqw = v3_scalars(V3_2)
    v3c.update(sqrt_price_x96=str(sw), tick=tw, liquidity=str(lqw))
    sv4, tv4, lqv4, pf4, lf4 = v4_scalars()
    v4.update(sqrt_price_x96=str(sv4), tick=tv4, liquidity=str(lqv4),
              protocol_fee=pf4, lp_fee=lf4)

    fixture = {
        "_doc": (f"Exact V3-V4-V3 fee-1 path pool states at block {TARGET}. "
                 "DB liquidity snapshots + on-chain scalars read at TARGET; "
                 "populated by capture_fee1_v3v4v3_fixture.py."),
        "target_block": TARGET,
        "v4_hop": {
            "hop_index": RECORDED_V4_HOP_INDEX,
            "zero_for_one": False,  # the live repro V4 hop
            "input": str(RECORDED_V4_INPUT),
            "predicted_output": str(RECORDED_V4_PREDICTED),
            "onchain_actual": str(RECORDED_V4_ACTUAL),
        },
        "pools": {"v3_0": v3a, "v4": v4, "v3_2": v3c},
        "path": [
            {"hop": 0, "pool": "v3_0", "zero_for_one": HOP0_ZFO},
            {"hop": 1, "pool": "v4", "zero_for_one": HOP1_ZFO},
            {"hop": 2, "pool": "v3_2", "zero_for_one": HOP2_ZFO},
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/fee1_v3v4v3_block{TARGET}.json"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    for n, p in (("v3_0", v3a), ("v4", v4), ("v3_2", v3c)):
        print("%s ticks=%d sqrt=%s liq=%s tick=%s"
              % (n, len(p["tick_data"]), p["sqrt_price_x96"], p["liquidity"], p["tick"]))
    print("v4_hop:", fixture["v4_hop"])


if __name__ == "__main__":
    main()
