"""Install the chain-1 RPC endpoints the typed config cascade reads, governed.

``ArbitrageConfig.build`` resolves its node endpoints through the
``degenbot-config`` process holder (``resolve_rpc_uris`` reads the installed
``resolved_config()`` verdict), and the holder is FIRST-WINS: the FFI module
init installs ONE config for the process, so a bare
``monkeypatch.setenv("DEGENBOT_RPC_*", ...)`` after import is a silent NO-OP --
the ambient snapshot keeps governing and the override never reaches the next
construction. This helper therefore installs the loopback endpoints as a
CAPTURED environment through the config-holder seam
(``reinstall_config_holder_for_tests``), merged over the suite's declaration
(``tests.conftest.SUITE_ENV``) -- and like ``tests/conftest.py::governed_holder``,
it never writes the process environment.

Tests install loopback endpoints on the standard node ports so a stray dial
fails loudly; never connected to. Use as a context manager inside an autouse
fixture::

    @pytest.fixture(autouse=True)
    def _rpc_env() -> Iterator[None]:
        with rpc_env():
            yield
"""

from __future__ import annotations

from contextlib import contextmanager
from typing import TYPE_CHECKING

from degenbot._ffi import reinstall_config_holder_for_tests
from tests.conftest import SUITE_ENV

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
    """Govern the chain-1 RPC endpoints + config holder for one test.

    Installs the loopback ``DEGENBOT_RPC_*`` overrides as the captured
    environment (merged over ``SUITE_ENV``) and re-governs the holder; on exit
    it re-installs the suite's declared snapshot (same contract as
    ``tests/conftest.py::governed_holder``). The process environment is never
    written.
    """
    try:
        reinstall_config_holder_for_tests({**SUITE_ENV, **_LOOPBACK_ENV})
        yield
    finally:
        reinstall_config_holder_for_tests(SUITE_ENV)
