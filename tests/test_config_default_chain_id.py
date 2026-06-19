"""Tests for the ``default_chain_id`` config field (ADR-006 D5).

One Bot per chain — the chain identity lives in the config object. ``Bot``
reads it at construction and enforces the connected RPC's ``eth_chainId``
matches it. These tests cover the config-field mechanics offline; the
RPC-enforcement behavior is exercised by the fork-backed ``ConnectionManager``
tests in ``test_config.py`` (until the manager is retired in a later slice-8
increment) and the provider factory's own chain-mismatch raise.
"""

from pathlib import Path

from degenbot.config import DatabaseSettings, DegenbotConfig
from degenbot.provider import get_provider_from_config
from degenbot.provider.factory import get_provider_from_config as factory_get


def _config(chain_id: int | None) -> DegenbotConfig:
    return DegenbotConfig(
        database=DatabaseSettings(path=Path(":memory:")),
        rpc={1: "https://eth.llamarpc.com/"} if chain_id is not None else {},
        default_chain_id=chain_id,
    )


class TestDefaultChainIdField:
    def test_explicit_chain_id(self) -> None:
        config = _config(1)
        assert config.default_chain_id == 1

    def test_defaults_to_none_for_fresh_config(self) -> None:
        # A freshly-initialized config (no RPCs configured) has no default chain.
        config = _config(None)
        assert config.default_chain_id is None

    def test_factory_re_exported_from_lib(self) -> None:
        # The canonical factory lives in the lib layer (degenbot.provider),
        # not cli. ``degenbot.provider.get_provider_from_config`` and
        # ``degenbot.provider.factory.get_provider_from_config`` are the same
        # function — lib callers reach it without importing cli.
        assert get_provider_from_config is factory_get
