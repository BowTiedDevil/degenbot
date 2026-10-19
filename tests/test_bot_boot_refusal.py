"""A bot that cannot reach its node refuses fast and typed.

Provider boot is the one construction step that dials the network, so a bad
endpoint is exactly what an operator ships by mistake. A name that does not
resolve is deterministic — backoff cannot fix it — so the boot must fail
immediately on the build-refusal taxonomy and must not spend its retry budget
first (an inert endpoint used to burn ~51s of DNS retries before giving up).

The zero-retries claim is only observable when the fixture's unreachability is
a genuine name-lookup failure. A name that resolves to a refusing or hanging
port is a CONNECTION failure — retryable by design — so on a resolver that
answers the reserved fixture name (NXDOMAIN hijacking) the budget burns and
this gate fails while the product behaves correctly. The probe below therefore
declines such an environment instead of certifying a run that never saw the
fail-fast path.
"""

from __future__ import annotations

import logging
import socket

import pytest

from degenbot.bot import Bot
from degenbot.exceptions import DegenbotValueError

_FIXTURE_HOST = "offline-node.invalid"
UNRESOLVABLE_NODE = f"ws://{_FIXTURE_HOST}:8546"


def _unresolvable_node() -> str | None:
    """The fixture node when this environment can express the contract.

    Returns ``None`` when the resolver answers the fixture name: the boot would
    then see a connection failure (retryable, budget-burning), so a run here
    could neither demonstrate nor refute the zero-retries claim.
    """
    try:
        socket.getaddrinfo(_FIXTURE_HOST, 8546)
    except socket.gaierror:
        return UNRESOLVABLE_NODE
    return None


def test_the_probe_declines_the_fixture_when_the_resolver_answers_the_name(monkeypatch) -> None:
    def _answer(*args, **kwargs):
        return [(socket.AF_INET, socket.SOCK_STREAM, socket.IPPROTO_TCP, "", ("127.0.0.1", 8546))]

    monkeypatch.setattr(socket, "getaddrinfo", _answer)
    assert _unresolvable_node() is None


@pytest.mark.parametrize("errno", [socket.EAI_NONAME, socket.EAI_AGAIN], ids=["noname", "again"])
def test_the_probe_takes_the_fixture_on_any_name_lookup_failure(monkeypatch, errno) -> None:
    def _refuse(*args, **kwargs):
        raise socket.gaierror(errno, "name lookup failed")

    monkeypatch.setattr(socket, "getaddrinfo", _refuse)
    assert _unresolvable_node() == UNRESOLVABLE_NODE


def test_boot_refuses_an_unresolvable_node_typed_and_without_retries(caplog) -> None:
    node = _unresolvable_node()
    if node is None:
        pytest.skip(
            f"{_FIXTURE_HOST} resolves in this environment (reserved-name hijack); "
            "a connection failure is retryable by design, so the zero-retries "
            "contract cannot be observed here"
        )
    with caplog.at_level(logging.WARNING, logger="degenbot.rpc"):
        with pytest.raises(DegenbotValueError) as refusal:
            Bot(chain_id=1, database=":memory:", node=node)
    # The fail-fast classifier's own wording proves the refusal came from the
    # resolution verdict, not from an exhausted retry budget.
    assert "host does not resolve" in str(refusal.value)
    retries = [record for record in caplog.records if "WS connect retry" in record.getMessage()]
    assert retries == [], f"name resolution spent {len(retries)} retries before giving up"
