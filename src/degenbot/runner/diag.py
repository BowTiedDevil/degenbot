"""Incident-diagnostic harnesses for the driver cockpit.

The three operator-runtime probes — the tracemalloc diff thread, the
``/proc/self`` RSS/purge CSV sampler, and the faulthandler repeat dumper —
live here, configured through the typed loader (``ArbitrageConfig.diag``,
populated by :meth:`ArbitrageConfig.build`). The cockpit
(:meth:`~degenbot.runner.bot_runner.BotRunner.start`) arms them at startup;
any entrypoint that runs the cockpit (the settlement-arbitrage example, the
``degenbot`` console) gets the probes uniformly.
"""

from __future__ import annotations

import dataclasses
import os
import sys
import threading
import time
from pathlib import Path

from degenbot.logging import logger as bot_logger

#: The kernel's view of this process; the sampler's read root in production.
_DEFAULT_PROC_ROOT = Path("/proc/self")


@dataclasses.dataclass(frozen=True)
class DiagConfig:
    """The three probe toggles, carried from the declared diagnostics keys.

    No field has a default: the core schema declaration owns them (zero is
    OFF, the production posture), so a second literal here could drift from
    what an operator set.
    """

    #: >0 arms the tracemalloc diff thread (one snapshot per interval).
    tracemalloc_secs: float
    #: >0 arms the read-only ``/proc/self`` RSS/VmHWM CSV sampler.
    procmem_secs: float
    #: CSV output path for the proc-mem sampler.
    procmem_csv: str
    #: >0 arms the faulthandler repeat dump (all-thread stacks each timeout).
    faulthandler_timeout_secs: float


def _arm_tracemalloc(interval: float) -> None:
    """Print a tracemalloc snapshot diff to stderr every ``interval`` seconds.

    A flat traced-current under a climbing RSS pins the growth OUTSIDE the
    Python object graph (Rust heaps / allocator retention), splitting the
    diagnosis in half.
    """
    import ctypes
    import tracemalloc

    tracemalloc.start(1)
    last_snap = tracemalloc.take_snapshot()
    cycle = 0
    libc = ctypes.CDLL("libc.so.6")
    libc.fopen.restype = ctypes.c_void_p
    libc.malloc_info.argtypes = [ctypes.c_int, ctypes.c_void_p]
    libc.fclose.argtypes = [ctypes.c_void_p]
    libc.malloc_trim.argtypes = [ctypes.c_size_t]

    def dump_malloc_info() -> None:
        nonlocal cycle
        cycle += 1
        path = f"logs/malloc_info_{cycle}.json"
        f = libc.fopen(path.encode(), b"w")
        if f:
            libc.malloc_info(0, f)
            libc.fclose(f)

    def mem_reporter() -> None:
        nonlocal last_snap, cycle
        while True:
            time.sleep(interval)
            snap = tracemalloc.take_snapshot()
            dump_malloc_info()
            # glibc compaction: force release of free pages on arena tops.
            # Evidence probe — if RSS drops after this call the climb is
            # free-chunk retention, not a logical leak.
            libc.malloc_trim(0)
            stats = snap.compare_to(last_snap, "lineno")
            last_snap = snap
            current, peak = tracemalloc.get_traced_memory()
            lines = [
                f"[mem] traced-current={current / 1e6:.1f}MB peak={peak / 1e6:.1f}MB "
                + f"trim-cycle={cycle} top-growth:"
            ]
            lines.extend(
                f"[mem]   +{stat.size_diff / 1e6:8.1f}MB count={stat.count_diff:+7d} "
                + f"{stat.traceback[0]}"
                for stat in stats[:10]
            )
            newline = chr(10)
            sys.stderr.write(newline.join(lines) + newline)
            sys.stderr.flush()

    threading.Thread(target=mem_reporter, daemon=True, name="tracemalloc").start()
    bot_logger.info(f"[diag] tracemalloc armed: interval={interval}s")


def _arm_procmem(interval: float, csv_path: str, proc_root: Path) -> None:
    """Append one CSV row per interval: wall/mono clock, RSS, VmHWM, faults.

    Sibling to the tracemalloc probe, but deliberately READ-ONLY — no
    snapshots, no malloc_trim — so it never perturbs the allocator behavior
    being measured (fault staircase per block window is the dependent variable
    of the purge-delay matrix).

    Args:
        interval: Seconds between CSV rows.
        csv_path: The CSV file the sampler appends to.
        proc_root: The ``stat``/``statm``/``status`` read root. Production
            passes the kernel's live ``/proc/self``; a fixture hands a
            directory of the same three files.

    """
    import csv

    csv_p = Path(csv_path)
    if csv_p.parent != Path():
        csv_p.parent.mkdir(parents=True, exist_ok=True)

    def proc_mem_sampler() -> None:
        while True:
            time.sleep(interval)
            try:
                txt = (proc_root / "stat").read_bytes()
                stat = txt.rsplit(b")", 1)[1].split()
                min_flt, maj_flt = int(stat[7]), int(stat[9])  # fields 10, 12
                rss_pages = int((proc_root / "statm").read_bytes().split()[1])
                hwm_kb = 0
                with (proc_root / "status").open("rb") as vf:
                    for line in vf:
                        if line.startswith(b"VmHWM:"):
                            hwm_kb = int(line.split()[1])
                            break
                with csv_p.open("a", newline="", encoding="utf-8") as fh:
                    if fh.tell() == 0:
                        csv.writer(fh).writerow([
                            "t_epoch",
                            "t_mono",
                            "rss_kb",
                            "hwm_kb",
                            "min_flt",
                            "maj_flt",
                        ])
                    csv.writer(fh).writerow([
                        round(time.time(), 3),
                        round(time.perf_counter(), 3),
                        rss_pages * os.sysconf("SC_PAGE_SIZE") // 1024,
                        hwm_kb,
                        min_flt,
                        maj_flt,
                    ])
            except OSError:
                return

    threading.Thread(target=proc_mem_sampler, daemon=True, name="proc-mem-sampler").start()
    bot_logger.info(f"[diag] proc-mem sampler armed: interval={interval}s path={csv_path}")


def _arm_faulthandler(timeout: float) -> None:
    """Repeat-dump ALL thread stacks whenever any thread stalls past timeout."""
    import faulthandler

    faulthandler.enable()
    faulthandler.dump_traceback_later(timeout, repeat=True, exit=False)
    bot_logger.info(f"[diag] faulthandler armed: timeout={timeout}s repeat=True")


def arm_diagnostics(cfg: DiagConfig, *, proc_root: Path = _DEFAULT_PROC_ROOT) -> list[str]:
    """Arm every configured probe; return the armed probe names (stable order).

    Zero-config arms nothing — production default. Called once per session
    from the cockpit's ``start()``; idempotent across runners (threads are
    daemon and probe-scoped).

    Args:
        cfg: The probe toggles read off the declared diagnostics keys.
        proc_root: Where the proc-mem sampler reads ``stat``/``statm``/
            ``status``. The kernel's live ``/proc/self`` unless a test
            injects a fixture directory with the same shape.

    Returns:
        The armed probe names, in stable arm order.

    """
    armed: list[str] = []
    if cfg.tracemalloc_secs > 0:
        _arm_tracemalloc(cfg.tracemalloc_secs)
        armed.append("tracemalloc")
    if cfg.procmem_secs > 0:
        _arm_procmem(cfg.procmem_secs, cfg.procmem_csv, proc_root)
        armed.append("procmem")
    if cfg.faulthandler_timeout_secs > 0:
        _arm_faulthandler(cfg.faulthandler_timeout_secs)
        armed.append("faulthandler")
    return armed
