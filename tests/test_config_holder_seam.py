"""Per-test ``DEGENBOT_RPC_*`` overrides govern new constructions.

Production first-wins is unchanged: the FFI module-init boot installs ONE typed
config into the process-wide holder, and every later construction observes that
snapshot. The suite needs the opposite for A/B tests — write an env override,
re-govern the snapshot, and let the NEXT resolution observe it — so the
sanctioned raw-FFI seam ``degenbot._ffi.reinstall_config_holder_for_tests()``
re-loads env+file and reinstalls both the holder and the shell's published
verdict.

These tests are RED-CAPABLE: remove the seam and the fixture cannot re-govern
the holder, so the first construction still resolves the prior (ambient) value
and the env-A assertions trip.

The fixture lives in ``tests/conftest.py`` (``governed_holder``); it restores
the process snapshot at teardown, so a test that does not opt in is unaffected.
"""

from __future__ import annotations

import os
from collections.abc import Callable

from degenbot import _ffi
from degenbot.config import resolve_http_rpc_uri
from degenbot.provider.factory import get_provider_from_config

#: The operator-namespace names — deliberately the ``DEGENBOT_RPC_*`` family,
#: NOT the conftest ``_rpc`` vocabulary (``ETHEREUM_*_NODE_*``), which is the
#: harness's own namespace and must not be coupled.
#:
#: The WS family is used (not HTTP): request scope prefers a WS endpoint over
#: an HTTP one, so a WS override is the one whose re-governed value the
#: request-scope resolution observes directly from the holder's typed table.
_WS_NODE_ENV = "DEGENBOT_RPC_WS_CHAINID_1"

_ENV_A = "wss://seam-a.example:1111"
_ENV_B = "wss://seam-b.example:2222"


def test_env_override_governs_the_next_construction(
    governed_holder: Callable[[dict[str, str]], None],
) -> None:
    """env A -> fresh resolution is A; re-govern; env B -> fresh resolution is B.

    The prior behavior silently kept A: the holder was a process-once snapshot,
    so the B override never reached a later construction.
    """
    governed_holder({_WS_NODE_ENV: _ENV_A})

    # The holder (what ``boot_host`` reads at construction) AND the shell
    # verdict (what the Python driver helpers read) both resolve the override.
    assert _ffi.resolved_node_uri_for_tests(1, "request") == _ENV_A
    assert resolve_http_rpc_uri(1) == _ENV_A

    # A fresh construction consumes the cascade: the provider factory is the
    # exact step ``Bot.__init__`` runs when no provider is injected.
    seen: list[str] = []

    def _recording_provider(endpoint: str, *, chain_id: int) -> object:
        seen.append(endpoint)
        return object()

    get_provider_from_config(chain_id=1, provider_factory=_recording_provider)
    assert seen == [_ENV_A]

    # The re-govern that used to be ignored: B now wins over A.
    governed_holder({_WS_NODE_ENV: _ENV_B})
    assert _ffi.resolved_node_uri_for_tests(1, "request") == _ENV_B
    assert resolve_http_rpc_uri(1) == _ENV_B
    assert resolve_http_rpc_uri(1) != _ENV_A


def test_snapshot_without_opt_in_is_not_a_seam_value() -> None:
    """A test that never opts in keeps the ambient cascade, unchanged.

    Guards criterion 4: the seam must not alter the default suite's behavior.
    This environment may export a ``DEGENBOT_RPC_WS_CHAINID_1`` of its own, so
    the assertion is that the holder and the verdict AGREE and that neither is
    one of the seam's values — not that a particular layer won.
    """
    held = _ffi.resolved_node_uri_for_tests(1, "request")

    assert held == resolve_http_rpc_uri(1)
    assert held not in {_ENV_A, _ENV_B}


def test_the_environment_restores_after_a_governed_test() -> None:
    """Sanity: the fixture's teardown leaves the process env as it found it."""
    assert os.environ.get(_WS_NODE_ENV) != _ENV_A
    assert os.environ.get(_WS_NODE_ENV) != _ENV_B
