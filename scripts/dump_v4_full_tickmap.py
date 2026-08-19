#!/usr/bin/env python3
"""Enumerate the FULL on-chain V4 tickmap for a pool and compare against the DB.

The degenbot DB stores only the *tracked* V4 tickmap (a sparse subset recorded
at `liquidity_update_block`). The live in-process revm sim, by contrast, reads
the **real full on-chain PoolManager storage** via RPC. When those diverge, a
solver predicting fill on the tracked band can halt on-chain against positions
the DB does not carry (the path-5000 `EMPTY-HALT` class).

This script settles the fidelity question rigourously:
  1. Builds the full set of possible tick-bitmap word positions the pool could
     use (bounded by Uniswap's int24 tick range / `tick_spacing`).
  2. Fetches *every* word's bitmap in one Multicall3 `aggregate3` (batched).
  3. Decodes the set bits into initialized ticks.
  4. Fetches each initialized tick's `liquidity_net`/`liquidity_gross` in a
     second Multicall3 `aggregate3`.
  5. Loads the DB's tracked `tick_data` and diffs the two.

If on-chain shows ticks the DB does NOT track, the DB's V4 tickmap is
incomplete (fidelity gap → solver-vs-sim divergence). If they match 1:1, the
DB is faithful.

Usage (archive RPC required; default host.containers.internal:8545):

    python3 scripts/dump_v4_full_tickmap.py --pool-id 0x929b9b09... --block 25704509

    # Override RPC / state-view / multicall / db if desired:
    python3 scripts/dump_v4_full_tickmap.py --pool-id 0x929b9b09... \
        --block 25704509 --rpc http://... --state-view 0x7fFE...     \
        --multicall3 0xcA11... --db ~/.config/degenbot/degenbot.db

Exit 0 = fetch+diff completed (prints report); non-zero = RPC/CLI error.
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import urllib.request
from pathlib import Path

# Canonical on-chain defaults.
DEFAULT_RPC = "http://host.containers.internal:8545"
DEFAULT_STATE_VIEW = "0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227"  # V4 StateView
DEFAULT_MULTICALL3 = "0xcA11bde05977b3631167028862bE2a173976CA11"  # canonical Multicall3
_db_default = str(Path("~").expanduser() / ".config/degenbot/degenbot.db")
DEFAULT_DB = os.environ.get("DEGENBOT_DB", _db_default)

# Uniswap V4 tick bounds (int24), inclusive.
MIN_TICK = -887_272
MAX_TICK = 887_272

# Multicall3 `aggregate3((address,bool,bytes)[])((bool,bytes)[])` selector.
AGGREGATE3_SEL = "0x82ad56cb"


def _rpc(rpc_url: str, method: str, params: list) -> dict:
    req = urllib.request.Request(
        rpc_url,
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=180) as resp:
        out = json.load(resp)
    if "error" in out:
        raise RuntimeError(f"{method}: {out['error']}")
    return out["result"]


def _encode_get_tick_bitmap(pool_id: str, word: int) -> str:
    """Encode `getTickBitmap(bytes32,int16)` — selector + poolId + sign-extended int16 word."""
    word_b = (word & 0xFFFFFFFF).to_bytes(4, "big").hex()
    if word < 0:
        # sign-extend to 32 bytes: prefix 0xffff... for the high 28 bytes
        word_b = "f" * 56 + word_b  # 28 bytes of 0xff + 4-byte two's-complement
    return f"0x1c7ccb4c{pool_id[2:].lower()}{word_b.zfill(64)}"


def _encode_get_tick_liquidity(pool_id: str, tick: int) -> str:
    """Encode `getTickLiquidity(bytes32,int24)` — selector + poolId + sign-extended int24 tick."""
    tick6 = (tick & 0xFFFFFF).to_bytes(3, "big").hex()
    if tick < 0:
        tick6 = "f" * 58 + tick6  # sign-extend: 29 bytes of 0xff + 3-byte value
    return f"0xcaedab54{pool_id[2:].lower()}{tick6.zfill(64)}"


def _abi_encode_aggregate3(calls: list[tuple[str, str]]) -> str:
    """ABI-encode aggregate3 via the Rust-backed degenbot.abi core."""
    from degenbot.abi import encode  # lazy: keep this script importable without the bot

    elements = []
    for tgt, calldata in calls:
        # raw 20-byte target so no EIP-55 case assumption is needed
        elements.append((bytes.fromhex(tgt[2:]), True, bytes.fromhex(calldata[2:])))
    args = encode(["(address,bool,bytes)[]"], [elements]).hex()
    return f"{AGGREGATE3_SEL}{args}"


def _decode_tick_liquidity(return_bytes: bytes) -> tuple[int, int]:
    """Decode `getTickLiquidity` returns `(uint128 gross, int128 net)`.

    The `net` is `int128` returned as a 256-bit ABI word sign-extended to the
    full 32 bytes (negative int128 => 0xffff… prefix). Sign-extend from bit 255.
    """
    if len(return_bytes) < 64:
        return (0, 0)
    gross = int.from_bytes(return_bytes[:32], "big")
    net_b = int.from_bytes(return_bytes[32:64], "big")
    if net_b >= (1 << 255):
        net_b -= 1 << 256
    return (gross, net_b)


def min_word(spacing: int) -> int:
    return (MIN_TICK // spacing) >> 8


def max_word(spacing: int) -> int:
    return (MAX_TICK // spacing) >> 8


def _decode_word_ticks(word: int, bitmap: int, spacing: int) -> list[int]:
    """Decode a nonzero bitmap into the set of initialized ticks it marks."""
    ticks = []
    for bit in range(256):
        if (bitmap >> bit) & 1:
            ticks.append(((word << 8) + bit) * spacing)
    return ticks


def load_db_tickmap(db_path: str, pool_id: str) -> dict:
    """Load the DB's tracked tick_data for a V4 pool by pool_hash."""
    con = sqlite3.connect(db_path)
    try:
        row = con.execute(
            "SELECT m.id, v.liquidity_update_block FROM uniswap_v4_pools v "
            "JOIN managed_pools m ON m.id = v.managed_pool_id "
            "WHERE lower(v.pool_hash)=lower(?)",
            (pool_id,),
        ).fetchone()
        if not row:
            print(f"DB: no managed V4 pool for {pool_id}", file=sys.stderr)
            return {"tick_data": {}, "liquidity_update_block": None}
        (mid, upd) = row
        ticks = con.execute(
            "SELECT tick, liquidity_net, liquidity_gross "
            "FROM managed_pool_liquidity_positions WHERE managed_pool_id=? ORDER BY tick",
            (mid,),
        ).fetchall()
        return {
            "tick_data": {t: {"liquidity_net": n, "liquidity_gross": g} for t, n, g in ticks},
            "liquidity_update_block": upd,
        }
    finally:
        con.close()


def fetch_onchain_tickmap(rpc_url, state_view, mc3, pool_id, block, spacing, batch=300):
    """Fetch the full on-chain initialized tick set via two multicall passes."""
    words = list(range(min_word(spacing), max_word(spacing) + 1))
    print(
        f"on-chain word positions to scan: {len(words)} [{min_word(spacing)}..{max_word(spacing)}]",
        file=sys.stderr,
    )

    # Pass 1: all tick bitmaps.
    set_ticks: set[int] = set()
    word_map: dict[int, int] = {}
    for i in range(0, len(words), batch):
        chunk = words[i : i + batch]
        calls = [(state_view, _encode_get_tick_bitmap(pool_id, w)) for w in chunk]
        calldata = _abi_encode_aggregate3(calls)
        res = _rpc(rpc_url, "eth_call", [{"to": mc3, "data": calldata}, hex(block)])
        # Decode aggregate3 returns: per-element 32-byte success + 32-byte offset.
        rbytes = bytes.fromhex(res[2:])
        m = 0
        for w in chunk:
            # ABI: dynamic array of tuple -> each element has a bytes tail.
            pass
        from degenbot.abi import decode  # lazy: keep this script importable without the bot

        # degenbot.abi decodes tuple elements as plain lists (unpacked below)
        results = decode(["(bool,bytes)[]"], rbytes)[0]
        if len(results) != len(chunk):
            raise RuntimeError(f"aggregate3 returned {len(results)} rows, expected {len(chunk)}")
        for w, (ok, rdata) in zip(chunk, results):
            bitmap = int.from_bytes(rdata, "big") if len(rdata) else 0
            if bitmap:
                word_map[w] = bitmap
                set_ticks.update(_decode_word_ticks(w, bitmap, spacing))
    print(
        f"on-chain initialized ticks: {len(set_ticks)} across {len(word_map)} nonzero words",
        file=sys.stderr,
    )

    # Pass 2: liquidity net/gross per initialized tick.
    onchain: dict[int, dict] = {}
    tick_list = sorted(set_ticks)
    for i in range(0, len(tick_list), batch):
        chunk = tick_list[i : i + batch]
        calls = [(state_view, _encode_get_tick_liquidity(pool_id, t)) for t in chunk]
        calldata = _abi_encode_aggregate3(calls)
        res = _rpc(rpc_url, "eth_call", [{"to": mc3, "data": calldata}, hex(block)])

        rbytes = bytes.fromhex(res[2:])
        results = decode(["(bool,bytes)[]"], rbytes)[0]
        if len(results) != len(chunk):
            raise RuntimeError(f"aggregate3 returned {len(results)} rows, expected {len(chunk)}")
        for t, (ok, rdata) in zip(chunk, results):
            g, n = _decode_tick_liquidity(rdata)
            onchain[t] = {"liquidity_net": n, "liquidity_gross": g}
    return onchain


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__.splitlines()[0], formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--pool-id", required=True, help="V4 pool_id (0x-64 hex)")
    ap.add_argument("--block", type=lambda s: int(s, 0), required=True, help="block number")
    ap.add_argument("--spacing", type=int, default=1, help="tick_spacing (default 1)")
    ap.add_argument("--rpc", default=DEFAULT_RPC)
    ap.add_argument("--state-view", default=DEFAULT_STATE_VIEW)
    ap.add_argument("--multicall3", default=DEFAULT_MULTICALL3)
    ap.add_argument("--db", default=DEFAULT_DB)
    ap.add_argument("--batch", type=int, default=300, help="calls per aggregate3 (default 300)")
    args = ap.parse_args()

    pid = args.pool_id.lower()
    if not pid.startswith("0x") or len(pid) != 66:
        raise SystemExit(f"pool-id must be 0x-64 hex, got {args.pool_id!r}")

    onchain = fetch_onchain_tickmap(
        args.rpc, args.state_view, args.multicall3, pid, args.block, args.spacing, args.batch
    )
    db = load_db_tickmap(args.db, pid)

    db_ticks = set(db["tick_data"])
    oc_ticks = set(onchain)

    only_db = sorted(db_ticks - oc_ticks)
    only_oc = sorted(oc_ticks - db_ticks)

    print("\n=== DB tracked V4 tickmap ===")
    print(f"  liquidity_update_block={db['liquidity_update_block']}  n_ticks={len(db_ticks)}")
    for t in sorted(db_ticks):
        d = db["tick_data"][t]
        o = onchain.get(t)
        d_net = int(d["liquidity_net"])
        d_gross = int(d["liquidity_gross"])
        match = o and o["liquidity_net"] == d_net and o["liquidity_gross"] == d_gross
        print(
            f"  tick {t:>8}  db_net={d_net} db_gross={d_gross}"
            f"  onchain_net={o['liquidity_net'] if o else '-'}"
            f"  onchain_gross={o['liquidity_gross'] if o else '-'}  "
            f"[{'MATCH' if match else 'DIFF'}]"
        )

    print("\n=== On-chain ticks the DB does NOT track ===")
    if only_oc:
        for t in only_oc:
            o = onchain[t]
            print(f"  tick {t:>8}  net={o['liquidity_net']} gross={o['liquidity_gross']}")
    else:
        print("  (none — DB tickmap is a superset / exact match of on-chain)")

    print("\n=== DB ticks with NO on-chain position ===")
    if only_db:
        for t in only_db:
            print(f"  tick {t:>8}")
    else:
        print("  (none)")

    print(f"\non-chain n_ticks={len(oc_ticks)}  db n_ticks={len(db_ticks)}")

    # Verdict.
    if only_oc:
        print(
            f"\nRESULT: FIDELITY GAP — on-chain has {len(only_oc)} initialized "
            f"tick(s) the DB does not track. DB tickmap is INCOMPLETE."
        )
        return 0
    if only_db:
        print(
            f"\nRESULT: DB tracks {len(only_db)} tick(s) absent on-chain "
            f"(stale). DB has phantom liquidity."
        )
        return 0
    print("\nRESULT: DB tickmap is EXACTLY FAITHFUL — no divergent ticks. On-chain == DB 1:1.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
