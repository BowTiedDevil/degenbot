"""Ethereum mainnet settlement-arbitrage bot: Uniswap V2/V3/V4 arbitrage using the Rust engine.

A thin Python entrypoint over the Rust-owned ArbitrageEngine and the
``degenbot.runner`` driver (epic 5TSYKN). The runtime driver — config,
path registration, result consumption, dispatch — lives in the
``degenbot.runner`` package; this file is an ``argv → BotRunner`` entrypoint
that owns only the CLI policy (argparse, SIGINT wrapper, dotenv read).

Startup sequence (owned by :class:`~degenbot.runner.BotRunner`):
1. Subscribe to WS (event buffering begins)
2. Load DB snapshots (V3 + V4 tick data)
3. Backfill snapshot→WS gap via Rust engine
4. Resume pump (Rust owns all event processing from here)
5. Start result consumer task (rolling start)
6. build_paths() (paths eagerly solved, results dispatched concurrently)
7. Consumer task continues as the permanent main loop

The old driver code that lived here (``BackrunSession``→``BotRunner``,
``build_paths``, ``consume_result_batches``, the dispatch/render helpers, and
the shared constants) has moved to ``degenbot.runner`` (epic 5TSYKN).
"""

import asyncio
import contextlib
import os
import sys
import time

import dotenv

from degenbot._ffi.diagnostics import mark_progress, start_gil_probe
from degenbot.logging import logger as bot_logger
from degenbot.runner import BotRunner
from degenbot.runner.cli import build_arbitrage_arg_parser
from degenbot.runner.config import ArbitrageConfig


async def main() -> None:
    """Parse CLI args, build + run the BotRunner, and await the pump loop."""
    parser = build_arbitrage_arg_parser()
    args = parser.parse_args()
    dry_run = not args.live

    # ergo 66H3KJ: start the GIL-acquire-latency probe + main-loop stuck-
    # watchdog BEFORE any other work. The probe runs on its own std::thread
    # and never needs the GIL to make progress.
    start_gil_probe(interval_ms=50, threshold_ms=100, stuck_ms=30_000)
    mark_progress()

    # ── Gated incident instrumentation (missed-WS-pong diagnosis) ──────────
    # With DEGENBOT_FAULTHANDLER_TIMEOUT_SECS > 0, arm a faulthandler repeat
    # dump: whenever ANY thread stalls (main loop busy past the timeout), the
    # CURRENT native + Python stacks of ALL threads are written to stderr
    # (tee'd into logs/bot_run.log by run_bot.sh) — including the C/Rust frame
    # of a GIL holder, which sys._current_frames() / logging cannot reveal.
    # ── Gated memory instrumentation (RSS-growth diagnosis) ──────────────
    # With DEGENBOT_TRACEMALLOC_SECS > 0, a daemon thread prints a tracemalloc
    # snapshot diff to stderr every N seconds: total Python-reachable memory +
    # the top growth sites (depth-1). A flat traced-current under a climbing
    # RSS would pin the growth OUTSIDE the Python object graph (Rust heaps /
    # allocator retention), splitting the diagnosis in half.
    tm_timeout = float(os.environ.get("DEGENBOT_TRACEMALLOC_SECS", "0"))
    if tm_timeout > 0:
        import ctypes
        import threading
        import tracemalloc

        tracemalloc.start(1)
        mem_state = {"snap": tracemalloc.take_snapshot(), "n": 0}
        libc = ctypes.CDLL("libc.so.6")
        libc.fopen.restype = ctypes.c_void_p
        libc.malloc_info.argtypes = [ctypes.c_int, ctypes.c_void_p]
        libc.fclose.argtypes = [ctypes.c_void_p]
        libc.malloc_trim.argtypes = [ctypes.c_size_t]

        def dump_malloc_info() -> None:
            mem_state["n"] += 1
            path = f"/workspaces/degenbot/logs/malloc_info_{mem_state['n']}.json"
            f = libc.fopen(path.encode(), b"w")
            if f:
                libc.malloc_info(0, f)
                libc.fclose(f)

        def mem_reporter() -> None:
            while True:
                time.sleep(tm_timeout)
                snap = tracemalloc.take_snapshot()
                dump_malloc_info()
                # glibc compaction: force release of free pages on arena tops.
                # Evidence probe — if RSS drops after this call the climb is
                # free-chunk retention, not a logical leak.
                libc.malloc_trim(0)
                stats = snap.compare_to(mem_state["snap"], "lineno")
                mem_state["snap"] = snap
                current, peak = tracemalloc.get_traced_memory()
                lines = [
                    f"[mem] traced-current={current / 1e6:.1f}MB peak={peak / 1e6:.1f}MB "
                    + f"trim-cycle={mem_state['n']} top-growth:"
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

    fh_timeout = float(os.environ.get("DEGENBOT_FAULTHANDLER_TIMEOUT_SECS", "0"))
    if fh_timeout > 0:
        import faulthandler

        faulthandler.enable()
        faulthandler.dump_traceback_later(fh_timeout, repeat=True, exit=False)
        bot_logger.info(f"[diag] faulthandler armed: timeout={fh_timeout}s repeat=True")

    if args.permutation is not None:
        bot_logger.info(f"[startup] Permutation filter from CLI: {args.permutation}")
    if not dry_run:
        bot_logger.info("\n*** LIVE MODE — BOT WILL SUBMIT REAL TRANSACTIONS! ***\n")

    env = dotenv.dotenv_values("examples/mainnet.env")
    try:
        cfg = ArbitrageConfig.from_env(
            env,
            live=not dry_run,
            permutation=args.permutation,
            cli_http=args.node_http,
            cli_ws=args.node_ws,
        )
    except ValueError as exc:
        bot_logger.error(str(exc))
        return

    # BotRunner owns the full startup handshake + phase ordering.
    # Ctrl-C: a SIGINT during `await runner.run()` unwinds through
    # `BotRunner.__aexit__` → `shutdown()` (stops the Rust pump). The
    # KeyboardInterrupt is caught here so the operator sees a single clean line.
    try:
        async with BotRunner(cfg) as session:
            # NWTUM3: optional operator command channel on a Unix socket.
            operator = None
            operator_task = None
            if args.operator_socket:
                from degenbot.operator.operator_channel import (
                    OperatorServer,
                    step_from_wire,
                )

                async def operator_handler(op: str, payload: dict) -> dict:
                    if op == "add_path":
                        steps = [step_from_wire(s) for s in payload["steps"]]
                        directions = payload.get("directions")
                        await session.enqueue_path(steps, directions=directions)
                        return {"detail": f"enqueued {len(steps)}-hop path"}
                    if op == "discover":
                        bound = payload.get("bound")
                        n = await session.trigger_discovery(bound=bound)
                        return {"detail": f"discovery processed {n} paths"}
                    return {"error": f"unknown op {op!r}"}

                operator = OperatorServer(operator_handler, socket_path=args.operator_socket)
                operator_task = asyncio.create_task(operator.serve(), name="operator-server")
                bot_logger.info(f"[operator] listening on {args.operator_socket}")
            try:
                await session.run()
            finally:
                if operator_task is not None:
                    operator_task.cancel()
                    with contextlib.suppress(asyncio.CancelledError):
                        await operator_task
                    await operator.close()
    except (KeyboardInterrupt, asyncio.CancelledError):
        bot_logger.info("[shutdown] interrupted — Rust pump stopped, exiting.")


if __name__ == "__main__":
    start = time.perf_counter()
    asyncio.run(main())
    bot_logger.info(f"Completed in {time.perf_counter() - start:.2f}s")
