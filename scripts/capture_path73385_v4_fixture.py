#!/usr/bin/env python3
"""Capture the V4 concentrated-liquidity pool state behind the block-25706469
V3-V4-V3 sim failure (paths 73385 / 110399) into a JSON fixture.

The failing live sim (`[sim-fail] ... bucket=empty kind=halt gas=6269`, target
= USDT, on the V4 hop of a V3-V4-V3 USDC->...->USDT path) halts on a depth-8
empty-calldata USDT frame. This capture records the EXACT V4 CL pool state
(pool_id `0x8aa4e11c...`, USDC/USDT, fee=10, tick_spacing=1) at the solve
block, so the gas-probe harness can measure, on the REAL v4-core PoolManager,
how much gas this concentrated-liquidity swap alone consumes at the recorded
input — answering whether "the concentrated-liquidity swaps" legitimately
consume ~16.7M gas, or whether the halt is a local gas-starvation, not total
exhaustion.

Modeled 1:1 on `capture_fee1_v3v4v3_fixture.py` (DB liquidity snapshot + cast
on-chain scalars at TARGET). The output JSON feeds
`path73385_v4_gas_probe` (rust/crates/degenbot/examples).

Usage (overrides come from env, see below):

    FIX_TARGET=25706469 python3 scripts/capture_path73385_v4_fixture.py
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


TARGET = _env_int("FIX_TARGET", 25706469)  # solve block from the live sim-fail log
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")
SV = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"  # V4 StateView

# The failing V4 hop identity (path 73385 / 110399 at block 25706469):
#   USDC(0)->USDT(1), fee_currency0=10, tick_spacing=1
V4_PID = _env("FIX_V4_PID",
              "0x8aa4e11cbdf30eedc92100f4c8a31ff748e201d44712cc8c90d189edaa8e4e47")

# Recorded solve (from the live `[sim-diag]` / `[sim-revert-swap]` lines):
#   path 73385: optimal_input=44421383036608956,
#               hop_outputs=[85060245, 85097884, 44421879564949974]
#   V4 hop (index 1): zero_for_one=True (sell USDC/currency0 buy USDT/currency1),
#       actual_in=85060245, predicted=85097884, onchain actual_out=85097881
V4_ZFO = _env("FIX_V4_ZFO", "1") == "1"
V4_INPUT = _env_int("FIX_V4_INPUT", 85060245)
V4_PREDICTED = _env_int("FIX_V4_PREDICTED", 85097884)
V4_ACTUAL = _env_int("FIX_V4_ACTUAL", 85097881)


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


def v4_scalars():
    cmd = ["cast", "call", SV, "getSlot0(bytes32)(uint160,int24,uint24,uint24)", V4_PID,
           "--rpc-url", RPC, "--block", str(TARGET)]
    out = [ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    sq, tick, protocol_fee, lp_fee = int(out[0], 0), int(out[1], 0), int(out[2], 0), int(out[3], 0)
    liquidity = int([ln.split()[0] for ln in subprocess.check_output(
        ["cast", "call", SV, "getLiquidity(bytes32)(uint128)", V4_PID,
         "--rpc-url", RPC, "--block", str(TARGET)], text=True).splitlines() if ln.strip()][0], 0)
    return sq, tick, liquidity, protocol_fee, lp_fee


def load_v4_pool(cur):
    (t0, t1, f0, f1, denom, ts, ublk, mp) = cur.execute(
        """SELECT t0.address, t1.address, uv4.fee_currency0, uv4.fee_currency1,
                  uv4.fee_denominator, uv4.tick_spacing, uv4.liquidity_update_block,
                  uv4.managed_pool_id
           FROM uniswap_v4_pools uv4
           JOIN erc20_tokens t0 ON t0.id=uv4.currency0_id
           JOIN erc20_tokens t1 ON t1.id=uv4.currency1_id
           WHERE uv4.pool_hash=?""", (V4_PID,)).fetchone()
    tick_rows = cur.execute(
        "SELECT tick, liquidity_net, liquidity_gross FROM managed_pool_liquidity_positions "
        "WHERE managed_pool_id=?", (mp,)).fetchall()
    tick_data = {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in tick_rows}
    return {"family": "uniswap_v4", "pool_manager":
            "0x000000000004444c5dc75cb358380d2e3de08a90", "pool_id": V4_PID,
            "currency0": t0, "currency1": t1, "fee_currency0": f0, "fee_currency1": f1,
            "fee_denominator": denom, "tick_spacing": ts, "liquidity_update_block": ublk,
            "tick_data": tick_data}


def main():
    cur = sqlite3.connect(DB)
    v4 = load_v4_pool(cur)
    cur.close()
    sq, tick, liq, pf, lf = v4_scalars()
    v4.update(sqrt_price_x96=str(sq), tick=tick, liquidity=str(liq),
              protocol_fee=pf, lp_fee=lf)

    fixture = {
        "_doc": (f"Exact V4 CL pool state (path 73385 V3-V4-V3, USDC/USDT fee10 ts1) at "
                 f"block {TARGET}. DB liquidity snapshot + on-chain scalars read at "
                 f"TARGET; populated by capture_path73385_v4_fixture.py."),
        "target_block": TARGET,
        "recorded_solve": {
            "optimal_input": "44421383036608956",
            "hop_outputs": ["85060245", "85097884", "44421879564949974"],
            "v4_hop_index": 1,
            "v4_zero_for_one": V4_ZFO,
            "v4_input": str(V4_INPUT),
            "v4_predicted_output": str(V4_PREDICTED),
            "v4_onchain": str(V4_ACTUAL),
        },
        "pools": {"v4": v4},
        "path": [{"hop": 1, "pool": "v4", "zero_for_one": V4_ZFO}],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path73385_v4_block{TARGET}.json"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    print("v4 ticks=%d sqrt=%s liq=%s tick=%s protocol_fee=%d lp_fee=%d"
          % (len(v4["tick_data"]), v4["sqrt_price_x96"], v4["liquidity"], v4["tick"],
             v4["protocol_fee"], v4["lp_fee"]))
    print("recorded_solve:", fixture["recorded_solve"])


if __name__ == "__main__":
    main()
