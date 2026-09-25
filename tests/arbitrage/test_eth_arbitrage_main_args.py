"""Tests for the settlement-arbitrage bot's argument parser.

The parser is extracted into :func:`degenbot.runner.cli.build_arbitrage_arg_parser`
so the CLI surface — especially the ``--node`` cascade override — is verifiable
without running the full async session. ``from_env``'s handling of the override
is covered by ``test_arbitrage_config.py::TestRpcCascade``.
"""

from __future__ import annotations

import pytest

from degenbot.runner.cli import build_arbitrage_arg_parser


class TestParserNodeFlag:
    def test_node_defaults_to_none(self) -> None:
        parser = build_arbitrage_arg_parser()
        args = parser.parse_args([])
        assert args.node is None

    @pytest.mark.parametrize(
        "value",
        ["https://from-cli.example", "wss://from-cli.example", "ipc:///tmp/anvil.ipc"],
    )
    def test_node_is_parsed_verbatim(self, value: str) -> None:
        """The flag does not classify; the core does, from the value itself."""
        parser = build_arbitrage_arg_parser()
        args = parser.parse_args(["--node", value])
        assert args.node == value

    def test_existing_flags_still_present(self) -> None:
        parser = build_arbitrage_arg_parser()
        args = parser.parse_args(["--live", "--permutation", "V3-V4-V3"])
        assert args.live is True
        assert args.permutation == "V3-V4-V3"

    def test_the_flag_appears_in_help(self) -> None:
        parser = build_arbitrage_arg_parser()
        help_text = parser.format_help()
        assert "--node" in help_text
        assert "--node-http" not in help_text
        assert "--node-ws" not in help_text
