"""Settlement-arbitrage bot CLI (argparse) building — stable package home.

The ``argv -> BotRunner`` entrypoint's argument parser (epic 5TSYKN). The
example ``examples/eth_settlement_arbitrage_v2_v3_v4_rust.py`` is a thin wrapper that calls
:func:`build_arbitrage_arg_parser`; keeping the parser in the package makes the
CLI surface (notably the ``--node`` cascade override)
directly testable without importing from ``examples/``.
"""

from __future__ import annotations

import argparse


def build_arbitrage_arg_parser() -> argparse.ArgumentParser:
    """Build the settlement-arbitrage example's argument parser.

    Extracted so the CLI surface (especially the ``--node``
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
            "Sets the path permutation filter on the config."
        ),
    )
    parser.add_argument(
        "--node",
        type=str,
        default=None,
        help=(
            "RPC endpoint override for the arbitrage chain (Ethereum mainnet). "
            "One flag, self-classifying: ws:// fills the ws key, http:// the http "
            "key, ipc:// or a path the ipc key. Highest-priority layer of the "
            "cascade: --node > DEGENBOT_RPC_{IPC,WS,HTTP}_CHAINID_1 > the "
            "operator file's [nodes.*] tables > error."
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
