#!/usr/bin/env python3
"""Capture the exact V3→V2→V2 pool states for the live path-11053 V2 sim
under-delivery at block 25695693 into a JSON fixture.

Modeled 1:1 on `capture_path11354_v3v2v3_fixture.py` (the V3-V2-V3 1-wei sim
under-delivery harness) and `capture_fee1_v3v4v3_fixture.py` (the V3-V4-V3
overdraw harness). Captures the V3-V2-V2 path whose V2 hop (the SAME
USDT/stETH pair 0x648Ef94C as path 11354) under-delivered massively in sim:

  hop0 V3 0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36  WETH/USDT  fee=3000 zfo=true
  hop1 V2 0x648Ef94C6D205016A385Fb4C54aB6e422F5142c5  USDT/stETH fee=997/1000 zfo=false
  hop2 V2 0x3cC0B797f95E1d33EcDC8BA23Fb4898060F835eA  stETH/WETH fee=9975/10000 zfo=true

[sim-diag] block=25695693 solve_block=25695693 age=0
           optimal_input=7812466961517
           hop_outputs=[14826, 8246881364465, 8053151054828]
[sim-revert-swap] hop=1 family=V2 emitter=0x648Ef94C... actual_in=14826
                  actual_out=8091930949192 predicted=8246881364465 matched=false
→ the solver predicted 8246881364465 stETH out of hop1, the sim delivered
  8091930949192 (155e9 wei / ~1.88% LOWER). The engine reserves match on-chain
  at the block byte-for-byte, and constant-product (997/1000) @ 14826 == the
  predicted exactly — so this pins whether the solver is reproducible from the
  reconstructed on-chain reserves (exonerating it and localizing the huge
  under-delivery to the sim side) or is a genuine over-prediction.

Consumed by `rust/crates/degenbot/examples/path11053_v3v2v2_solver_fixture.rs`.

Usage: run with the DB present (DB liquidity snapshot + on-chain scalars at
TARGET read via cast):

    python3 scripts/capture_path11053_v3v2v2_fixture.py
"""

import json
import os
import sqlite3
import subprocess

RPC = "http://host.containers.internal:8545"
TARGET = 25695693  # solve block (path 11053) from [sim-diag]
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")

POOLS = {
    "v3_0": {  # hop0 WETH/USDT 0.30%
        "family": "uniswap_v3",
        "address": "0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36",
        "db_id": 599529,
    },
    "v2_1": {  # hop1 USDT/stETH (Uniswap-V2-fee 0.30%: 997/1000)
        "family": "uniswap_v2",
        "address": "0x648Ef94C6D205016A385Fb4C54aB6e422F5142c5",
        "db_id": 546692,
        "fee_gamma": 997,
        "fee_denom": 1000,
    },
    "v2_2": {  # hop2 stETH/WETH (PancakeSwap-V2 0.25%: 9975/10000)
        "family": "pancakeswap_v2",
        "address": "0x3cC0B797f95E1d33EcDC8BA23Fb4898060F835eA",
        "db_id": 175694,
        "fee_gamma": 9975,
        "fee_denom": 10000,
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
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id,address FROM erc20_tokens")}
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
            ticks = cur.execute(
                "SELECT tick,liquidity_net,liquidity_gross FROM liquidity_positions "
                "WHERE pool_id=? ORDER BY tick", (pid,)).fetchall()
            p.update(token0=tokens[t0], token1=tokens[t1], tick_spacing=ts,
                     fee_token0=f0, fee_token1=f1, fee_denominator=fd,
                     tick_data={t: {"liquidity_net": str(n), "liquidity_gross": str(g)}
                                for t, n, g in ticks})
            sq, tick, liq = v3_scalars(spec["address"])
            p.update(sqrt_price_x96=str(sq), tick=tick, liquidity=str(liq),
                     liquidity_update_block=TARGET)
            print("%s: ticks=%d sqrt=%s liq=%s tick=%s spacing=%s fee=%s"
                  % (key, len(p["tick_data"]), p["sqrt_price_x96"], p["liquidity"],
                     p["tick"], p["tick_spacing"], p["fee_token0"]))
        else:  # V2 reserves live (on-chain; DB holds no reserves)
            r0, r1, ts = v2_reserves(spec["address"])
            (t0, t1) = cur.execute("SELECT token0_id,token1_id FROM pools WHERE id=?",
                                   (pid,)).fetchone()
            p.update(reserve0=str(r0), reserve1=str(r1), block_number=ts,
                     token0=tokens[t0], token1=tokens[t1],
                     fee_gamma=spec["fee_gamma"], fee_denom=spec["fee_denom"])
            print("%s: reserve0=%s reserve1=%s t0=%s t1=%s fee=%s/%s"
                  % (key, p["reserve0"], p["reserve1"], p["token0"], p["token1"],
                     p["fee_gamma"], p["fee_denom"]))
        out_pools[key] = p
    cur.close()

    fixture = {
        "_doc": (f"Exact path-11053 V3-V2-V2 pool states at solve_block {TARGET}. "
                 "DB liquidity snapshot + on-chain scalars/reserves read at TARGET."),
        "target_block": TARGET,
        "recorded_solve": {
            "optimal_input": "7812466961517",         # [sim-diag] WETH in
            "hop_outputs": ["14826", "8246881364465", "8053151054828"],
            "v2_hop_index": 1,
            "v2_input": "14826",                      # USDT fed into the V2 pair (hop0 out)
            "v2_predicted": "8246881364465",          # solver hop_outputs[1]
            "v2_actual": "8091930949192",             # [sim-revert-swap] on-chain actual (stETH)
        },
        "pools": out_pools,
        "path": [
            {"hop": 0, "pool": "v3_0", "zero_for_one": True},
            {"hop": 1, "pool": "v2_1", "zero_for_one": False},
            {"hop": 2, "pool": "v2_2", "zero_for_one": True},
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path11053_v3v2v2_block{TARGET}.json"
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    print("v2_hop:", fixture["recorded_solve"])


if __name__ == "__main__":
    main()
