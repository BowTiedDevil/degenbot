"""Tests for the provider interface and adapter."""

import pytest
from hexbytes import HexBytes

from degenbot.anvil_fork import AnvilFork
from degenbot.provider import AlloyProvider, EthereumProvider, LogFilter, ProviderAdapter
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI

WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


@pytest.fixture
def alloy_provider(fork_mainnet_full: AnvilFork) -> AlloyProvider:
    """Create an AlloyProvider from the mainnet fork."""
    return AlloyProvider(fork_mainnet_full.http_url)


class TestProviderAdapter:
    """Test ProviderAdapter with both Web3 and AlloyProvider."""

    def test_from_alloy_creates_adapter(self, alloy_provider: AlloyProvider):
        """Test creating adapter from AlloyProvider."""
        adapter = ProviderAdapter.from_alloy(alloy_provider)

        assert adapter.provider_type == "alloy"
        assert adapter.underlying is alloy_provider
        assert adapter.is_connected() is True
        assert "ProviderAdapter" in repr(adapter)

    def test_from_web3_creates_adapter(self, fork_mainnet_full: AnvilFork):
        """Test creating adapter from Web3."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        assert adapter.provider_type == "web3"
        assert adapter.underlying is fork_mainnet_full.w3
        assert adapter.is_connected() is True

    def test_adapter_has_required_interface(self, alloy_provider: AlloyProvider):
        """Test that adapter satisfies EthereumProvider protocol."""
        adapter = ProviderAdapter.from_alloy(alloy_provider)

        # Should have all required properties and methods (check class, not instance)
        assert hasattr(type(adapter), "chain_id") or "chain_id" in dir(adapter)
        assert hasattr(type(adapter), "block_number") or "block_number" in dir(adapter)
        # Methods don't trigger RPC calls
        assert hasattr(adapter, "get_block_number")
        assert hasattr(adapter, "get_block")
        assert hasattr(adapter, "get_logs")
        assert hasattr(adapter, "call")
        assert hasattr(adapter, "get_code")
        assert hasattr(adapter, "is_connected")


class TestProviderAdapterWithLiveConnection:
    """Test ProviderAdapter with live RPC connections."""

    def test_alloy_adapter_get_chain_id(self):
        """Test getting chain ID through adapter."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)
            chain_id = adapter.chain_id
            assert chain_id == 1

    def test_alloy_adapter_get_block_number(self):
        """Test getting block number through adapter."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)
            block_number = adapter.get_block_number()
            assert isinstance(block_number, int)
            assert block_number > 0

    def test_alloy_adapter_get_block(self):
        """Test getting block through adapter."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)
            block = adapter.get_block(18_000_000)
            assert block is not None
            assert block.get("number") == 18_000_000

    def test_alloy_adapter_get_block_with_string_identifier(self):
        """Test getting block with string identifier through adapter."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)
            # Test "latest" string identifier
            block = adapter.get_block("latest")
            assert block is not None
            assert block.get("number") is not None

    def test_alloy_adapter_get_code(self):
        """Test getting contract code through adapter."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)
            code = adapter.get_code(WETH_ADDRESS, 18_000_000)
            assert isinstance(code, (bytes, HexBytes))
            assert len(code) > 0

    def test_alloy_adapter_call(self):
        """Test eth_call through adapter."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)
            # totalSupply() selector
            calldata = HexBytes("0x18160ddd")
            result = adapter.call(
                to=WETH_ADDRESS,
                data=calldata,
                block=18_000_000,
            )
            assert isinstance(result, (bytes, HexBytes))
            assert len(result) == 32  # uint256 return

    def test_alloy_adapter_get_logs(self):
        """Test getting logs through adapter."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)
            logs = adapter.get_logs(
                from_block=18_000_000,
                to_block=18_000_010,
            )
            assert isinstance(logs, list)

    def test_alloy_adapter_get_storage_at(self):
        """Test getting storage through adapter."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)
            # Slot 0 of WETH contract at a specific block
            storage = adapter.get_storage_at(WETH_ADDRESS, 0, 18_000_000)
            assert isinstance(storage, (bytes, HexBytes))
            assert len(storage) == 32  # Always 32 bytes

    def test_alloy_adapter_get_storage_at_large_position(self):
        """Test getting storage with large position (like mapping slots)."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)
            # Large position simulating a mapping slot
            large_position = 0x6C34D219A4B1E5E2F2E3D4C5B6A7F8E9D0C1B2A3F4E5D6C7B8A9F0E1D2C3B4A5
            storage = adapter.get_storage_at(WETH_ADDRESS, large_position, 18_000_000)
            assert isinstance(storage, (bytes, HexBytes))
            assert len(storage) == 32  # Always 32 bytes

    def test_alloy_adapter_properties(self):
        """Test adapter properties with live connection."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as alloy:
            adapter = ProviderAdapter.from_alloy(alloy)

            # Properties should work without calling methods
            assert adapter.chain_id == 1
            assert adapter.block_number > 0
            assert adapter.provider_type == "alloy"


class TestAlloyProvider:
    """Test AlloyProvider direct interface (no nested eth namespace)."""

    def test_provider_has_direct_interface(self):
        """Test that AlloyProvider exposes methods directly."""
        # Check the class has the required interface, no need for a live connection
        assert hasattr(AlloyProvider, "chain_id")
        assert hasattr(AlloyProvider, "block_number")
        assert hasattr(AlloyProvider, "get_block_number")
        assert hasattr(AlloyProvider, "get_block")
        assert hasattr(AlloyProvider, "get_logs")
        assert hasattr(AlloyProvider, "call")
        assert hasattr(AlloyProvider, "get_code")
        assert hasattr(AlloyProvider, "is_connected")

    def test_provider_satisfies_protocol(self):
        """Test that AlloyProvider satisfies EthereumProvider protocol."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as provider:
            # Should be recognized as implementing the protocol
            assert isinstance(provider, EthereumProvider)

    def test_provider_direct_access(self):
        """Test accessing methods directly on AlloyProvider."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as provider:
            # Direct property access
            assert provider.chain_id == 1
            assert provider.block_number > 0

            # Direct method access
            block = provider.get_block(18_000_000)
            assert block is not None
            assert block.get("number") == 18_000_000


class TestWeb3Adapter:
    """Test ProviderAdapter with Web3 from AnvilFork."""

    def test_web3_adapter_delegates_to_eth_namespace(self, fork_mainnet_full: AnvilFork):
        """Test that Web3 adapter properly delegates to eth namespace."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        # Properties should delegate to eth namespace
        assert adapter.chain_id == 1
        assert adapter.block_number > 0

        # Methods should delegate to eth namespace
        block_number = adapter.get_block_number()
        assert isinstance(block_number, int)
        assert block_number > 0

    def test_web3_adapter_get_block(self, fork_mainnet_full: AnvilFork):
        """Test get_block through Web3 adapter."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)
        block = adapter.get_block(18_000_000)

        assert block is not None
        assert block.get("number") == 18_000_000

    def test_web3_adapter_get_block_string_identifier(self, fork_mainnet_full: AnvilFork):
        """Test get_block with string identifier through Web3 adapter."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        # Test various block identifiers
        block_latest = adapter.get_block("latest")
        assert block_latest is not None
        assert block_latest.get("number") is not None

        block_earliest = adapter.get_block("earliest")
        assert block_earliest is not None
        assert block_earliest.get("number") == 0

    def test_web3_adapter_call(self, fork_mainnet_full: AnvilFork):
        """Test eth_call through Web3 adapter."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        # totalSupply() selector
        calldata = HexBytes("0x18160ddd")
        result = adapter.call(
            to=WETH_ADDRESS,
            data=calldata,
            block=18_000_000,
        )

        assert isinstance(result, (bytes, HexBytes))
        assert len(result) == 32  # uint256 return

    def test_web3_adapter_get_code(self, fork_mainnet_full: AnvilFork):
        """Test get_code through Web3 adapter."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        code = adapter.get_code(WETH_ADDRESS, 18_000_000)
        assert isinstance(code, (bytes, HexBytes))
        assert len(code) > 0

    def test_web3_adapter_get_logs(self, fork_mainnet_full: AnvilFork):
        """Test get_logs through Web3 adapter."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        logs = adapter.get_logs(
            from_block=18_000_000,
            to_block=18_000_010,
        )
        assert isinstance(logs, list)

    def test_web3_adapter_get_balance(self, fork_mainnet_full: AnvilFork):
        """Test get_balance through Web3 adapter."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        balance = adapter.get_balance(WETH_ADDRESS, 18_000_000)
        assert isinstance(balance, int)
        assert balance >= 0

    def test_web3_adapter_get_storage_at(self, fork_mainnet_full: AnvilFork):
        """Test get_storage_at through Web3 adapter."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        storage = adapter.get_storage_at(WETH_ADDRESS, 0, 18_000_000)
        assert isinstance(storage, (bytes, HexBytes))
        assert len(storage) == 32  # Always 32 bytes

    def test_web3_adapter_get_transaction_count(self, fork_mainnet_full: AnvilFork):
        """Test get_transaction_count through Web3 adapter."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        count = adapter.get_transaction_count(WETH_ADDRESS, 18_000_000)
        assert isinstance(count, int)
        assert count >= 0

    def test_web3_adapter_is_connected(self, fork_mainnet_full: AnvilFork):
        """Test is_connected through Web3 adapter."""
        adapter = ProviderAdapter.from_web3(fork_mainnet_full.w3)

        assert adapter.is_connected() is True


class TestLogFilter:
    """Test LogFilter dataclass."""

    def test_log_filter_creation(self):
        """Test LogFilter can be created with valid block range."""
        log_filter = LogFilter(from_block=1000, to_block=2000)
        assert log_filter.from_block == 1000
        assert log_filter.to_block == 2000
        assert log_filter.addresses == []
        assert log_filter.topics == []

    def test_log_filter_with_addresses(self):
        """Test LogFilter with contract addresses."""
        log_filter = LogFilter(from_block=1000, to_block=2000, addresses=[WETH_ADDRESS])
        assert len(log_filter.addresses) == 1

    def test_log_filter_with_topics(self):
        """Test LogFilter with topic filters."""
        log_filter = LogFilter(
            from_block=1000,
            to_block=2000,
            topics=[["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"]],
        )
        assert len(log_filter.topics) == 1

    def test_log_filter_invalid_range(self):
        """Test LogFilter raises error for invalid block range."""
        with pytest.raises(ValueError, match="to_block must be >= from_block"):
            LogFilter(from_block=2000, to_block=1000)
