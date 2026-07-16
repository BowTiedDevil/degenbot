"""Tests for the `degenbot.abi` home — the stable mirror for ``_ffi.abi``.

Exercises the three public functions (``encode``, ``decode``,
``decode_single``). The Rust ``degenbot-abi`` core is the only backend;
``eth_abi`` is used here solely as a reference encoder for round-trip
fixtures.
"""

import eth_abi.abi
import pytest
from hexbytes import HexBytes

from degenbot.abi import AbiDecodeError, AbiEncodeError, decode, decode_single, encode


class TestEncode:
    """Round-trip and parity tests for ``encode``."""

    def test_encode_uint256_address_rust_parity(self) -> None:
        """Rust backend encodes byte-for-byte identically to eth_abi."""
        types = ["uint256", "address"]
        args = [100, "0x" + "00" * 20]
        result = encode(types, args)
        assert isinstance(result, bytes)
        assert len(result) == 64
        assert result == eth_abi.abi.encode(types, args)

    def test_encode_uint256(self) -> None:
        """Simple uint256 encoding."""
        result = encode(["uint256"], [42])
        assert len(result) == 32
        assert result == eth_abi.abi.encode(["uint256"], [42])

    def test_encode_empty_types(self) -> None:
        """Empty types list produces empty bytes."""
        result = encode([], [])
        assert isinstance(result, bytes)


class TestDecode:
    """Round-trip and parity tests for ``decode``."""

    def test_decode_uint256(self) -> None:
        """Decode uint256 from eth_abi-encoded data."""
        data = eth_abi.abi.encode(["uint256"], [12345])
        result = decode(["uint256"], data)
        assert result == (12345,)

    def test_decode_uint256_hexbytes(self) -> None:
        """Decode accepts HexBytes input."""
        data = eth_abi.abi.encode(["uint256"], [12345])
        result = decode(["uint256"], HexBytes(data))
        assert result == (12345,)

    def test_decode_address_checksum(self) -> None:
        """Addresses are checksummed by default (the only supported mode)."""
        from degenbot.checksum_cache import get_checksum_address

        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = eth_abi.abi.encode(["address"], [addr])
        result = decode(["address"], data)
        assert result[0] == get_checksum_address(addr)

    def test_decode_multiple_types(self) -> None:
        """Decode multiple types at once."""
        from degenbot.checksum_cache import get_checksum_address

        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = eth_abi.abi.encode(
            ["uint256", "address", "bool"],
            [100, addr, True],
        )
        result = decode(["uint256", "address", "bool"], data)
        assert result[0] == 100
        assert result[1] == get_checksum_address(addr)
        assert result[2] is True

    def test_decode_bytes(self) -> None:
        """Decode dynamic bytes."""
        test_value = b"hello world"
        data = eth_abi.abi.encode(["bytes"], [test_value])
        result = decode(["bytes"], data)
        assert result[0] == test_value

    def test_decode_string(self) -> None:
        """Decode string."""
        test_value = "Hello, Ethereum!"
        data = eth_abi.abi.encode(["string"], [test_value])
        result = decode(["string"], data)
        assert result[0] == test_value

    def test_decode_dynamic_array(self) -> None:
        """Decode dynamic array."""
        test_value = [1, 2, 3, 4, 5]
        data = eth_abi.abi.encode(["uint256[]"], [test_value])
        result = decode(["uint256[]"], data)
        assert list(result[0]) == test_value

    def test_decode_fixed_array(self) -> None:
        """Decode fixed-size array."""
        test_value = [10, 20, 30]
        data = eth_abi.abi.encode(["uint256[3]"], [test_value])
        result = decode(["uint256[3]"], data)
        assert list(result[0]) == test_value

    def test_decode_address_array(self) -> None:
        """Decode address array."""
        from degenbot.checksum_cache import get_checksum_address

        addr1 = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        addr2 = "0x66f9664f97f2b50f62d13ea064982f936de76657"
        data = eth_abi.abi.encode(["address[]"], [[addr1, addr2]])
        result = decode(["address[]"], data)
        assert result[0][0] == get_checksum_address(addr1)
        assert result[0][1] == get_checksum_address(addr2)

    def test_decode_empty_types_raises(self) -> None:
        """Decoding with empty types list raises AbiDecodeError."""
        with pytest.raises(AbiDecodeError, match="ABI decoding failed"):
            decode([], b"some data")

    def test_decode_hexbytes_and_bytes_same_result(self) -> None:
        """HexBytes and plain bytes produce the same result."""
        raw = eth_abi.abi.encode(["uint256", "bool"], [100, True])
        from_bytes = decode(["uint256", "bool"], raw)
        from_hex = decode(["uint256", "bool"], HexBytes(raw))
        assert from_bytes == from_hex


class TestDecodeSingle:
    """Tests for ``decode_single``."""

    def test_decode_single_uint256(self) -> None:
        """Decode a single uint256."""
        data = eth_abi.abi.encode(["uint256"], [42])
        result = decode_single("uint256", data)
        assert result == 42

    def test_decode_single_address(self) -> None:
        """Decode a single address (checksummed)."""
        from degenbot.checksum_cache import get_checksum_address

        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = eth_abi.abi.encode(["address"], [addr])
        result = decode_single("address", data)
        assert result == get_checksum_address(addr)

    def test_decode_single_hexbytes(self) -> None:
        """Decode single with HexBytes input."""
        data = eth_abi.abi.encode(["uint256"], [999])
        result = decode_single("uint256", HexBytes(data))
        assert result == 999


class TestUnsupportedTypes:
    """Fixed-point and other unsupported types raise (no eth_abi fallback).

    The Rust ``degenbot-abi`` core raises ``NotImplementedError`` for these;
    the home wraps it as ``AbiEncodeError`` / ``AbiDecodeError``.
    """

    def test_decode_fixed128x18_raises(self) -> None:
        """fixed128x18 decode is not supported — raises AbiDecodeError."""
        data = eth_abi.abi.encode(["fixed128x18"], [1])
        with pytest.raises(AbiDecodeError, match="ABI decoding failed"):
            decode(["fixed128x18"], data)

    def test_encode_fixed128x18_raises(self) -> None:
        """fixed128x18 encode is not supported — raises AbiEncodeError."""
        with pytest.raises(AbiEncodeError, match="ABI encoding failed"):
            encode(["fixed128x18"], [1])


class TestErrors:
    """Error types surface correctly."""

    def test_encode_error_on_bad_type(self) -> None:
        """Bad type raises AbiEncodeError."""
        with pytest.raises(AbiEncodeError):
            encode(["not_a_type"], [1])

    def test_decode_error_on_bad_data(self) -> None:
        """Bad data raises AbiDecodeError."""
        with pytest.raises(AbiDecodeError):
            decode(["uint256"], b"\x00" * 10)

    def test_decode_single_error_on_bad_data(self) -> None:
        """Bad data raises AbiDecodeError."""
        with pytest.raises(AbiDecodeError):
            decode_single("uint256", b"\x00" * 10)
