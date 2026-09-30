"""Tests for the public ABI decoder home (`degenbot.abi`).

Every behavior is cross-checked live against the independent ``eth_abi``
implementation: decoder inputs are produced by ``eth_abi``'s encoder, and
our encoder is asserted byte-identical to it — no hand-pinned byte
vectors. Hypothesis round-trips cover the fuzz-widths.
"""

import eth_abi
import hypothesis
import hypothesis.strategies as st
import pytest

from degenbot.abi import AbiDecodeError, decode, decode_single, encode
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

ADDR1 = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
ADDR2 = "0x66f9664f97f2b50f62d13ea064982f936de76657"

# (abi_type, value) — the oracle always encodes with eth_abi.
BASIC_CASES = [
    ("uint256", 0),
    ("uint256", 100),
    ("uint256", 2**256 - 1),
    ("uint8", 255),
    ("int256", 100),
    ("int256", -1),
    ("address", ADDR1),
    ("bool", True),
    ("bool", False),
    ("bytes32", b"test" + b"\x00" * 28),
    ("bytes", b""),
    ("bytes", bytes.fromhex("deadbeef")),
    ("string", "test"),
    ("uint256[]", [1, 2, 3]),
    ("uint256[3]", [1, 2, 3]),
    ("address[]", [ADDR1, ADDR2]),
]


def _expected(value: object) -> object:
    """Normalize an expected value to the public decoder's output form.

    The public decoder EIP-55 checksums addresses (including inside
    arrays); everything else round-trips unchanged.
    """
    if isinstance(value, str) and value.startswith("0x") and len(value) == 42:
        return get_checksum_address(value)
    if isinstance(value, list):
        return [_expected(item) for item in value]
    return value


class TestBasicTypes:
    """Decoding of basic static and dynamic types, oracle-encoded by eth_abi."""

    @pytest.mark.parametrize(("abi_type", "value"), BASIC_CASES)
    def test_decode_eth_abi_data(self, abi_type: str, value: object) -> None:
        """Our decoder decodes eth_abi-encoded data to the same value."""
        data = eth_abi.encode([abi_type], [value])
        (result,) = decode([abi_type], data)
        assert result == _expected(value)

    @pytest.mark.parametrize(("abi_type", "value"), BASIC_CASES)
    def test_decode_single_eth_abi_data(self, abi_type: str, value: object) -> None:
        """Single-value decode matches on eth_abi-encoded data."""
        data = eth_abi.encode([abi_type], [value])
        assert decode_single(abi_type, data) == _expected(value)

    @pytest.mark.parametrize(("abi_type", "value"), BASIC_CASES)
    def test_encode_byte_parity_with_eth_abi(self, abi_type: str, value: object) -> None:
        """Our encoder is byte-identical to eth_abi's."""
        assert encode([abi_type], [value]) == eth_abi.encode([abi_type], [value])

    def test_address_returns_eip55_checksum(self) -> None:
        """The public decoder EIP-55 checksums addresses (the only mode)."""
        data = eth_abi.encode(["address"], [ADDR1])
        (result,) = decode(["address"], data)
        assert result == get_checksum_address(ADDR1)


class TestMultipleTypes:
    """Decoding multiple types at once, oracle-encoded by eth_abi."""

    def test_uint256_and_address(self) -> None:
        """uint256 + address decode from eth_abi-encoded data."""
        types = ["uint256", "address"]
        values = [100, ADDR1]
        num, addr = decode(types, eth_abi.encode(types, values))
        assert num == 100
        assert addr == get_checksum_address(ADDR1)

    def test_multiple_static_types(self) -> None:
        """uint256 + bool + address decode from eth_abi-encoded data."""
        types = ["uint256", "bool", "address"]
        values = [100, True, ADDR1]
        num, flag, addr = decode(types, eth_abi.encode(types, values))
        assert (num, flag) == (100, True)
        assert addr == get_checksum_address(ADDR1)

    def test_encode_parity(self) -> None:
        """Multi-type encoding is byte-identical to eth_abi's."""
        types = ["uint256", "bool", "address"]
        values = [100, True, ADDR1]
        assert encode(types, values) == eth_abi.encode(types, values)


class TestAliasTypes:
    """``uint``/``int`` aliases resolve to uint256/int256."""

    def test_uint_alias(self) -> None:
        """``uint`` decodes as ``uint256``."""
        data = eth_abi.encode(["uint256"], [100])
        assert decode_single("uint", data) == 100

    def test_int_alias(self) -> None:
        """``int`` decodes as ``int256``."""
        data = eth_abi.encode(["int256"], [100])
        assert decode_single("int", data) == 100


class TestErrorHandling:
    """Error handling and edge cases (typed through the public home)."""

    def test_empty_types_list(self):
        """Test that empty types list raises AbiDecodeError."""
        with pytest.raises(AbiDecodeError, match="Types list cannot be empty"):
            decode([], b"test")

    def test_empty_data(self):
        """Test that empty data raises AbiDecodeError."""
        with pytest.raises(AbiDecodeError, match="Data cannot be empty"):
            decode_single("uint256", b"")

    def test_insufficient_data(self):
        """Test that insufficient data raises AbiDecodeError."""
        data = bytes.fromhex("0" * 30)  # Only 30 bytes, need 32
        with pytest.raises(AbiDecodeError, match="Decoding failed"):
            decode_single("uint256", data)

    def test_fixed_point_not_implemented(self):
        """Test that fixed-point types raise AbiDecodeError (wrapped core NotImplementedError).

        eth_abi encodes fixed128x18 happily — the input here is
        oracle-produced, isolating the failure to our decoder's
        intentionally unsupported type.
        """
        data = eth_abi.encode(["fixed128x18"], [1])
        with pytest.raises(AbiDecodeError, match="Fixed-point types"):
            decode_single("fixed128x18", data)


class TestEthAbiParity:
    """Byte-level parity with eth_abi on mixed head/tail (dynamic) layouts."""

    @pytest.mark.parametrize(
        ("types", "values"),
        [
            (["uint256", "string"], [100, "hello"]),
            (["string", "uint256"], ["hello", 100]),
            (["address", "bytes", "bool"], [ADDR1, b"\x01\x02", True]),
            (["uint256[]", "string"], [[1, 2], "abc"]),
            (["bytes32", "bytes", "address[]"], [b"x" * 32, b"", [ADDR1]]),
        ],
    )
    def test_mixed_layout_parity(self, types: list[str], values: list[object]) -> None:
        """Encode is byte-identical and decode round-trips eth_abi's output."""
        assert encode(types, values) == eth_abi.encode(types, values)
        results = decode(types, eth_abi.encode(types, values))
        for result, value in zip(results, values, strict=True):
            assert result == _expected(value)


class TestHypothesisStaticTypes:
    """Property-based tests: cross-encoder parity + decode round-trips."""

    @hypothesis.given(value=st.integers(min_value=MIN_UINT8, max_value=MAX_UINT8))
    def test_uint8_hypothesis(self, value: int) -> None:
        """uint8 encode parity + decode round-trip."""
        data = encode(["uint8"], [value])
        assert data == eth_abi.encode(["uint8"], [value])
        assert decode_single("uint8", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT16, max_value=MAX_UINT16))
    def test_uint16_hypothesis(self, value: int) -> None:
        """uint16 encode parity + decode round-trip."""
        data = encode(["uint16"], [value])
        assert data == eth_abi.encode(["uint16"], [value])
        assert decode_single("uint16", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT24, max_value=MAX_UINT24))
    def test_uint24_hypothesis(self, value: int) -> None:
        """uint24 encode parity + decode round-trip."""
        data = encode(["uint24"], [value])
        assert data == eth_abi.encode(["uint24"], [value])
        assert decode_single("uint24", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT32, max_value=MAX_UINT32))
    def test_uint32_hypothesis(self, value: int) -> None:
        """uint32 encode parity + decode round-trip."""
        data = encode(["uint32"], [value])
        assert data == eth_abi.encode(["uint32"], [value])
        assert decode_single("uint32", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT64, max_value=MAX_UINT64))
    def test_uint64_hypothesis(self, value: int) -> None:
        """uint64 encode parity + decode round-trip."""
        data = encode(["uint64"], [value])
        assert data == eth_abi.encode(["uint64"], [value])
        assert decode_single("uint64", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT128, max_value=MAX_UINT128))
    def test_uint128_hypothesis(self, value: int) -> None:
        """uint128 encode parity + decode round-trip."""
        data = encode(["uint128"], [value])
        assert data == eth_abi.encode(["uint128"], [value])
        assert decode_single("uint128", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_UINT256, max_value=MAX_UINT256))
    def test_uint256_hypothesis(self, value: int) -> None:
        """uint256 encode parity + decode round-trip."""
        data = encode(["uint256"], [value])
        assert data == eth_abi.encode(["uint256"], [value])
        assert decode_single("uint256", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT16, max_value=MAX_INT16))
    def test_int16_hypothesis(self, value: int) -> None:
        """int16 encode parity + decode round-trip."""
        data = encode(["int16"], [value])
        assert data == eth_abi.encode(["int16"], [value])
        assert decode_single("int16", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT24, max_value=MAX_INT24))
    def test_int24_hypothesis(self, value: int) -> None:
        """int24 encode parity + decode round-trip."""
        data = encode(["int24"], [value])
        assert data == eth_abi.encode(["int24"], [value])
        assert decode_single("int24", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT32, max_value=MAX_INT32))
    def test_int32_hypothesis(self, value: int) -> None:
        """int32 encode parity + decode round-trip."""
        data = encode(["int32"], [value])
        assert data == eth_abi.encode(["int32"], [value])
        assert decode_single("int32", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT64, max_value=MAX_INT64))
    def test_int64_hypothesis(self, value: int) -> None:
        """int64 encode parity + decode round-trip."""
        data = encode(["int64"], [value])
        assert data == eth_abi.encode(["int64"], [value])
        assert decode_single("int64", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT128, max_value=MAX_INT128))
    def test_int128_hypothesis(self, value: int) -> None:
        """int128 encode parity + decode round-trip."""
        data = encode(["int128"], [value])
        assert data == eth_abi.encode(["int128"], [value])
        assert decode_single("int128", data) == value

    @hypothesis.given(value=st.integers(min_value=MIN_INT256, max_value=MAX_INT256))
    def test_int256_hypothesis(self, value: int) -> None:
        """int256 encode parity + decode round-trip."""
        data = encode(["int256"], [value])
        assert data == eth_abi.encode(["int256"], [value])
        assert decode_single("int256", data) == value

    @hypothesis.given(address_bytes=st.binary(min_size=20, max_size=20))
    def test_address_hypothesis(self, address_bytes: bytes) -> None:
        """Address encode parity + checksummed decode round-trip."""
        data = encode(["address"], ["0x" + address_bytes.hex()])
        assert data == eth_abi.encode(["address"], ["0x" + address_bytes.hex()])
        result = decode_single(abi_type="address", data=data)
        assert result.lower() == "0x" + address_bytes.hex()

    @hypothesis.given(value=st.booleans())
    def test_bool_hypothesis(self, *, value: bool) -> None:
        """bool encode parity + decode round-trip."""
        data = encode(["bool"], [value])
        assert data == eth_abi.encode(["bool"], [value])
        assert decode_single("bool", data) is value

    @hypothesis.given(value=st.binary(min_size=32, max_size=32))
    def test_bytes32_hypothesis(self, value: bytes) -> None:
        """bytes32 encode parity + decode round-trip."""
        data = encode(["bytes32"], [value])
        assert data == eth_abi.encode(["bytes32"], [value])
        assert decode_single("bytes32", data) == value
