"""§4.2 parity for `_decode_reserve_configuration_bitmap` (ADR-005 §4.2/§4.3).

The Python ``_decode_reserve_configuration_bitmap`` (in
``cli/aave/event_handlers.py``) is the **parity oracle** for the Rust core's
``degenbot_rs.db_decode_reserve_configuration_bitmap`` (a PyO3 wrapper over
``degenbot_db::write::decode_reserve_configuration_bitmap``). This test asserts
the two decoders agree field-for-field across the full Aave V3 reserve-config
bit space (ltv / liquidation-threshold / -bonus / decimals / active / frozen /
borrowing-enabled / stable-rate / reserve-factor / borrow-cap / supply-cap /
debt-ceiling / liquidation-protocol-fee / unbacked-mint-cap / e-mode-category /
flash-loan / isolation-mode / borrowable-in-isolation — the full set per the
``write.rs`` docstring + the Python oracle).

Per §4.3, this parity test is **temporary**: once GREEN, CZM7TI retires the
Python oracle (``_decode_reserve_configuration_bitmap``) + this test together
(the Rust ``#[cfg(test)]`` corpus in ``write.rs`` stays as the permanent
regression set; this cross-check deleted with the oracle).

The bit masks + shifts are reproduced VERBATIM in the Rust core (the
``decode_reserve_configuration_bitmap`` docstring states this). This test is the
§4.2 proof that the reproduction is byte-exact — including the known overlap of
``unbacked_mint_cap`` (bits 168-203) + ``e_mode_category`` (bits 168-175), and
the ``e_mode_category_id`` None-when-0 convention.
"""

from __future__ import annotations

import pytest

from degenbot.cli.aave.event_handlers import _decode_reserve_configuration_bitmap
from degenbot.degenbot_rs import db_decode_reserve_configuration_bitmap

# The full key set both decoders emit (the §4.2 parity contract — column-by-column).
EXPECTED_KEYS = frozenset({
    "ltv",
    "liquidation_threshold",
    "liquidation_bonus",
    "decimals",
    "is_active",
    "is_frozen",
    "borrowing_enabled",
    "stable_rate_borrowing_enabled",
    "reserve_factor",
    "borrow_cap",
    "supply_cap",
    "debt_ceiling",
    "liquidation_protocol_fee",
    "unbacked_mint_cap",
    "e_mode_category_id",
    "flash_loan_enabled",
    "isolation_mode",
    "borrowable_in_isolation",
})


def _field(bitmap: int, shift: int, mask: int) -> int:
    """`(bitmap >> shift) & mask` — the decode helper (for corpus construction)."""
    return (bitmap >> shift) & mask


def _set_bits(shift: int, value: int) -> int:
    """Place `value` at bit `shift` (the INVERSE of the decode, for corpus construction)."""
    mask = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF
    return (value & mask) << shift


# Maximum value for each field (mask width).
_MAX = {
    "ltv": (0, 0xFFFF),
    "liquidation_threshold": (16, 0xFFFF),
    "liquidation_bonus": (32, 0xFFFF),
    "decimals": (48, 0xFF),
    "reserve_factor": (64, 0xFFFF),
    "borrow_cap": (80, 0xFFFFFFFF),
    "supply_cap": (116, 0xFFFFFFFF),
    "liquidation_protocol_fee": (152, 0xFFFF),
    "unbacked_mint_cap": (168, 0xFFFFFFFF),
    "debt_ceiling": (212, 0xFFFFFFFFFF),
}

# Flag bits (single-bit fields).
_FLAG_BITS = [56, 57, 58, 59, 61, 62, 63]


def _corpus() -> list[tuple[str, int]]:
    """A corpus of bitmaps exercising every field's bit range + corner cases."""
    bitmaps: list[tuple[str, int]] = [
        ("all-zero", 0),
        ("all-ones-bits-0-251", (1 << 252) - 1),
    ]
    # Each multi-bit field at its max (independently).
    bitmaps.extend((f"{name}=max", _set_bits(shift, mask)) for name, (shift, mask) in _MAX.items())
    # Each flag bit set independently.
    bitmaps.extend((f"flag-bit-{bit}", 1 << bit) for bit in _FLAG_BITS)
    # All flags set together (bits 56-63) + the e_mode/overlap corner cases:
    # unbacked_mint_cap=max ALSO sets e_mode_category low byte
    # (bits 168-175 = 0xFF > 0 → e_mode_category_id = 255, NOT None);
    # e_mode_category=255 alone (subset of unbacked_mint_cap);
    # e_mode_category=1 (minimum non-zero → Some(1), the None-when-0 boundary).
    bitmaps.extend([
        ("all-flags", sum(1 << b for b in range(56, 64))),
        ("overlap-unbacked-max-implies-emode-255", _set_bits(168, 0xFFFFFFFF)),
        ("e_mode_category=255", _set_bits(168, 0xFF)),
        ("e_mode_category=1", _set_bits(168, 0x01)),
    ])
    # A realistic-ish Aave config: ltv=8000, lt=8250, bonus=10500, decimals=18,
    # active+borrowing, reserve_factor=1000, e_mode=0 (None).
    realistic = (
        _set_bits(0, 8000)
        | _set_bits(16, 8250)
        | _set_bits(32, 10500)
        | _set_bits(48, 18)
        | (1 << 56)  # active
        | (1 << 58)  # borrowing_enabled
        | _set_bits(64, 1000)
    )
    # The realistic config + the e_mode=1 variant + a high-bit debt_ceiling
    # (bits 212-251 — exercises the U256 path: the bitmap exceeds 64 bits so
    # the Rust U256 extraction + Python big-int must agree).
    bitmaps.extend([
        ("realistic-emode-none", realistic),
        ("realistic-emode-1", realistic | _set_bits(168, 0x01)),
        ("debt-ceiling-high-bit", 1 << 251),
    ])
    return bitmaps


@pytest.mark.parametrize(
    ("label", "bitmap"), _corpus(), ids=lambda v: v if isinstance(v, str) else str(v)
)
def test_bit_decode_rust_matches_python_oracle(label: str, bitmap: int) -> None:
    """The Rust core's bit-decode matches the Python oracle field-for-field."""
    rust = db_decode_reserve_configuration_bitmap(bitmap)
    py = _decode_reserve_configuration_bitmap(bitmap)
    # Key set parity (catches a missing/extra key — a transcription drift).
    assert set(rust) == EXPECTED_KEYS, f"{label}: Rust keys {set(rust) ^ EXPECTED_KEYS}"
    assert set(py) == EXPECTED_KEYS, f"{label}: Python keys {set(py) ^ EXPECTED_KEYS}"
    # Value parity (byte-for-byte, including bools, ints, + e_mode_category_id None-when-0).
    assert rust == py, f"{label}: Rust {rust} != Python {py}"
