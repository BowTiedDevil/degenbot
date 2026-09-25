"""The ``degenbot`` console entry point — a passthrough, not a tree (ADR-051 D3)."""

import sys

from degenbot._ffi import cli_main


def main() -> None:
    """Run the Rust-owned console, exiting with its code.

    Raises:
        SystemExit: always, carrying the console's process exit code.

    """
    raise SystemExit(cli_main(sys.argv[1:]))
