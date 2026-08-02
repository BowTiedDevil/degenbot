#!/usr/bin/env python3
"""Capture the exact pool states for path-13308 (block 25664704) into a JSON fixture.

Path 13308 is V3-V4-V3:
  hop0 V3  0x60594a405d53811d3bc4766596efd80fd545a270  DAI/WETH  fee 500  zfo=False
  hop1 V4  manager 0x000000000004444c5dc75cb358380d2e3de08a90
           pool_id 0xd967702f17f83d907b36e66c9a62eb50ac327432c581d5b273a76519692434be
           DAI/USDC fee 100 zfo=True
  hop2 V3  0x1ac1a8feaaea1900c4166deeed0c11cc10669d36  USDC/WETH fee 500 zfo=True

Verified: all three pools' DB liquidity snapshots are CURRENT at the target block
(no V3 Mint/Burn in the backfill window; the V4 pool's last ModifyLiquidity is
exactly the DB `liquidity_update_block` 25610799 and none follow through 25664704 —
confirmed via topic0=0xf208f491…/topic1=pool_id, the canonical event hash from
degenbot-decoders). Scalars read at 25664704 via the on-chain V3 slot0()/liquidity()
and the StateView getSlot0()/getLiquidity(); v3_0 and v4 liquidity match the bot's
captured post-swap values byte-for-byte (sqrt/tick 1 step apart = pre-arb vs mid-swap).

Recorded solve (ground truth from the trapping [sim-diag]):
  optimal_input = 1982369771046931
  hop_outputs   = [3720117117094320378, 3719677, 1982489173871955]
"""

import json
import os
import sqlite3

RPC = "http://host.containers.internal:8545"
TARGET = 25664704
DB = os.path.expanduser("~/.config/degenbot/degenbot.db")
SV = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"

V3_0 = "0x60594a405d53811d3bc4766596efd80fd545a270"  # DAI/WETH (uniswap_v3)
V3_2 = "0x1ac1a8feaaea1900c4166deeed0c11cc10669d36"  # USDC/WETH (pancakeswap_v3)
V4_MGR = "0x000000000004444c5dc75cb358380d2e3de08a90"
V4_PID = "0xd967702f17f83d907b36e66c9a62eb50ac327432c581d5b273a76519692434be"

UNISWAP_V3_MINT_EVENT_HASH = "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde"
UNISWAP_V3_BURN_EVENT_HASH = "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c"
V4_MODIFY_LIQUIDITY_TOPIC = "0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec"

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


def to_i256_raw(hexstr):
    v = int(hexstr, 16)
    return v - (1 << 256) if v & (1 << 255) else v


def load_v3_liquidity_pool(cur, pool_id, addr):
    """DB stale tick map for a V3-family pool. Verified-current at TARGET."""
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
        "liquidity_update_block": TARGET,  # verified current at target
        "tick_data": tick_data,
    }


def v3_scalars(addr):
    import subprocess

    def cc(sig, arg=None):
        cmd = ["cast", "call", SV if False else addr, sig, "--rpc-url", RPC, "--block", str(TARGET)]
        out = subprocess.check_output(cmd, text=True)
        parts = [ln.split()[0] for ln in out.splitlines() if ln.strip()]
        return int(parts[arg], 0) if arg is not None else [int(p, 0) for p in parts[:6]]

    vals = cc("slot0()(uint160,int24,uint16,uint16,uint16,uint8,bool)")
    sqrt_price_x96 = vals[0]
    tick = vals[1]
    liquidity = cc("liquidity()(uint128)", 0)
    return sqrt_price_x96, tick, liquidity


def v4_scalars():
    import subprocess

    cmd = ["cast", "call", SV, "getSlot0(bytes32)(uint160,int24,uint24,uint24)", V4_PID, "--rpc-url", RPC, "--block", str(TARGET)]
    out = [ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()]
    sqrt_price_x96 = int(out[0], 0)
    tick = int(out[1], 0)
    protocol_fee = int(out[2], 0)
    lp_fee = int(out[3], 0)
    liquidity = int(
        [ln.split()[0] for ln in subprocess.check_output(
            ["cast", "call", SV, "getLiquidity(bytes32)(uint128)", V4_PID, "--rpc-url", RPC, "--block", str(TARGET)],
            text=True,
        ).splitlines() if ln.strip()][0],
        0,
    )
    return sqrt_price_x96, tick, liquidity, protocol_fee, lp_fee


def load_v4_pool(cur):
    (t0, t1, f0, f1, fd, ts, ub) = cur.execute(
        """SELECT t0.address, t1.address, uv4.fee_currency0, uv4.fee_currency1,
                  uv4.fee_denominator, uv4.tick_spacing, uv4.liquidity_update_block
           FROM uniswap_v4_pools uv4
           JOIN erc20_tokens t0 ON t0.id=uv4.currency0_id
           JOIN erc20_tokens t1 ON t1.id=uv4.currency1_id
           WHERE uv4.pool_hash=?""",
        (V4_PID,),
    ).fetchone()
    mp_id = cur.execute("SELECT managed_pool_id FROM uniswap_v4_pools WHERE pool_hash=?", (V4_PID,)).fetchone()[0]
    tick_rows = cur.execute(
        "SELECT tick, liquidity_net, liquidity_gross FROM managed_pool_liquidity_positions WHERE managed_pool_id=?",
        (mp_id,),
    ).fetchall()
    tick_data = {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in tick_rows}
    return {
        "family": "uniswap_v4",
        "pool_manager": V4_MGR,
        "pool_id": V4_PID,
        "currency0": t0,
        "currency1": t1,
        "fee_currency0": f0,
        "fee_currency1": f1,
        "fee_denominator": fd,
        "tick_spacing": ts,
        "liquidity_update_block": TARGET,
        "tick_data": tick_data,
    }


def main():
    cur = sqlite3.connect(DB)
    v3a = load_v3_liquidity_pool(cur, 599533, V3_0)
    v3c = load_v3_liquidity_pool(cur, 610500, V3_2)
    v4 = load_v4_pool(cur)
    cur.close()

    sa, ta, lqa = v3_scalars(V3_0)
    v3a.update(sqrt_price_x96=str(sa), tick=ta, liquidity=str(lqa))
    sw, tw, lqw = v3_scalars(V3_2)
    v3c.update(sqrt_price_x96=str(sw), tick=tw, liquidity=str(lqw))
    sv4, tv4, lqv4, pf4, lf4 = v4_scalars()
    v4.update(
        sqrt_price_x96=str(sv4), tick=tv4, liquidity=str(lqv4),
        protocol_fee=pf4, lp_fee=lf4,
    )

    fixture = {
        "_doc": (
            f"Exact V3-V4-V3 path-13308 pool states at block {TARGET}. Verified: DB "
            "liquidity snapshots current at target (no drift); scalars read on-chain at "
            "TARGET; v3_0+v4 liquidity byte-match the bot's captured post-swap state "
            "(sqrt/tick 1 step = pre-arb vs mid-swap)."
        ),
        "target_block": TARGET,
        "recorded_solve": {
            "optimal_input": 1982369771046931,
            "hop_outputs": [3720117117094320378, 3719677, 1982489173871955],
        },
        "pools": {"v3_0": v3a, "v4": v4, "v3_2": v3c},
        "path": [
            {"hop": 0, "kind": "v3", "pool": "v3_0", "zero_for_one": False},
            {"hop": 1, "kind": "v4", "pool": "v4", "zero_for_one": True},
            {"hop": 2, "kind": "v3", "pool": "v3_2", "zero_for_one": True},
        ],
    }
    out = "/workspaces/degenbot/tests/fixtures/path13308_v3v4v3_block25664704.json"
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)
    for n, p in (("v3_0", v3a), ("v4", v4), ("v3_2", v3c)):
        print(
            "%s ticks=%d sqrt=%s liq=%s tick=%s"
            % (n, len(p["tick_data"]), p["sqrt_price_x96"], p["liquidity"], p["tick"])
        )
    print("recorded_solve:", fixture["recorded_solve"])


if __name__ == "__main__":
    main()
