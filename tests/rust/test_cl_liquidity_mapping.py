"""Parity test: the `cl_get_tick_word_and_bit_position` PyO3 seam (ZJEL3N).

The Rust seam (``degenbot._ffi.cl_get_tick_word_and_bit_position``) wraps
the pure-Rust core ``degenbot_concentrated_liquidity_math::liquidity_mapping::get_tick_word_
and_bit_position`` (the Uniswap V3 ``TickBitmap.position`` Solidity-equivalent:
EVM division truncates toward zero on the spacing compression, then ``word =
compressed >> 8`` + ``bit = compressed.rem_euclid(256)``).

The 6 V3/V4 callsites (the V3+V4 liquidity pools + the 4 builders) now route
through this seam, + the pure-Python ``apply_liquidity_mapping_update`` /
``flip_tick`` / ``get_tick_word_and_bit_position`` mirrors were RETIRED with
their parity oracle (§4.3) once the callsite cutover + the Rust
self-contained ``#[cfg(test)]`` suite in
``degenbot-concentrated-liquidity-math/src/liquidity_mapping.rs`` (68 tests) stayed green.
The Python-side parity test below asserts against HARDCODED expectations
derived from the Solidity ``TickBitmap.position`` spec — it no longer depends on
the retired Python oracle, so it remains a valid regression guard that the seam
stays wired + byte-correct (the lasting byte-for-byte guarantee is the Rust
``#[cfg(test)]`` suite).
"""

from __future__ import annotations

import pytest

from degenbot.uniswap.math import (
    get_tick_word_and_bit_position as cl_get_tick_word_and_bit_position,
)

# Hardcoded ``TickBitmap.position`` expectations for the parametrized cases
# (EVM toward-zero division on the spacing compression, then word=compressed>>8,
# bit=compressed.rem_euclid(256)). Derived from the Solidity spec — NOT from the
# retired Python oracle.
_EXPECTED_WORD_BIT: dict[tuple[int, int], tuple[int, int]] = {
    # Positive ticks
    (1, 1): (0, 1),
    (256, 1): (1, 0),
    (257, 1): (1, 1),
    (0, 1): (0, 0),
    (1000, 10): (0, 100),
    (887_220, 60): (57, 195),  # a real V3 mainnet tick
    (230, 1): (0, 230),
    # Negative ticks (toward-zero division + rem_euclid(256) bit)
    (-230, 1): (-1, 26),
    (-230, 10): (-1, 233),
    (-60, 60): (-1, 255),
    (-887_220, 60): (-58, 61),
    (-1, 1): (-1, 255),
    (-256, 1): (-1, 0),
    # Spacing edges
    (60, 60): (0, 1),
    (-60, 1): (-1, 196),
}


@pytest.mark.parametrize(
    ("tick", "tick_spacing"),
    list(_EXPECTED_WORD_BIT),
)
def test_cl_get_tick_word_and_bit_position_parity(tick: int, tick_spacing: int) -> None:
    """The Rust seam ``cl_get_tick_word_and_bit_position`` must return the
    expected ``(word, bit)`` for the Uniswap V3 ``TickBitmap.position``
    Solidity-equivalent — including the negative-tick toward-zero division +
    ``rem_euclid(256)`` bit-position edge (the V3 hot path must not regress on
    tick-bitmap lookups)."""
    expected = _EXPECTED_WORD_BIT[tick, tick_spacing]
    rs_word, rs_bit = cl_get_tick_word_and_bit_position(tick, tick_spacing)
    assert (rs_word, int(rs_bit)) == expected, (
        f"tick={tick} spacing={tick_spacing}: rust={(rs_word, int(rs_bit))} expected={expected}"
    )
