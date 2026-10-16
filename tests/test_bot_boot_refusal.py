"""A bot that cannot reach its node refuses fast and typed.

Provider boot is the one construction step that dials the network, so a bad
endpoint is exactly what an operator ships by mistake. A name that does not
resolve is deterministic — backoff cannot fix it — so the boot must fail
immediately on the build-refusal taxonomy and must not spend its retry budget
first (an inert endpoint used to burn ~51s of DNS retries before giving up).
"""

from __future__ import annotations

import logging

import pytest

from degenbot.bot import Bot
from degenbot.exceptions import DegenbotValueError

UNRESOLVABLE_NODE = "ws://offline-node.invalid:8546"


def test_boot_refuses_an_unresolvable_node_typed_and_without_retries(caplog) -> None:
    with caplog.at_level(logging.WARNING, logger="degenbot.rpc"):
        with pytest.raises(DegenbotValueError):
            Bot(chain_id=1, database=":memory:", node=UNRESOLVABLE_NODE)
    retries = [record for record in caplog.records if "WS connect retry" in record.getMessage()]
    assert retries == [], f"name resolution spent {len(retries)} retries before giving up"
