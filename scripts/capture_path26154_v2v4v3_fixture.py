#!/usr/bin/env python3
"""Capture the exact V2→V4→V3 pool states for the path-26154 empty-Halt sim
failure at block 25700805 into a JSON fixture.

Modeled on `capture_fee1_v3v4v3_fixture.py` (V3/V4 on-chain scalars + DB
tick_data snapshot) and `capture_path205_v2v4v3_fixture.py` (V2 getReserves).
The live bot halted loudly (ADR-021 tripwire, rc=3) on this path:

    [sim-fail] path=26154 type=V2-V4-V3 bucket=empty revert@depth=6
               target=PoolManager label=empty kind=halt gas=4463235 revert=0x
    [sim-bals] path=26154 ... combined d=+0        <- whole path reverted

Path shape (WETH→WETH backrun):
    hop0 V2 Sushi  MATIC/WETH   zfo=False  (WETH→MATIC)
    hop1 V4        UNI/MATIC    zfo=False  (MATIC→UNI)  <- THE failing leg
    hop2 V3 Uni    UNI/WETH     zfo=True   (UNI→WETH)

Investigation verdict (this capture pins it):
  * All three pools' ON-CHAIN scalars at TARGET match the bot's solver state
    exactly — the solver had faithful reserves/sq/liquidity (V2 reserve0/1,
    V4 sq/liq, V3 sq/tick/liq all match the live [solver-st] line).
  * The V4 leg reverts EMPTY because the pool's DB-tracked tickmap is a SINGLE
    tracked position `[-257352, 35067]` at current tick 35050: the MATIC→UNI
    swap (zfo=false, pushing tick UP) has only ~17 ticks of headroom before
    liquidity hits zero at the range top → the V4 swap cannot fill → empty
    Halt (no revert data). That is the ADR-021-tripwire halt the live bot hit.
  * OPEN HYPOTHESIS (matches the stale/bad-state theory): the DB V4 tickmap
    is from liquidity_update_block=22472469, ~3.3M blocks before TARGET. If
    the on-chain pool at TARGET has MORE positions than this single band, the
    swap may be genuinely fillable on-chain and the empty Halt is an artifact
    of the bot's STALE/incomplete tracked tickmap. Settle with a Tier-3 oracle
    (revm vs real PoolManager bytecode + on-chain state at TARGET).

Run (DB present + archive RPC on host.containers.internal:8545):

    python3 scripts/capture_path26154_v2v4v3_fixture.py
"""

import json
import os
import sqlite3
import subprocess

RPC = "http://host.containers.internal:8545"
TARGET = 25700805  # solve block from the live [sim-diag]
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")
SV = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"  # V4 StateView

# ---- pool identity (resolved against the DB + live [sim-fixture]) ----
V2_POOL = {
    "db_id": 31763,
    "kind": "sushiswap_v2",
    "address": "0x7f8F7Dd53D1F3ac1052565e3ff451D7fE666a311",
    "fee_token0": 3, "fee_token1": 3, "fee_denominator": 1000,  # 0.30%
}
V3_POOL = {
    "db_id": 599587,
    "kind": "uniswap_v3",
    "address": "0xfaA318479b7755b2dBfDD34dC306cb28B420Ad12",
    "fee_token0": 500, "fee_token1": 500, "fee_denominator": 1000000,  # 0.05%
}
V4_MGR = "0x000000000004444c5dc75cb358380d2e3de08a90"  # canonical PoolManager
V4_PID = "0x929b9b092b066f35f70943ba7e03de5baf9c1d11c98cd02de2258f0e0eec2d40"
V4_MANAGED_ID = 3032  # DB managed_pool_id for the liquidity-positions snapshot

# ---- recorded solve (path 26154, block 25700805, from [sim-diag]/[sim-fixture]) ----
RECORDED = {
    "optimal_input": "604293636570",           # in-token (WETH) fed to hop0
    "hop_outputs": ["15351327867202109", "460882096151249", "975063101966"],
    "v4_hop_index": 1,
    "v4_zero_for_one": False,                  # MATIC→UNI (token1→token0)
    "v4_input": "15351327867202109",           # hop0 WETH→MATIC out, fed to V4
    "v4_predicted_output": "460882096151249",  # solver hop_outputs[1]
    "v4_onchain": "EMPTY-HALT",                # actual: V4 swap reverted empty
    "sim_bucket": "empty",
}

# ---- per-hop direction (matches live path) ----
HOP0_ZFO = False
HOP1_ZFO = False
HOP2_ZFO = True


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
    cmd = ["cast", "call", addr, "slot0()(uint160,int24,uint16,uint16,uint16,uint8,bool)",
           "--rpc-url", RPC, "--block", str(TARGET)]
    out = [ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    sq, tick = int(out[0], 0), int(out[1], 0)
    liq = int([ln.split()[0] for ln in subprocess.check_output(
        ["cast", "call", addr, "liquidity()(uint128)", "--rpc-url", RPC, "--block", str(TARGET)],
        text=True).splitlines() if ln.strip()][0], 0)
    return sq, tick, liq


def v4_scalars():
    def cc(sig, types):
        cmd = ["cast", "call", SV, sig, V4_PID, "--rpc-url", RPC, "--block", str(TARGET)]
        return [int(ln.split()[0], 0) for ln in
                subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    sq, tick, protocol_fee, lp_fee = cc("getSlot0(bytes32)(uint160,int24,uint24,uint24)",
                                        "(uint160,int24,uint24,uint24)")
    liq = cc("getLiquidity(bytes32)(uint128)", "(uint128)")[0]
    return sq, tick, liq, protocol_fee, lp_fee


def load_v2_pool(cur):
    (t0, t1) = cur.execute("SELECT token0_id, token1_id FROM pools WHERE id=?",
                           (V2_POOL["db_id"],)).fetchone()
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id, address, symbol FROM erc20_tokens")}
    r0, r1, ts = v2_reserves(V2_POOL["address"])
    return {"family": V2_POOL["kind"], "address": V2_POOL["address"],
            "token0": tokens.get(t0), "token1": tokens.get(t1),
            "reserve0": str(r0), "reserve1": str(r1), "block_number": ts,
            "fee_token0": V2_POOL["fee_token0"], "fee_token1": V2_POOL["fee_token1"],
            "fee_denominator": V2_POOL["fee_denominator"]}


def load_v3_pool(cur):
    (t0, t1, ts, f0, f1, fd) = cur.execute(
        "SELECT p.token0_id, p.token1_id, u.tick_spacing, u.fee_token0, u.fee_token1, "
        "u.fee_denominator FROM pools p JOIN uniswap_v3_pools u ON u.pool_id=p.id "
        "WHERE p.id=?", (V3_POOL["db_id"],)).fetchone()
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id, address, symbol FROM erc20_tokens")}
    rows = cur.execute("SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions "
                       "WHERE pool_id=?", (V3_POOL["db_id"],)).fetchall()
    tick_data = {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in rows}
    sq, tick, liq = v3_scalars(V3_POOL["address"])
    return {"family": V3_POOL["kind"], "address": V3_POOL["address"],
            "token0": tokens.get(t0), "token1": tokens.get(t1),
            "tick_spacing": ts, "fee_token0": f0, "fee_token1": f1, "fee_denominator": fd,
            "liquidity_update_block": TARGET, "tick_data": tick_data,
            "sqrt_price_x96": str(sq), "tick": tick, "liquidity": str(liq)}


def load_v4_pool(cur):
    (t0, t1, f0, f1, fd, ts, mpid, ublk) = cur.execute(
        "SELECT uv4.currency0_id, uv4.currency1_id, uv4.fee_currency0, uv4.fee_currency1, "
        "uv4.fee_denominator, uv4.tick_spacing, uv4.managed_pool_id, uv4.liquidity_update_block "
        "FROM uniswap_v4_pools uv4 WHERE uv4.pool_hash=?", (V4_PID,)).fetchone()
    tokens = {r[0]: (r[1], r[2]) for r in cur.execute("SELECT id, address, symbol FROM erc20_tokens")}
    rows = cur.execute("SELECT tick, liquidity_net, liquidity_gross FROM "
                       "managed_pool_liquidity_positions WHERE managed_pool_id=?",
                       (mpid,)).fetchall()
    tick_data = {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in rows}
    sq, tick, liq, protocol_fee, lp_fee = v4_scalars()
    c0 = tokens.get(t0)
    c1 = tokens.get(t1)
    return {"family": "uniswap_v4", "pool_manager": V4_MGR, "pool_id": V4_PID,
            "currency0": c0[0] if c0 else None, "currency1": c1[0] if c1 else None,
            "currency0_symbol": c0[1] if c0 else None, "currency1_symbol": c1[1] if c1 else None,
            "db_currency0": tokens.get(t0)[0] if tokens.get(t0) else None,
            "db_currency1": tokens.get(t1)[0] if tokens.get(t1) else None,
            "fee_currency0": f0, "fee_currency1": f1, "fee_denominator": fd,
            "tick_spacing": ts, "managed_pool_id": mpid,
            "liquidity_update_block": ublk, "tick_data": tick_data,
            "sqrt_price_x96": str(sq), "tick": tick, "liquidity": str(liq),
            "protocol_fee": protocol_fee, "lp_fee": lp_fee}


def main():
    cur = sqlite3.connect(DB)
    v2 = load_v2_pool(cur)
    v3 = load_v3_pool(cur)
    v4 = load_v4_pool(cur)
    cur.close()

    fixture = {
        "_doc": (f"Exact V2-V4-V3 path-26154 empty-Halt pool states at block {TARGET}. "
                 "DB tick_data snapshots + on-chain scalars read at TARGET; "
                 "populated by scripts/capture_path26154_v2v4v3_fixture.py. "
                 "V4 leg reverts EMPTY because the tracked position "
                 f"[-257352, 35067] at current tick {v4['tick']} leaves ~17 ticks "
                 "of headroom above before liquidity hits zero. OPEN: DB V4 "
                 f"tickmap is from block {v4['liquidity_update_block']} (3.3M blocks "
                 f"before TARGET) - the on-chain pool may hold more positions at "
                 f"TARGET; settle with a Tier-3 oracle."),
        "target_block": TARGET,
        "recorded_solve": RECORDED,
        "pools": {"v2_0": v2, "v4": v4, "v3_2": v3},
        "path": [
            {"hop": 0, "pool": "v2_0", "zero_for_one": HOP0_ZFO},
            {"hop": 1, "pool": "v4", "zero_for_one": HOP1_ZFO},
            {"hop": 2, "pool": "v3_2", "zero_for_one": HOP2_ZFO},
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path26154_v2v4v3_block{TARGET}.json"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    for n, p in (("v2_0", v2), ("v4", v4), ("v3_2", v3)):
        if p["family"] == "sushiswap_v2":
            print("%s r0=%s r1=%s t0=%s t1=%s" % (n, p["reserve0"], p["reserve1"],
                                                   p["token0"], p["token1"]))
        else:
            print("%s ticks=%d sqrt=%s liq=%s tick=%s"
                  % (n, len(p["tick_data"]), p["sqrt_price_x96"], p["liquidity"], p["tick"]))
    print("v4 currencies (path view):", v4["currency0"], v4["currency1"],
          "; DB-currencies:", v4["db_currency0"], v4["db_currency1"])
    print("recorded_solve:", fixture["recorded_solve"])


if __name__ == "__main__":
    main()
