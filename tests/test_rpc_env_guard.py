"""Red-capable guard: ``tests/helpers/rpc_env`` must govern the config holder.

The chain-1 RPC resolution path is HOLDER-FED: ``ArbitrageConfig.build``
delegates to ``degenbot.config.resolve_rpc_uris``, which reads the installed
``resolved_config()`` verdict, and the holder is first-wins (installed once at
FFI module init). A bare ``monkeypatch.setenv("DEGENBOT_RPC_*", ...)`` after
import is therefore a silent no-op; only a re-seam
(``reinstall_config_holder_for_tests``) makes the override govern the next
construction.

These tests are RED if that seam is ever removed from the helper: the ambient
snapshot would keep governing and the loopback assertions would trip (the prior
helper did exactly this -- see the chunk sign-off's current-behavior
demonstration).
"""

from __future__ import annotations

from degenbot.config import resolve_http_rpc_uri, resolve_ws_rpc_uri
from degenbot.provider.factory import get_provider_from_config
from tests.helpers.rpc_env import rpc_env
from tests.helpers.seam import resolved_node_uri_for_tests

#: Request scope prefers a WS endpoint over HTTP, so the loopback override the
#: helper installs surfaces as the WS twin on the request path.
_LOOPBACK_REQUEST = "ws://localhost:8546"
_LOOPBACK_SUBSCRIPTION = "ws://localhost:8546"


def test_rpc_env_governs_the_holder_request_scope() -> None:
    """Inside the helper, request scope resolves the loopback WS twin."""
    with rpc_env():
        assert resolved_node_uri_for_tests(1, "request") == _LOOPBACK_REQUEST
        assert resolve_http_rpc_uri(1) == _LOOPBACK_REQUEST


def test_rpc_env_governs_the_holder_subscription_scope() -> None:
    """Inside the helper, subscription scope resolves the loopback WS twin."""
    with rpc_env():
        assert resolved_node_uri_for_tests(1, "subscription") == _LOOPBACK_SUBSCRIPTION
        assert resolve_ws_rpc_uri(1) == _LOOPBACK_SUBSCRIPTION


def test_rpc_env_governs_the_consumer_construction() -> None:
    """The holder-fed consumer step (the provider factory) sees the override.

    ``Bot.__init__`` runs exactly this step when no provider is injected, so
    this is the path the helper exists to steer.
    """
    with rpc_env():
        seen: list[str] = []

        def _recording_provider(endpoint: str, *, chain_id: int) -> object:
            seen.append(endpoint)
            return object()

        get_provider_from_config(chain_id=1, provider_factory=_recording_provider)
        assert seen == [_LOOPBACK_REQUEST]


def test_rpc_env_restores_the_ambient_snapshot_on_exit() -> None:
    """The context manager re-governs the ambient snapshot on exit."""
    ambient = resolved_node_uri_for_tests(1, "request")
    with rpc_env():
        assert resolved_node_uri_for_tests(1, "request") == _LOOPBACK_REQUEST
    assert resolved_node_uri_for_tests(1, "request") == ambient
    assert resolve_http_rpc_uri(1) == ambient
