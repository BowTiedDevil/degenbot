"""
Tests for Rust → Python error propagation at the FFI boundary.

Every Rust Result<T, E> crosses the PyO3 boundary as a Python exception.
These tests verify the mapping is correct for all boundary functions.
"""

import pytest

from degenbot.degenbot_rs import (
    decode,
    decode_single,
    encode_function_call,
    encode_single,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
)

from degenbot.uniswap.v3_libraries import MAX_SQRT_RATIO, MAX_TICK, MIN_TICK


class TestTickMathBoundaryErrors:
    """Test tick math error propagation across the FFI boundary."""

    def test_sqrt_ratio_below_min_tick(self):
        """Tick below MIN_TICK should raise ValueError, not crash."""
        with pytest.raises(ValueError, match="Invalid tick value"):
            get_sqrt_ratio_at_tick(MIN_TICK - 1)

    def test_sqrt_ratio_above_max_tick(self):
        """Tick above MAX_TICK should raise ValueError, not crash."""
        with pytest.raises(ValueError, match="Invalid tick value"):
            get_sqrt_ratio_at_tick(MAX_TICK + 1)

    def test_sqrt_ratio_at_extreme_negative(self):
        """Extreme negative tick should raise ValueError."""
        with pytest.raises(ValueError, match="Invalid tick value"):
            get_sqrt_ratio_at_tick(-(2**31))

    def test_sqrt_ratio_at_extreme_positive(self):
        """Extreme positive tick should raise ValueError."""
        with pytest.raises(ValueError, match="Invalid tick value"):
            get_sqrt_ratio_at_tick(2**31 - 1)

    def test_tick_at_sqrt_ratio_at_max(self):
        """Sqrt ratio >= MAX_SQRT_RATIO should raise ValueError."""
        with pytest.raises(ValueError, match="Sqrt ratio out of bounds"):
            get_tick_at_sqrt_ratio(MAX_SQRT_RATIO)

    def test_tick_at_sqrt_ratio_above_max(self):
        """Sqrt ratio > MAX_SQRT_RATIO should raise ValueError."""
        with pytest.raises(ValueError, match="Sqrt ratio out of bounds"):
            get_tick_at_sqrt_ratio(MAX_SQRT_RATIO + 1)

    def test_tick_at_sqrt_ratio_zero(self):
        """Sqrt ratio of 0 should raise ValueError."""
        with pytest.raises(ValueError, match="Sqrt ratio out of bounds"):
            get_tick_at_sqrt_ratio(0)


class TestAbiDecoderBoundaryErrors:
    """Test ABI decoder error propagation across the FFI boundary."""

    def test_decode_single_empty_data(self):
        """Empty bytes should raise ValueError, not segfault."""
        with pytest.raises(ValueError, match="Data cannot be empty"):
            decode_single("uint256", b"")

    def test_decode_single_truncated_data(self):
        """Truncated data should raise ValueError."""
        with pytest.raises(ValueError):
            decode_single("uint256", b"\x00" * 16)  # only 16 bytes, need 32

    def test_decode_empty_types(self):
        """Empty types list should raise ValueError."""
        with pytest.raises(ValueError, match="Types list cannot be empty"):
            decode([], b"\x00" * 32)

    def test_decode_unsupported_type(self):
        """Unknown type string should raise ValueError."""
        with pytest.raises(ValueError, match="Unsupported"):
            decode(["foobar"], b"\x00" * 32)

    def test_decode_wrong_type_count(self):
        """Mismatched type/value count should raise ValueError."""
        with pytest.raises(ValueError):
            decode(["uint256", "bool"], b"\x00" * 32)


class TestAbiEncoderBoundaryErrors:
    """Test ABI encoder error propagation across the FFI boundary."""

    def test_encode_invalid_signature(self):
        """Invalid function signature should raise ValueError."""
        with pytest.raises(ValueError):
            encode_function_call("not_a_valid_type", [])

    def test_encode_wrong_arg_count(self):
        """Wrong number of arguments should raise ValueError."""
        with pytest.raises(ValueError):
            # transfer requires 2 args, provide 1
            encode_function_call(
                "transfer(address,uint256)",
                ["0x" + "00" * 20],
            )

    def test_encode_single_invalid_type(self):
        """Invalid type in encode_single should raise ValueError."""
        with pytest.raises(ValueError):
            encode_single("foobar", 42)

    def test_encode_single_bytes_too_large(self):
        """bytes32 with > 32 bytes should raise ValueError."""
        with pytest.raises(ValueError):
            encode_single("bytes32", b"\x00" * 33)


def test_roundtrip_tick_to_ratio_and_back():
    """Test that tick -> sqrt_ratio -> tick roundtrip is consistent."""
    for tick in [
        -500000, -100000, -10000, -1000, -100, -10, -1, 0,
        1, 10, 100, 1000, 10000, 100000, 500000,
    ]:
        sqrt_ratio = get_sqrt_ratio_at_tick(tick)
        tick_back = get_tick_at_sqrt_ratio(sqrt_ratio)
        assert tick_back == tick, f"Roundtrip failed for tick={tick}: got {tick_back}"
