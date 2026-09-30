"""PyO3 seam tests for the Balancer V2 FixedPoint arithmetic leaf.

The Rust `degenbot-balancer-math` core (`fixed_point.rs`) is exposed via
the `degenbot._ffi.balancer_math` seam and re-exported as
`degenbot.balancer.math`; the stable companion's rate scaling is its
in-tree consumer. This module is the seam-level gate (exposure + boundary
error-mapping + basic parity); the math itself is cross-checked by the
frozen `degenbot-balancer-math` Rust `#[cfg(test)]` corpus + the
`oracle_crosscheck.rs` snapshot.

Error contract (mirrors `bal_err`): `ZERO_DIVISION` / `DIV_INTERNAL` raise
`OverflowError`/`ValueError` with the Solidity revert tag — NOT
`EVMRevertError`.
"""

from __future__ import annotations

import pytest

from degenbot._ffi.balancer_math import (
    fixed_point_div_down,
    fixed_point_div_up,
    fixed_point_mul_down,
)
from degenbot.balancer.libraries.constants import ONE
from degenbot.constants import MAX_UINT256


class TestFixedPointExposure:
    def test_three_fns_exposed_from_rust_seam(self) -> None:
        # import above already asserts exposure; this pins the contract.
        assert callable(fixed_point_mul_down)
        assert callable(fixed_point_div_down)
        assert callable(fixed_point_div_up)


class TestFixedPointParity:
    def test_mul_down_identity(self) -> None:
        assert fixed_point_mul_down(5 * ONE, ONE) == 5 * ONE

    def test_mul_down_rounds_down(self) -> None:
        # (3 * (1e18+1)) // 1e18 == 3 (rounds down)
        assert fixed_point_mul_down(3, ONE + 1) == 3

    def test_mul_down_zero(self) -> None:
        assert fixed_point_mul_down(0, 5 * ONE) == 0

    def test_div_down_identity(self) -> None:
        assert fixed_point_div_down(5 * ONE, ONE) == 5 * ONE

    def test_div_down_rounds_down(self) -> None:
        # (5e18+1) * 1e18 // 1e30 == 5000000 (rounds down)
        assert fixed_point_div_down(5 * ONE + 1, ONE * 10**12) == 5_000_000

    def test_div_down_zero_numerator(self) -> None:
        assert fixed_point_div_down(0, ONE) == 0

    def test_div_up_rounds_up(self) -> None:
        # ((5e18+1) * 1e18 - 1) // 1e30 + 1 == 5000001 (rounds up)
        assert fixed_point_div_up(5 * ONE + 1, ONE * 10**12) == 5_000_001

    def test_div_up_zero_numerator(self) -> None:
        assert fixed_point_div_up(0, ONE) == 0


class TestFixedPointBoundaryErrors:
    def test_div_down_zero_denominator_raises_value_error(self) -> None:
        with pytest.raises(ValueError, match="ZERO_DIVISION"):
            fixed_point_div_down(ONE, 0)

    def test_div_up_zero_denominator_raises_value_error(self) -> None:
        with pytest.raises(ValueError, match="ZERO_DIVISION"):
            fixed_point_div_up(ONE, 0)

    def test_div_down_overflow_raises_overflow_error(self) -> None:
        # a * ONE overflows uint256 → DIV_INTERNAL
        with pytest.raises(OverflowError, match="DIV_INTERNAL"):
            fixed_point_div_down(MAX_UINT256, 1)

    def test_div_up_overflow_raises_overflow_error(self) -> None:
        with pytest.raises(OverflowError, match="DIV_INTERNAL"):
            fixed_point_div_up(MAX_UINT256, 1)
