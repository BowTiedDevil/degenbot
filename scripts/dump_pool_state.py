#!/usr/bin/env python3
"""Pull pool identity + tick_data from the static degenbot DB for a repro.

The DB is static (the bot does not write it), so this can be run at any time
during an offline investigation to rebuild the fixture inputs for a captured
over-prediction path (UO3JM4). It emits a JSON blob shaped like the fixture
`PoolJson` fields so it can be dropped into the repro harness.

Handles:
  * V4 pool by **pool_hash** (0x + 64 hex)  -> identity + managed-pool ticks
  * V3 pool by **address** (0x + 40 hex)     -> identity + liquidity ticks
    (Uniswap / PancakeSwap / SushiSwap V3, matched by `pools.kind`+exchange)

Live scalars (sqrt_price_x96 / tick / liquidity / protocol_fee) are NOT in the
DB — they are solve-time on-chain state taken from the `[solver-st]` /
`[debug-v4-solve]` lines in the captured log context. This tool supplies the
static per-pool fields; fill the scalars from the event snapshot.

Usage:
  python scripts/dump_pool_state.py <0x..pool_hash_or_address>
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3

DB = os.environ.get("DEGENBOT_DB", os.path.expanduser("~/.config/degenbot/degenbot.db"))


def _token(con, token_id):
    row = con.execute(
        "SELECT address, symbol, decimals FROM erc20_tokens WHERE id=?",
        (token_id,),
    ).fetchone()
    return {"address": row[0], "symbol": row[1], "decimals": row[2]} if row else None


def _v4(con, pool_hash):
    row = con.execute(
        "SELECT m.id, m.kind, v.pool_hash, v.currency0_id, v.currency1_id, "
        "v.fee_currency0, v.fee_currency1, v.fee_denominator, v.tick_spacing, "
        "v.liquidity_update_block, p.address AS mgr "
        "FROM uniswap_v4_pools v "
        "JOIN managed_pools m ON m.id = v.managed_pool_id "
        "JOIN pool_managers p ON p.id = m.manager_id "
        "WHERE lower(v.pool_hash)=lower(?)",
        (pool_hash,),
    ).fetchone()
    if not row:
        raise SystemExit(f"no managed V4 pool for pool_hash {pool_hash}")
    (_id, kind, _ph, c0, c1, fc0, fc1, fden, spacing, upd, mgr) = row
    ticks = con.execute(
        "SELECT tick, liquidity_net, liquidity_gross "
        "FROM managed_pool_liquidity_positions WHERE managed_pool_id=? "
        "ORDER BY tick",
        (_id,),
    ).fetchall()
    return {
        "kind": kind,
        "family": "uniswap_v4",
        "pool_manager": mgr,
        "pool_id": pool_hash,
        "currency0": _token(con, c0),
        "currency1": _token(con, c1),
        "fee_currency0": fc0,
        "fee_currency1": fc1,
        "fee_denominator": fden,
        "tick_spacing": spacing,
        "liquidity_update_block": upd,
        "tick_data": [{"tick": t, "liquidity_net": n, "liquidity_gross": g}
                      for t, n, g in ticks],
        "n_ticks": len(ticks),
    }


def _v3(con, address):
    prow = con.execute(
        "SELECT p.id, p.kind, p.token0_id, p.token1_id, x.name AS exchange "
        "FROM pools p JOIN exchanges x ON x.id = p.exchange_id "
        "WHERE lower(p.address)=lower(?)",
        (address,),
    ).fetchone()
    if not prow:
        raise SystemExit(f"no V3 pool for address {address}")
    (pid, kind, t0, t1, exchange) = prow
    # Which V3 fee/spacing table holds this pool, based on kind.
    table = {
        "uniswap_v3": "uniswap_v3_pools",
        "pancakeswap_v3": "pancakeswap_v3_pools",
        "sushiswap_v3": "sushiswap_v3_pools",
    }.get(kind)
    if not table:
        raise SystemExit(f"kind {kind!r} is not a V3 pool table")
    urow = con.execute(
        f"SELECT tick_spacing, fee_token0, fee_token1, fee_denominator "
        f"FROM {table} WHERE pool_id=?",
        (pid,),
    ).fetchone()
    if not urow:
        raise SystemExit(f"no {table} row for pool {address}")
    (spacing, ft0, ft1, fden) = urow
    ticks = con.execute(
        "SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions "
        "WHERE pool_id=? ORDER BY tick",
        (pid,),
    ).fetchall()
    return {
        "kind": kind,
        "family": exchange,
        "address": address,
        "token0": _token(con, t0),
        "token1": _token(con, t1),
        "tick_spacing": spacing,
        "fee_token0": ft0,
        "fee_token1": ft1,
        "fee_denominator": fden,
        "tick_data": [{"tick": t, "liquidity_net": n, "liquidity_gross": g}
                      for t, n, g in ticks],
        "n_ticks": len(ticks),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("identifier", help="V4 pool_hash (0x+64) or V3 address (0x+40)")
    ap.add_argument("--pretty", action="store_true")
    args = ap.parse_args()
    ident = args.identifier
    hexlen = len(ident) - (2 if ident.lower().startswith("0x") else 0)
    con = sqlite3.connect(DB)
    if hexlen == 64:
        out = _v4(con, ident)
    elif hexlen == 40:
        out = _v3(con, ident)
    else:
        raise SystemExit(f"unrecognized identifier hex length {hexlen} (want 64 or 40)")
    print(json.dumps(out, indent=2 if args.pretty else None))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
