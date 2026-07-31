"""FFI smoke tests for the verified-non-FoT hard guard (ergo 3O535Q).

The ``set_fot_verified_non_fot`` setter seeds the Rust
``FeeOnTransferRegistry`` with the operator's manually-verified standard-ERC-20
set. This is a hard classifier invariant — NOT an exemption: if the classifier
ever CONFIRMS one of these, ``fot_tokens`` panics. The panic behavior itself is
covered by the Rust unit tests in ``fot_registry.rs`` (driving a confirmation
from Python would require the full dispatch-feedback loop); these tests pin the
FFI registration + address parsing + the no-panic-until-confirmation contract.
"""

from __future__ import annotations

import pytest

from degenbot._ffi.submission import PyDispatcher

# WBTC — the canonical false-positive victim from the original report.
WBTC = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


def test_set_fot_verified_non_fot_accepts_verified_set() -> None:
    """A fresh dispatcher can be seeded with the verified set with no error."""
    d = PyDispatcher.for_block(10)
    d.set_fot_verified_non_fot([WBTC, WETH])
    # No confirmation yet → no panic; the guard is inert until a confirmation.
    assert d.fot_tokens(10) == []


def test_set_fot_verified_non_fot_empty_disables_guard() -> None:
    d = PyDispatcher.for_block(10)
    d.set_fot_verified_non_fot([])
    assert d.fot_tokens(10) == []


def test_set_fot_verified_non_fot_invalid_address_raises() -> None:
    d = PyDispatcher.for_block(10)
    with pytest.raises(ValueError, match="invalid verified non-FoT token"):
        d.set_fot_verified_non_fot(["0xnotanaddress"])


def test_set_fot_verified_non_fot_replaces_wholesale() -> None:
    d = PyDispatcher.for_block(10)
    d.set_fot_verified_non_fot([WBTC])
    # A second seed with a different set replaces the first wholesale.
    d.set_fot_verified_non_fot([WETH])
    assert d.fot_tokens(10) == []
