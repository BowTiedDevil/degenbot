"""
Integration tests for the Alloy-based Ethereum RPC provider.

These tests demonstrate that the Rust-based Alloy integration is functional,
covering provider operations, contract interactions, and connection management.
"""

import pytest

from degenbot.contract import (
    Contract,
    decode_return_data,
    encode_function_call,
    get_function_selector,
)
from degenbot.provider import AlloyProvider, LogFilter
from tests.conftest import ETHEREUM_ARCHIVE_NODE_HTTP_URI

WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


class TestContractUtilities:
    """Test contract utility functions that don't require RPC connection."""

    def test_get_function_selector_basic(self):
        """Test function selector calculation for common ERC20 functions."""
        assert get_function_selector("transfer(address,uint256)") == "0xa9059cbb"
        assert get_function_selector("balanceOf(address)") == "0x70a08231"
        assert get_function_selector("totalSupply()") == "0x18160ddd"
        assert get_function_selector("decimals()") == "0x313ce567"

    def test_get_function_selector_with_returns(self):
        """Test function selector ignores return type declaration."""
        # Selector should be the same regardless of return type declaration
        selector_without_return = get_function_selector("balanceOf(address)")
        selector_with_return = get_function_selector("balanceOf(address) returns (uint256)")
        assert selector_without_return == selector_with_return == "0x70a08231"

    def test_encode_function_call_simple(self):
        """Test encoding function calls without arguments."""
        calldata = encode_function_call("totalSupply()")
        # Should have 4-byte selector only
        assert len(calldata) == 4
        assert calldata.hex()[:8] == "18160ddd"

    def test_encode_function_call_with_address(self):
        """Test encoding function calls with address argument."""
        calldata = encode_function_call(
            "balanceOf(address)", ["0x742d35Cc6634C0532925a3b8D4C9db96590d6B75"]
        )
        # Should have 4-byte selector + 32-byte padded address
        assert len(calldata) == 36
        assert calldata[:4].hex() == "70a08231"

    def test_encode_function_call_with_uint256(self):
        """Test encoding function calls with uint256 argument."""
        calldata = encode_function_call(
            "transfer(address,uint256)",
            ["0x742d35Cc6634C0532925a3b8D4C9db96590d6B75", "1000000000000000000"],
        )
        # Should have 4-byte selector + 32-byte address + 32-byte uint256
        assert len(calldata) == 68
        assert calldata[:4].hex() == "a9059cbb"

    def test_decode_return_data_uint256(self):
        """Test decoding uint256 return values."""
        # 1000000000000000000 (1 ETH in wei) encoded as uint256
        data = bytes.fromhex("0de0b6b3a7640000".rjust(64, "0"))
        decoded = decode_return_data(data, ["uint256"])
        assert decoded == ["1000000000000000000"]

    def test_decode_return_data_address(self):
        """Test decoding address return values."""
        # Address 0x742d35Cc6634C0532925a3b8D4C9db96590d6B75 padded to 32 bytes
        data = bytes.fromhex("000000000000000000000000742d35Cc6634C0532925a3b8D4C9db96590d6B75")
        decoded = decode_return_data(data, ["address"])
        # Addresses are returned in lowercase, not checksummed
        assert decoded == ["0x742d35cc6634c0532925a3b8d4c9db96590d6b75"]

    def test_decode_return_data_bool(self):
        """Test decoding bool return values."""
        # true encoded as bool
        data_true = bytes.fromhex(
            "0000000000000000000000000000000000000000000000000000000000000001"
        )
        decoded_true = decode_return_data(data_true, ["bool"])
        assert decoded_true == ["true"]

        # false encoded as bool
        data_false = bytes.fromhex("0" * 64)
        decoded_false = decode_return_data(data_false, ["bool"])
        assert decoded_false == ["false"]

    def test_decode_return_data_multiple_values(self):
        """Test decoding multiple return values."""
        # Two uint256 values: 1000 and 2000
        data = bytes.fromhex(
            "00000000000000000000000000000000000000000000000000000000000003e8"  # 1000
            "00000000000000000000000000000000000000000000000000000000000007d0"  # 2000
        )
        decoded = decode_return_data(data, ["uint256", "uint256"])
        assert decoded == ["1000", "2000"]


class TestLogFilter:
    """Test LogFilter dataclass functionality."""

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


class TestContractClass:
    """Test Contract class initialization and properties.

    Note: Contract initialization requires a valid provider connection,
    so initialization tests are skipped when no connection is available.
    """

    def test_contract_initialization(self):
        """Test Contract can be initialized with address."""
        contract = Contract(WETH_ADDRESS)
        assert contract.address == WETH_ADDRESS

    def test_contract_address_property(self):
        """Test contract address property returns correct value."""
        address = "0x742d35Cc6634C0532925a3b8D4C9db96590d6B75"
        contract = Contract(address)
        assert contract.address == address

    def test_contract_static_methods(self):
        """Test Contract static utility methods (these don't require initialization)."""
        # Test get_function_selector (static method)
        selector = Contract.get_function_selector("transfer(address,uint256)")
        assert selector == "0xa9059cbb"

        # Test decode_return_data (static method)
        data = bytes.fromhex("0de0b6b3a7640000".rjust(64, "0"))
        decoded = Contract.decode_return_data(data, output_types=["uint256"])
        assert decoded == ["1000000000000000000"]

    def test_module_level_functions(self):
        """Test module-level utility functions (don't require Contract instance)."""
        # Test get_function_selector
        selector = get_function_selector("transfer(address,uint256)")
        assert selector == "0xa9059cbb"

        # Test encode_function_call
        calldata = encode_function_call(
            "balanceOf(address)", args=["0x742d35Cc6634C0532925a3b8D4C9db96590d6B75"]
        )
        assert len(calldata) == 36

        # Test decode_return_data
        data = bytes.fromhex("0de0b6b3a7640000".rjust(64, "0"))
        decoded = decode_return_data(data, output_types=["uint256"])
        assert decoded == ["1000000000000000000"]


class TestProviderInitialization:
    """Test AlloyProvider initialization and basic properties."""

    def test_provider_initialization(self):
        """Test AlloyProvider can be initialized with default parameters."""
        provider = AlloyProvider("https://example.com/rpc")
        assert provider.rpc_url == "https://example.com/rpc"

    def test_provider_initialization_with_custom_params(self):
        """Test AlloyProvider can be initialized with custom parameters."""
        provider = AlloyProvider(
            rpc_url="https://example.com/rpc",
            max_retries=5,
            max_blocks_per_request=1000,
        )
        assert provider.rpc_url == "https://example.com/rpc"

    def test_provider_context_manager(self):
        """Test AlloyProvider works as context manager."""
        with AlloyProvider("https://example.com/rpc") as provider:
            assert provider.rpc_url == "https://example.com/rpc"
        # Provider should be closed after exiting context


class TestProviderWithLiveConnection:
    """Sync provider tests requiring live RPC.

    Note: The sync AlloyProvider uses tokio::runtime::Handle::try_current() which
    requires an existing tokio runtime. For live RPC tests, use AsyncAlloyProvider
    with @pytest.mark.asyncio instead (see test_alloy_async_integration.py).
    """

    def test_get_block_number(self):
        """Test fetching current block number from live RPC."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as provider:
            block_number = provider.get_block_number()
            assert isinstance(block_number, int)
            assert block_number > 0

    def test_get_chain_id(self):
        """Test fetching chain ID from live RPC."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as provider:
            chain_id = provider.get_chain_id()
            assert chain_id == 1  # Ethereum mainnet

    def test_get_logs(self):
        """Test fetching logs with filter from live RPC."""
        with AlloyProvider(ETHEREUM_ARCHIVE_NODE_HTTP_URI) as provider:
            # WETH contract
            log_filter = LogFilter(
                from_block=18000000,
                to_block=18000010,
                addresses=["0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"],
            )
            logs = provider.get_logs(log_filter)
            assert isinstance(logs, list)


class TestContractWithLiveConnection:
    """Contract tests that require a live RPC connection.

    Note: Contract initialization currently requires provider injection
    which is not yet fully implemented.
    """

    def test_contract_call_balance_of(self):
        """Test calling balanceOf on WETH contract."""
        weth = Contract(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            provider_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        )
        # Call balanceOf for a known address with return type specified
        balance = weth.call(
            "balanceOf(address) returns (uint256)", ["0x742d35Cc6634C0532925a3b8D4C9db96590d6B75"]
        )
        assert len(balance) == 1
        assert int(balance[0]) >= 0

    def test_contract_batch_call(self):
        """Test batch calling multiple functions."""
        weth = Contract(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            provider_url=ETHEREUM_ARCHIVE_NODE_HTTP_URI,
        )
        # Batch call name, symbol, decimals, totalSupply with return types
        results = weth.batch_call([
            ("name() returns (string)", []),
            ("symbol() returns (string)", []),
            ("decimals() returns (uint8)", []),
            ("totalSupply() returns (uint256)", []),
        ])
        assert len(results) == 4
        name, symbol, decimals, total_supply = [r[0] for r in results]
        assert name == "Wrapped Ether"
        assert symbol == "WETH"
        assert int(decimals) == 18
        assert int(total_supply) > 0
