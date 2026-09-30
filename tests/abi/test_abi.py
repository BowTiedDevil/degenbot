"""Tests for the `degenbot.abi` home — the stable mirror for ``_ffi.abi``.

Exercises the public functions (``encode``, ``encode_packed``, ``decode``,
``decode_single``). The Rust ``degenbot-abi`` core is the only backend.
``encode``/``decode`` behavior is cross-checked live against the
independent ``eth_abi`` implementation; ``encode_packed`` has no eth_abi
equivalent, so its fixtures stay hand-pinned.
"""

import eth_abi
import pytest

from degenbot.abi import (
    AbiDecodeError,
    AbiEncodeError,
    decode,
    decode_single,
    encode,
    encode_packed,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.utils.bytes import to_bytes


class TestEncode:
    """Round-trip and parity tests for ``encode``."""

    def test_encode_uint256_address_eth_abi_parity(self) -> None:
        """Encoding is byte-for-byte identical to eth_abi."""
        types = ["uint256", "address"]
        args = [100, "0x" + "00" * 20]
        result = encode(types, args)
        assert isinstance(result, bytes)
        assert len(result) == 64
        assert result == eth_abi.encode(types, args)

    def test_encode_uint256(self) -> None:
        """Simple uint256 encoding."""
        result = encode(["uint256"], [42])
        assert len(result) == 32
        assert result == eth_abi.encode(["uint256"], [42])

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
        data = eth_abi.encode(["uint256"], [12345])
        result = decode(["uint256"], data)
        assert result == (12345,)

    def test_decode_uint256_bytes(self) -> None:
        """Decode accepts bytes input."""
        data = eth_abi.encode(["uint256"], [12345])
        result = decode(["uint256"], to_bytes(data))
        assert result == (12345,)

    def test_decode_address_checksum(self) -> None:
        """Addresses are checksummed by default (the only supported mode)."""
        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = eth_abi.encode(["address"], [addr])
        result = decode(["address"], data)
        assert result[0] == get_checksum_address(addr)

    def test_decode_multiple_types(self) -> None:
        """Decode multiple types at once."""
        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        types = ["uint256", "address", "bool"]
        data = eth_abi.encode(types, [100, addr, True])
        result = decode(types, data)
        assert result[0] == 100
        assert result[1] == get_checksum_address(addr)
        assert result[2] is True

    def test_decode_bytes(self) -> None:
        """Decode dynamic bytes."""
        test_value = b"hello world"
        data = eth_abi.encode(["bytes"], [test_value])
        result = decode(["bytes"], data)
        assert result[0] == test_value

    def test_decode_string(self) -> None:
        """Decode string."""
        test_value = "Hello, Ethereum!"
        data = eth_abi.encode(["string"], [test_value])
        result = decode(["string"], data)
        assert result[0] == test_value

    def test_decode_dynamic_array(self) -> None:
        """Decode dynamic array."""
        test_value = [1, 2, 3, 4, 5]
        data = eth_abi.encode(["uint256[]"], [test_value])
        result = decode(["uint256[]"], data)
        assert list(result[0]) == test_value

    def test_decode_fixed_array(self) -> None:
        """Decode fixed-size array."""
        test_value = [10, 20, 30]
        data = eth_abi.encode(["uint256[3]"], [test_value])
        result = decode(["uint256[3]"], data)
        assert list(result[0]) == test_value

    def test_decode_address_array(self) -> None:
        """Decode address array."""
        addr1 = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        addr2 = "0x66f9664f97f2b50f62d13ea064982f936de76657"
        data = eth_abi.encode(["address[]"], [[addr1, addr2]])
        result = decode(["address[]"], data)
        assert result[0][0] == get_checksum_address(addr1)
        assert result[0][1] == get_checksum_address(addr2)

    def test_decode_empty_types_raises(self) -> None:
        """Decoding with empty types list raises AbiDecodeError."""
        with pytest.raises(AbiDecodeError, match="ABI decoding failed"):
            decode([], b"some data")

    def test_decode_normalized_bytes_same_result(self) -> None:
        """bytes and plain bytes produce the same result."""
        raw = eth_abi.encode(["uint256", "bool"], [100, True])
        from_bytes = decode(["uint256", "bool"], raw)
        from_hex = decode(["uint256", "bool"], to_bytes(raw))
        assert from_bytes == from_hex


class TestDecodeSingle:
    """Tests for ``decode_single``."""

    def test_decode_single_uint256(self) -> None:
        """Decode a single uint256."""
        data = eth_abi.encode(["uint256"], [42])
        result = decode_single("uint256", data)
        assert result == 42

    def test_decode_single_address(self) -> None:
        """Decode a single address (checksummed)."""
        addr = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = eth_abi.encode(["address"], [addr])
        result = decode_single("address", data)
        assert result == get_checksum_address(addr)

    def test_decode_single_bytes(self) -> None:
        """Decode single with bytes input."""
        data = eth_abi.encode(["uint256"], [999])
        result = decode_single("uint256", to_bytes(data))
        assert result == 999


class TestUnsupportedTypes:
    """Fixed-point and other unsupported types raise (no eth_abi fallback).

    The Rust ``degenbot-abi`` core raises ``NotImplementedError`` for these;
    the home wraps it as ``AbiEncodeError`` / ``AbiDecodeError``.
    """

    def test_decode_fixed128x18_raises(self) -> None:
        """fixed128x18 decode is not supported — raises AbiDecodeError.

        eth_abi encodes fixed128x18 happily, isolating the failure to our
        decoder's intentionally unsupported type.
        """
        data = eth_abi.encode(["fixed128x18"], [1])
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


# Pinned tuple vectors from eth_abi 5.x.
_SINGLE_TUP_HEX = (
    "000000000000000000000000000000000000000000000000000000000000002000000000000000000000"
    "000011111111111111111111111111111111111111110000000000000000000000000000000000000000"
    "000000000000000000000001000000000000000000000000000000000000000000000000000000000000"
    "006000000000000000000000000000000000000000000000000000000000000000010100000000000000"
    "000000000000000000000000000000000000000000000000"
)
_DYNARR1_TUP_HEX = (
    "000000000000000000000000000000000000000000000000000000000000002000000000000000000000"
    "000000000000000000000000000000000000000000010000000000000000000000000000000000000000"
    "000000000000000000000020000000000000000000000000111111111111111111111111111111111111"
    "111100000000000000000000000000000000000000000000000000000000000000010000000000000000"
    "000000000000000000000000000000000000000000000060000000000000000000000000000000000000"
    "000000000000000000000000000101000000000000000000000000000000000000000000000000000000"
    "00000000"
)
_NESTED_TUP_HEX = (
    "000000000000000000000000000000000000000000000000000000000000002000000000000000000000"
    "000000000000000000000000000000000000000000010000000000000000000000000000000000000000"
    "000000000000000000000007000000000000000000000000000000000000000000000000000000000000"
    "007b0000000000000000000000000000000000000000000000000000000000000001"
)
_FIXED2_TUP_HEX = (
    "000000000000000000000000000000000000000000000000000000000000000100000000000000000000"
    "000000000000000000000000000000000000000000010000000000000000000000000000000000000000"
    "000000000000000000000002000000000000000000000000000000000000000000000000000000000000"
    "0000"
)
_NESTEDDEC_TUP_HEX = (
    "000000000000000000000000000000000000000000000000000000000000002a00000000000000000000"
    "000011111111111111111111111111111111111111110000000000000000000000000000000000000000"
    "000000000000000000000001"
)


_TUP_ADDR = "0x1111111111111111111111111111111111111111"


class TestTuples:
    """Solidity tuple type strings with pinned byte vectors.

    Value shapes follow Solidity semantics: one outer value per type;
    an array's value is a list whose elements are themselves tuples
    (each a list/tuple of the component values)."""

    def test_encode_tuple(self) -> None:
        result = encode(["(address,bool,bytes)"], [(_TUP_ADDR, True, b"\x01")])
        assert len(result) == 192
        assert result == bytes.fromhex(_SINGLE_TUP_HEX)

    def test_encode_tuple_in_dynamic_array(self) -> None:
        result = encode(["(address,bool,bytes)[]"], [[(_TUP_ADDR, True, b"\x01")]])
        assert result == bytes.fromhex(_DYNARR1_TUP_HEX)

    def test_encode_nested_tuple_array(self) -> None:
        result = encode(["(uint8,(uint256,bool))[]"], [[(7, (123, True))]])
        assert result == bytes.fromhex(_NESTED_TUP_HEX)

    def test_encode_fixed_array_of_tuples(self) -> None:
        result = encode(["(uint256,bool)[2]"], [[(1, True), (2, False)]])
        assert result == bytes.fromhex(_FIXED2_TUP_HEX)

    def test_decode_nested_tuple(self) -> None:
        data = bytes.fromhex(_NESTEDDEC_TUP_HEX)
        result = decode(["((uint256,address),bool)"], data)
        # tuple components decode as plain Python lists (like arrays)
        assert result[0][0][0] == 42
        assert result[0][0][1].lower() == _TUP_ADDR.lower()
        assert result[0][1] is True

    def test_decode_tuple_elements_are_lists(self) -> None:
        data = bytes.fromhex(_NESTEDDEC_TUP_HEX)
        result = decode(["((uint256,address),bool)"], data)
        assert isinstance(result[0], list)
        assert isinstance(result[0][0], list)
        assert result[0][0][0] == 42
        assert result[0][1] is True

    def test_tuple_mismatched_component_count(self) -> None:
        with pytest.raises(AbiEncodeError):
            encode(["(uint256,bool)"], [(1,)])
