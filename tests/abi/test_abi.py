"""Tests for the `degenbot.abi` home — the stable mirror for ``_ffi.abi``.

Exercises the three public functions (``encode``, ``decode``,
``decode_single``). The Rust ``degenbot-abi`` core is the only backend;
reference fixtures are byte-pinned from eth_abi 5.x (provenance noted
inline).
"""

import pytest

from degenbot.abi import (
    AbiDecodeError,
    AbiEncodeError,
    decode,
    decode_single,
    encode,
    encode_packed,
)
from degenbot.utils.bytes import to_bytes


class TestEncode:
    """Round-trip and parity tests for ``encode``."""

    def test_encode_uint256_address_rust_parity(self) -> None:
        """Rust backend encodes byte-for-byte identically to eth_abi."""
        types = ["uint256", "address"]
        args = [100, "0x" + "00" * 20]
        result = encode(types, args)
        assert isinstance(result, bytes)
        assert len(result) == 64
        assert result == bytes.fromhex("00" * 31 + "64" + "00" * 32)  # pinned eth_abi 5.x

    def test_encode_uint256(self) -> None:
        """Simple uint256 encoding."""
        result = encode(["uint256"], [42])
        assert len(result) == 32
        assert result == bytes.fromhex(
            "000000000000000000000000000000000000000000000000000000000000002a"
        )  # pinned eth_abi 5.x

    def test_encode_empty_types(self) -> None:
        """Empty types list produces empty bytes."""
        result = encode([], [])
        assert isinstance(result, bytes)


class TestEncodePacked:
    """Parity tests for ``encode_packed`` (Solidity ``abi.encodePacked``)."""

    def test_packed_address_address_bool(self) -> None:
        """Aerodrome CREATE2 salt: (address, address, bool) packs to 41 bytes."""
        addr1 = "0x" + "11" * 20
        addr2 = "0x" + "22" * 20
        result = encode_packed(["address", "address", "bool"], [addr1, addr2, True])
        expected = bytes.fromhex("11" * 20 + "22" * 20 + "01")  # pinned eth_abi 5.x
        assert result == expected
        assert len(result) == 41

    def test_packed_address_address(self) -> None:
        """Uniswap V2 CREATE2 salt: (address, address) packs to 40 bytes."""
        addr1 = "0x" + "aa" * 20
        addr2 = "0x" + "bb" * 20
        result = encode_packed(["address", "address"], [addr1, addr2])
        expected = b"\xaa" * 20 + b"\xbb" * 20  # pinned eth_abi 5.x
        assert result == expected
        assert len(result) == 40

    def test_packed_uint24(self) -> None:
        """uint24 packs to 3 bytes big-endian, no padding."""
        result = encode_packed(["uint24"], [0x010203])
        expected = bytes.fromhex("010203")  # pinned eth_abi 5.x
        assert result == expected
        assert result == b"\x01\x02\x03"

    def test_packed_int8_negative(self) -> None:
        """int8 -1 packs to a single 0xff byte (two's complement)."""
        result = encode_packed(["int8"], [-1])
        expected = bytes.fromhex("ff")  # pinned eth_abi 5.x
        assert result == expected
        assert result == b"\xff"

    def test_packed_mixed_widths(self) -> None:
        """uint24 + address packs to 3 + 20 = 23 bytes."""
        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        result = encode_packed(["uint24", "address"], [0x010203, addr])
        expected = bytes.fromhex(
            "010203d3cda913deb6f67967b99d67acdfa1712c293601"
        )  # pinned eth_abi 5.x
        assert result == expected

    def test_packed_bytes32(self) -> None:
        """bytes32 packs to 32 bytes, no padding change."""
        value = b"\x00" * 32
        result = encode_packed(["bytes32"], [value])
        expected = b"\x00" * 32  # pinned eth_abi 5.x
        assert result == expected

    def test_packed_bytes_as_address(self) -> None:
        """20-byte bytes is accepted as ``address`` (eth_abi parity)."""
        addr_bytes = to_bytes("0x" + "11" * 20)
        result = encode_packed(["address", "address"], [addr_bytes, addr_bytes])
        expected = b"\x11" * 40  # pinned eth_abi 5.x
        assert result == expected

    def test_packed_empty(self) -> None:
        """Empty types list produces empty bytes."""
        result = encode_packed([], [])
        assert isinstance(result, bytes)
        assert len(result) == 0

    def test_packed_rejects_fixed_point(self) -> None:
        """fixed128x18 raises AbiEncodeError (no eth_abi fallback)."""
        with pytest.raises(AbiEncodeError, match="packed encoding failed"):
            encode_packed(["fixed128x18"], [1])

    def test_packed_mismatched_counts_raises(self) -> None:
        """Mismatched types/values count raises ValueError -> AbiEncodeError."""
        with pytest.raises(AbiEncodeError):
            encode_packed(["uint256", "bool"], [42])


class TestDecode:
    """Round-trip and parity tests for ``decode``."""

    def test_decode_uint256(self) -> None:
        """Decode uint256 from eth_abi-encoded data."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000003039"
        )  # pinned eth_abi 5.x
        result = decode(["uint256"], data)
        assert result == (12345,)

    def test_decode_uint256_bytes(self) -> None:
        """Decode accepts bytes input."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000003039"
        )  # pinned eth_abi 5.x
        result = decode(["uint256"], to_bytes(data))
        assert result == (12345,)

    def test_decode_address_checksum(self) -> None:
        """Addresses are checksummed by default (the only supported mode)."""
        from degenbot.checksum_cache import get_checksum_address

        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = bytes.fromhex(
            "000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"
        )  # pinned eth_abi 5.x
        result = decode(["address"], data)
        assert result[0] == get_checksum_address(addr)

    def test_decode_multiple_types(self) -> None:
        """Decode multiple types at once."""
        from degenbot.checksum_cache import get_checksum_address

        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000064000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c2936010000000000000000000000000000000000000000000000000000000000000001"
        )  # pinned eth_abi 5.x
        result = decode(["uint256", "address", "bool"], data)
        assert result[0] == 100
        assert result[1] == get_checksum_address(addr)
        assert result[2] is True

    def test_decode_bytes(self) -> None:
        """Decode dynamic bytes."""
        test_value = b"hello world"
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000b68656c6c6f20776f726c64000000000000000000000000000000000000000000"
        )  # pinned eth_abi 5.x
        result = decode(["bytes"], data)
        assert result[0] == test_value

    def test_decode_string(self) -> None:
        """Decode string."""
        test_value = "Hello, Ethereum!"
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000001048656c6c6f2c20457468657265756d2100000000000000000000000000000000"
        )  # pinned eth_abi 5.x
        result = decode(["string"], data)
        assert result[0] == test_value

    def test_decode_dynamic_array(self) -> None:
        """Decode dynamic array."""
        test_value = [1, 2, 3, 4, 5]
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000000500000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000300000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000005"
        )  # pinned eth_abi 5.x
        result = decode(["uint256[]"], data)
        assert list(result[0]) == test_value

    def test_decode_fixed_array(self) -> None:
        """Decode fixed-size array."""
        test_value = [10, 20, 30]
        data = bytes.fromhex(
            "000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000014000000000000000000000000000000000000000000000000000000000000001e"
        )  # pinned eth_abi 5.x
        result = decode(["uint256[3]"], data)
        assert list(result[0]) == test_value

    def test_decode_address_array(self) -> None:
        """Decode address array."""
        from degenbot.checksum_cache import get_checksum_address

        addr1 = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        addr2 = "0x66f9664f97f2b50f62d13ea064982f936de76657"
        data = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000002000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c29360100000000000000000000000066f9664f97f2b50f62d13ea064982f936de76657"
        )  # pinned eth_abi 5.x
        result = decode(["address[]"], data)
        assert result[0][0] == get_checksum_address(addr1)
        assert result[0][1] == get_checksum_address(addr2)

    def test_decode_empty_types_raises(self) -> None:
        """Decoding with empty types list raises AbiDecodeError."""
        with pytest.raises(AbiDecodeError, match="ABI decoding failed"):
            decode([], b"some data")

    def test_decode_normalized_bytes_same_result(self) -> None:
        """bytes and plain bytes produce the same result."""
        raw = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000000640000000000000000000000000000000000000000000000000000000000000001"
        )  # pinned eth_abi 5.x
        from_bytes = decode(["uint256", "bool"], raw)
        from_hex = decode(["uint256", "bool"], to_bytes(raw))
        assert from_bytes == from_hex


class TestDecodeSingle:
    """Tests for ``decode_single``."""

    def test_decode_single_uint256(self) -> None:
        """Decode a single uint256."""
        data = bytes.fromhex(
            "000000000000000000000000000000000000000000000000000000000000002a"
        )  # pinned eth_abi 5.x
        result = decode_single("uint256", data)
        assert result == 42

    def test_decode_single_address(self) -> None:
        """Decode a single address (checksummed)."""
        from degenbot.checksum_cache import get_checksum_address

        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = bytes.fromhex(
            "000000000000000000000000d3cda913deb6f67967b99d67acdfa1712c293601"
        )  # pinned eth_abi 5.x
        result = decode_single("address", data)
        assert result == get_checksum_address(addr)

    def test_decode_single_bytes(self) -> None:
        """Decode single with bytes input."""
        data = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000003e7"
        )  # pinned eth_abi 5.x
        result = decode_single("uint256", to_bytes(data))
        assert result == 999


class TestUnsupportedTypes:
    """Fixed-point and other unsupported types raise (no eth_abi fallback).

    The Rust ``degenbot-abi`` core raises ``NotImplementedError`` for these;
    the home wraps it as ``AbiEncodeError`` / ``AbiDecodeError``.
    """

    def test_decode_fixed128x18_raises(self) -> None:
        """fixed128x18 decode is not supported — raises AbiDecodeError."""
        data = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000"
        )  # pinned eth_abi 5.x
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
