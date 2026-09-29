"""The provider factory resolves the endpoint through the installed cascade.

The factory owns no resolution of its own: it takes the session chain and the
explicit-override endpoint, delegates to the resolver over the installed typed
config, and hands the chain to the Rust core, which reads ``eth_chainId`` once
and refuses a misconfigured endpoint. These tests pin that division — the
factory builds from the RESOLVED uri, it hands the chain to the core that
enforces the match, and an unresolved endpoint is the cascade's own refusal.

The chain-binding refusal itself is the core's, exercised end to end against
fake nodes in ``tests/provider/test_chain_binding.py``.
"""

from __future__ import annotations

from typing import Any

import pytest

from degenbot.config import RpcNotConfiguredError
from degenbot.provider.factory import ChainIdentityMismatchError
from degenbot.provider.factory import get_provider_from_config

# A chain id no operator file or harness sets, so the refusal is genuinely the
# absence of every layer rather than a leak from the developer's environment.
_UNCONFIGURED_CHAIN = 988877


class _FakeAlloy:
    """Stand-in for the Rust AlloyProvider pyclass.

    It records the chain the factory bound the endpoint to (``bound_to``) —
    the handoff the real pyclass turns into the core's ``eth_chainId`` check.
    """

    def __init__(self, endpoint: str, *, chain_id: int | None = None) -> None:
        self.endpoint = endpoint
        self.bound_to = chain_id


class _FakeChainMismatchError(ValueError):
    """The core refusal, stand-in for the pyo3 class the factory catches.

    A pyo3 exception is not constructible from Python, so the seam is pinned
    by substituting the class the factory's ``except`` names.
    """

    def __init__(self, expected: int, actual: int, endpoint: str) -> None:
        self.expected = expected
        self.actual = actual
        self.endpoint = endpoint
        super().__init__(f"{endpoint} serves {actual}, bound to {expected}")


class TestFactoryDelegatesToCascade:
    """The factory builds the provider from the resolved uri, not from a config."""

    def test_uses_the_resolver_uri(self) -> None:
        resolved: list[tuple[int, str | None]] = []

        def fake_resolve(chain_id: int, /, *, node: str | None = None) -> str:
            resolved.append((chain_id, node))
            return "http://from-resolver.example"

        constructed: list[tuple[str, int | None]] = []

        def fake_provider(endpoint: str, *, chain_id: int | None = None) -> _FakeAlloy:
            constructed.append((endpoint, chain_id))
            return _FakeAlloy(endpoint, chain_id=chain_id)

        result = get_provider_from_config(
            chain_id=1,
            node="http://from-cli.example",
            resolve_uri=fake_resolve,
            provider_factory=fake_provider,
        )

        assert resolved == [(1, "http://from-cli.example")]
        assert constructed == [("http://from-resolver.example", 1)]
        assert isinstance(result, _FakeAlloy)

    def test_the_chain_id_reaches_the_core_that_enforces_it(self) -> None:
        """The ``eth_chainId`` check is the core's; the factory's part is the handoff."""
        provider = get_provider_from_config(
            chain_id=1,
            resolve_uri=lambda chain_id, /, *, node=None: "http://x.example",
            provider_factory=lambda endpoint, *, chain_id=None: _FakeAlloy(
                endpoint, chain_id=chain_id
            ),
        )

        assert provider.bound_to == 1


class TestFactoryTranslatesTheChainRefusal:
    """The check is Rust's; re-homing its refusal is the factory's only job."""

    def test_it_is_still_a_value_error(self) -> None:
        """A caller that already handled a misconfigured endpoint keeps working."""
        assert issubclass(ChainIdentityMismatchError, ValueError)

    def test_the_core_refusal_becomes_a_degenbot_error(self) -> None:
        def refusing(endpoint: str, *, chain_id: int | None = None) -> object:
            raise _FakeChainMismatchError(chain_id or 0, (chain_id or 0) + 1, endpoint)

        with pytest.raises(ChainIdentityMismatchError, match="wrong chain") as refusal:
            get_provider_from_config(
                chain_id=1,
                resolve_uri=lambda chain_id, /, *, node=None: "http://x.example",
                provider_factory=refusing,
                chain_mismatch_error=_FakeChainMismatchError,
            )

        assert refusal.value.message is not None
        assert "1" in refusal.value.message
        assert "2" in refusal.value.message


class TestFactoryRaisesWhenNoSource:
    """No layer supplied an endpoint: the cascade's own refusal escapes."""

    def test_raises_rpc_not_configured(self) -> None:
        with pytest.raises(RpcNotConfiguredError) as exc_info:
            get_provider_from_config(chain_id=_UNCONFIGURED_CHAIN)

        assert "DEGENBOT_RPC_HTTP_CHAINID_" + str(_UNCONFIGURED_CHAIN) in str(exc_info.value)

    def test_it_stays_a_value_error(self) -> None:
        with pytest.raises(ValueError):  # noqa: PT011 - the class is the assertion
            get_provider_from_config(chain_id=_UNCONFIGURED_CHAIN)


class TestFactoryTakesOverrideKeywords:
    """The override keywords are the explicit layer, ahead of every configured one."""

    def test_the_node_override_is_classified_by_the_core(self) -> None:
        seen: dict[str, Any] = {}

        def fake_resolve(chain_id: int, /, *, node: str | None = None) -> str:
            seen["node"] = node
            return "http://from-resolver.example"

        get_provider_from_config(
            chain_id=1,
            node="ipc:///tmp/anvil.ipc",
            resolve_uri=fake_resolve,
            provider_factory=lambda endpoint, *, chain_id=None: _FakeAlloy(
                endpoint, chain_id=chain_id
            ),
        )

        assert seen["node"] == "ipc:///tmp/anvil.ipc"

    def test_a_retired_override_keyword_is_refused(self) -> None:
        """The factory's surface is the override keywords and nothing else."""
        with pytest.raises(TypeError, match="unexpected keyword argument"):
            get_provider_from_config(chain_id=1, fallback_http="http://x.example")
