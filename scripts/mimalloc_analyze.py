#!/usr/bin/env python3
"""Derive the mimalloc purge-delay decision metrics from one soak's artifacts.

Usage: uv run python scripts/mimalloc_analyze.py <artifacts_dir> [...]
Each dir must contain procmem.csv + bot_log_tail.log (+ hp.json). Writes
<dir>/summary.json and prints one JSON blob per dir.

Metrics (epic AZZDBI):
- per-block-window minor-fault deltas (mimalloc purges pages -> next block refaults them)
- RSS at block boundaries (refill shape / plateau)
- hotpath solve-phase avg/p95 as the latency guard metric

Tolerates headerless procmem.csv (early sampler builds wrote rows without a header).
"""

import csv
import json
import re
import statistics
import sys
from datetime import datetime
from pathlib import Path

COLS = ("t_epoch", "t_mono", "rss_kb", "hwm_kb", "min_flt", "maj_flt")
TS_RE = re.compile(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+)Z")
BLOCK_RE = re.compile(r"current_block=(\d+)")
PROBES = [
    "SolveCoordinator::on_drain",
    "EngineHandle::solve_dirty",
    "mixed.solve_path_inner",
    "cl_solve.active_set",
    "arb_solve.rayon_solve",
]


def parse_iso(ts: str) -> float:
    # TS_RE captures WITHOUT the trailing Z (naive string) -> interpret as UTC,
    # never local: local-tz parsing skewed epochs by the host offset (PDT +7h)
    # and silently emptied every per-block window.
    dt = datetime.fromisoformat(ts[:-1] if ts.endswith("Z") else ts)
    if dt.tzinfo is None:
        from datetime import timezone
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.timestamp()


def load_log_epochs(log_path: Path):
    """(main_loop_start_epoch, [(epoch, current_block)]) from dispatch lines."""
    out = []
    main_loop_start = None
    ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")  # ANSI SGR codes sit between == and digits
    for raw in log_path.read_text(errors="replace").splitlines():
        line = ANSI_RE.sub("", raw)
        m = TS_RE.search(line)
        if not m:
            continue
        epoch = parse_iso(m.group(1))
        if main_loop_start is None and "Entering main loop" in line:
            main_loop_start = epoch
        if "[dispatch-phase] fan-out ENTER" in line:
            b = BLOCK_RE.search(line)
            if b:
                out.append((epoch, int(b.group(1))))
    return main_loop_start, out


def load_csv(path: Path):
    rows = []
    with open(path) as fh:
        reader = csv.reader(fh)
        first = next(reader, None)
        if first is None:
            return rows
        if first and first[0] == "t_epoch":
            idx = {name: i for i, name in enumerate(first)}
        else:
            idx = {name: i for i, name in enumerate(COLS)}
            it = iter([first])
        else_rows = []
        for r in reader:
            try:
                rows.append(
                    (float(r[idx["t_epoch"]]), int(r[idx["rss_kb"]]), int(r[idx["hwm_kb"]]),
                     int(r[idx["min_flt"]]), int(r[idx["maj_flt"]]))
                )
            except (ValueError, IndexError, KeyError):
                continue
    rows.sort()
    return rows


def sample_at(rows, epoch, key_idx):
    """Last sample with t <= epoch + small jitter (pre-event, not post-refill)."""
    lo, hi, best = 0, len(rows) - 1, None
    while lo <= hi:
        mid = (lo + hi) // 2
        if rows[mid][0] <= epoch + 0.2:
            best = mid
            lo = mid + 1
        else:
            hi = mid - 1
    return None if best is None else rows[best][key_idx]


def load_log_epochs_dispatch(path: Path):
    """Compact extract: dispatch lines only (full-run coverage, no ANSI ambiguity)."""
    return load_log_epochs(path)


def analyze(d: Path, log_override=None) -> dict:
    rows = load_csv(d / "procmem.csv")
    log_path = log_override if log_override else d / "dispatch_events.log"
    if not Path(log_path).exists():
        log_path = d / "bot_log_tail.log"
    main_start, blocks = load_log_epochs_dispatch(Path(log_path))
    out = {"dir": str(d), "samples": len(rows)}
    if not rows:
        return out
    t0, t1 = rows[0][0], rows[-1][0]
    if main_start:
        steady = [r for r in rows if r[0] >= main_start]
        out["steady_samples"] = len(steady)
        if steady:
            out["steady_rss_kb_min"] = min(r[1] for r in steady)
            out["steady_rss_kb_max"] = max(r[1] for r in steady)
            out["steady_rss_kb_last"] = steady[-1][1]
    out["vmhwm_kb_last"] = rows[-1][2]
    out["maj_flt_total"] = rows[-1][4]

    # per-block fault deltas over consecutive dispatch points spanned by the CSV
    pts = [(e, b) for (e, b) in blocks if t0 <= e <= t1]
    deltas, block_ids = [], []
    for i in range(len(pts) - 1):
        (e0, b0), (e1, b1) = pts[i], pts[i + 1]
        f0, f1 = sample_at(rows, e0, 3), sample_at(rows, e1, 3)
        dur = e1 - e0
        if f0 is None or f1 is None or b1 - b0 not in (1, 2) or not (5.0 <= dur <= 60.0):
            continue  # gaps, reorg double-counts, sampler edges
        deltas.append((f1 - f0, dur, e0, b0))
        block_ids.append(b0)
    if deltas:
        faults = [x[0] for x in deltas]
        durs = [x[1] for x in deltas]
        out["blocks_analyzed"] = len(deltas)
        out["block_span"] = [block_ids[0], block_ids[-1]]
        out["min_flt_per_block_mean"] = round(statistics.mean(faults), 1)
        out["min_flt_per_block_std"] = round(statistics.stdev(faults), 1) if len(faults) > 1 else 0.0
        out["min_flt_per_block_min"] = min(faults)
        out["min_flt_per_block_max"] = max(faults)
        out["block_interval_mean_s"] = round(statistics.mean(durs), 2)
        rb = [sample_at(rows, e, 1) for (e, _) in pts]
        rb = [x for x in rb if x is not None]
        if len(rb) > 2:
            out["rss_at_block_mean_kb"] = round(statistics.mean(rb))
            out["rss_at_block_min_kb"] = min(rb)
            out["rss_at_block_max_kb"] = max(rb)

    stats_file = d / "mimalloc_stats.log"
    if stats_file.exists():
        txt = stats_file.read_text(errors="replace")
        m = re.search(r"peak rss: ([0-9.]+ [KMGT]iB)", txt)
        if m:
            out["mimalloc_peak_rss"] = m.group(1)
        m = re.search(r"purged\s*:\s*([0-9.]+\s*[KMGT]?iB?)", txt)
        out["mimalloc_purged"] = m.group(1).strip() if m else None

    hp_file = d / "hp.json"
    if hp_file.exists():
        hp = json.loads(hp_file.read_text())
        data = hp.get("functions_timing", {}).get("data", [])
        by_name = {x["name"]: x for x in data}
        out["solve"] = {
            n: {"calls": by_name[n]["calls"], "avg": by_name[n]["avg"], "p95": by_name[n].get("p95")}
            for n in PROBES
            if n in by_name
        }
    return out


def main() -> None:
    args = [a for a in sys.argv[1:] if a != "--log"]
    log_override = None
    if "--log" in sys.argv:
        i = sys.argv.index("--log")
        log_override = sys.argv[i + 1]
        args = [a for a in args if a != log_override]
    if not args:
        sys.exit("usage: mimalloc_analyze.py <dir> [...] [--log <bot_run.log>]")
    for arg in args:
        d = Path(arg)
        s = analyze(d, log_override=log_override)
        (d / "summary.json").write_text(json.dumps(s, indent=2))
        print(f"== {d.name} ==")
        print(json.dumps(s, indent=2)[:2200])


if __name__ == "__main__":
    main()
