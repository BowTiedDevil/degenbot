#!/usr/bin/env python3
"""Observe-only backrun soak (task NYVL2F, gate 1).

Ingests the MEVBlocker searcher feed, classifies every event, and writes a
tear-down JSON report. ZERO submissions, ZERO gas, zero repo writes outside
logs/backrun/ — the proof gate before any bid path is ever exercised.

Usage:
    uv run --no-sync python scripts/backrun/observe_soak.py --minutes 30

Fails (exit 1) if:
- the feed never connects, or
- zero events arrive for the whole window (feed health), or
- more than 5 wall-clock reconnects (feed stability).
"""

from __future__ import annotations

import argparse
import json
import time
from collections import Counter
from pathlib import Path

from degenbot._ffi import backrun as br


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--minutes", type=int, default=30)
    ap.add_argument("--out", default="logs/backrun/soak_report.json")
    args = ap.parse_args()

    feed = br.PyBackrunFeed()  # production defaults: mainnet, 48s watchdog, 4096 ring
    started = time.time()
    deadline = started + args.minutes * 60

    classes: Counter[str] = Counter()
    reasons: Counter[str] = Counter()
    unique_targets: set[str] = set()
    events_total = 0
    sample_swap: dict | None = None
    samples_file = Path("logs/backrun/first_samples.json")
    samples_file.parent.mkdir(parents=True, exist_ok=True)
    samples: list[dict] = []

    print(f"[soak] observe-only window: {args.minutes} min (zero submissions)")
    last_report = 0.0
    while time.time() < deadline:
        time.sleep(2.0)
        events = feed.drain()
        if events:
            events_total += len(events)
            for e in events:
                target = e["to"] or "0x0000000000000000000000000000000000000000"
                unique_targets.add(target)
                cls = br.classify_target(target, e["data"])
                classes[cls["kind"]] += 1
                if cls["kind"] == "opaque":
                    reasons[cls["reason"]] += 1
                if cls["kind"] == "swap" and sample_swap is None:
                    sample_swap = {"target": target, "legs": cls["legs"]}
                if len(samples) < 25:
                    samples.append(
                        {
                            "hash": e["hash"],
                            "to": target,
                            "class": cls,
                        }
                    )
        st = feed.status()
        if time.time() - last_report >= 60:
            last_report = time.time()
            print(
                f"[soak] {int(time.time() - started)}s: events={events_total} "
                f"classes={dict(classes)} reconnects={st['reconnects']} "
                f"connected={st['connected']}"
            )
            samples_file.write_text(json.dumps(samples, indent=2))

    st = feed.status()
    report = {
        "window_minutes": args.minutes,
        "wall_seconds": int(time.time() - started),
        "events_total": events_total,
        "classes": dict(classes),
        "opaque_reasons": dict(reasons),
        "unique_targets": len(unique_targets),
        "first_swap_sample": sample_swap,
        "feed_status": st,
        "submissions": 0,  # observe-only by construction
    }
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2))
    print(f"[soak] report: {out}")
    feed.stop()

    if st["accepted"] == 0:
        print("[soak] FAIL: feed delivered zero events")
        return 1
    if st["reconnects"] > 5:
        print("[soak] FAIL: feed unstable (>5 reconnects)")
        return 1
    print("[soak] PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
