"""The provider factory must resolve the endpoint via the RPC cascade.

Regression guard for the bug where ``get_provider_from_config`` read
``config.rpc.get(chain_id)`` directly, bypassing ``resolve_rpc_uris`` / its
HTTP-only sibling ``resolve_http_rpc_uri``. In the devcontainer the config.toml
``rpc[1]`` points at ``http://localhost:8545`` — the container's own loopback,
unreachable to the host's anvil — so a CLI ``pool update`` (HTTP-only) failed
with a connection refused even though the devcontainer exported the canonical
``DEGENBOT_RPC_HTTP_CHAINID_1`` override.

These tests pin (a) the factory delegates endpoint selection to the resolver
(building from the resolved URI, never ``config.rpc``), and (b) it raises the
cascade's ``RpcNotConfiguredError`` (naming the chain-id envvar) when no source
is configured — proving the env layer is consulted.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from degenbot.config import DatabaseSettings, DegenbotConfig, RpcNotConfiguredError
from degenbot.provider import factory as factory_mod
from degenbot.provider.factory import get_provider_from_config

_HTTP_ENV = "DEGENBOT_RPC_HTTP_CHAINID_1"


def _empty_config() -> DegenbotConfig:
    return DegenbotConfig(database=DatabaseSettings(path=Path(":memory:")), rpc={})


class _FakeProvider:
    def __init__(self, endpoint_uri: str) -> None:
        self.endpoint_uri = endpoint_uri


class _FakeEth:
    chain_id = 1  # matches the chain_id we resolve below


class _FakeW3:
    def __init__(self, provider: _FakeProvider) -> None:
        self.provider = provider
        self.eth = _FakeEth()

    @property
    def middleware_onion(self) -> MagicMock:
        return MagicMock()


class TestFactoryDelegatesToCascade:
    """The factory builds the provider from the resolver's URI, not config.rpc."""

    def test_uses_resolver_uri_not_config_rpc(
        self,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        # config.rpc[1] deliberately points at the *wrong* endpoint; if the
        # factory read it directly it would build HTTPProvider with this URI.
        config = DegenbotConfig(
            database=DatabaseSettings(path=Path(":memory:")),
            rpc={1: "http://localhost:8545"},
        )
        monkeypatch.delenv(_HTTP_ENV, raising=False)

        resolved: list[str] = []

        def fake_resolve(chain_id: int, /, *, config=None):
            resolved.append("called")
            return "http://from-resolver.example"

        monkeypatch.setattr(factory_mod, "resolve_http_rpc_uri", fake_resolve)
        monkeypatch.setattr(factory_mod, "HTTPProvider", _FakeProvider)

        captured_provider: list[_FakeProvider] = []

        def fake_from_web3(w3: _FakeW3) -> object:
            captured_provider.append(w3.provider)
            return object()

        monkeypatch.setattr(factory_mod, "Web3", _FakeW3)
        monkeypatch.setattr(factory_mod.ProviderAdapter, "from_web3", staticmethod(fake_from_web3))

        get_provider_from_config(chain_id=1, config=config)

        assert resolved == ["called"]
        assert captured_provider[0].endpoint_uri == "http://from-resolver.example"


class TestFactoryRaisesWhenNoSource:
    """No source in any cascade layer → RpcNotConfiguredError naming the envvar."""

    def test_raises_rpc_not_configured(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv(_HTTP_ENV, raising=False)

        with pytest.raises(RpcNotConfiguredError) as exc_info:
            get_provider_from_config(chain_id=1, config=_empty_config())

        assert _HTTP_ENV in str(exc_info.value)
