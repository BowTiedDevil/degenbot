"""Tests for the settlement-arbitrage bot's argument parser.

The parser is extracted into :func:`degenbot.runner.cli.build_arbitrage_arg_parser`
so the CLI surface — especially the ``--node-http`` / ``--node-ws`` cascade
overrides — is verifiable without running the full async session. ``from_env``'s
handling of ``cli_http`` / ``cli_ws`` is covered by
``test_arbitrage_config.py::TestCliOverride``.
"""

from __future__ import annotations

import pytest

from degenbot.runner.cli import build_arbitrage_arg_parser


class TestParserNodeFlags:
    def test_node_http_node_ws_default_none(self) -> None:
        parser = build_arbitrage_arg_parser()
        args = parser.parse_args([])
        assert args.node_http is None
        assert args.node_ws is None

    def test_node_http_node_ws_parsed(self) -> None:
        parser = build_arbitrage_arg_parser()
        args = parser.parse_args([
            "--node-http",
            "https://from-cli.example",
            "--node-ws",
            "wss://from-cli.example",
        ])
        assert args.node_http == "https://from-cli.example"
        assert args.node_ws == "wss://from-cli.example"

    def test_existing_flags_still_present(self) -> None:
        parser = build_arbitrage_arg_parser()
        args = parser.parse_args(["--live", "--permutation", "V3-V4-V3"])
        assert args.live is True
        assert args.permutation == "V3-V4-V3"

    @pytest.mark.parametrize("flag", ["--node-http", "--node-ws"])
    def test_flags_appear_in_help(self, flag: str) -> None:
        parser = build_arbitrage_arg_parser()
        help_text = parser.format_help()
        assert flag in help_text
