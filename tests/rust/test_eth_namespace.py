"""
Tests for the EthNamespace Web3-compatible adapter in AlloyProvider.

These tests verify that the eth namespace provides web3.py-compatible methods
including both working implementations and stub methods that raise NotImplementedError.
"""

import inspect

import pytest

from degenbot.provider import AlloyProvider


class TestEthNamespaceProperties:
    """Test eth namespace properties and basic structure."""

    def test_eth_namespace_exists(self):
        """Test that AlloyProvider has an eth namespace attribute."""
        provider = AlloyProvider("https://example.com/rpc")
        assert hasattr(provider, "eth")
        assert provider.eth is not None

    def test_eth_namespace_type(self):
        """Test that eth namespace has the correct type."""
        provider = AlloyProvider("https://example.com/rpc")
        assert type(provider.eth).__name__ == "EthNamespace"


class TestEthNamespaceStubs:
    """Test eth namespace stub methods raise NotImplementedError."""

    @pytest.fixture
    def provider(self):
        """Create a provider for testing stubs."""
        return AlloyProvider("https://example.com/rpc")

    def test_get_balance_raises_not_implemented(self, provider):
        """Test get_balance raises NotImplementedError."""
        with pytest.raises(NotImplementedError, match="get_balance not yet implemented"):
            provider.eth.get_balance("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75")

    def test_get_balance_with_block_identifier(self, provider):
        """Test get_balance accepts block_identifier parameter."""
        with pytest.raises(NotImplementedError, match="get_balance not yet implemented"):
            provider.eth.get_balance(
                "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", block_identifier=18000000
            )

    def test_get_code_raises_not_implemented(self, provider):
        """Test get_code raises NotImplementedError."""
        with pytest.raises(NotImplementedError, match="get_code not yet implemented"):
            provider.eth.get_code("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")

    def test_get_code_with_block_identifier(self, provider):
        """Test get_code accepts block_identifier parameter."""
        with pytest.raises(NotImplementedError, match="get_code not yet implemented"):
            provider.eth.get_code(
                "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2", block_identifier="latest"
            )

    def test_get_transaction_count_raises_not_implemented(self, provider):
        """Test get_transaction_count raises NotImplementedError."""
        with pytest.raises(NotImplementedError, match="get_transaction_count not yet implemented"):
            provider.eth.get_transaction_count("0x742d35Cc6634C0532925a3b8D4C9db96590d6B75")

    def test_get_transaction_count_with_block_identifier(self, provider):
        """Test get_transaction_count accepts block_identifier parameter."""
        with pytest.raises(NotImplementedError, match="get_transaction_count not yet implemented"):
            provider.eth.get_transaction_count(
                "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", block_identifier="pending"
            )

    def test_get_block_method_exists(self, provider):
        """Test get_block method exists and is callable."""
        assert callable(provider.eth.get_block)

    def test_get_block_accepts_number(self, provider):
        """Test get_block accepts block number."""
        # Just check the signature is valid, don't call it without connection

        sig = inspect.signature(provider.eth.get_block)
        params = list(sig.parameters.keys())
        assert "block_identifier" in params

    def test_get_block_accepts_full_transactions(self, provider):
        """Test get_block accepts full_transactions parameter."""

        sig = inspect.signature(provider.eth.get_block)
        assert "full_transactions" in sig.parameters


class TestWeb3Compatibility:
    """Test Web3.py compatibility patterns."""

    def test_eth_namespace_access_pattern(self):
        """Test that eth namespace follows web3.py pattern."""
        provider = AlloyProvider("https://example.com/rpc")

        # These should all be accessible without errors (check on class, not instance)
        eth_class = type(provider.eth)
        assert hasattr(eth_class, "chain_id")
        assert hasattr(eth_class, "block_number")
        assert hasattr(provider.eth, "get_balance")
        assert hasattr(provider.eth, "get_code")
        assert hasattr(provider.eth, "get_transaction_count")
        assert hasattr(provider.eth, "get_block")

    def test_chain_id_is_property(self):
        """Test chain_id is a property, not a method."""
        provider = AlloyProvider("https://example.com/rpc")
        # Check that chain_id is defined as a property (hasattr without calling)
        assert hasattr(type(provider.eth), "chain_id")
        # Verify it's a property descriptor
        assert isinstance(type(provider.eth).__dict__["chain_id"], property)

    def test_block_number_is_property(self):
        """Test block_number is a property, not a method."""
        provider = AlloyProvider("https://example.com/rpc")
        # Check that block_number is defined as a property (hasattr without calling)
        assert hasattr(type(provider.eth), "block_number")
        # Verify it's a property descriptor
        assert isinstance(type(provider.eth).__dict__["block_number"], property)

    def test_get_balance_is_method(self):
        """Test get_balance is a method."""
        provider = AlloyProvider("https://example.com/rpc")
        assert callable(provider.eth.get_balance)

    def test_get_code_is_method(self):
        """Test get_code is a method."""
        provider = AlloyProvider("https://example.com/rpc")
        assert callable(provider.eth.get_code)

    def test_get_transaction_count_is_method(self):
        """Test get_transaction_count is a method."""
        provider = AlloyProvider("https://example.com/rpc")
        assert callable(provider.eth.get_transaction_count)

    def test_get_block_is_method(self):
        """Test get_block is a method."""
        provider = AlloyProvider("https://example.com/rpc")
        assert callable(provider.eth.get_block)

    def test_method_signatures_match_web3(self):
        """Test that method signatures are compatible with web3.py."""
        provider = AlloyProvider("https://example.com/rpc")

        # Test get_balance signature
        sig = inspect.signature(provider.eth.get_balance)
        params = list(sig.parameters.keys())
        assert "address" in params
        assert "block_identifier" in params

        # Test get_code signature
        sig = inspect.signature(provider.eth.get_code)
        params = list(sig.parameters.keys())
        assert "address" in params
        assert "block_identifier" in params

        # Test get_transaction_count signature
        sig = inspect.signature(provider.eth.get_transaction_count)
        params = list(sig.parameters.keys())
        assert "address" in params
        assert "block_identifier" in params

        # Test get_block signature
        sig = inspect.signature(provider.eth.get_block)
        params = list(sig.parameters.keys())
        assert "block_identifier" in params
        assert "full_transactions" in params

    def test_get_balance_default_block_identifier(self):
        """Test get_balance has correct default for block_identifier."""
        provider = AlloyProvider("https://example.com/rpc")
        sig = inspect.signature(provider.eth.get_balance)
        default = sig.parameters["block_identifier"].default
        assert default == "latest"

    def test_get_code_default_block_identifier(self):
        """Test get_code has correct default for block_identifier."""
        provider = AlloyProvider("https://example.com/rpc")
        sig = inspect.signature(provider.eth.get_code)
        default = sig.parameters["block_identifier"].default
        assert default == "latest"

    def test_get_transaction_count_default_block_identifier(self):
        """Test get_transaction_count has correct default for block_identifier."""
        provider = AlloyProvider("https://example.com/rpc")
        sig = inspect.signature(provider.eth.get_transaction_count)
        default = sig.parameters["block_identifier"].default
        assert default == "latest"

    def test_get_block_default_parameters(self):
        """Test get_block has correct defaults."""
        provider = AlloyProvider("https://example.com/rpc")
        sig = inspect.signature(provider.eth.get_block)
        block_default = sig.parameters["block_identifier"].default
        full_tx_default = sig.parameters["full_transactions"].default
        assert block_default == "latest"
        assert full_tx_default is False
