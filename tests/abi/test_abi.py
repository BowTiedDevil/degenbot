"""Tests for the `degenbot.abi` home — the stable mirror for ``_ffi.abi``.

Exercises the three public functions (``encode``, ``decode``, ``decode_single``)
and the Rust→``eth_abi`` fixed-point fallback that lives as a private seam.
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
        """Empty types list produces empty bytes (or eth_abi's variant)."""
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


class TestFixedPointFallback:
    """The Rust→eth_abi fallback for fixed-point types.

    These types are not supported by the Rust core; the private seam falls
    back to ``eth_abi`` so callers never have to know about the gap.
    """

    def test_decode_fixed128x18_fallback(self) -> None:
        """fixed128x18 decode routes through eth_abi fallback."""
        data = eth_abi.abi.encode(["fixed128x18"], [1])
        result = decode(["fixed128x18"], data)
        assert result == (1,)

    def test_encode_fixed128x18_fallback(self) -> None:
        """fixed128x18 encode routes through eth_abi fallback."""
        result = encode(["fixed128x18"], [1])
        assert result == eth_abi.abi.encode(["fixed128x18"], [1])


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


class TestEnvVar:
    """``DEGENBOT_USE_RUST_ABI_DECODER`` controls the backend."""

    @pytest.mark.parametrize(
        ("env_value", "expected_rust"),
        [
            ("1", True),
            ("true", True),
            ("yes", True),
            ("0", False),
            ("false", False),
            ("no", False),
            ("off", False),
        ],
    )
    def test_env_var_controls_backend(
        self,
        env_value: str,
        expected_rust: bool,  # noqa: FBT001
        monkeypatch: pytest.MonkeyPatch,
    ) -> None:
        """The env var selects Rust vs eth_abi."""
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", env_value)
        from degenbot.abi import _use_rust_backend

        assert _use_rust_backend() is expected_rust

    def test_env_var_unset_uses_rust(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """Unset env var defaults to Rust."""
        monkeypatch.delenv("DEGENBOT_USE_RUST_ABI_DECODER", raising=False)
        from degenbot.abi import _use_rust_backend

        assert _use_rust_backend() is True

    def test_env_var_case_insensitive(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """Env var is case-insensitive."""
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "FALSE")
        from degenbot.abi import _use_rust_backend

        assert _use_rust_backend() is False

    def test_decode_uses_env_var_backend(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """When env var forces eth_abi, decode still produces correct values."""
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "false")
        data = eth_abi.abi.encode(["uint256"], [42])
        result = decode(["uint256"], data)
        assert result == (42,)
