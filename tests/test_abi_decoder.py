"""
Tests for the Rust-based ABI decoder.

This module tests the degenbot.abi_decoder module against eth_abi.abi
to ensure compatibility and correctness.
"""

import eth_abi.abi
import pytest

from degenbot.abi_decoder import decode, decode_single


class TestBasicTypes:
    """Test decoding of basic static types."""

    def test_uint256(self):
        """Test decoding uint256 values."""
        # Test zero
        data = bytes.fromhex("0" * 64)
        result = decode_single("uint256", data)
        assert result == 0
        assert result == eth_abi.abi.decode(["uint256"], data)[0]

        # Test 100
        data = bytes.fromhex("0" * 62 + "64")
        result = decode_single("uint256", data)
        assert result == 100
        assert result == eth_abi.abi.decode(["uint256"], data)[0]

        # Test max value
        data = bytes.fromhex("f" * 64)
        result = decode_single("uint256", data)
        assert result == 2**256 - 1
        assert result == eth_abi.abi.decode(["uint256"], data)[0]

    def test_uint8(self):
        """Test decoding uint8 values."""
        data = bytes.fromhex("0" * 62 + "ff")
        result = decode_single("uint8", data)
        assert result == 255
        assert result == eth_abi.abi.decode(["uint8"], data)[0]

    def test_int256(self):
        """Test decoding int256 values."""
        # Test positive
        data = bytes.fromhex("0" * 62 + "64")
        result = decode_single("int256", data)
        assert result == 100
        assert result == eth_abi.abi.decode(["int256"], data)[0]

        # Test negative (two's complement)
        data = bytes.fromhex("f" * 64)
        result = decode_single("int256", data)
        assert result == -1
        assert result == eth_abi.abi.decode(["int256"], data)[0]

    def test_address(self):
        """Test decoding address values."""
        data = bytes.fromhex("0" * 24 + "d3cda913deb6f67967b99d67acdfa1712c293601")
        result = decode_single("address", data)
        # Both should produce valid EIP-55 checksummed addresses
        # Compare lowercase to avoid case differences in checksum
        assert result.lower() == "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        assert result.lower() == eth_abi.abi.decode(["address"], data)[0].lower()

    def test_bool(self):
        """Test decoding bool values."""
        # Test True
        data = bytes.fromhex("0" * 62 + "01")
        result = decode_single("bool", data)
        assert result is True
        assert result == eth_abi.abi.decode(["bool"], data)[0]

        # Test False
        data = bytes.fromhex("0" * 64)
        result = decode_single("bool", data)
        assert result is False
        assert result == eth_abi.abi.decode(["bool"], data)[0]

    def test_bytes32(self):
        """Test decoding bytes32 values."""
        data = bytes.fromhex("7465737400000000000000000000000000000000000000000000000000000000")
        result = decode_single("bytes32", data)
        assert result == b"test" + b"\x00" * 28
        assert result == eth_abi.abi.decode(["bytes32"], data)[0]


class TestDynamicTypes:
    """Test decoding of dynamic types (bytes, string, arrays)."""

    def test_bytes_empty(self):
        """Test decoding empty bytes."""
        # Empty bytes: offset (32) + length (0)
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000020"  # offset
            "0000000000000000000000000000000000000000000000000000000000000000"  # length
        )
        result = decode_single("bytes", data)
        assert result == b""
        assert result == eth_abi.abi.decode(["bytes"], data)[0]

    def test_bytes_non_empty(self):
        """Test decoding non-empty bytes."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000020"  # offset
            "0000000000000000000000000000000000000000000000000000000000000004"  # length = 4
            "deadbeef00000000000000000000000000000000000000000000000000000000"  # data
        )
        result = decode_single("bytes", data)
        assert result == bytes.fromhex("deadbeef")
        assert result == eth_abi.abi.decode(["bytes"], data)[0]

    def test_string(self):
        """Test decoding string values."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000020"  # offset
            "0000000000000000000000000000000000000000000000000000000000000004"  # length = 4
            "7465737400000000000000000000000000000000000000000000000000000000"  # "test"
        )
        result = decode_single("string", data)
        assert result == "test"
        assert result == eth_abi.abi.decode(["string"], data)[0]

    def test_dynamic_array_uint256(self):
        """Test decoding dynamic uint256 arrays."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000020"  # offset
            "0000000000000000000000000000000000000000000000000000000000000003"  # length = 3
            "0000000000000000000000000000000000000000000000000000000000000001"  # 1
            "0000000000000000000000000000000000000000000000000000000000000002"  # 2
            "0000000000000000000000000000000000000000000000000000000000000003"  # 3
        )
        result = decode_single("uint256[]", data)
        assert result == [1, 2, 3]
        assert result == list(eth_abi.abi.decode(["uint256[]"], data)[0])

    def test_fixed_array_uint256(self):
        """Test decoding fixed-size uint256 arrays."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000001"  # 1
            "0000000000000000000000000000000000000000000000000000000000000002"  # 2
            "0000000000000000000000000000000000000000000000000000000000000003"  # 3
        )
        result = decode_single("uint256[3]", data)
        assert result == [1, 2, 3]
        assert result == list(eth_abi.abi.decode(["uint256[3]"], data)[0])

    def test_dynamic_array_address(self):
        """Test decoding dynamic address arrays."""
        addr1 = "d3cda913deb6f67967b99d67acdfa1712c293601"
        addr2 = "66f9664f97f2b50f62d13ea064982f936de76657"
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000020"  # offset
            "0000000000000000000000000000000000000000000000000000000000000002"  # length = 2
            "000000000000000000000000"
            + addr1  # address 1
            + "000000000000000000000000"
            + addr2  # address 2
        )
        result = decode_single("address[]", data)
        # Compare lowercase to avoid case differences in EIP-55 checksums
        expected_lower = [
            "0xd3cda913deb6f67967b99d67acdfa1712c293601",
            "0x66f9664f97f2b50f62d13ea064982f936de76657",
        ]
        result_lower = [addr.lower() for addr in result]
        assert result_lower == expected_lower


class TestMultipleTypes:
    """Test decoding multiple types at once."""

    def test_uint256_and_address(self):
        """Test decoding uint256 and address together."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000064"  # uint256 = 100
            "000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"  # address
        )
        result = decode(["uint256", "address"], data)
        assert result[0] == 100
        # Compare lowercase to avoid case differences in EIP-55 checksums
        assert result[1].lower() == "0xd3cda913deb6f67967b99d67acdfa1712c293601"

        # Compare with eth_abi
        python_result = eth_abi.abi.decode(["uint256", "address"], data)
        assert result[0] == python_result[0]
        assert result[1].lower() == python_result[1].lower()

    def test_multiple_static_types(self):
        """Test decoding multiple static types."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000064"  # uint256
            "0000000000000000000000000000000000000000000000000000000000000001"  # bool = true
            "000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"  # address
        )
        result = decode(["uint256", "bool", "address"], data)
        assert result[0] == 100
        assert result[1] is True
        assert result[2] == "0xd3CdA913deB6f67967B99D67aCDFa1712C293601"


class TestTypeAliases:
    """Test that type aliases work correctly."""

    def test_uint_alias(self):
        """Test that 'uint' is an alias for 'uint256'."""
        data = bytes.fromhex("0" * 62 + "64")
        result = decode_single("uint", data)
        assert result == 100
        assert result == eth_abi.abi.decode(["uint256"], data)[0]

    def test_int_alias(self):
        """Test that 'int' is an alias for 'int256'."""
        data = bytes.fromhex("0" * 62 + "64")
        result = decode_single("int", data)
        assert result == 100
        assert result == eth_abi.abi.decode(["int256"], data)[0]


class TestErrorHandling:
    """Test error handling and edge cases."""

    def test_empty_types_list(self):
        """Test that empty types list raises ValueError."""
        with pytest.raises(ValueError, match="Types list cannot be empty"):
            decode([], b"test")

    def test_empty_data(self):
        """Test that empty data raises ValueError."""
        with pytest.raises(ValueError, match="Data cannot be empty"):
            decode_single("uint256", b"")

    def test_insufficient_data(self):
        """Test that insufficient data raises ValueError."""
        data = bytes.fromhex("0" * 30)  # Only 30 bytes, need 32
        with pytest.raises(ValueError, match="Insufficient data"):
            decode_single("uint256", data)

    def test_fixed_point_not_implemented(self):
        """Test that fixed-point types raise NotImplementedError."""
        data = bytes.fromhex("0" * 64)
        with pytest.raises(NotImplementedError, match="Fixed-point types"):
            decode_single("fixed128x18", data)

    def test_non_strict_not_implemented(self):
        """Test that non-strict mode raises NotImplementedError."""
        data = bytes.fromhex("0" * 64)
        with pytest.raises(NotImplementedError, match="Non-strict decoding"):
            decode_single("uint256", data, strict=False)


class TestEthAbiCompatibility:
    """Test that our decoder produces the same results as eth_abi.abi."""

    def test_all_basic_types(self):
        """Test all basic static types match eth_abi."""
        test_cases = [
            ("uint256", bytes.fromhex("0" * 62 + "64")),
            ("uint8", bytes.fromhex("0" * 62 + "ff")),
            ("int256", bytes.fromhex("0" * 62 + "64")),
            ("address", bytes.fromhex("0" * 24 + "d3cda913deb6f67967b99d67acdfa1712c293601")),
            ("bool", bytes.fromhex("0" * 62 + "01")),
            ("bytes32", bytes.fromhex("74657374" + "0" * 56)),
        ]

        for ty, data in test_cases:
            rust_result = decode_single(ty, data)
            python_result = eth_abi.abi.decode([ty], data)[0]
            # For addresses, compare case-insensitively since different libraries
            # may use different EIP-55 checksum algorithms
            if ty == "address":
                assert rust_result.lower() == python_result.lower(), f"Mismatch for type {ty}"
            else:
                assert rust_result == python_result, f"Mismatch for type {ty}"
