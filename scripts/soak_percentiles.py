#!/usr/bin/env python3
"""Soak percentile scraper: Prometheus histogram -> p50/p90/p95/p99 table.

Usage: python3 scripts/soak_percentiles.py [metrics_url] [label]

Examples:
  python3 scripts/soak_percentiles.py                       # 127.0.0.1:9464, all series
  python3 scripts/soak_percentiles.py http://127.0.0.1:9464/metrics

Percentiles are the ceil-index estimate over cumulative `*_bucket` le
values (same method as stateview-feasibility doc §3), so numbers are
byte-comparable against the baseline table there.
"""

from __future__ import annotations

import re
import sys
import urllib.request

DEFAULT_URL = "http://127.0.0.1:9464/metrics"
SERIES = [
    "degenbot_block_header_to_solved_seconds",
    "degenbot_block_log_burst_seconds",
    "degenbot_stage_publish_cycle_seconds",
    "degenbot_block_header_to_first_log_seconds",
    "degenbot_block_settle_wait_seconds",
    "degenbot_solve_duration_seconds",
    "degenbot_solve_gate_duration_seconds",
    "degenbot_state_lock_wait_seconds",
    "degenbot_state_lock_hold_seconds",
    "degenbot_state_apply_seconds",
    "degenbot_log_decode_seconds",
]


def parse_hist(text: str, name: str):
    buckets, count, ssum = [], None, 0.0
    for raw in text.splitlines():
        line = raw.strip()
        if not line.startswith(name + "_"):
            continue
        suffix = line[len(name) + 1 :]
        value = line.rsplit("}", 1)[-1].split()[-1]
        if suffix.startswith("bucket"):
            le = re.search(r'le="([^"]+)"', line).group(1)
            buckets.append((le, float(value)))
        elif suffix.startswith("count"):
            count = float(value)
        elif suffix.startswith("sum"):
            ssum = float(value)
    return buckets, count, ssum


def pct(buckets, count, q):
    if not count:
        return None
    target = q * count
    for le, v in buckets:
        if le == "+Inf":
            continue
        if v >= target:
            return float(le)
    return None


def main() -> int:
    url = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_URL
    text = urllib.request.urlopen(url, timeout=10).read().decode()
    print(f"# scrape {url}")
    hdr = f"{'series':<46} {'count':>10} {'p50':>8} {'p90':>8} {'p95':>8} {'p99':>8}".format()
    print(hdr)
    for name in SERIES:
        buckets, count, ssum = parse_hist(text, name)
        if not count:
            print(f"{name:<46} {'(no samples)':>10}")
            continue
        cells = [pct(buckets, count, q) for q in (0.5, 0.9, 0.95, 0.99)]
        def fmt(x):
            return f"{x:g}" if x is not None else "?"
        print(
            f"{name:<46} {int(count):>10} {fmt(cells[0]):>8} {fmt(cells[1]):>8} "
            f"{fmt(cells[2]):>8} {fmt(cells[3]):>8}  (sum={ssum:g}s)"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
