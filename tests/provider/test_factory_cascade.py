"""The provider factory must resolve the endpoint via the RPC cascade.

Regression guard for the bug where ``get_provider_from_config`` read
``config.rpc.get(chain_id)`` directly, bypassing ``resolve_rpc_uris`` / its
HTTP-only sibling ``resolve_http_rpc_uri``. In the devcontainer the config.toml
``rpc[1]`` points at ``http://localhost:8545`` — the container's own loopback,
unreachable to the host's anvil — so a CLI ``pool update`` (HTTP-only) failed
with a connection refused even though the devcontainer exported the canonical
``DEGENBOT_RPC_HTTP_CHAINID_1`` override.

These tests pin (a) the factory delegates endpoint selection to the resolver
(building from the resolved URI, never ``config.rpc``), (b) it constructs an
alloy provider unconditionally (no web3 branches) and hands it the resolved
chain (the core enforces ``eth_chainId``), and (c) it raises the cascade's
``RpcNotConfiguredError`` (naming the chain-id envvar) when no source is
configured — proving the env layer is consulted.
"""

from __future__ import annotations

from pathlib import Path

import pytest

import degenbot.provider as provider_mod
from degenbot.config import DatabaseSettings, DegenbotConfig, RpcNotConfiguredError
from degenbot.provider import factory as factory_mod
from degenbot.provider.factory import get_provider_from_config

_HTTP_ENV = "DEGENBOT_RPC_HTTP_CHAINID_1"


def _empty_config() -> DegenbotConfig:
    return DegenbotConfig(database=DatabaseSettings(path=Path(":memory:")), rpc={})


class _FakeAlloy:
    """Stand-in for the Rust AlloyProvider pyclass.

    It records the chain the factory bound the endpoint to (``bound_to``) —
    the handoff the real pyclass turns into the core's ``eth_chainId`` check.
    """

    def __init__(self, endpoint: str, *, chain_id: int | None = None) -> None:
        self.endpoint = endpoint
        self.bound_to = chain_id
        self._chain_id = chain_id if chain_id is not None else 1

    def get_chain_id(self) -> int:
        return self._chain_id


class TestFactoryDelegatesToCascade:
    """The factory builds the provider from the resolver's URI, not config.rpc."""

    def test_uses_resolver_uri_not_config_rpc(
        self,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        # config.rpc[1] deliberately points at the *wrong* endpoint; if the
        # factory read it directly it would build the provider with this URI.
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

        constructed: list[tuple[str, int | None]] = []

        def fake_alloy(endpoint: str, *, chain_id: int | None = None) -> _FakeAlloy:
            constructed.append((endpoint, chain_id))
            return _FakeAlloy(endpoint, chain_id=chain_id)

        monkeypatch.setattr(provider_mod, "AlloyProvider", fake_alloy)

        result = get_provider_from_config(chain_id=1, config=config)

        assert resolved == ["called"]
        assert constructed == [("http://from-resolver.example", 1)]
        assert isinstance(result, _FakeAlloy)
        assert result.endpoint == "http://from-resolver.example"

    def test_the_chain_id_reaches_the_core_that_enforces_it(
        self,
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        # The `eth_chainId` check now lives in the Rust core: the factory's
        # part is handing the resolved chain to the provider, so this test
        # pins the HANDOFF. The refusal itself is the core's, exercised
        # end-to-end in ``tests/provider/test_chain_binding.py``.
        monkeypatch.delenv(_HTTP_ENV, raising=False)
        config = _empty_config()

        def fake_resolve(chain_id: int, /, *, config=None):
            return "http://from-resolver.example"

        monkeypatch.setattr(factory_mod, "resolve_http_rpc_uri", fake_resolve)
        monkeypatch.setattr(
            provider_mod,
            "AlloyProvider",
            lambda endpoint, *, chain_id=None: _FakeAlloy(endpoint, chain_id=chain_id),
        )

        result = get_provider_from_config(chain_id=1, config=config)

        assert result.bound_to == 1


class TestFactoryRaisesWhenNoSource:
    """No source in any cascade layer → RpcNotConfiguredError naming the envvar."""

    def test_raises_rpc_not_configured(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.delenv(_HTTP_ENV, raising=False)

        with pytest.raises(RpcNotConfiguredError) as exc_info:
            get_provider_from_config(chain_id=1, config=_empty_config())

        assert _HTTP_ENV in str(exc_info.value)
