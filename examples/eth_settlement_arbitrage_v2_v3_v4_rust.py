"""Ethereum mainnet settlement-arbitrage bot: Uniswap V2/V3/V4 arbitrage using the Rust engine.

A thin Python entrypoint over the Rust-owned ArbitrageEngine and the
``degenbot.runner`` driver. The runtime driver — config,
path registration, result consumption, dispatch — lives in the
``degenbot.runner`` package; this file is an ``argv → BotRunner`` entrypoint
that owns only the CLI policy (argparse, SIGINT wrapper). Operator identity
reaches ``ArbitrageConfig.build`` through the process environment, exported by the launch
shell from ``bot.env``.

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
the shared constants) has moved to ``degenbot.runner``.
"""

import asyncio
import contextlib
import sys
import time

from degenbot._ffi.diagnostics import mark_progress, start_gil_probe
from degenbot.exceptions import BootRefused
from degenbot.logging import logger as bot_logger
from degenbot.runner import BotRunner
from degenbot.runner.bot_runner import ActivationGateRefused
from degenbot.runner.cli import build_arbitrage_arg_parser
from degenbot.runner.config import ArbitrageConfig, RpcCascadeOverrides


async def main() -> None:
    """Parse CLI args, build + run the BotRunner, and await the pump loop."""
    parser = build_arbitrage_arg_parser()
    args = parser.parse_args()
    dry_run = not args.live

    # Start the GIL-acquire-latency probe + main-loop stuck-
    # watchdog BEFORE any other work. The probe runs on its own std::thread
    # and never needs the GIL to make progress.
    start_gil_probe(interval_ms=50, threshold_ms=100, stuck_ms=30_000)
    mark_progress()

    if args.permutation is not None:
        bot_logger.info(f"[startup] Permutation filter from CLI: {args.permutation}")
    if not dry_run:
        bot_logger.info("\n*** LIVE MODE — BOT WILL SUBMIT REAL TRANSACTIONS! ***\n")

    # Operator/executor identity comes from the process environment, which the
    # launch shell fills from bot.env — no secret lands in a workspace file. The
    # injection stance is a declared key (:data:`DEGENBOT_INJECT_EXECUTOR_CODE`),
    # and the retired bare spelling is refused with a loud error.
    try:
        cfg = ArbitrageConfig.build(
            live=not dry_run,
            permutation=args.permutation,
            rpc=RpcCascadeOverrides(node=args.node),
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
            # Optional operator command channel on a Unix socket.
            operator = None
            operator_task = None
            if args.operator_socket:
                from degenbot.operator.operator_channel import (
                    OperatorServer,
                    handle_fleet_posture_op,
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
                    if op in {"set_fleet_posture", "get_fleet_posture"}:
                        # The live cordon-threshold re-tune + read, routed
                        # through the shared helper (the degenbot.fleet
                        # mirror home mints on first use).
                        return handle_fleet_posture_op(op, payload)
                    return {"error": f"unknown op {op!r}"}

                operator = OperatorServer(operator_handler, socket_path=args.operator_socket)
                operator_task = asyncio.create_task(operator.serve(), name="operator-server")
                bot_logger.info(f"[operator] listening on {args.operator_socket}")
            try:
                bot_logger.info(f"[session] ended: {(await session.run()).name}")
            finally:
                if operator_task is not None:
                    operator_task.cancel()
                    with contextlib.suppress(asyncio.CancelledError):
                        await operator_task
                    await operator.close()
    except ActivationGateRefused as exc:
        # The posture gate refused the fleet: same fail-fast discipline as
        # the boot refusal — one named line, EX_CONFIG, no partial run().
        bot_logger.error(f"[activation-gate] REFUSED — {exc}")
        sys.exit(78)
    except BootRefused as exc:
        # The library surfaces the TYPED fleet boot refusal (it never aborts
        # the host process); the BINARY owns the loud fail-fast exit — one
        # named line, non-zero status, the same message discipline as the
        # abort path. 78 is sysexits EX_CONFIG: the host cannot host the
        # fleet configuration.
        bot_logger.error(f"[fleet-boot] REFUSED — {exc}")
        sys.exit(78)
    except (KeyboardInterrupt, asyncio.CancelledError):
        bot_logger.info("[shutdown] interrupted — Rust pump stopped, exiting.")


if __name__ == "__main__":
    start = time.perf_counter()
    asyncio.run(main())
    bot_logger.info(f"Completed in {time.perf_counter() - start:.2f}s")
