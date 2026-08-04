#!/usr/bin/env python3
"""Capture the exact V3→V2→V3 pool states for the live path-11354 IIA failure
at block 25678283 into a JSON fixture.

Modeled 1:1 on `capture_fee1_v3v4v3_fixture.py` (the V3-V4-V3 overdraw harness)
and `capture_path10956_v3v3v2_fixture.py` (the V3-V3-V2 IIA harness) — the
DB-liquidity-snapshot + on-chain-scalar split both use. This captures the
V3-V2-V3 path whose V2 hop produced a 1-wei sim-side under-delivery:

  hop0 V3 0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36  WETH/USDT  fee=3000 zfo=true
  hop1 V2 0x648Ef94C6D205016A385Fb4C54aB6e422F5142c5  USDT/stETH fee=997/1000 zfo=false
  hop2 V3 0x63818BbDd21E69bE108A23aC1E84cBf66399Bd7D  stETH/WETH fee=10000 zfo=true

[sim-diag] block=25678283 optimal_input=14822228230440
           hop_outputs=[27415, 15166900278115, 15000409935601]
[sim-revert-swap] hop=1 family=V2 emitter=0x648Ef94C... actual_in=27415
                  actual_out=15166900278114 predicted=15166900278115 matched=false
→ V2_SWAP_CALC delivered 1 wei less stETH (…114) than the solver predicted
  (…115) → the outer V3c hop's fixed input (…115) was not met → IIA.

The fixture is consumed by
`rust/crates/degenbot/examples/path11354_v3v2v3_solver_fixture.rs`, which
rebuilds the three pools, runs the production Möbius solver, and compares the
solver's V2-hop output against the byte-exact constant-product oracle and the
recorded predicted/actual from the log — localizing the 1-wei to the sim side
if the solver is byte-consistent with constant-product at the on-chain reserves.

Usage: run with the DB present (DB liquidity snapshot + on-chain scalars at
TARGET read via cast):

    python3 scripts/capture_path11354_v3v2v3_fixture.py
"""

import json
import os
import sqlite3
import subprocess

RPC = "http://host.containers.internal:8545"
TARGET = 25678789  # solve block (path 11354) from [sim-diag] (live recurrence, 2nd data point)
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")

POOLS = {
    "v3_0": {  # hop0 WETH/USDT 0.30%
        "family": "uniswap_v3",
        "address": "0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36",
        "db_id": 599529,
    },
    "v2_1": {  # hop1 USDT/stETH 0.30%
        "family": "uniswap_v2",
        "address": "0x648Ef94C6D205016A385Fb4C54aB6e422F5142c5",
        "db_id": 546692,
    },
    "v3_2": {  # hop2 stETH/WETH 1.00%
        "family": "uniswap_v3",
        "address": "0x63818BbDd21E69bE108A23aC1E84cBf66399Bd7D",
        "db_id": 606471,
    },
}


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
    cmd = ["cast", "call", addr, "slot0()(uint160,int24,uint16,uint16,uint16,uint8)",
           "--rpc-url", RPC, "--block", str(TARGET)]
    out = [ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    liq = [ln.split()[0] for ln in subprocess.check_output(
        ["cast", "call", addr, "liquidity()(uint128)", "--rpc-url", RPC, "--block", str(TARGET)],
        text=True).splitlines() if ln.strip()][0]
    return int(out[0], 0), int(out[1], 0), int(liq, 0)


def v2_reserves(addr):
    cmd = ["cast", "call", addr, "getReserves()(uint112,uint112,uint32)",
           "--rpc-url", RPC, "--block", str(TARGET)]
    out = [ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    return int(out[0], 0), int(out[1], 0), int(out[2], 0)


def main():
    cur = sqlite3.connect(DB)
    out_pools = {}
    for key, spec in POOLS.items():
        p = {"family": spec["family"], "address": spec["address"]}
        pid = spec["db_id"]
        if spec["family"] == "uniswap_v3":
            (t0, t1, ts, f0, f1, fd) = cur.execute(
                "SELECT p.token0_id,p.token1_id,uv3.tick_spacing,uv3.fee_token0,"
                "uv3.fee_token1,uv3.fee_denominator "
                "FROM pools p JOIN uniswap_v3_pools uv3 ON uv3.pool_id=p.id WHERE p.id=?",
                (pid,)).fetchone()
            tokens = {r[0]: r[1] for r in cur.execute("SELECT id,address FROM erc20_tokens")}
            ticks = cur.execute(
                "SELECT tick,liquidity_net,liquidity_gross FROM liquidity_positions "
                "WHERE pool_id=? ORDER BY tick", (pid,)).fetchall()
            p.update(token0=tokens[t0], token1=tokens[t1], tick_spacing=ts,
                     fee_token0=f0, fee_token1=f1, fee_denominator=fd,
                     tick_data={t: {"liquidity_net": str(n), "liquidity_gross": str(g)}
                                for t, n, g in ticks})
            # on-chain scalars at TARGET (the split-tick-clock driver: DB holds
            # the liquidity snapshot, cast reads the current sqrt/tick/liq).
            sq, tick, liq = v3_scalars(spec["address"])
            p.update(sqrt_price_x96=str(sq), tick=tick, liquidity=str(liq),
                     liquidity_update_block=TARGET)
            print("%s: ticks=%d sqrt=%s liq=%s tick=%s spacing=%s fee=%s"
                  % (key, len(p["tick_data"]), p["sqrt_price_x96"], p["liquidity"],
                     p["tick"], p["tick_spacing"], p["fee_token0"]))
        else:  # uniswap_v2 reserves (live on-chain; DB holds no reserves)
            r0, r1, ts = v2_reserves(spec["address"])
            tokens = {r[0]: r[1] for r in cur.execute("SELECT id,address FROM erc20_tokens")}
            (t0, t1) = cur.execute("SELECT token0_id,token1_id FROM pools WHERE id=?",
                                   (pid,)).fetchone()
            p.update(reserve0=str(r0), reserve1=str(r1), block_number=ts,
                     token0=tokens[t0], token1=tokens[t1])
            print("%s: reserve0=%s reserve1=%s t0=%s t1=%s"
                  % (key, p["reserve0"], p["reserve1"], p["token0"], p["token1"]))
        out_pools[key] = p
    cur.close()

    fixture = {
        "_doc": (f"Exact path-11354 V3-V2-V3 pool states at solve_block {TARGET}. "
                 "DB liquidity snapshots + on-chain scalars read at TARGET."),
        "target_block": TARGET,
        "recorded_solve": {
            "optimal_input": "17959852090245",         # [sim-diag] WETH in
            "hop_outputs": ["33379", "18419152813209", "18216961785892"],
            "v2_hop_index": 1,
            "v2_input": "33379",                           # USDT fed into the V2 pair
            "v2_predicted": "18419152813209",              # solver hop_outputs[1]
            "v2_actual": "18419152813208",                 # [sim-revert-swap] on-chain actual (stETH)
        },
        "pools": out_pools,
        "path": [
            {"hop": 0, "pool": "v3_0", "zero_for_one": True},
            {"hop": 1, "pool": "v2_1", "zero_for_one": False},
            {"hop": 2, "pool": "v3_2", "zero_for_one": True},
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path11354_v3v2v3_block{TARGET}.json"
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    print("v2_hop:", fixture["recorded_solve"])


if __name__ == "__main__":
    main()
