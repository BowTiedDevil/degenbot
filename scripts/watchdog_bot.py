#!/usr/bin/env python3
"""Background watchdog for the live bot.

Tails `logs/bot_run.log` from the byte offset at startup and watches for the
bot's failure markers. On the FIRST detection it snapshots the matched line +
tail context to `logs/watchdog/FAILURE.txt` (and mirrors it to stdout) then
exits 0 so a supervising loop can wake and investigate. It also detects an
unexpected exit of the bot process itself (a stronger signal — the bot is
designed to exit on fail via DEGENBOT_SIM_EXIT_ON_FAIL / the solver-state
abort), recording that as a separate marker `logs/watchdog/PROC_DOWN.txt`.

Designed to run under `setsid`/`nohup` from a supervising loop:

    python scripts/watchdog_bot.py --log logs/bot_run.log \
        --pidfile logs/bot_run.pid --interval 10 \
        > logs/watchdog/out.log 2>&1 &

Usage:
  python scripts/watchdog_bot.py [--log PATH] [--pidfile PATH] [--interval SECS]
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import time

# Failure markers, checked only against NEWLY-appended bytes (so historical
# failures in the pre-existing log do not re-trigger a fresh watchdog).
#
# Deliberately EXCLUDES diagnostics that are NOT crashes:
#   * bare `[sim-fail]` / `[sim-trap]` — with DEGENBOT_SIM_EXIT_ON_FAIL=0
#     (production trade-through) every thin-margin / no-profit candidate emits
#     a `[sim-fail] ... bucket=no-profit` line: ROUTINE arb-filtering, benign.
#   * `[sim-revert-swap] ... matched=false` — the per-hop actual-vs-predicted
#     over-prediction diagnostic (UO3JM4 / W2UWZO residue). It never stops
#     the bot under trade-through (the unprofitable path just reverts) and is
#     a tracked investigation with its own harness, not a crash.
# The STOP-worthy signals are what actually aborts/crashes the pump:
# the solver-state desync tripwire (UO3JM4 / ADR-021) and any hard crash.
# Unexpected process death is handled separately (PROC_DOWN.txt).
FAILURE_PATTERNS = [
    re.compile(r"SOLVER-STATE\] ABORT"),
    re.compile(r"verified desync"),
    re.compile(r"panicked"),
    re.compile(r"panicked at"),
    re.compile(r"Traceback \(most recent call last\)"),
    re.compile(r"RecursionError"),
]


def _pid_alive(pidfile: str) -> bool:
    try:
        with open(pidfile) as f:
            pid = int(f.read().strip())
    except (OSError, ValueError):
        return False
    try:
        # pid 0 or -1 is invalid; send signal 0 to test liveness.
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True  # exists, owned by another user


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", default="logs/bot_run.log")
    ap.add_argument("--pidfile", default="logs/bot_run.pid")
    ap.add_argument("--interval", type=int, default=10)
    args = ap.parse_args()

    outdir = os.path.dirname(os.path.abspath(args.log)) + "/watchdog"
    os.makedirs(outdir, exist_ok=True)
    fail_marker = os.path.join(outdir, "FAILURE.txt")
    down_marker = os.path.join(outdir, "PROC_DOWN.txt")

    # Start watching from the CURRENT end of the log (ignore history).
    try:
        pos = os.path.getsize(args.log)
    except OSError:
        pos = 0

    # A small grace so a just-started bot (mid-rebuild) isn't misread as down.
    saw_alive_once = False
    dead_streak = 0

    print(f"[watchdog] watching {args.log} from byte {pos} (interval {args.interval}s)",
          flush=True)
    sys.stdout.flush()

    while True:
        # 1) New failure lines?
        try:
            with open(args.log, "rb") as f:
                f.seek(pos)
                new = f.read().decode("utf-8", errors="replace")
                pos = f.tell()
        except OSError:
            new = ""

        for line in new.splitlines():
            if any(p.search(line) for p in FAILURE_PATTERNS):
                ctx = _tail_context(args.log, n=60)
                with open(fail_marker, "w") as fh:
                    fh.write(f"timestamp={time.strftime('%Y-%m-%dT%H:%M:%S%z')}\n"
                             f"matched={line.strip()[:400]}\n--- context ---\n{ctx}\n")
                print(f"[watchdog] FAILURE DETECTED:\n{line}\n", flush=True)
                print(ctx, flush=True)
                return 0

        # 2) Did the bot process unexpectedly die?
        alive = _pid_alive(args.pidfile)
        if alive:
            saw_alive_once = True
            dead_streak = 0
        else:
            if saw_alive_once:
                dead_streak += 1
                if dead_streak >= max(2, 30 // max(1, args.interval)):
                    with open(down_marker, "w") as fh:
                        fh.write(f"timestamp={time.strftime('%Y-%m-%dT%H:%M:%S%z')}\n"
                                 f"bot process down (pidfile {args.pidfile})\n"
                                 f"--- tail ---\n{_tail_context(args.log, n=60)}\n")
                    print("[watchdog] BOT PROCESS DOWN (unexpected exit)", flush=True)
                    return 0

        time.sleep(args.interval)


def _tail_context(log: str, n: int) -> str:
    try:
        with open(log, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            f.seek(max(0, size - 8192))
            tail = f.read().decode("utf-8", errors="replace")
        return "\n".join(tail.splitlines()[-n:])
    except OSError as e:
        return f"<unable to read log: {e}>"


if __name__ == "__main__":
    sys.exit(main())
