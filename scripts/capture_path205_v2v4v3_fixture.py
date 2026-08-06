#!/usr/bin/env python3
"""Capture the on-chain V2 pool state for the live path-205 V2-V4-V3 sim
failure at block 25695845 into a JSON fixture.

Modeled 1:1 on `capture_path11053_v3v2v2_fixture.py` / `capture_path11354_*`
(the V2 sim under-delivery harnesses). The live bot failed a whole family of
V2-V4-V3 paths (201, 203, 205, 206, 207, 208, 209, 210) at block 25695845 —
every hop0 V2 pool read **reserve0=0 reserve1=0** in the sim:

    [v2-calc-trace] pair 0xb01C29F3... BNB/WETH zfo=false fee=25
                    reserve0=0 reserve1=0      <- SIM read (zero!)
    [sim-fail] path=205 type=V2-V4-V3 bucket=empty revert@depth=1
               target=0x0d6d4c3c... sel=0xab5898e8 (execute) kind=revert
               gas=6315 swaps_before=0 captured_swaps=[]
    [dispatch-phase] fan-out EXIT survivors=0   <- fail-fast exit (exit 3)

The on-chain pool at the SAME block 25695845 is healthy — `getReserves()` =
(BNB 1886669965567926, WETH 625016974983751) — i.e. the sim read a zeroed
reserve for a live pool (the path-11354 sim-state-artifact class, but on a
PancakeV2 BNB/WETH pair). This fixture pins the pool's authoritative on-chain
state so a reconstruct probe (e.g. `sim_state_probe_v2_pair` pointed at this
block/pair) can show a FAITHFUL sim reads the real reserves, localizing the
`0` to the sim-side cache/state — not a genuine empty pool.

Consumed by a probe/harness modeled on the other `*_solver_fixture` harnesses.

Usage: run with the DB present + live RPC (reserves/tokens read via cast):

    python3 scripts/capture_path205_v2v4v3_fixture.py
"""

import json
import os
import sqlite3
import subprocess

RPC = "http://host.containers.internal:8545"
TARGET = 25695845  # solve block (paths 201-210) from [sim-diag]
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")

# hop0 V2 BNB/WETH — the pool the sim read as reserve0=0/reserve1=0.
POOL = {
    "family": "pancakeswap_v2",
    "address": "0xb01C29F391f2abC76E7dBDb62E5fbE62bA9AC4Ed",
    "db_id": 170624,
    "fee_gamma": 9975,  # fee=25 -> 9975/10000
    "fee_denom": 10000,
}
# One representative recorded solve (path 205) for the reconstruction harness.
PATH_205 = {
    "optimal_input": "5085843084476",          # [sim-diag] in-token amount
    "hop_outputs": ["15190397493966", "9757", "5123961542619"],
    "v2_hop_index": 0,
    "v2_input": "5085843084476",
    "v2_predicted": "15190397493966",
    "sim_read_reserve0": "0",                  # [v2-calc-trace] the anomaly
    "sim_read_reserve1": "0",
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


def v2_reserves(addr):
    cmd = ["cast", "call", addr, "getReserves()(uint112,uint112,uint32)",
           "--rpc-url", RPC, "--block", str(TARGET)]
    out = [ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    return int(out[0], 0), int(out[1], 0), int(out[2], 0)


def main():
    cur = sqlite3.connect(DB)
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id,address FROM erc20_tokens")}
    (t0, t1) = cur.execute("SELECT token0_id,token1_id FROM pools WHERE id=?",
                           (POOL["db_id"],)).fetchone()
    r0, r1, ts = v2_reserves(POOL["address"])
    p = {
        "family": POOL["family"],
        "address": POOL["address"],
        "token0": tokens[t0],
        "token1": tokens[t1],
        "reserve0": str(r0),
        "reserve1": str(r1),
        "block_number": ts,
        "fee_gamma": POOL["fee_gamma"],
        "fee_denom": POOL["fee_denom"],
    }
    cur.close()
    print("v2_hop0: reserve0=%s reserve1=%s t0=%s t1=%s fee=%s/%s"
          % (p["reserve0"], p["reserve1"], p["token0"], p["token1"],
             p["fee_gamma"], p["fee_denom"]))

    fixture = {
        "_doc": (f"Exact path-205 (V2-V4-V3 family) hop0 V2 BNB/WETH on-chain "
                 f"state at solve_block {TARGET}. The live sim read "
                 "reserve0=0 reserve1=0 for this pool at the same block."),
        "target_block": TARGET,
        "recorded_solve": PATH_205,
        "pools": {"v2_0": p},
        "path": [
            {"hop": 0, "pool": "v2_0", "family": "v2"},
            {"hop": 1, "pool": "v4", "family": "v4"},     # BNB/USDT (unresolved here)
            {"hop": 2, "pool": "v3", "family": "v3"},     # WETH/USDT (unresolved here)
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path205_v2v4v3_block{TARGET}.json"
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)


if __name__ == "__main__":
    main()
