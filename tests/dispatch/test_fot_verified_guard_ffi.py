"""The FoT registry verdict comes from runtime suspicion alone.



A token with no suspicion evidence is never FoT, and a fresh dispatcher reports

no FoT tokens.
"""

from __future__ import annotations

from tests.helpers.seam import Dispatcher


def test_dispatcher_fot_tokens_starts_empty() -> None:
    """A fresh dispatcher has no FoT evidence and reports no FoT tokens."""
    d = Dispatcher.for_block(10)
    assert d.fot_tokens(10) == []
