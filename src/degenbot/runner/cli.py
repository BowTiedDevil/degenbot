"""Settlement-arbitrage bot CLI (argparse) building — stable package home.

The ``argv -> BotRunner`` entrypoint's argument parser (epic 5TSYKN). The
example ``examples/eth_backrun_v2_v3_v4_rust.py`` is a thin wrapper that calls
:func:`build_backrun_arg_parser`; keeping the parser in the package makes the
CLI surface (notably the ``--node-http`` / ``--node-ws`` cascade overrides)
directly testable without importing from ``examples/``.
"""

from __future__ import annotations

import argparse


def build_backrun_arg_parser() -> argparse.ArgumentParser:
    """Build the settlement-arbitrage example's argument parser.

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
            "HTTP RPC endpoint for the arbitrage chain (Ethereum mainnet). "
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
            "WebSocket RPC endpoint for the arbitrage chain (Ethereum mainnet). "
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
