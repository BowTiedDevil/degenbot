"""Tests for the AlloyProvider interface."""

from collections.abc import Iterator

import eth_abi
import pytest
import web3
from eth_utils import keccak
from hexbytes import HexBytes

from degenbot.fork import AnvilFork
from degenbot.provider import (
    AlloyProvider,
    LogFilter,
)
from tests.standalone_anvil import seed as seed_catalog

WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


@pytest.fixture
def alloy_provider(fork_mainnet_full: AnvilFork) -> Iterator[AlloyProvider]:
    """Create an AlloyProvider from the mainnet fork."""
    provider = AlloyProvider(fork_mainnet_full.http_url)
    try:
        yield provider
    finally:
        provider.close()


def _ping_calldata() -> bytes:
    selector = keccak(text="ping(uint256,bytes32)")[:4]
    return selector + eth_abi.encode(["uint256", "bytes32"], [42, b"\x00" * 32])


@pytest.fixture
def standalone_provider(standalone_anvil: AnvilFork) -> Iterator[AlloyProvider]:
    """An AlloyProvider over the seeded standalone anvil (no upstream RPC)."""
    provider = AlloyProvider(standalone_anvil.http_url)
    try:
        yield provider
    finally:
        provider.close()


@pytest.fixture
def emitted_block(standalone_anvil: AnvilFork) -> int:
    """Emit a real ``Ping`` log and return its block number (for get_logs)."""
    w3 = web3.Web3(web3.HTTPProvider(standalone_anvil.http_url))
    sender = w3.eth.accounts[0]
    tx = w3.eth.send_transaction({
        "from": sender,
        "to": seed_catalog.EVENT_EMITTER,
        "data": _ping_calldata(),
        "chainId": seed_catalog.CHAIN_ID,
    })
    return w3.eth.wait_for_transaction_receipt(tx, timeout=10)["blockNumber"]


class TestAlloyProviderAdapter:
    """Test AlloyProvider direct interface."""

    @pytest.mark.online_rpc
    def test_adapter_properties(self, alloy_provider: AlloyProvider):
        """Test that the provider exposes the expected interface."""
        adapter = alloy_provider

        assert adapter.provider_type == "alloy"
        assert adapter.is_connected() is True
        assert "AlloyProvider" in repr(adapter)

    @pytest.mark.online_rpc
    def test_fork_provider_is_alloy(self, fork_mainnet_full: AnvilFork):
        """Test that the fork provider is an AlloyProvider."""
        adapter = fork_mainnet_full.provider

        assert adapter.provider_type == "alloy"
        assert adapter.is_connected() is True

    @pytest.mark.online_rpc
    def test_adapter_has_required_interface(self, alloy_provider: AlloyProvider):
        """Test that adapter satisfies the provider interface."""
        adapter = alloy_provider

        # Should have all required properties and methods
        assert hasattr(adapter, "chain_id")
        assert hasattr(adapter, "block_number")
        assert hasattr(adapter, "get_block_number")
        assert hasattr(adapter, "get_block")
        assert hasattr(adapter, "get_logs")
        assert hasattr(adapter, "call")
        assert hasattr(adapter, "get_code")
        assert hasattr(adapter, "is_connected")


class TestAlloyProviderWithLiveConnection:
    """Test AlloyProvider against the seeded standalone anvil (no upstream RPC)."""

    def test_get_chain_id(self, standalone_provider: AlloyProvider):
        """Test getting chain ID."""
        assert standalone_provider.chain_id == seed_catalog.CHAIN_ID

    def test_get_block_number(self, standalone_provider: AlloyProvider):
        """Test getting block number."""
        block_number = standalone_provider.get_block_number()
        assert isinstance(block_number, int)
        assert block_number > 0

    def test_get_block(self, standalone_provider: AlloyProvider):
        """Test getting block."""
        block = standalone_provider.get_block(1)
        assert block is not None
        assert block.get("number") == 1

    def test_get_block_with_string_identifier(self, standalone_provider: AlloyProvider):
        """Test getting block with string identifier."""
        block = standalone_provider.get_block("latest")
        assert block is not None
        assert block.get("number") is not None

        block = standalone_provider.get_block("earliest")
        assert block is not None
        assert block.get("number") == 0

    def test_get_code(self, standalone_provider: AlloyProvider):
        """Test getting contract code."""
        code = standalone_provider.get_code(seed_catalog.TOKEN)
        assert isinstance(code, (bytes, HexBytes))
        assert len(code) > 0

    def test_call(self, standalone_provider: AlloyProvider):
        """Test eth_call."""
        # SimpleToken.totalSupply() (ERC20 totalSupply selector 0x18160ddd).
        result = standalone_provider.call(
            to=seed_catalog.TOKEN,
            data=HexBytes("0x18160ddd"),
        )
        assert isinstance(result, (bytes, HexBytes))
        assert len(result) == 32  # uint256 return

    def test_get_logs(self, standalone_provider: AlloyProvider, emitted_block: int):
        """Test getting logs."""
        logs = standalone_provider.get_logs(
            from_block=0,
            to_block=emitted_block,
            addresses=[seed_catalog.EVENT_EMITTER],
        )
        assert isinstance(logs, list)
        assert len(logs) > 0

    def test_get_storage_at(self, standalone_provider: AlloyProvider):
        """Test getting storage."""
        storage = standalone_provider.get_storage_at(seed_catalog.TOKEN, 0)
        assert isinstance(storage, (bytes, HexBytes))
        assert len(storage) == 32

    def test_get_storage_at_large_position(self, standalone_provider: AlloyProvider):
        """Test getting storage with large position."""
        large_position = 0x6C34D219A4B1E5E2F2E3D4C5B6A7F8E9D0C1B2A3F4E5D6C7B8A9F0E1D2C3B4A5
        storage = standalone_provider.get_storage_at(seed_catalog.TOKEN, large_position)
        assert isinstance(storage, (bytes, HexBytes))
        assert len(storage) == 32

    def test_properties(self, standalone_provider: AlloyProvider):
        """Test adapter properties."""
        assert standalone_provider.chain_id == seed_catalog.CHAIN_ID
        assert standalone_provider.block_number > 0
        assert standalone_provider.provider_type == "alloy"


class TestAlloyProviderDirect:
    """Test AlloyProvider direct interface (no nested eth namespace)."""

    def test_provider_has_direct_interface(self):
        """Test that AlloyProvider exposes methods directly."""
        assert hasattr(AlloyProvider, "chain_id")
        assert hasattr(AlloyProvider, "block_number")
        assert hasattr(AlloyProvider, "get_block_number")
        assert hasattr(AlloyProvider, "get_block")
        assert hasattr(AlloyProvider, "get_logs")
        assert hasattr(AlloyProvider, "call")
        assert hasattr(AlloyProvider, "get_code")
        assert hasattr(AlloyProvider, "is_connected")

    def test_provider_direct_access(self, standalone_provider: AlloyProvider):
        """Test accessing methods directly on AlloyProvider."""
        assert standalone_provider.chain_id == seed_catalog.CHAIN_ID
        assert standalone_provider.block_number > 0

        block = standalone_provider.get_block(1)
        assert block is not None
        assert block.get("number") == 1


class TestForkProvider:
    """Test AlloyProvider from AnvilFork."""

    @pytest.mark.online_rpc
    def test_fork_provider_delegates_to_eth_namespace(self, fork_mainnet_full: AnvilFork):
        """Test that the fork provider delegates to the RPC endpoint."""
        adapter = fork_mainnet_full.provider

        assert adapter.chain_id == 1
        assert adapter.block_number > 0

        block_number = adapter.get_block_number()
        assert isinstance(block_number, int)
        assert block_number > 0

    @pytest.mark.online_rpc
    def test_fork_provider_get_block(self, fork_mainnet_full: AnvilFork):
        """Test get_block through the fork provider."""
        adapter = fork_mainnet_full.provider
        block = adapter.get_block(18_000_000)

        assert block is not None
        assert block.get("number") == 18_000_000

    @pytest.mark.online_rpc
    def test_fork_provider_get_block_string_identifier(self, fork_mainnet_full: AnvilFork):
        """Test get_block with string identifier."""
        adapter = fork_mainnet_full.provider

        block_latest = adapter.get_block("latest")
        assert block_latest is not None
        assert block_latest.get("number") is not None

        block_earliest = adapter.get_block("earliest")
        assert block_earliest is not None
        assert block_earliest.get("number") == 0

    @pytest.mark.online_rpc
    def test_fork_provider_call(self, fork_mainnet_full: AnvilFork):
        """Test eth_call through the fork provider."""
        adapter = fork_mainnet_full.provider

        calldata = HexBytes("0x18160ddd")
        result = adapter.call(
            to=WETH_ADDRESS,
            data=calldata,
            block=18_000_000,
        )

        assert isinstance(result, (bytes, HexBytes))
        assert len(result) == 32

    @pytest.mark.online_rpc
    def test_fork_provider_get_code(self, fork_mainnet_full: AnvilFork):
        """Test get_code through the fork provider."""
        adapter = fork_mainnet_full.provider

        code = adapter.get_code(WETH_ADDRESS, 18_000_000)
        assert isinstance(code, (bytes, HexBytes))
        assert len(code) > 0

    @pytest.mark.online_rpc
    def test_fork_provider_get_logs(self, fork_mainnet_full: AnvilFork):
        """Test get_logs through the fork provider."""
        adapter = fork_mainnet_full.provider

        logs = adapter.get_logs(
            from_block=18_000_000,
            to_block=18_000_010,
        )
        assert isinstance(logs, list)

    @pytest.mark.online_rpc
    def test_fork_provider_get_balance(self, fork_mainnet_full: AnvilFork):
        """Test get_balance through the fork provider."""
        adapter = fork_mainnet_full.provider

        balance = adapter.get_balance(WETH_ADDRESS, 18_000_000)
        assert isinstance(balance, int)
        assert balance >= 0

    @pytest.mark.online_rpc
    def test_fork_provider_get_storage_at(self, fork_mainnet_full: AnvilFork):
        """Test get_storage_at through the fork provider."""
        adapter = fork_mainnet_full.provider

        storage = adapter.get_storage_at(WETH_ADDRESS, 0, 18_000_000)
        assert isinstance(storage, (bytes, HexBytes))
        assert len(storage) == 32

    @pytest.mark.online_rpc
    def test_fork_provider_get_transaction_count(self, fork_mainnet_full: AnvilFork):
        """Test get_transaction_count through the fork provider."""
        adapter = fork_mainnet_full.provider

        count = adapter.get_transaction_count(WETH_ADDRESS, 18_000_000)
        assert isinstance(count, int)
        assert count >= 0

    @pytest.mark.online_rpc
    def test_fork_provider_is_connected(self, fork_mainnet_full: AnvilFork):
        """Test is_connected through the fork provider."""
        adapter = fork_mainnet_full.provider

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
