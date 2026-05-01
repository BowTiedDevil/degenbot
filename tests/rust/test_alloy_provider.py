"""
Tests for AlloyProvider's direct interface.

These tests verify that AlloyProvider exposes the EthereumProvider
interface with correct method signatures and default values.
"""

import inspect

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.provider import AlloyProvider


@pytest.fixture
def alloy_provider(fork_mainnet_full: AnvilFork) -> AlloyProvider:
    """Create an AlloyProvider from the mainnet fork."""
    return AlloyProvider(fork_mainnet_full.http_url)


class TestAlloyProviderInterface:
    """Test AlloyProvider's direct interface."""

    def test_provider_has_required_properties(self, alloy_provider: AlloyProvider):
        """Test that AlloyProvider has required properties."""
        assert hasattr(type(alloy_provider), "chain_id")
        assert hasattr(type(alloy_provider), "block_number")
        assert isinstance(type(alloy_provider).__dict__["chain_id"], property)
        assert isinstance(type(alloy_provider).__dict__["block_number"], property)

    def test_provider_has_required_methods(self, alloy_provider: AlloyProvider):
        """Test that AlloyProvider has required methods."""
        assert callable(alloy_provider.get_block_number)
        assert callable(alloy_provider.get_block)
        assert callable(alloy_provider.get_logs)
        assert callable(alloy_provider.call)
        assert callable(alloy_provider.get_code)
        assert callable(alloy_provider.is_connected)

    def test_provider_has_all_rust_methods(self, alloy_provider: AlloyProvider):
        """Test that all Rust-exposed methods are callable from Python."""
        # Methods with full Rust implementations
        assert callable(alloy_provider.get_gas_price)
        assert callable(alloy_provider.get_chain_id)
        assert callable(alloy_provider.get_transaction)
        assert callable(alloy_provider.get_transaction_receipt)
        assert callable(alloy_provider.get_storage_at)
        assert callable(alloy_provider.estimate_gas)
        assert callable(alloy_provider.close)
        # Stub methods (raise NotImplementedError)
        assert callable(alloy_provider.get_balance)
        assert callable(alloy_provider.get_transaction_count)

    def test_provider_has_rpc_url_property(self, alloy_provider: AlloyProvider):
        """Test that rpc_url is exposed as a property."""
        assert hasattr(type(alloy_provider), "rpc_url")
        assert isinstance(type(alloy_provider).__dict__["rpc_url"], property)


class TestAlloyProviderMethodSignatures:
    """Test method signatures match the expected interface."""

    def test_get_code_signature(self, alloy_provider: AlloyProvider):
        """Test get_code accepts address and block_number parameters."""
        sig = inspect.signature(alloy_provider.get_code)
        params = list(sig.parameters.keys())
        assert "address" in params
        assert "block_number" in params

    def test_call_signature(self, alloy_provider: AlloyProvider):
        """Test call accepts to, data, and block_number parameters."""
        sig = inspect.signature(alloy_provider.call)
        params = list(sig.parameters.keys())
        assert "to" in params
        assert "data" in params
        assert "block_number" in params

    def test_get_block_signature(self, alloy_provider: AlloyProvider):
        """Test get_block accepts block_number parameter."""
        sig = inspect.signature(alloy_provider.get_block)
        params = list(sig.parameters.keys())
        assert "block_number" in params

    def test_get_logs_signature(self, alloy_provider: AlloyProvider):
        """Test get_logs accepts LogFilter or keyword arguments."""
        sig = inspect.signature(alloy_provider.get_logs)
        params = sig.parameters
        assert "filter_param" in params
        # from_block and to_block should be keyword-only
        assert "from_block" in params
        assert params["from_block"].kind == inspect.Parameter.KEYWORD_ONLY
        assert "to_block" in params
        assert params["to_block"].kind == inspect.Parameter.KEYWORD_ONLY

    def test_get_storage_at_signature(self, alloy_provider: AlloyProvider):
        """Test get_storage_at accepts address, position, block_number."""
        sig = inspect.signature(alloy_provider.get_storage_at)
        params = list(sig.parameters.keys())
        assert "address" in params
        assert "position" in params
        assert "block_number" in params

    def test_get_transaction_signature(self, alloy_provider: AlloyProvider):
        """Test get_transaction accepts tx_hash parameter."""
        sig = inspect.signature(alloy_provider.get_transaction)
        params = list(sig.parameters.keys())
        assert "tx_hash" in params

    def test_get_transaction_receipt_signature(self, alloy_provider: AlloyProvider):
        """Test get_transaction_receipt accepts tx_hash parameter."""
        sig = inspect.signature(alloy_provider.get_transaction_receipt)
        params = list(sig.parameters.keys())
        assert "tx_hash" in params


class TestAlloyProviderReturnTypes:
    """Test that Rust return types map correctly to Python types."""

    def test_get_gas_price_returns_int(self, alloy_provider: AlloyProvider):
        """get_gas_price should return int (not str)."""
        result = alloy_provider.get_gas_price()
        assert isinstance(result, int), f"Expected int, got {type(result)}"
        assert result >= 0

    def test_get_block_number_returns_int(self, alloy_provider: AlloyProvider):
        """get_block_number should return int."""
        result = alloy_provider.get_block_number()
        assert isinstance(result, int)
        assert result > 0

    def test_get_chain_id_returns_int(self, alloy_provider: AlloyProvider):
        """get_chain_id should return int."""
        result = alloy_provider.get_chain_id()
        assert isinstance(result, int)

    def test_get_storage_at_returns_hexbytes(self, alloy_provider: AlloyProvider):
        """get_storage_at should return HexBytes (functional, not stub)."""
        from hexbytes import HexBytes
        result = alloy_provider.get_storage_at(
            "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", 0
        )
        assert isinstance(result, HexBytes)
        assert len(result) == 32

    def test_get_transaction_returns_dict_or_none(self, alloy_provider: AlloyProvider):
        """get_transaction should return dict or None for missing tx."""
        result = alloy_provider.get_transaction(
            "0x" + "00" * 32
        )
        assert result is None or isinstance(result, dict)

    def test_estimate_gas_returns_int(self, alloy_provider: AlloyProvider):
        """estimate_gas should return int."""
        from hexbytes import HexBytes
        result = alloy_provider.estimate_gas(
            to="0x742d35Cc6634C0532925a3b8D4C9db96590d6B75",
            data=HexBytes(b""),
        )
        assert isinstance(result, int)
        assert result >= 0


class TestAlloyProviderStubMethods:
    """Test stub methods that raise NotImplementedError."""

    def test_get_balance_raises_not_implemented(self, alloy_provider: AlloyProvider):
        """Test get_balance raises NotImplementedError."""
        with pytest.raises(NotImplementedError, match="get_balance not implemented"):
            alloy_provider.get_balance("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75")

    def test_get_balance_with_block_raises_not_implemented(self, alloy_provider: AlloyProvider):
        """Test get_balance with block raises NotImplementedError."""
        with pytest.raises(NotImplementedError, match="get_balance not implemented"):
            alloy_provider.get_balance("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", 18000000)

    def test_get_transaction_count_raises_not_implemented(self, alloy_provider: AlloyProvider):
        """Test get_transaction_count raises NotImplementedError."""
        with pytest.raises(NotImplementedError, match="get_transaction_count not implemented"):
            alloy_provider.get_transaction_count("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75")


class TestAlloyProviderConnection:
    """Test connection-related methods."""

    def test_is_connected_returns_true(self, alloy_provider: AlloyProvider):
        """Test is_connected returns True for AlloyProvider."""
        assert alloy_provider.is_connected() is True

    def test_close_method_exists(self, alloy_provider: AlloyProvider):
        """Test close method exists."""
        assert callable(alloy_provider.close)


class TestAlloyProviderContextManager:
    """Test context manager functionality."""

    def test_context_manager_enter_exit(self, fork_mainnet_full: AnvilFork):
        """Test AlloyProvider works as context manager."""
        with AlloyProvider(fork_mainnet_full.http_url) as provider:
            assert provider is not None
            assert isinstance(provider, AlloyProvider)


class TestProviderDefaults:
    """Test default parameter values."""

    def test_get_code_default_block_number(self, alloy_provider: AlloyProvider):
        """Test get_code has None default for block_number (latest)."""
        sig = inspect.signature(alloy_provider.get_code)
        default = sig.parameters["block_number"].default
        assert default is None

    def test_call_default_block_number(self, alloy_provider: AlloyProvider):
        """Test call has None default for block_number (latest)."""
        sig = inspect.signature(alloy_provider.call)
        default = sig.parameters["block_number"].default
        assert default is None

    def test_get_block_default_block_number(self, alloy_provider: AlloyProvider):
        """Test get_block has no default - requires block number."""
        sig = inspect.signature(alloy_provider.get_block)
        # block_number is required, no default
        assert sig.parameters["block_number"].default is inspect.Parameter.empty

    def test_get_storage_at_default_block_number(self, alloy_provider: AlloyProvider):
        """Test get_storage_at has None default for block_number (latest)."""
        sig = inspect.signature(alloy_provider.get_storage_at)
        default = sig.parameters["block_number"].default
        assert default is None
