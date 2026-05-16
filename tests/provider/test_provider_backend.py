"""
Tests for ProviderBackend protocol and explicit method delegation.

Validates that:
1. ProviderBackend merges EthereumProvider + _SyncProviderBackend
2. Explicit methods on ProviderAdapter delegate to backend correctly
3. Explicit methods take precedence over any fallback behavior
4. Block guard simplification works for _Web3Adapter
"""

from unittest.mock import MagicMock

import pytest
from hexbytes import HexBytes

from degenbot.provider.interface import ProviderAdapter, ProviderBackend


class FakeWeb3:
    """Fake web3 instance for testing."""

    def __init__(self) -> None:
        self.eth = MagicMock()
        self.eth.chain_id = 1
        self.eth.block_number = 18_000_000
        self.eth.get_block_number.return_value = 18_000_000
        self.eth.get_block.return_value = {"timestamp": 1_700_000_000}
        self.eth.get_code.return_value = HexBytes(b"\x00")
        self.eth.get_balance.return_value = 10**18
        self.eth.get_storage_at.return_value = HexBytes(b"\x00")
        self.eth.get_transaction_count.return_value = 0
        self.eth.call.return_value = HexBytes(b"\x00")

    def is_connected(self) -> bool:
        return True


class TestProviderBackendProtocol:
    """Test that the merged ProviderBackend protocol works."""

    def test_web3_adapter_satisfies_protocol(self) -> None:
        """_Web3Adapter satisfies ProviderBackend protocol."""
        w3 = FakeWeb3()
        adapter = ProviderAdapter.from_web3(w3)
        backend = adapter._backend
        assert isinstance(backend, ProviderBackend)

    def test_protocol_has_call_raw(self) -> None:
        """ProviderBackend includes call_raw (was missing from EthereumProvider)."""
        assert hasattr(ProviderBackend, "call_raw")

    def test_protocol_has_close(self) -> None:
        """ProviderBackend includes close (was missing from EthereumProvider)."""
        assert hasattr(ProviderBackend, "close")


class TestGetattrDelegation:
    """Test that explicit methods delegate to backend correctly."""

    def test_delegated_method_forwards_to_backend(self) -> None:
        """Methods not defined explicitly on ProviderAdapter forward to backend."""
        w3 = FakeWeb3()
        adapter = ProviderAdapter.from_web3(w3)
        # get_code is a pure delegation method (forwarded via __getattr__)
        result = adapter.get_code("0x1234", block=None)
        assert result == HexBytes(b"\x00")

    def test_private_attribute_raises_attribute_error(self) -> None:
        """__getattr__ does not intercept private attributes."""
        w3 = FakeWeb3()
        adapter = ProviderAdapter.from_web3(w3)
        with pytest.raises(AttributeError):
            _ = adapter._nonexistent

    def test_explicit_method_takes_precedence(self) -> None:
        """Methods defined explicitly on ProviderAdapter are not delegated."""
        w3 = FakeWeb3()
        adapter = ProviderAdapter.from_web3(w3)
        # call() is defined explicitly on ProviderAdapter
        # It should still work normally, not go through __getattr__
        result = adapter.call("0x1234", b"\x00")
        assert result == HexBytes(b"\x00")

    def test_getattr_nonexistent_raises(self) -> None:
        """Asking for a truly non-existent method raises AttributeError."""
        w3 = FakeWeb3()
        adapter = ProviderAdapter.from_web3(w3)
        with pytest.raises(AttributeError):
            _ = adapter.nonexistent_method


class TestBlockGuardSimplification:
    """Test that block=None works with simplified _Web3Adapter."""

    def test_call_with_none_block(self) -> None:
        """_Web3Adapter.call() works when block=None."""
        w3 = FakeWeb3()
        adapter = ProviderAdapter.from_web3(w3)
        result = adapter.call("0x1234", b"\x00", block=None)
        assert result == HexBytes(b"\x00")

    def test_call_with_explicit_block(self) -> None:
        """_Web3Adapter.call() works with explicit block number."""
        w3 = FakeWeb3()
        adapter = ProviderAdapter.from_web3(w3)
        result = adapter.call("0x1234", b"\x00", block=18_000_000)
        assert result == HexBytes(b"\x00")

    def test_get_code_with_none_block(self) -> None:
        """_Web3Adapter.get_code() works when block=None."""
        w3 = FakeWeb3()
        adapter = ProviderAdapter.from_web3(w3)
        result = adapter.get_code("0x1234", block=None)
        assert result == HexBytes(b"\x00")

    def test_get_balance_with_none_block(self) -> None:
        """_Web3Adapter.get_balance() works when block=None."""
        w3 = FakeWeb3()
        adapter = ProviderAdapter.from_web3(w3)
        result = adapter.get_balance("0x1234", block=None)
        assert result == 10**18
