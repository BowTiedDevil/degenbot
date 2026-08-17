#!/usr/bin/env python3
"""Capture the exact V3→V4→V2 pool states for the live path-110302 V2 K-invariant
failure at block 25711761 into a JSON fixture.

Modeled 1:1 on `capture_path5000_v2v4v3_fixture.py` (V3/V4 on-chain scalars via
cast + DB tick_data snapshot) and `capture_path11053_v3v2v2_fixture.py` (V2
getReserves via cast). The live bot halted loudly (ADR-021 tripwire, rc=3) on
this path:

    [sim-fail] path=110302 type=V3-V4-V2 bucket=UniswapV2: K revert@depth=4
               target=0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852 kind=revert
               gas=29850 revert=0x08c379a0…"UniswapV2: K"
    [sim-diag] optimal_input=30261840128124434
               hop_outputs=[58199277, 58233015, 30263206881291235]
    [sim-revert-swap] hop=1 family=V4 ... actual_in=58199277 actual_out=58233015
                      predicted=58233015 matched=true   <- hop1 V4 OK
    [sim-fixture] hop[2] V2 pool=0x0d4a11d5eeaac28ec3f61d100daf4d40471f1852
                  t0=WETH t1=USDT fee=30 zfo=False       <- THE failing leg

Path shape (WETH→WETH settlement arbitrage):
    hop0 V3 PancakeSwap USDC/WETH  zfo=False  (WETH→USDC)   fee=100
    hop1 V4        USDC/USDT       zfo=True   (USDC→USDT)   fee=8  spacing=1
    hop2 V2 UniV2  WETH/USDT       zfo=False  (USDT→WETH)   fee=997/1000  <- fails

Recorded V2 hop (index 2): input 58233015 USDT -> predicted 30263206881291235
WETH. But byte-exact constant-product getAmountOut(58233015) = 30263206361603722
(over-prediction 519,687,513 wei = getAmountOut(58233016), one wei MORE input than
hop1 produced). The executor encodes V2 as an EXACT-OUT swap
(amount0Out=30263206881291235), which needs getAmountIn=58233016 USDT but only
58233015 is delivered -> 1 wei short -> UniswapV2: K.

Consumed by
`rust/crates/degenbot/examples/path110302_v3v4v2_solver_fixture.rs`.

Usage: run with the DB present (DB liquidity snapshot + on-chain scalars at
TARGET read via cast):

    python3 scripts/capture_path110302_v3v4v2_fixture.py
"""

import json
import os
import sqlite3
import subprocess

RPC = "http://host.containers.internal:8545"
TARGET = 25711761  # solve block (path 110302) from [sim-diag]
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")
SV = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"  # V4 StateView

# ---- pool identity (resolved against the DB + live [sim-fixture]) ----
V3_POOL = {  # hop0 PancakeSwapV3 USDC/WETH fee=100
    "family": "pancakeswap_v3",
    "address": "0x1445F32D1A74872bA41f3D8cF4022E9996120b31",
    "db_id": 621009,
}
V4_MGR = "0x000000000004444c5dc75cb358380d2e3de08a90"  # canonical PoolManager
V4_PID = "0x395f91b34aa34a477ce3bc6505639a821b286a62b1a164fc1887fa3a5ef713a5"
V4_MANAGED_ID = 3380  # DB managed_pool_id for the liquidity-positions snapshot
V2_POOL = {  # hop2 UniV2 WETH/USDT fee=997/1000
    "family": "uniswap_v2",
    "address": "0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852",
    "db_id": 145,
    "fee_gamma": 997,
    "fee_denom": 1000,
}

# ---- recorded solve (path 110302, block 25711761, from [sim-diag]/[sim-fixture]) ----
# The V2 hop's on-chain `amount0Out` decoded from the revert calldata
# (0x022c0d9f... amount0Out=0x6b8439efec6be3=30263206881291235) equals the
# recorded predicted output; store it as v2_predicted.
RECORDED = {
    "optimal_input": "30261840128124434",           # in-token (WETH) fed to hop0
    "hop_outputs": ["58199277", "58233015", "30263206881291235"],
    "sim_bucket": "UniswapV2: K",
    "v2_hop_index": 2,
    "v2_zero_for_one": False,                       # USDT→WETH (token1→token0)
    "v2_input": "58233015",                         # hop1 USDT out, fed to V2
    "v2_predicted": "30263206881291235",            # solver hop_outputs[2] == on-chain amount0Out
    "v2_actual": "30263206361603722",               # byte-exact getAmountOut(58233015) — what the pool legitimately pays
}

# ---- per-hop direction (matches live path) ----
HOP0_ZFO = False
HOP1_ZFO = True
HOP2_ZFO = False


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


def v3_scalars(addr):
    cmd = ["cast", "call", addr, "slot0()(uint160,int24,uint16,uint16,uint16,uint8)",
           "--rpc-url", RPC, "--block", str(TARGET)]
    out = [ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    sq, tick = int(out[0], 0), int(out[1], 0)
    liq = int([ln.split()[0] for ln in subprocess.check_output(
        ["cast", "call", addr, "liquidity()(uint128)", "--rpc-url", RPC, "--block", str(TARGET)],
        text=True).splitlines() if ln.strip()][0], 0)
    return sq, tick, liq


def v4_scalars():
    def cc(sig):
        cmd = ["cast", "call", SV, sig, V4_PID, "--rpc-url", RPC, "--block", str(TARGET)]
        return [int(ln.split()[0], 0) for ln in
                subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    sq, tick, protocol_fee, lp_fee = cc(
        "getSlot0(bytes32)(uint160,int24,uint24,uint24)")
    liq = cc("getLiquidity(bytes32)(uint128)")[0]
    return sq, tick, liq, protocol_fee, lp_fee


def main():
    cur = sqlite3.connect(DB)
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id,address FROM erc20_tokens")}
    out_pools = {}

    # hop0: V3 — PancakeSwapV3 fee lives in pancakeswap_v3_pools.
    (t0, t1, ts, f0, f1, fd) = cur.execute(
        "SELECT p.token0_id,p.token1_id,uv3.tick_spacing,uv3.fee_token0,"
        "uv3.fee_token1,uv3.fee_denominator FROM pools p "
        "JOIN pancakeswap_v3_pools uv3 ON uv3.pool_id=p.id WHERE p.id=?",
        (V3_POOL["db_id"],)).fetchone()
    ticks = cur.execute(
        "SELECT tick,liquidity_net,liquidity_gross FROM liquidity_positions "
        "WHERE pool_id=? ORDER BY tick", (V3_POOL["db_id"],)).fetchall()
    p3 = {
        "family": V3_POOL["family"], "address": V3_POOL["address"],
        "token0": tokens[t0], "token1": tokens[t1], "tick_spacing": ts,
        "fee_token0": f0, "fee_token1": f1, "fee_denominator": fd,
        "tick_data": {t: {"liquidity_net": str(n), "liquidity_gross": str(g)}
                      for t, n, g in ticks},
    }
    sq, tick, liq = v3_scalars(V3_POOL["address"])
    p3.update(sqrt_price_x96=str(sq), tick=tick, liquidity=str(liq),
              liquidity_update_block=TARGET)
    out_pools["v3_0"] = p3
    print("v3_0: ticks=%d sqrt=%s liq=%s tick=%s spacing=%s fee=%s"
          % (len(p3["tick_data"]), p3["sqrt_price_x96"], p3["liquidity"],
             p3["tick"], p3["tick_spacing"], p3["fee_token0"]))

    # hop1: V4 — currency + fee + tick data from uniswap_v4_pools / managed pools.
    (pid, c0, c1, f0, f1, fd, ts, ublk) = cur.execute(
        "SELECT pool_hash,currency0_id,currency1_id,fee_currency0,fee_currency1,"
        "fee_denominator,tick_spacing,liquidity_update_block FROM uniswap_v4_pools "
        "WHERE managed_pool_id=?", (V4_MANAGED_ID,)).fetchone()
    ticks = cur.execute(
        "SELECT tick,liquidity_net,liquidity_gross FROM managed_pool_liquidity_positions "
        "WHERE managed_pool_id=? ORDER BY tick", (V4_MANAGED_ID,)).fetchall()
    sq, tick, liq, protocol_fee, lp_fee = v4_scalars()
    p4 = {
        "family": "uniswap_v4", "pool_manager": V4_MGR, "pool_id": pid,
        "currency0": tokens[c0], "currency1": tokens[c1],
        "fee_currency0": f0, "fee_currency1": f1, "fee_denominator": fd,
        "tick_spacing": ts, "managed_pool_id": V4_MANAGED_ID,
        "liquidity_update_block": ublk,
        "tick_data": {t: {"liquidity_net": str(n), "liquidity_gross": str(g)}
                      for t, n, g in ticks},
        "sqrt_price_x96": str(sq), "tick": tick, "liquidity": str(liq),
        "protocol_fee": protocol_fee, "lp_fee": lp_fee,
    }
    out_pools["v4"] = p4
    print("v4: ticks=%d sqrt=%s liq=%s tick=%s fee=%s spacing=%s (ublk=%s)"
          % (len(p4["tick_data"]), p4["sqrt_price_x96"], p4["liquidity"],
             p4["tick"], p4["fee_currency0"], p4["tick_spacing"], ublk))

    # hop2: V2 — reserves on-chain.
    (t0, t1) = cur.execute("SELECT token0_id,token1_id FROM pools WHERE id=?",
                           (V2_POOL["db_id"],)).fetchone()
    r0, r1, ts = v2_reserves(V2_POOL["address"])
    p2 = {
        "family": V2_POOL["family"], "address": V2_POOL["address"],
        "token0": tokens[t0], "token1": tokens[t1],
        "reserve0": str(r0), "reserve1": str(r1), "block_number": ts,
        "fee_gamma": V2_POOL["fee_gamma"], "fee_denom": V2_POOL["fee_denom"],
    }
    out_pools["v2_2"] = p2
    print("v2_2: reserve0=%s reserve1=%s t0=%s t1=%s fee=%s/%s"
          % (p2["reserve0"], p2["reserve1"], p2["token0"], p2["token1"],
             p2["fee_gamma"], p2["fee_denom"]))

    cur.close()

    fixture = {
        "_doc": (f"Exact path-110302 V3-V4-V2 pool states at solve_block {TARGET}. "
                 "DB liquidity snapshots + on-chain scalars/reserves read at TARGET. "
                 "V2 hop over-predicts output by 519,687,513 wei (getAmountOut(58233016) "
                 "vs the true getAmountOut(58233015)=30263206361603722), and the executor "
                 "encodes it as an exact-out swap amount0Out=30263206881291235 which needs "
                 "getAmountIn=58233016 USDT but only 58233015 is delivered -> UniswapV2: K."),
        "target_block": TARGET,
        "recorded_solve": RECORDED,
        "pools": out_pools,
        "path": [
            {"hop": 0, "pool": "v3_0", "zero_for_one": HOP0_ZFO},
            {"hop": 1, "pool": "v4", "zero_for_one": HOP1_ZFO},
            {"hop": 2, "pool": "v2_2", "zero_for_one": HOP2_ZFO},
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path110302_v3v4v2_block{TARGET}.json"
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    print("recorded_solve:", fixture["recorded_solve"])


if __name__ == "__main__":
    main()
