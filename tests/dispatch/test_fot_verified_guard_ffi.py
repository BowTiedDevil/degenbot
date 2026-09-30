"""The FoT registry has no operator-attestation surface.



Fee-on-transfer verdicts come from runtime suspicion alone: a token with no

suspicion evidence is never FoT, and two distinct failing pools confirm it.

There is no operator ``verified_non_fot`` setter — an attestation cannot

suppress a verdict.
"""

from __future__ import annotations

from degenbot._ffi.submission import Dispatcher


def test_dispatcher_has_no_fot_verified_non_fot_setter() -> None:
    """The retired operator-attestation setter must not exist on the FFI."""
    assert not hasattr(Dispatcher, "set_fot_verified_non_fot")


def test_dispatcher_fot_tokens_starts_empty() -> None:
    """A fresh dispatcher has no FoT evidence and reports no FoT tokens."""
    d = Dispatcher.for_block(10)
    assert d.fot_tokens(10) == []
