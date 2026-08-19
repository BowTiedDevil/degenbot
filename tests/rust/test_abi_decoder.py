"""Tests for the Rust-based ABI decoder.

This module tests the Rust decoder against pinned byte vectors pinned
from eth_abi 5.x at EMAU46 time, plus Hypothesis round-trips for the
fuzz-widths (cross-encoder comparison retired with eth_abi).
"""

import hypothesis
import hypothesis.strategies as st
import pytest

from degenbot._ffi.abi import decode as decode_rs
from degenbot._ffi.abi import decode_single as decode_single_rs
from degenbot.abi import encode as abi_encode
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import (
    MAX_INT16,
    MAX_INT24,
    MAX_INT32,
    MAX_INT64,
    MAX_INT128,
    MAX_INT256,
    MAX_UINT8,
    MAX_UINT16,
    MAX_UINT24,
    MAX_UINT32,
    MAX_UINT64,
    MAX_UINT128,
    MAX_UINT256,
    MIN_INT16,
    MIN_INT24,
    MIN_INT32,
    MIN_INT64,
    MIN_INT128,
    MIN_INT256,
    MIN_UINT8,
    MIN_UINT16,
    MIN_UINT24,
    MIN_UINT32,
    MIN_UINT64,
    MIN_UINT128,
    MIN_UINT256,
)


class TestBasicTypes:
    """Test decoding of basic static types."""

    def test_uint256(self):
        """Test decoding uint256 values."""
        # Test zero
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000000"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("uint256", data)
        assert result == 0

        # Test 100
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000064"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("uint256", data)
        assert result == 100

        # Test max value
        data = bytes.fromhex(
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("uint256", data)
        assert result == 2**256 - 1

    def test_uint8(self):
        """Test decoding uint8 values."""
        data = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000000ff"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("uint8", data)
        assert result == 255

    def test_int256(self):
        """Test decoding int256 values."""
        # Test positive
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000064"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("int256", data)
        assert result == 100

        # Test negative (two's complement)
        data = bytes.fromhex(
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("int256", data)
        assert result == -1

    def test_address(self):
        """Test decoding address values."""
        address = "0xd3cda913deb6f67967b99d67acdfa1712c293601"  # ruff: ignore[unused-variable] - provenance for the pin below
        address_bytes = bytes.fromhex(
            "000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"
        )  # pinned eth_abi 5.x

        checksum_result = decode_single_rs(
            abi_type="address",
            data=address_bytes,
            checksum=True,
        )
        lower_result = decode_single_rs(
            abi_type="address",
            data=address_bytes,
            checksum=False,
        )

        eth_abi_result = "0xd3cda913deb6f67967b99d67acdfa1712c293601"  # pinned (lowercase)

        # Rust decoder returns EIP-55 checksummed addresses
        assert checksum_result == get_checksum_address(eth_abi_result)
        assert lower_result == eth_abi_result

    def test_bool(self):
        """Test decoding bool values."""
        # Test True
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000001"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("bool", data)
        assert result is True

        # Test False
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000000"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("bool", data)
        assert result is False

    def test_bytes32(self):
        """Test decoding bytes32 values."""
        test_value = b"test" + b"\x00" * 28
        data = bytes.fromhex(
            "7465737400000000000000000000000000000000000000000000000000000000"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("bytes32", data)
        assert result == test_value


class TestDynamicTypes:
    """Test decoding of dynamic types (bytes, string, arrays)."""

    def test_bytes_empty(self):
        """Test decoding empty bytes."""
        data = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000000"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("bytes", data)
        assert result == b""

    def test_bytes_non_empty(self):
        """Test decoding non-empty bytes."""
        test_value = bytes.fromhex("deadbeef")
        data = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000004deadbeef00000000000000000000000000000000000000000000000000000000"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("bytes", data)
        assert result == test_value

    def test_string(self):
        """Test decoding string values."""
        test_value = "test"
        data = bytes.fromhex(
            "000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000047465737400000000000000000000000000000000000000000000000000000000"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("string", data)
        assert result == test_value

    def test_dynamic_array_uint256(self):
        """Test decoding dynamic uint256 arrays."""
        test_value = [1, 2, 3]
        data = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000003000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000003"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("uint256[]", data)
        assert result == test_value

    def test_fixed_array_uint256(self):
        """Test decoding fixed-size uint256 arrays."""
        test_value = [1, 2, 3]
        data = bytes.fromhex(
            "000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000003"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("uint256[3]", data)
        assert result == test_value

    def test_dynamic_array_address(self):
        """Test decoding dynamic address arrays."""
        addr1 = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        addr2 = "0x66f9664f97f2b50f62d13ea064982f936de76657"
        test_value = [addr1, addr2]
        data = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000002000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c29360100000000000000000000000066f9664f97f2b50f62d13ea064982f936de76657"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("address[]", data)
        # Compare lowercase to avoid case differences in EIP-55 checksums
        expected_lower = [addr.lower() for addr in test_value]
        result_lower = [addr.lower() for addr in result]
        assert result_lower == expected_lower


class TestMultipleTypes:
    """Test decoding multiple types at once."""

    def test_uint256_and_address(self):
        """Test decoding uint256 and address together."""
        test_values = [100, "0xd3cda913deb6f67967b99d67acdfa1712c293601"]  # ruff: ignore[unused-variable] - provenance for the pin below
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000064000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"
        )  # pinned eth_abi 5.x
        rust_num, rust_addr = decode_rs(types=["uint256", "address"], data=data, checksum=False)

        assert rust_num == 100
        assert rust_addr == "0xd3cda913deb6f67967b99d67acdfa1712c293601"

        # Compare with eth_abi
        py_num, py_addr = 100, "0xd3cda913deb6f67967b99d67acdfa1712c293601"  # pinned eth_abi 5.x
        assert rust_num == py_num
        assert rust_addr.lower() == py_addr

    def test_multiple_static_types(self):
        """Test decoding multiple static types."""
        test_values = [100, True, "0xd3cda913deb6f67967b99d67acdfa1712c293601"]  # ruff: ignore[unused-variable] - provenance for the pin below
        data = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000000640000000000000000000000000000000000000000000000000000000000000001000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"
        )  # pinned eth_abi 5.x
        result = decode_rs(["uint256", "bool", "address"], data)
        assert result[0] == 100
        assert result[1] is True
        assert result[2] == "0xd3CdA913deB6f67967B99D67aCDFa1712C293601"


class TestTypeAliases:
    """Test that type aliases work correctly."""

    def test_uint_alias(self):
        """Test that 'uint' is an alias for 'uint256'."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000064"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("uint", data)
        assert result == 100

    def test_int_alias(self):
        """Test that 'int' is an alias for 'int256'."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000064"
        )  # pinned eth_abi 5.x
        result = decode_single_rs("int", data)
        assert result == 100


class TestErrorHandling:
    """Test error handling and edge cases."""

    def test_empty_types_list(self):
        """Test that empty types list raises ValueError."""
        with pytest.raises(ValueError, match="Types list cannot be empty"):
            decode_rs([], b"test")

    def test_empty_data(self):
        """Test that empty data raises ValueError."""
        with pytest.raises(ValueError, match="Data cannot be empty"):
            decode_single_rs("uint256", b"")

    def test_insufficient_data(self):
        """Test that insufficient data raises ValueError."""
        data = bytes.fromhex("0" * 30)  # Only 30 bytes, need 32
        with pytest.raises(ValueError, match="Decoding failed"):
            decode_single_rs("uint256", data)

    def test_fixed_point_not_implemented(self):
        """Test that fixed-point types raise NotImplementedError."""
        data = bytes.fromhex("0" * 64)
        with pytest.raises(NotImplementedError, match="Fixed-point types"):
            decode_single_rs("fixed128x18", data)


class TestEthAbiCompatibility:
    """Pinned reference vectors for the basic static types."""

    def test_all_basic_types(self):
        """Decode pinned eth_abi 5.x vectors for all basic static types."""
        # (type, pinned data hex, expected) - pinned from eth_abi 5.x at EMAU46 time
        pinned_cases = [
            (
                "uint256",
                "0000000000000000000000000000000000000000000000000000000000000064",
                100,
            ),
            (
                "uint8",
                "00000000000000000000000000000000000000000000000000000000000000ff",
                255,
            ),
            (
                "int256",
                "0000000000000000000000000000000000000000000000000000000000000064",
                100,
            ),
            (
                "address",
                "000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601",
                "0xd3cda913deb6f67967b99d67acdfa1712c293601",
            ),
            (
                "bool",
                "0000000000000000000000000000000000000000000000000000000000000001",
                True,
            ),
            (
                "bytes32",
                "7465737400000000000000000000000000000000000000000000000000000000",
                b"test\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
            ),
        ]

        for type_, data_hex, expected in pinned_cases:
            data = bytes.fromhex(data_hex)
            rust_result = decode_single_rs(type_, data)
            if type_ == "address":
                assert rust_result.lower() == expected, f"Mismatch for type {type_}"
            else:
                assert rust_result == expected, f"Mismatch for type {type_}"


class TestHypothesisStaticTypes:
    """Property-based tests for static types using Hypothesis."""

    @hypothesis.given(value=st.integers(min_value=MIN_UINT8, max_value=MAX_UINT8))
    def test_uint8_hypothesis(self, value: int) -> None:
        """Test uint8 decoding with random values."""
        data = abi_encode(["uint8"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("uint8", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT16, max_value=MAX_UINT16))
    def test_uint16_hypothesis(self, value: int) -> None:
        """Test uint16 decoding with random values."""
        data = abi_encode(["uint16"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("uint16", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT24, max_value=MAX_UINT24))
    def test_uint24_hypothesis(self, value: int) -> None:
        """Test uint24 decoding with random values."""
        data = abi_encode(["uint24"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("uint24", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT128, max_value=MAX_UINT128))
    def test_uint128_hypothesis(self, value: int) -> None:
        """Test uint128 decoding with random values."""
        data = abi_encode(["uint128"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("uint128", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT256, max_value=MAX_UINT256))
    def test_uint256_hypothesis(self, value: int) -> None:
        """Test uint256 decoding with random values."""
        data = abi_encode(["uint256"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("uint256", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT16, max_value=MAX_INT16))
    def test_int16_hypothesis(self, value: int) -> None:
        """Test int16 decoding with random values."""
        data = abi_encode(["int16"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("int16", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT24, max_value=MAX_INT24))
    def test_int24_hypothesis(self, value: int) -> None:
        """Test int24 decoding with random values."""
        data = abi_encode(["int24"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("int24", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT32, max_value=MAX_INT32))
    def test_int32_hypothesis(self, value: int) -> None:
        """Test int32 decoding with random values."""
        data = abi_encode(["int32"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("int32", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT64, max_value=MAX_INT64))
    def test_int64_hypothesis(self, value: int) -> None:
        """Test int64 decoding with random values."""
        data = abi_encode(["int64"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("int64", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT128, max_value=MAX_INT128))
    def test_int128_hypothesis(self, value: int) -> None:
        """Test int128 decoding with random values."""
        data = abi_encode(["int128"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("int128", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT256, max_value=MAX_INT256))
    def test_int256_hypothesis(self, value: int) -> None:
        """Test int256 decoding with random values."""
        data = abi_encode(["int256"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("int256", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT32, max_value=MAX_UINT32))
    def test_uint32_hypothesis(self, value: int) -> None:
        """Test uint32 decoding with random values."""
        data = abi_encode(["uint32"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("uint32", data)
        assert rust_result == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT64, max_value=MAX_UINT64))
    def test_uint64_hypothesis(self, value: int) -> None:
        """Test uint64 decoding with random values."""
        data = abi_encode(["uint64"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("uint64", data)
        assert rust_result == value

    @hypothesis.given(address_bytes=st.binary(min_size=20, max_size=20))
    def test_address_hypothesis(self, address_bytes: bytes) -> None:
        """Test address decoding with random values."""
        byte_encoded_address = abi_encode(["address"], [address_bytes])
        rust_result = decode_single_rs(abi_type="address", data=byte_encoded_address)  # round-trip
        # Rust decoder returns EIP-55 checksummed addresses
        assert rust_result.lower() == "0x" + address_bytes.hex()  # decode of pinned layout

    def test_bool_hypothesis(self) -> None:
        """Test bool decoding with random values."""
        for value in (True, False):
            data = abi_encode(["bool"], [value])  # encode/decode round-trip
            rust_result = decode_single_rs("bool", data)
            assert rust_result is value

    @hypothesis.given(value=st.binary(min_size=32, max_size=32))
    def test_bytes32_hypothesis(self, value: bytes) -> None:
        """Test bytes32 decoding with random values."""
        data = abi_encode(["bytes32"], [value])  # encode/decode round-trip
        rust_result = decode_single_rs("bytes32", data)
        assert rust_result == value
