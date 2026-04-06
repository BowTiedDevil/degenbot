"""
Tests for the ABI encoding/decoding adapter.
"""

import eth_abi.abi
import pytest
from hexbytes import HexBytes

from degenbot.abi_adapter import (
    AbiAdapter,
    AbiBackend,
    AbiDecodeError,
    AbiEncodeError,
    AbiUnsupportedOperation,
    _get_default_backend_from_env,
    decode,
    decode_single,
    encode,
    get_default_adapter,
    get_default_backend,
)


class TestAbiAdapter:
    """
    Tests for the AbiAdapter class.
    """

    def test_default_backend_is_rust(self) -> None:
        """
        Test that the default backend is Rust.
        """
        adapter = AbiAdapter()
        assert adapter.backend == AbiBackend.RUST

    def test_backend_setter(self) -> None:
        """
        Test that the backend can be changed.
        """
        adapter = AbiAdapter()
        assert adapter.backend == AbiBackend.RUST

        adapter.backend = AbiBackend.ETH_ABI
        assert adapter.backend == AbiBackend.ETH_ABI

        adapter.backend = AbiBackend.RUST
        assert adapter.backend == AbiBackend.RUST

    def test_encode_with_eth_abi_backend(self) -> None:
        """
        Test encoding with eth_abi backend.
        """
        adapter = AbiAdapter(backend=AbiBackend.ETH_ABI)
        result = adapter.encode(["uint256"], [42])
        assert isinstance(result, bytes)
        assert len(result) == 32

        # Verify by decoding
        decoded = eth_abi.abi.decode(["uint256"], result)
        assert decoded[0] == 42

    def test_encode_with_rust_backend_raises(self) -> None:
        """
        Test that encoding with Rust backend raises AbiUnsupportedOperation.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        with pytest.raises(AbiUnsupportedOperation, match="Encoding is not supported"):
            adapter.encode(["uint256"], [42])

    def test_decode_uint256_rust_backend(self) -> None:
        """
        Test decoding uint256 with Rust backend.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        data = eth_abi.abi.encode(["uint256"], [12345])
        result = adapter.decode(["uint256"], data)
        assert result == (12345,)

    def test_decode_uint256_eth_abi_backend(self) -> None:
        """
        Test decoding uint256 with eth_abi backend.
        """
        adapter = AbiAdapter(backend=AbiBackend.ETH_ABI)
        data = eth_abi.abi.encode(["uint256"], [12345])
        result = adapter.decode(["uint256"], data)
        assert result == (12345,)

    def test_decode_address_checksum_true(self) -> None:
        """
        Test that addresses are checksummed when checksum=True.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        address = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = eth_abi.abi.encode(["address"], [address])
        result = adapter.decode(["address"], data, checksum=True)
        # Rust returns EIP-55 checksummed address
        assert result[0] == "0xd3CdA913deB6f67967B99D67aCDFa1712C293601"

    def test_decode_address_checksum_false(self) -> None:
        """
        Test that addresses are lowercase when checksum=False.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        address = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = eth_abi.abi.encode(["address"], [address])
        result = adapter.decode(["address"], data, checksum=False)
        assert result[0] == address

    def test_decode_multiple_types(self) -> None:
        """
        Test decoding multiple types at once.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        data = eth_abi.abi.encode(
            ["uint256", "address", "bool"],
            [100, "0xd3cda913deb6f67967b99d67acdfa1712c293601", True],
        )
        result = adapter.decode(["uint256", "address", "bool"], data)
        assert result[0] == 100
        assert result[1] == "0xd3CdA913deB6f67967B99D67aCDFa1712C293601"
        assert result[2] is True

    def test_decode_single_uint256(self) -> None:
        """
        Test decode_single for uint256.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        data = eth_abi.abi.encode(["uint256"], [999])
        result = adapter.decode_single("uint256", data)
        assert result == 999

    def test_decode_single_address(self) -> None:
        """
        Test decode_single for address.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        address = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = eth_abi.abi.encode(["address"], [address])
        result = adapter.decode_single("address", data)
        assert result == "0xd3CdA913deB6f67967B99D67aCDFa1712C293601"

    def test_decode_invalid_data_raises(self) -> None:
        """
        Test that invalid data raises AbiDecodeError.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        with pytest.raises(AbiDecodeError, match="ABI decoding failed"):
            adapter.decode(["uint256"], b"")

    def test_decode_fallback_to_eth_abi_for_unsupported_types(self) -> None:
        """
        Test that decoding falls back to eth_abi for unsupported types.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        # fixed-point types are not supported by Rust decoder
        # This should fall back to eth_abi
        # Note: eth_abi encodes fixed-point as integers (scaled)
        data = eth_abi.abi.encode(["fixed168x10"], [15])  # 1.5 scaled by 10^10
        # The Rust decoder raises NotImplementedError, adapter should fall back
        result = adapter.decode(["fixed168x10"], data)
        assert len(result) == 1

    def test_supports_encoding(self) -> None:
        """
        Test supports_encoding method.
        """
        rust_adapter = AbiAdapter(backend=AbiBackend.RUST)
        eth_adapter = AbiAdapter(backend=AbiBackend.ETH_ABI)

        assert rust_adapter.supports_encoding() is False
        assert eth_adapter.supports_encoding() is True

    def test_supports_type(self) -> None:
        """
        Test supports_type method.
        """
        rust_adapter = AbiAdapter(backend=AbiBackend.RUST)
        eth_adapter = AbiAdapter(backend=AbiBackend.ETH_ABI)

        # Encoding support
        assert rust_adapter.supports_type("uint256", "encode") is False
        assert eth_adapter.supports_type("uint256", "encode") is True

        # Decode support - common types
        assert rust_adapter.supports_type("uint256", "decode") is True
        assert eth_adapter.supports_type("uint256", "decode") is True

        # Fixed-point types - not supported by Rust
        assert rust_adapter.supports_type("fixed128x18", "decode") is False
        assert eth_adapter.supports_type("fixed128x18", "decode") is True


class TestModuleFunctions:
    """
    Tests for module-level convenience functions.
    """

    def test_encode(self) -> None:
        """
        Test the module-level encode function.
        """
        result = encode(["uint256", "address"], [100, "0x" + "00" * 20])
        assert isinstance(result, bytes)
        # Verify by decoding
        decoded = eth_abi.abi.decode(["uint256", "address"], result)
        assert decoded[0] == 100

    def test_decode_default_rust_backend(self) -> None:
        """
        Test that module-level decode uses Rust backend by default.
        """
        data = eth_abi.abi.encode(["uint256"], [42])
        result = decode(["uint256"], data)
        assert result == (42,)

    def test_decode_eth_abi_backend(self) -> None:
        """
        Test decode with explicit eth_abi backend.
        """
        data = eth_abi.abi.encode(["uint256"], [42])
        result = decode(["uint256"], data, backend=AbiBackend.ETH_ABI)
        assert result == (42,)

    def test_decode_single_default_rust_backend(self) -> None:
        """
        Test that module-level decode_single uses Rust backend by default.
        """
        data = eth_abi.abi.encode(["address"], ["0xd3cda913deb6f67967b99d67acdfa1712c293601"])
        result = decode_single("address", data)
        # Should return checksummed address
        assert result == "0xd3CdA913deB6f67967B99D67aCDFa1712C293601"

    def test_decode_single_checksum_false(self) -> None:
        """
        Test decode_single with checksum=False.
        """
        address = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        data = eth_abi.abi.encode(["address"], [address])
        result = decode_single("address", data, checksum=False)
        assert result == address

    def test_get_default_adapter(self) -> None:
        """
        Test that get_default_adapter returns a Rust-backed adapter.
        """
        adapter = get_default_adapter()
        assert adapter.backend == AbiBackend.RUST


class TestErrorHandling:
    """
    Tests for error handling.
    """

    def test_encode_invalid_type(self) -> None:
        """
        Test that encoding with invalid type raises AbiEncodeError.
        """
        # eth_abi raises ParseError for invalid type strings
        with pytest.raises(AbiEncodeError):
            encode(["invalid_type"], [42])

    def test_decode_empty_types_list(self) -> None:
        """
        Test that decoding with empty types list raises AbiDecodeError.
        """
        data = b"some data"
        with pytest.raises(AbiDecodeError, match="ABI decoding failed"):
            decode([], data)

    def test_decode_insufficient_data(self) -> None:
        """
        Test that decoding with insufficient data raises AbiDecodeError.
        """
        with pytest.raises(AbiDecodeError, match="ABI decoding failed"):
            decode(["uint256"], b"\x00" * 16)  # Need 32 bytes, only 16 provided

    def test_decode_single_empty_data(self) -> None:
        """
        Test that decode_single with empty data raises AbiDecodeError.
        """
        with pytest.raises(AbiDecodeError, match="ABI decoding failed"):
            decode_single("uint256", b"")


class TestDynamicTypes:
    """
    Tests for dynamic types (bytes, string, arrays).
    """

    def test_decode_bytes(self) -> None:
        """
        Test decoding dynamic bytes.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        test_value = b"hello world"
        data = eth_abi.abi.encode(["bytes"], [test_value])
        result = adapter.decode(["bytes"], data)
        assert result[0] == test_value

    def test_decode_string(self) -> None:
        """
        Test decoding string.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        test_value = "Hello, Ethereum!"
        data = eth_abi.abi.encode(["string"], [test_value])
        result = adapter.decode(["string"], data)
        assert result[0] == test_value

    def test_decode_dynamic_array(self) -> None:
        """
        Test decoding dynamic array.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        test_value = [1, 2, 3, 4, 5]
        data = eth_abi.abi.encode(["uint256[]"], [test_value])
        result = adapter.decode(["uint256[]"], data)
        assert list(result[0]) == test_value

    def test_decode_fixed_array(self) -> None:
        """
        Test decoding fixed-size array.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        test_value = [10, 20, 30]
        data = eth_abi.abi.encode(["uint256[3]"], [test_value])
        result = adapter.decode(["uint256[3]"], data)
        assert list(result[0]) == test_value

    def test_decode_address_array(self) -> None:
        """
        Test decoding address array.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        addr1 = "0xd3cda913deb6f67967b99d67acdfa1712c293601"
        addr2 = "0x66f9664f97f2b50f62d13ea064982f936de76657"
        data = eth_abi.abi.encode(["address[]"], [[addr1, addr2]])
        result = adapter.decode(["address[]"], data)
        # Should be checksummed addresses
        assert "0xd3CdA913deB6f67967B99D67aCDFa1712C293601" in result[0]


class TestHexBytesSupport:
    """
    Tests for HexBytes handling.
    """

    def test_decode_with_hexbytes_rust_backend(self) -> None:
        """
        Test that HexBytes can be passed to decode with Rust backend.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        raw_data = eth_abi.abi.encode(["uint256"], [42])
        hexbytes_data = HexBytes(raw_data)

        # Should work with HexBytes directly
        result = adapter.decode(["uint256"], hexbytes_data)
        assert result == (42,)

    def test_decode_with_hexbytes_eth_abi_backend(self) -> None:
        """
        Test that HexBytes works with eth_abi backend.
        """
        adapter = AbiAdapter(backend=AbiBackend.ETH_ABI)
        raw_data = eth_abi.abi.encode(["uint256"], [42])
        hexbytes_data = HexBytes(raw_data)

        # Should work - HexBytes is bytes-compatible
        result = adapter.decode(["uint256"], hexbytes_data)
        assert result == (42,)

    def test_decode_single_with_hexbytes_rust_backend(self) -> None:
        """
        Test that HexBytes can be passed to decode_single with Rust backend.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        raw_data = eth_abi.abi.encode(["address"], ["0xd3cda913deb6f67967b99d67acdfa1712c293601"])
        hexbytes_data = HexBytes(raw_data)

        result = adapter.decode_single("address", hexbytes_data)
        assert result == "0xd3CdA913deB6f67967B99D67aCDFa1712C293601"

    def test_decode_single_with_hexbytes_eth_abi_backend(self) -> None:
        """
        Test that HexBytes works with eth_abi backend.
        """
        adapter = AbiAdapter(backend=AbiBackend.ETH_ABI)
        raw_data = eth_abi.abi.encode(["address"], ["0xd3cda913deb6f67967b99d67acdfa1712c293601"])
        hexbytes_data = HexBytes(raw_data)

        result = adapter.decode_single("address", hexbytes_data)
        assert result.lower() == "0xd3cda913deb6f67967b99d67acdfa1712c293601"

    def test_module_decode_with_hexbytes(self) -> None:
        """
        Test module-level decode with HexBytes.
        """
        raw_data = eth_abi.abi.encode(["uint256", "bool"], [100, True])
        hexbytes_data = HexBytes(raw_data)

        # Rust backend
        result = decode(["uint256", "bool"], hexbytes_data, backend=AbiBackend.RUST)
        assert result == (100, True)

        # eth_abi backend
        result = decode(["uint256", "bool"], hexbytes_data, backend=AbiBackend.ETH_ABI)
        assert result == (100, True)

    def test_module_decode_single_with_hexbytes(self) -> None:
        """
        Test module-level decode_single with HexBytes.
        """
        raw_data = eth_abi.abi.encode(["uint256"], [999])
        hexbytes_data = HexBytes(raw_data)

        # Rust backend
        result = decode_single("uint256", hexbytes_data, backend=AbiBackend.RUST)
        assert result == 999

        # eth_abi backend
        result = decode_single("uint256", hexbytes_data, backend=AbiBackend.ETH_ABI)
        assert result == 999

    def test_hexbytes_and_bytes_produce_same_result(self) -> None:
        """
        Test that HexBytes and plain bytes produce identical results.
        """
        adapter = AbiAdapter(backend=AbiBackend.RUST)
        raw_data = eth_abi.abi.encode(
            ["uint256", "address", "bool", "bytes32"],
            [12345, "0xd3cda913deb6f67967b99d67acdfa1712c293601", True, b"test" + b"\x00" * 28],
        )

        hexbytes_data = HexBytes(raw_data)

        result_bytes = adapter.decode(
            ["uint256", "address", "bool", "bytes32"], raw_data
        )
        result_hexbytes = adapter.decode(
            ["uint256", "address", "bool", "bytes32"], hexbytes_data
        )

        assert result_bytes == result_hexbytes


class TestEnvironmentVariable:
    """
    Tests for DEGENBOT_USE_RUST_ABI_DECODER environment variable.
    """

    def test_env_var_true_uses_rust(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that DEGENBOT_USE_RUST_ABI_DECODER=true uses Rust backend.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "true")
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.RUST

    def test_env_var_false_uses_eth_abi(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that DEGENBOT_USE_RUST_ABI_DECODER=false uses eth_abi backend.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "false")
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.ETH_ABI

    def test_env_var_0_uses_eth_abi(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that DEGENBOT_USE_RUST_ABI_DECODER=0 uses eth_abi backend.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "0")
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.ETH_ABI

    def test_env_var_no_uses_eth_abi(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that DEGENBOT_USE_RUST_ABI_DECODER=no uses eth_abi backend.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "no")
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.ETH_ABI

    def test_env_var_off_uses_eth_abi(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that DEGENBOT_USE_RUST_ABI_DECODER=off uses eth_abi backend.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "off")
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.ETH_ABI

    def test_env_var_1_uses_rust(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that DEGENBOT_USE_RUST_ABI_DECODER=1 uses Rust backend.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "1")
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.RUST

    def test_env_var_yes_uses_rust(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that DEGENBOT_USE_RUST_ABI_DECODER=yes uses Rust backend.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "yes")
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.RUST

    def test_env_var_unset_uses_rust(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that unset DEGENBOT_USE_RUST_ABI_DECODER defaults to Rust backend.
        """
        monkeypatch.delenv("DEGENBOT_USE_RUST_ABI_DECODER", raising=False)
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.RUST

    def test_env_var_case_insensitive(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that environment variable is case-insensitive.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "FALSE")
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.ETH_ABI

        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "True")
        backend = _get_default_backend_from_env()
        assert backend == AbiBackend.RUST

    def test_adapter_uses_env_var_default(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that AbiAdapter uses environment variable as default.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "false")
        adapter = AbiAdapter()  # No explicit backend
        assert adapter.backend == AbiBackend.ETH_ABI

        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "true")
        adapter = AbiAdapter()  # No explicit backend
        assert adapter.backend == AbiBackend.RUST

    def test_adapter_explicit_backend_overrides_env(
        self, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        """
        Test that explicit backend parameter overrides environment variable.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "false")
        adapter = AbiAdapter(backend=AbiBackend.RUST)  # Explicit backend
        assert adapter.backend == AbiBackend.RUST

    def test_decode_uses_env_var_backend(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test that module-level decode respects environment variable.
        """
        data = eth_abi.abi.encode(["uint256"], [42])

        # With Rust backend
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "true")
        result = decode(["uint256"], data)  # backend=None uses env var
        assert result == (42,)

        # With eth_abi backend
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "false")
        result = decode(["uint256"], data)  # backend=None uses env var
        assert result == (42,)

    def test_get_default_backend_function(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """
        Test the get_default_backend function.
        """
        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "false")
        assert get_default_backend() == AbiBackend.ETH_ABI

        monkeypatch.setenv("DEGENBOT_USE_RUST_ABI_DECODER", "true")
        assert get_default_backend() == AbiBackend.RUST
