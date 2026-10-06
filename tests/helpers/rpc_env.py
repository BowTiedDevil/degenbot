"""Install the chain-1 RPC endpoints the typed config cascade reads.

``ArbitrageConfig.build`` resolves its node endpoints from the
``DEGENBOT_RPC_*`` envvars (see ``tests/test_config_rpc.py``); tests install
loopback endpoints on the standard node ports so the build never falls back
to ambient shell state and a stray dial fails loudly. Never connected to.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import pytest


def rpc_env(monkeypatch: pytest.MonkeyPatch) -> None:
    """Set the chain-1 RPC envvars to the loopback endpoints."""

    monkeypatch.setenv("DEGENBOT_RPC_HTTP_CHAINID_1", "http://localhost:8545")
    monkeypatch.setenv("DEGENBOT_RPC_WS_CHAINID_1", "ws://localhost:8546")
