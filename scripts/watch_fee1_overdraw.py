#!/usr/bin/env python3
"""Watch the live settlement-arbitrage bot log for the next V4 over-prediction (UO3JM4).

Detects `[sim-revert-swap] ... matched=false` on a V4 hop (predicted > actual)
and records a compact **historical snapshot** of everything needed to drive a
later repro offline:

  * the matched=false event (path_id, hop, solve_block, predicted/actual)
  * the path's `[solver-st]` hops line (per-hop family / sq / liq / fee / zfo
    at solve time) and the nearest `[sim-diag]` / `[sim-fail]` / `[debug-v4-solve]`
    context — the **live on-chain scalars** that the DB does NOT hold
  * whether a `[debug-v4-solve]` pool-id hint was captured near the event

The degenbot DB is **static** (the bot does not write it), so it is NOT
snapshotted here. Pool identity + tick_data are pulled on demand during
investigation with `scripts/dump_pool_state.py` (resolve pool_hash from the
path's hop token pairs, which appear in the `[sim-fail]` hops field).

Artifacts land under `logs/fee1_snapshots/<ts>_path<p>_hop<h>_block<b>/`. The
watcher ONLY reads the bot log — it never truncates or writes to it.

Usage:  python scripts/watch_fee1_overdraw.py [--log PATH] [--poll SECS]
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import sys
import time
from collections import deque

ANSI = re.compile(r"\x1b\[[0-9;]*m")
EVENT_RE = re.compile(
    r"sim-revert-swap\]\s+path_id=(\d+)\s+hop=(\d+)\s+emit=(\d+)\s+"
    r"family=(\S+)\s+emitter=0x[0-9a-fA-F]+\s+actual_out=(\d+)\s+"
    r"predicted=(\d+)\s+matched=(\S+)"
)
SOLVE_BLOCK_RE = re.compile(r"sim-diag\]\s+.*?solve_block\":(\d+)")


def strip(line: str) -> str:
    return ANSI.sub("", line).strip()


def uniq(seq):
    seen, out = set(), []
    for x in seq:
        if x not in seen:
            seen.add(x)
            out.append(x)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", default="logs/bot_run.log")
    ap.add_argument("--poll", type=float, default=10.0)
    ap.add_argument("--min-predicted", type=int, default=0,
                    help="ignore events below this predicted output (wei spam filter)")
    args = ap.parse_args()

    root = "logs/fee1_snapshots"
    os.makedirs(root, exist_ok=True)
    audit = os.path.join(root, "watch_audit.log")

    offset = os.path.getsize(args.log)
    print(f"[watch] tails {args.log} (offset={offset}), snapshots->{root}/", flush=True)

    tail: deque[str] = deque(maxlen=5000)

    while True:
        size = os.path.getsize(args.log)
        if size > offset:
            with open(args.log, "rb") as f:
                f.seek(offset)
                data = f.read(size - offset)
            offset = size
            batch = []
            for raw in data.decode("utf-8", errors="replace").splitlines():
                line = strip(raw)
                if line:
                    tail.append(line)
                    batch.append(line)

            # Scan only the newly-appended batch (never re-scan the tail, or a
            # prior event would re-snapshot on every poll).
            for line in batch:
                m = EVENT_RE.search(line)
                if not m:
                    continue
                pid, hop, emit, family, actual, predicted, matched = m.groups()
                if family != "V4" or matched != "false":
                    continue
                if int(actual) >= int(predicted):
                    continue  # not an over-prediction
                if int(predicted) < args.min_predicted:
                    continue
                _record(args, root, audit, tail, pid, hop, int(actual), int(predicted), line)
        elif size < offset:
            print("[watch] log rotated/truncated — resetting offset", flush=True)
            offset = size
            tail.clear()
        time.sleep(args.poll)


def _record(args, root, audit, tail, pid, hop, actual, predicted, event_line):
    ts = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    solve_block = "unknown"
    for line in reversed(tail):
        if f"path_id\":{pid}" in line and "sim-diag" in line:
            bm = SOLVE_BLOCK_RE.search(line)
            if bm:
                solve_block = bm.group(1)
                break

    d = os.path.join(root, f"{ts}_path{pid}_hop{hop}_block{solve_block}")
    os.makedirs(d, exist_ok=True)

    ctx = [l for l in tail if f"path_id={pid}" in l or f"path_id\":{pid}" in l]
    # Nearest debug-v4-solve (a recently-solved V4 pool) as an identity hint.
    v4_hint = [l for l in tail if "debug-v4-solve" in l][-6:]

    payload = {
        "utc_capture": ts,
        "path_id": int(pid),
        "hop": int(hop),
        "family": "V4",
        "actual_out": actual,
        "predicted_output": predicted,
        "overdraw_wei": predicted - actual,
        "solve_block": solve_block,
        "event_line": event_line,
        "path_context_lines": uniq(ctx),
        "v4_debug_hint_lines": uniq(v4_hint),
        "note": (
            "DB is static (bot does not write it) — pool identity + tick_data "
            "are pulled on demand with scripts/dump_pool_state.py, resolving "
            "pool_hash from the path's hop token pairs in the sim-fail hops "
            "field. The scalars above (solver-st sq/liq/fee/zfo, debug-v4-solve "
            "tick/liquidity/protocol_fee) are the solve-time on-chain truth the "
            "DB does not hold."
        ),
    }
    with open(os.path.join(d, "event.json"), "w") as f:
        json.dump(payload, f, indent=2)

    with open(audit, "a") as f:
        f.write(f"{ts} path={pid} hop={hop} actual={actual} predicted={predicted} "
                f"block={solve_block}\n")
    print(f"[watch] CAPTURED over-prediction path={pid} hop={hop} "
          f"actual={actual} predicted={predicted} block={solve_block} -> {d}",
          flush=True)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        print("\n[watch] stopped", flush=True)
