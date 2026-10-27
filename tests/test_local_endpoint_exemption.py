"""The local-endpoint exemption has ONE home: ``degenbot-rpc``'s predicate.

``degenbot._ffi.provider.is_local_rpc_url`` is a view of
``degenbot_rpc::provider::is_local_rpc_url``. Two lanes ask it: the core, when it
decides whether to attach the configured transport throttle (``nodes.rate_limit_*``
— see the provider's ``configured_rate_limit``), and this Python driver shell,
when :func:`degenbot.fork._default_fork_compute_units_per_second` derives a fork
target's upstream cap. A second host list in Python would let the two lanes
disagree about which hostnames a local node owns, so the parity asserted below
holds by construction rather than by two lists kept in step.

The predicate classifies by parsed host, against a substring scan
(``"://localhost"`` anywhere in the string).
``test_differing_inputs_state_parsed_host_semantics`` enumerates every input the
two spellings disagree on and states the chosen verdict.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot.fork import _default_fork_compute_units_per_second
from tests.helpers.seam import is_local_rpc_url

if TYPE_CHECKING:
    from collections.abc import Callable

#: The declared upstream cap a hosted fork target receives; a local target gets
#: no cap at all, so the cap's presence is the exemption's Python-visible face.
_CAP = 777
_CAP_ENV = {"DEGENBOT_ANVIL_FORK_COMPUTE_UNITS_PER_SECOND": str(_CAP)}

_LOCAL_URLS = (
    "http://localhost:8545",
    "http://localhost:8545/",
    "ws://localhost:8546",
    "http://127.0.0.1:8545/eth-mainnet",
    "http://[::1]:8545",
    "http://host.containers.internal:8545/",
    "http://host.docker.internal:8545/",
    "https://user:pw@localhost:8545",
    "http://LOCALHOST:8545",
    "HTTP://LocalHost:8545/",
)

_REMOTE_URLS = (
    "https://rpc.example.com",
    "http://localhost.example.com:8545",
    "http://127.0.0.1.nip.io:8545",
    "http://host.containers.internal.example.com:8545",
    "https://evil.example/?next=http://localhost:8545",
    "http://localhost:not-a-port",
    "",
)


def test_predicate_classifies_local_and_remote_endpoints() -> None:
    for url in _LOCAL_URLS:
        assert is_local_rpc_url(url) is True, url
    for url in _REMOTE_URLS:
        assert is_local_rpc_url(url) is False, url


def test_fork_default_cap_agrees_with_the_predicate(
    governed_holder: Callable[[dict[str, str]], None],
) -> None:
    """The Python path derives its cap from the SAME predicate the core throttles by."""
    governed_holder(_CAP_ENV)

    for url in _LOCAL_URLS:
        assert _default_fork_compute_units_per_second(url) is None, url
    for url in _REMOTE_URLS:
        assert _default_fork_compute_units_per_second(url) == _CAP, url

    # An in-memory node has no upstream at all.
    assert _default_fork_compute_units_per_second(None) is None


def test_differing_inputs_state_parsed_host_semantics() -> None:
    """Every input a substring scan classifies differently, decided for the host.

    ``http://localhost.example.com:8545`` — substring: local (it contains
    ``"://localhost"``); parsed host: remote. A remote name that merely starts
    with a local one still has an upstream per-IP budget to protect, so it is
    throttled and capped.

    ``http://localhost:not-a-port`` — substring: local; parsed host: remote, since
    the URL does not parse at all (no transport is ever built from it).

    ``https://user:pw@localhost:8545`` and ``http://LOCALHOST:8545`` — substring:
    remote (userinfo sits between the scheme and the host, and the scan was
    case-sensitive); parsed host: local, because the HOST is local whatever the
    userinfo or the hostname's case.
    """
    for url in (
        "http://localhost.example.com:8545",
        "http://localhost:not-a-port",
    ):
        assert is_local_rpc_url(url) is False, url
    for url in (
        "https://user:pw@localhost:8545",
        "http://LOCALHOST:8545",
    ):
        assert is_local_rpc_url(url) is True, url
