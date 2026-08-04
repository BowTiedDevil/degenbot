"""Capture the exact pool states for the live path-10956 V3-V3-V2 IIA failure
at block 25677777 (the split-tick-clock validation run's first conservative trap).

Path 10956 at solve_block 25677777:
  hop0 V3 0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36  WETH/USDT  fee=3000 zfo=true
  hop1 V3 0xDd2e0D86A45e4EF9bd490c2809E6405720cC357c  PYUSD/USDT fee=3000 zfo=false
  hop2 V2 0x4a4D2410C3D4cfa8Dd0D275bEDEfBd2f7B61Ba2E  PYUSD/WETH fee=30   zfo=true

[sim-diag]  optimal_input=1372336865300 hop_outputs=[2545,2541,1372801010696]
[sim-revert-swap] hop0 (V3 0x4e68): predicted=2545 actual_out=2544 matched=false  (+1 over-prediction)
[sim-fail] IIA revert @ hop1 pool 0xDd2e

DB liquidity snapshots + on-chain scalars read at TARGET. Emits a fixture in the
shape the `v3v30_hop0_probe.rs` harness (and the recovery scripts) consume.
"""

import json
import pathlib
import sqlite3
import subprocess
import urllib.request

RPC = "http://host.containers.internal:8545"
TARGET = 25677777  # solve block (path 10956)
DB = pathlib.Path("~/.config/degenbot/degenbot.db").expanduser()

POOLS = {
    "v3_0": {  # hop0 WETH/USDT 0.30%
        "family": "uniswap_v3",
        "address": "0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36",
        "db_id": 599529,
    },
    "v3_1": {  # hop1 PYUSD/USDT 0.30%
        "family": "uniswap_v3",
        "address": "0xDd2e0D86A45e4EF9bd490c2809E6405720cC357c",
        "db_id": 623025,
    },
    "v2_2": {  # hop2 PYUSD/WETH 0.30%
        "family": "uniswap_v2",
        "address": "0x4a4D2410C3D4cfa8Dd0D275bEDEfBd2f7B61Ba2E",
        "db_id": 250548,
    },
}


def rpc(method, params):

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
    cmd = [
        "cast",
        "call",
        addr,
        "slot0()(uint160,int24,uint16,uint16,uint16,uint8)",
        "--rpc-url",
        RPC,
        "--block",
        str(TARGET),
    ]
    out = [
        ln.split()[0] for ln in subprocess.check_output(cmd, text=True).splitlines() if ln.strip()
    ]
    liq = [
        ln.split()[0]
        for ln in subprocess.check_output(
            [
                "cast",
                "call",
                addr,
                "liquidity()(uint128)",
                "--rpc-url",
                RPC,
                "--block",
                str(TARGET),
            ],
            text=True,
        ).splitlines()
        if ln.strip()
    ][0]
    return int(out[0], 0), int(out[1], 0), int(liq, 0)


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
                (pid,),
            ).fetchone()
            tokens = {r[0]: r[1] for r in cur.execute("SELECT id,address FROM erc20_tokens")}
            ticks = cur.execute(
                "SELECT tick,liquidity_net,liquidity_gross FROM liquidity_positions "
                "WHERE pool_id=? ORDER BY tick",
                (pid,),
            ).fetchall()
            p.update(
                token0=tokens[t0],
                token1=tokens[t1],
                tick_spacing=ts,
                fee_token0=f0,
                fee_token1=f1,
                fee_denominator=fd,
                tick_data={
                    t: {"liquidity_net": str(n), "liquidity_gross": str(g)} for t, n, g in ticks
                },
            )
            sq, tick, liq = v3_scalars(spec["address"])
            p.update(
                sqrt_price_x96=str(sq), tick=tick, liquidity=str(liq), liquidity_update_block=TARGET
            )
            # also resolve the on-chain liquidity snapshot block from DB if present
            row = cur.execute(
                "SELECT liquidity_update_block FROM uniswap_v3_pools WHERE pool_id=?", (pid,)
            ).fetchone()
            if row and row[0]:
                p["liquidity_update_block"] = row[0]
        else:  # uniswap_v2 reserves (live scalars via cast; DB holds no reserves)
            cmd = [
                "cast",
                "call",
                spec["address"],
                "getReserves()(uint112,uint112,uint32)",
                "--rpc-url",
                RPC,
                "--block",
                str(TARGET),
            ]
            out = [
                ln.split()[0]
                for ln in subprocess.check_output(cmd, text=True).splitlines()
                if ln.strip()
            ]
            p.update(
                reserve0=str(int(out[0], 0)),
                reserve1=str(int(out[1], 0)),
                block_number=int(out[2], 0),
            )
        out_pools[key] = p
        if spec["family"] == "uniswap_v3":
            print(
                "%s: ticks=%d sqrt=%s liq=%s tick=%s spacing=%s fee=%s"
                % (
                    key,
                    len(p["tick_data"]),
                    p["sqrt_price_x96"],
                    p["liquidity"],
                    p["tick"],
                    p["tick_spacing"],
                    p["fee_token0"],
                )
            )
        else:
            print("%s: reserve0=%s reserve1=%s" % (key, p["reserve0"], p["reserve1"]))
    cur.close()

    fixture = {
        "_doc": f"Exact path-10956 V3-V3-V2 pool states at solve_block {TARGET}. "
        "DB liquidity snapshots + on-chain scalars read at TARGET.",
        "target_block": TARGET,
        "recorded_solve": {
            "optimal_input": "1372336865300",  # [sim-diag] WETH in
            "hop_outputs": ["2545", "2541", "1372801010696"],
            "hop0_actual_out": "2544",  # [sim-revert-swap] on-chain actual (micro-USDT)
            "hop0_predicted": "2545",  # solver hop_outputs[0]
        },
        "pools": out_pools,
        "path": [
            {"hop": 0, "pool": "v3_0", "zero_for_one": True},
            {"hop": 1, "pool": "v3_1", "zero_for_one": False},
            {"hop": 2, "pool": "v2_2", "zero_for_one": True},
        ],
    }
    out = f"/workspaces/degenbot/tests/fixtures/path10956_v3v3v2_block{TARGET}.json"
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1)
    print("wrote", out)


if __name__ == "__main__":
    main()
