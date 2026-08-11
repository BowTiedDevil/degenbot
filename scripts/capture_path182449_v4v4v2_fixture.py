#!/usr/bin/env python3
"""Capture the exact V4->V4->V2 pool states for the path-182449 terminal-V2
`UniswapV2: K` failure (block 25731019) into a JSON fixture.

Modeled 1:1 on `capture_path110302_v3v4v2_fixture.py` (terminal V2 exact-out
K-over-draw) and `capture_path142603_v4v4v3_fixture.py` (two V4 hops), with the
hop layout adjusted for the LIVE incident:

    path = V4(USDC/WETH 0x5a5c…) -> V4(USDC/USDT 0xe018…) -> V2(WETH/USDT 0x06da0f)
    solve_block = 25731019
    recorded_solve: optimal_input=4820058343725384 (WETH)
                    hop_outputs=[9079140, 9085365, 4820488856043000]
    sim-bucket = UniswapV2: K
    hop2 (V2) input = 9085365 USDT -> predicted = 4820488856043000 WETH

The Möbius solver over-predicts the terminal V2 output by ONE input wei:
getAmountOut(9085365) = 4820488325483365 but the emitted hop_outputs[2]
4820488856043000 == getAmountOut(9085366). The executor encodes V2 as an
exact-OUT swap (amount0Out=hop_outputs[2]), which needs getAmountIn=9085366
USDT but only 9085365 is delivered -> 1-wei short -> UniswapV2: K. Same fault
class as path-110302 (V3-V4-V2); the V4-V4-V2 composer now encodes the terminal
V2 hop as V2_SWAP_CALC to fence it.

Consumed by
`rust/crates/degenbot/examples/path182449_v4v4v2_solver_fixture.rs`.

Usage (DB liquidity snapshot + on-chain scalars/reserves at TARGET via cast):

    python3 scripts/capture_path182449_v4v4v2_fixture.py
"""

import json
import os
import sqlite3
import subprocess

RPC = "http://host.containers.internal:8545"
TARGET = 25731019  # solve block (path 182449) from [sim-diag]
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")
SV = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"  # V4 StateView

# ---- pool identity (DB ids resolved by address/hash on the live chain) ----
V4_A = {  # hop0: V4 USDC/WETH fee=200 spacing=4  (zfo=false, WETH→USDC)
    "managed_id": 32141,
    "pool_id": "0x5a5c7cab5f55c7ea020e97d4fa6dd5d99270e56ce76afa61d8cbddec0af92060",
}
V4_B = {  # hop1: V4 USDC/USDT fee=100 spacing=1  (zfo=true, USDC→USDT)
    "managed_id": 19,
    "pool_id": "0xe018f09af38956affdfeab72c2cefbcd4e6fee44d09df7525ec9dba3e51356a5",
}
V2_C = {  # hop2: SushiV2 WETH/USDT fee=997/1000  (zfo=false, USDT→WETH)
    "db_id": 8078,
    "address": "0x06da0fd433C1A5d7a4faa01111c044910A184553",
    "fee_gamma": 997,
    "fee_denom": 1000,
}

V4_MGR = "0x000000000004444c5dc75cb358380d2e3de08a90"  # canonical PoolManager

# ---- recorded solve (path 182449, block 25731019, from [sim-diag]/[sim-fixture]) ----
# The V2 hop's on-chain `amount0Out` decoded from the revert calldata
# (0x022c0d9f… amount0Out=0x11203585e87df8=4820488856043000) equals the recorded
# predicted output; store it as v2_predicted.
RECORDED = {
    "optimal_input": "4820058343725384",           # in-token (WETH) fed to hop0
    "hop_outputs": ["9079140", "9085365", "4820488856043000"],
    "sim_bucket": "UniswapV2: K",
    "v2_hop_index": 2,
    "v2_zero_for_one": False,                       # USDT→WETH (token1→token0)
    "v2_input": "9085365",                         # hop1 USDT out, fed to V2
    "v2_predicted": "4820488856043000",            # solver hop_outputs[2] == on-chain amount0Out
    "v2_actual": "4820488325483365",               # byte-exact getAmountOut(9085365) — what the pool legitimately pays
}

# ---- per-hop direction (matches live path) ----
HOP0_ZFO = False
HOP1_ZFO = True
HOP2_ZFO = False


def _env_int(name, default):
    v = os.environ.get(name)
    return default if v in (None, "") else int(v)


def _env(name, default):
    v = os.environ.get(name)
    return default if v in (None, "") else v


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


def v4_scalars(pool_id):
    def cc(sig):
        cmd = ["cast", "call", SV, sig, pool_id, "--rpc-url", RPC, "--block", str(TARGET)]
        return [int(ln.split()[0], 0) for ln in
                subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    sq, tick, protocol_fee, lp_fee = cc(
        "getSlot0(bytes32)(uint160,int24,uint24,uint24)")
    liq = cc("getLiquidity(bytes32)(uint128)")[0]
    return sq, tick, liq, protocol_fee, lp_fee


def load_v4(cur, managed_id, pool_hash):
    (t0, t1, f0, f1, fd, ts, ublk) = cur.execute(
        """SELECT currency0_id,currency1_id,fee_currency0,fee_currency1,
                  fee_denominator,tick_spacing,liquidity_update_block
           FROM uniswap_v4_pools WHERE managed_pool_id=?""", (managed_id,)).fetchone()
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id, address FROM erc20_tokens")}
    rows = cur.execute(
        "SELECT tick, liquidity_net, liquidity_gross FROM managed_pool_liquidity_positions "
        "WHERE managed_pool_id=? ORDER BY tick", (managed_id,)).fetchall()
    return {
        "family": "uniswap_v4", "pool_manager": V4_MGR, "pool_id": pool_hash,
        "currency0": tokens[t0], "currency1": tokens[t1],
        "fee_currency0": f0, "fee_currency1": f1, "fee_denominator": fd,
        "tick_spacing": ts, "liquidity_update_block": ublk,
        "tick_data": {t: {"liquidity_net": str(n), "liquidity_gross": str(g)}
                      for t, n, g in rows},
    }


def main():
    cur = sqlite3.connect(DB)
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id, address FROM erc20_tokens")}
    out_pools = {}

    # hop0: V4 USDC/WETH
    v4a = load_v4(cur, V4_A["managed_id"], V4_A["pool_id"])
    sa, ta, qa, pfa, lfa = v4_scalars(V4_A["pool_id"])
    v4a.update(sqrt_price_x96=str(sa), tick=ta, liquidity=str(qa),
               protocol_fee=pfa, lp_fee=lfa)
    out_pools["v4_a"] = v4a
    print("v4_a: ticks=%d sqrt=%s liq=%s tick=%s fee=%s spacing=%s"
          % (len(v4a["tick_data"]), v4a["sqrt_price_x96"], v4a["liquidity"],
             v4a["tick"], v4a["fee_currency0"], v4a["tick_spacing"]))

    # hop1: V4 USDC/USDT
    v4b = load_v4(cur, V4_B["managed_id"], V4_B["pool_id"])
    sb, tb, qb, pfb, lfb = v4_scalars(V4_B["pool_id"])
    v4b.update(sqrt_price_x96=str(sb), tick=tb, liquidity=str(qb),
               protocol_fee=pfb, lp_fee=lfb)
    out_pools["v4_b"] = v4b
    print("v4_b: ticks=%d sqrt=%s liq=%s tick=%s fee=%s spacing=%s"
          % (len(v4b["tick_data"]), v4b["sqrt_price_x96"], v4b["liquidity"],
             v4b["tick"], v4b["fee_currency0"], v4b["tick_spacing"]))

    # hop2: V2 WETH/USDT (reserves on-chain)
    (t0, t1) = cur.execute("SELECT token0_id,token1_id FROM pools WHERE id=?",
                           (V2_C["db_id"],)).fetchone()
    r0, r1, ts = v2_reserves(V2_C["address"])
    out_pools["v2_c"] = {
        "family": "sushiswap_v2", "address": V2_C["address"],
        "token0": tokens[t0], "token1": tokens[t1],
        "reserve0": str(r0), "reserve1": str(r1), "block_number": ts,
        "fee_gamma": V2_C["fee_gamma"], "fee_denom": V2_C["fee_denom"],
    }
    print("v2_c: reserve0=%s reserve1=%s t0=%s t1=%s fee=%s/%s"
          % (out_pools["v2_c"]["reserve0"], out_pools["v2_c"]["reserve1"],
             out_pools["v2_c"]["token0"], out_pools["v2_c"]["token1"],
             out_pools["v2_c"]["fee_gamma"], out_pools["v2_c"]["fee_denom"]))

    cur.close()

    fixture = {
        "_doc": (f"Exact path-182449 V4-V4-V2 pool states at solve_block {TARGET}. "
                 "DB liquidity snapshots + on-chain scalars/reserves read at TARGET. "
                 "Terminal V2 hop over-predicts output by 530,559,635 wei "
                 "(getAmountOut(9085366) vs the true getAmountOut(9085365)=4820488325483365), "
                 "and the executor encodes it as an exact-out swap amount0Out=4820488856043000 "
                 "which needs getAmountIn=9085366 USDT but only 9085365 is delivered "
                 "-> UniswapV2: K."),
        "target_block": TARGET,
        "recorded_solve": RECORDED,
        "pools": out_pools,
        "path": [
            {"hop": 0, "pool": "v4_a", "zero_for_one": HOP0_ZFO},
            {"hop": 1, "pool": "v4_b", "zero_for_one": HOP1_ZFO},
            {"hop": 2, "pool": "v2_c", "zero_for_one": HOP2_ZFO},
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path182449_v4v4v2_block{TARGET}.json"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    print("recorded_solve:", fixture["recorded_solve"])


if __name__ == "__main__":
    main()
