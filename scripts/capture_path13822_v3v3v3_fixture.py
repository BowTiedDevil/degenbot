#!/usr/bin/env python3
"""Capture the exact pool states for path-13822 (block 25696004) into a JSON fixture.

Path 13822 is V3-V3-V3 (route WETH->DAI->USDC->WETH):
  hop0 V3 0x60594a405d53811d3bc4766596efd80fd545a270  DAI/WETH  fee 500  spacing 10 zfo=False
  hop1 V3 0x5777d92f208679db4b9778590fa3cab3ac9e2168  DAI/USDC  fee 100  spacing  1 zfo=True   <-- 1-wei over-predict
  hop2 V3 0x1445f32d1a74872ba41f3d8cf4022e9996120b31  USDC/WETH fee 100  spacing  1 zfo=True   (pancakeswap_v3)

This is a NEW over-prediction class: the AGENTS.md UO3JM4 thin-`tick_spacing=1`
family, but the failing hop is a V3 pool (not V4). hop1's solver output
`4838936` over-predicts the sim's actual `4838935` by exactly 1 wei -> the take
overdrafts -> the executor reverts "IIA" -> the fail-fast SystemExit(3).

Recorded solve (ground truth from the trapping [sim-diag] / [sim-revert-swap]):
  optimal_input = 2544421820026072
  hop_outputs   = [4839212171793604540, 4838936, 2544451982555526]
  hop1 actual   = 4838935   (the fix target: solver hop1 MUST equal this, not 4838936)

Verified: DB liquidity snapshots current at the target block (no V3 Mint/Burn in
the (liquidity_update_block, TARGET] window for any of the three pools — asserted
via eth_getLogs topic0 scan); scalars read on-chain at TARGET.
"""

import json
import os
import sqlite3

RPC = "http://host.containers.internal:8545"
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")

# Env-var overrides so one script captures any V3-V3-V3 recurrence (mirrors
# `capture_fee1_v3v4v3_fixture.py`). Defaults are the original path-13822 case;
# override with FIX_TARGET / FIX_V3_2 / FIX_OPTIMAL_INPUT / FIX_HOP_OUTPUTS /
# FIX_HOP1_ACTUAL / FIX_PATH_ID to catch a fresh recurrence.
def _env(name, default):
    v = os.environ.get(name)
    return default if v is None or v == "" else v

def _env_int(name, default):
    return int(_env(name, default))

PATH_ID = _env("FIX_PATH_ID", "13822")
TARGET = _env_int("FIX_TARGET", 25696004)

V3_0 = "0x60594a405d53811d3bc4766596efd80fd545a270"  # DAI/WETH  uniswap_v3
V3_1 = "0x5777d92f208679db4b9778590fa3cab3ac9e2168"  # DAI/USDC  uniswap_v3 (failing)
V3_2 = _env("FIX_V3_2", "0x1445f32d1a74872ba41f3d8cf4022e9996120b31")  # USDC/WETH  default pancakeswap_v3

OPTIMAL_INPUT = _env_int("FIX_OPTIMAL_INPUT", 2544421820026072)
HOP_OUTPUTS = [int(x, 0) for x in _env("FIX_HOP_OUTPUTS", "4839212171793604540,4838936,2544451982555526").split(",")]
HOP1_ACTUAL = _env_int("FIX_HOP1_ACTUAL", 4838935)
HOP1_PREDICTED = _env_int("FIX_HOP1_PREDICTED", HOP_OUTPUTS[1])

UNISWAP_V3_MINT_EVENT_HASH = "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde"
UNISWAP_V3_BURN_EVENT_HASH = "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c"

SLOT0 = "0x3850c7bd"
LIQ = "0x1a686502"


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


def load_v3_pool(cur, pool_id, addr):
    kind = cur.execute("SELECT kind FROM pools WHERE id=?", (pool_id,)).fetchone()[0]
    tbl = {
        "uniswap_v3": "uniswap_v3_pools",
        "pancakeswap_v3": "pancakeswap_v3_pools",
        "sushiswap_v3": "sushiswap_v3_pools",
        "aerodrome_v3": "aerodrome_v3_pools",
    }[kind]
    (t0, t1, ts, ub, f0, f1, fd) = cur.execute(
        f"""SELECT p.token0_id, p.token1_id, {tbl}.tick_spacing,
                  {tbl}.liquidity_update_block, {tbl}.fee_token0, {tbl}.fee_token1,
                  {tbl}.fee_denominator
           FROM pools p JOIN {tbl} ON {tbl}.pool_id=p.id WHERE p.id=?""",
        (pool_id,),
    ).fetchone()
    tokens = {r[0]: r[1] for r in cur.execute("SELECT id, address FROM erc20_tokens")}
    tick_rows = cur.execute(
        "SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions WHERE pool_id=?",
        (pool_id,),
    ).fetchall()
    tick_data = {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in tick_rows}
    return {
        "family": kind,
        "address": addr,
        "token0": tokens[t0],
        "token1": tokens[t1],
        "tick_spacing": ts,
        "fee_token0": f0,
        "fee_token1": f1,
        "fee_denominator": fd,
        "liquidity_update_block": ub,
        "tick_data": tick_data,
    }


def verify_no_liquidity_events(pool_id, addr, ub):
    """Assert zero V3 Mint/Burn in (ub, TARGET] so the DB tick map is current."""
    if _env("FIX_ALLOW_STALE", "0") == "1":
        return
    if ub is None or TARGET <= ub:
        return
    for topic in (UNISWAP_V3_MINT_EVENT_HASH, UNISWAP_V3_BURN_EVENT_HASH):
        logs = rpc("eth_getLogs", [{
            "address": addr,
            "topics": [topic],
            "fromBlock": hex(ub + 1),
            "toBlock": hex(TARGET),
        }])
        if logs:
            raise RuntimeError(
                f"pool_id={pool_id} addr={addr} has {len(logs)} liquidity "
                f"events in ({ub},{TARGET}] — DB tick map NOT current at TARGET"
            )


def v3_scalars(addr):
    import subprocess

    def cc(sig, arg=None):
        cmd = ["cast", "call", addr, sig, "--rpc-url", RPC, "--block", str(TARGET)]
        out = subprocess.check_output(cmd, text=True)
        parts = [ln.split()[0] for ln in out.splitlines() if ln.strip()]
        return int(parts[arg], 0) if arg is not None else [int(p, 0) for p in parts[:6]]

    vals = cc("slot0()(uint160,int24,uint16,uint16,uint16,uint8,bool)")
    liquidity = cc("liquidity()(uint128)", 0)
    return vals[0], vals[1], liquidity


def main():
    cur = sqlite3.connect(DB)
    pools = {}
    for key, addr in (("v3_0", V3_0), ("v3_1", V3_1), ("v3_2", V3_2)):
        pid = cur.execute("SELECT id FROM pools WHERE lower(address)=?", (addr.lower(),)).fetchone()[0]
        p = load_v3_pool(cur, pid, addr)
        verify_no_liquidity_events(pid, addr, p["liquidity_update_block"])
        pools[key] = p
    cur.close()

    for key in ("v3_0", "v3_1", "v3_2"):
        sq, tick, liq = v3_scalars(pools[key]["address"])
        pools[key].update(sqrt_price_x96=str(sq), tick=tick, liquidity=str(liq))
        pools[key]["liquidity_update_block"] = TARGET  # verified current at target

    fixture = {
        "_doc": (
            f"Exact V3-V3-V3 path-{PATH_ID} pool states at block {TARGET}. Route "
            "WETH->DAI->USDC->WETH. Verified: DB liquidity snapshots current at "
            "TARGET (no Mint/Burn in (liquidity_update_block, TARGET]); scalars "
            "read on-chain at TARGET. hop1 (0x5777d92f, DAI/USDC fee 100 spacing 1) "
            f"is the over-prediction: solver {HOP1_PREDICTED} vs sim actual {HOP1_ACTUAL}."
        ),
        "target_block": TARGET,
        "recorded_solve": {
            "optimal_input": OPTIMAL_INPUT,
            "hop_outputs": HOP_OUTPUTS,
            "hop1_actual": HOP1_ACTUAL,
        },
        "pools": {"v3_0": pools["v3_0"], "v3_1": pools["v3_1"], "v3_2": pools["v3_2"]},
        "path": [
            {"hop": 0, "kind": "v3", "pool": "v3_0", "zero_for_one": False},
            {"hop": 1, "kind": "v3", "pool": "v3_1", "zero_for_one": True},
            {"hop": 2, "kind": "v3", "pool": "v3_2", "zero_for_one": True},
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path{PATH_ID}_v3v3v3_block{TARGET}.json"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    for n, p in (("v3_0", pools["v3_0"]), ("v3_1", pools["v3_1"]), ("v3_2", pools["v3_2"])):
        print("%s ticks=%d sqrt=%s liq=%s tick=%s" % (n, len(p["tick_data"]), p["sqrt_price_x96"], p["liquidity"], p["tick"]))
    print("recorded_solve:", fixture["recorded_solve"])


if __name__ == "__main__":
    main()
