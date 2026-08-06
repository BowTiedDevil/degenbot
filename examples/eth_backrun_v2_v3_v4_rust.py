"""Ethereum mainnet backrun bot: Uniswap V2/V3/V4 arbitrage using the Rust engine.

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
the shared constants) has moved to ``degenbot.runner``. The re-export block at
the bottom is a TEMPORARY shim so the existing test suite can keep importing
these names from this module; it is removed once the tests are rerouted to the
package (epic 5TSYKN).
"""

import argparse
import asyncio
import contextlib
import time

import dotenv

from degenbot._ffi.diagnostics import mark_progress, start_gil_probe
from degenbot.logging import logger as bot_logger
from degenbot.runner import BotRunner
from degenbot.runner.config import BackrunConfig
from degenbot.runner.driver_constants import PATH_PERMUTATION_FILTER
from degenbot.runner import driver_constants as _driver_constants


def _build_arg_parser() -> argparse.ArgumentParser:
    """Build the backrun example's argument parser.

    Extracted so the CLI surface (especially the ``--node-http`` / ``--node-ws``
    cascade overrides) is testable without running the full async session.

    Returns:
        The configured ``ArgumentParser`` (caller invokes ``parse_args``).
    """
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--live",
        action="store_true",
        help="Enable live mode (submits real transactions)",
    )
    parser.add_argument(
        "--permutation",
        type=str,
        default=None,
        help=(
            "Pool version permutation filter (e.g. V2-V3-V4). "
            "Only paths matching this 3-hop ordering will be built and simulated. "
            "Overrides PATH_PERMUTATION_FILTER in the driver."
        ),
    )
    parser.add_argument(
        "--node-http",
        type=str,
        default=None,
        help=(
            "HTTP RPC endpoint for the backrun chain (Ethereum mainnet). "
            "Highest-priority source in the RPC URI cascade: "
            "--node-http > DEGENBOT_RPC_HTTP_CHAINID_1 > NODE_HOST_HTTP "
            "> config.toml rpc[1] > error."
        ),
    )
    parser.add_argument(
        "--node-ws",
        type=str,
        default=None,
        help=(
            "WebSocket RPC endpoint for the backrun chain (Ethereum mainnet). "
            "Highest-priority source in the RPC URI cascade: "
            "--node-ws > DEGENBOT_RPC_WS_CHAINID_1 > NODE_HOST_WEBSOCKET "
            "> config.toml ws[1] > error."
        ),
    )
    parser.add_argument(
        "--operator-socket",
        type=str,
        default=None,
        help=(
            "Optional Unix domain socket path for the operator command channel "
            "(NWTUM3). When set, the bot hosts an OperatorServer here so the "
            "`degenbot path add` / `degenbot path discover` CLI can add a path "
            "or trigger bounded on-demand discovery on the LIVE pump without "
            "restarting it."
        ),
    )
    return parser


async def main() -> None:
    """Parse CLI args, build + run the BotRunner, and await the pump loop."""
    parser = _build_arg_parser()
    args = parser.parse_args()
    dry_run = not args.live

    # ergo 66H3KJ: start the GIL-acquire-latency probe + main-loop stuck-
    # watchdog BEFORE any other work. The probe runs on its own std::thread
    # and never needs the GIL to make progress.
    start_gil_probe(interval_ms=50, threshold_ms=100, stuck_ms=30_000)
    mark_progress()

    # Override the driver's PATH_PERMUTATION_FILTER from --permutation.
    if args.permutation is not None:
        _driver_constants.PATH_PERMUTATION_FILTER = {args.permutation}
        bot_logger.info(f"[startup] Permutation filter from CLI: {args.permutation}")
    if not dry_run:
        bot_logger.info("\n*** LIVE MODE — BOT WILL SUBMIT REAL TRANSACTIONS! ***\n")

    env = dotenv.dotenv_values("examples/mainnet.env")
    try:
        cfg = BackrunConfig.from_env(
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

                operator = OperatorServer(
                    operator_handler, socket_path=args.operator_socket
                )
                operator_task = asyncio.create_task(
                    operator.serve(), name="operator-server"
                )
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


# ──────────────────────────────────────────────────────────────────
# TEMPORARY re-export shim (epic 5TSYKN — removed by the test-reroute task).
# Tests historically imported the driver from this example module. Now that
# the driver lives in degenbot.runner, these names are forwarded so the suite
# (un-rerouted) keeps resolving to the SAME package objects. Do NOT add new
# logic here; logic belongs in degenbot.runner.
# ──────────────────────────────────────────────────────────────────
from degenbot.arbitrage.engine_registry import EngineRegistry  # noqa: E402,F401
from degenbot.dispatch import Dispatcher  # noqa: E402,F401
from degenbot.runner import (  # noqa: E402
    BackrunSession,
    PathRegistrationPipeline,
    ConstructionContext,
    build_paths,
    consume_result_batches,
    resolve_directions,
    run_registration_pipeline,
)
from degenbot.runner.dispatch import (  # noqa: E402,F401
    _dispatch_profitable,
    _render_sim_failures,
)
from degenbot.runner.consume import _tee_block_stream  # noqa: E402,F401
from degenbot.runner.driver_constants import (  # noqa: E402,F401
    ETH_MAINNET_ALLOWED_TOKENS,
    PANCAKESWAP_V3_MAINNET_FACTORY,
    REG_QUEUE_BOUND,
    REG_WORKERS,
    SUSHISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V4_POOL_MANAGER_ADDRESS,
    WETH_ADDRESS,
)
