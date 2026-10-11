"""Install the chain-1 RPC endpoints the typed config cascade reads, governed.

``ArbitrageConfig.build`` resolves its node endpoints through the
``degenbot-config`` process holder (``resolve_rpc_uris`` reads the installed
``resolved_config()`` verdict), and the holder is FIRST-WINS: the FFI module
init installs ONE config for the process from env + the operator file, so a
bare ``monkeypatch.setenv("DEGENBOT_RPC_*", ...)`` after import is a silent
NO-OP -- the ambient snapshot keeps governing and the override never reaches
the next construction. This helper therefore re-seams the holder
(``reinstall_config_holder_for_tests``) after setting the env, mirroring
``tests/conftest.py::governed_holder``, and re-governs the ambient snapshot at
teardown so a test that does not opt in is unchanged.

Tests install loopback endpoints on the standard node ports so a stray dial
fails loudly; never connected to. Use as a context manager inside an autouse
fixture::

    @pytest.fixture(autouse=True)
    def _rpc_env() -> Iterator[None]:
        with rpc_env():
            yield
"""

from __future__ import annotations

import os
from contextlib import contextmanager
from typing import TYPE_CHECKING

from degenbot._ffi import reinstall_config_holder_for_tests

if TYPE_CHECKING:
    from collections.abc import Iterator

#: The chain-1 loopback endpoints. Request scope prefers a WS endpoint over an
#: HTTP one, so the WS twin is what the request-scope resolution observes.
_LOOPBACK_ENV: dict[str, str] = {
    "DEGENBOT_RPC_HTTP_CHAINID_1": "http://localhost:8545",
    "DEGENBOT_RPC_WS_CHAINID_1": "ws://localhost:8546",
}


@contextmanager
def rpc_env() -> Iterator[None]:
    """Govern the chain-1 RPC envvars + config holder for one test.

    Applies the loopback ``DEGENBOT_RPC_*`` overrides, re-governs the holder,
    and on exit restores the process environment EXACTLY and re-governs the
    ambient snapshot (same contract as ``tests/conftest.py::governed_holder``).
    """
    saved = dict(os.environ)
    try:
        os.environ.update(_LOOPBACK_ENV)
        reinstall_config_holder_for_tests()
        yield
    finally:
        for name in list(os.environ):
            if name not in saved:
                del os.environ[name]
        os.environ.update(saved)
        reinstall_config_holder_for_tests()
