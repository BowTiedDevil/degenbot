"""
Tests for FastHexBytes conversion in AlloyProvider.

These tests verify that the provider returns FastHexBytes for hash/address/data fields.
"""

import pytest

from degenbot import FastHexBytes
from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.provider import AlloyProvider

WETH_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")
UNISWAP_V3_FACTORY = get_checksum_address("0x1F98431c8aD98523631AE4a59f267346ea31F984")


@pytest.fixture
def ethereum_mainnet_alloy_provider(fork_mainnet_full: AnvilFork) -> AlloyProvider:
    """
    Create an AlloyProvider from the mainnet fork.
    """

    return AlloyProvider(fork_mainnet_full.http_url)


class TestHexBytesConversion:
    """
    Test that appropriate fields are converted to FastHexBytes.
    """

    def test_get_logs_returns_hexbytes_for_hash_fields(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that get_logs returns FastHexBytes for address, topics, blockHash, and transactionHash.
        """

        # Fetch a known log from the Uniswap V3 factory
        logs = ethereum_mainnet_alloy_provider.get_logs(
            from_block=12_369_621,
            to_block=12_369_621,
            addresses=[UNISWAP_V3_FACTORY],
        )

        assert len(logs) > 0
        log = logs[0]

        assert isinstance(log["address"], FastHexBytes)

        assert isinstance(log["topics"], list)
        for topic in log["topics"]:
            assert isinstance(topic, FastHexBytes)

        assert isinstance(log["data"], FastHexBytes)

        if log.get("blockHash"):
            assert isinstance(log["blockHash"], FastHexBytes)

        if log.get("transactionHash"):
            assert isinstance(log["transactionHash"], FastHexBytes)

    def test_get_logs_returns_int_for_numeric_fields(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that get_logs returns int for numeric fields.
        """

        logs = ethereum_mainnet_alloy_provider.get_logs(
            from_block=12_369_621,
            to_block=12_369_621,
            addresses=[UNISWAP_V3_FACTORY],
        )

        assert len(logs) > 0
        log = logs[0]

        # Verify blockNumber is int
        assert isinstance(log["blockNumber"], int)

        # Verify logIndex is int
        assert isinstance(log["logIndex"], int)

    def test_eth_call_returns_hexbytes(self, ethereum_mainnet_alloy_provider: AlloyProvider):
        """
        Test that eth_call returns FastHexBytes.
        """

        # Call balanceOf for WETH
        # balanceOf selector: 0x70a08231
        result = ethereum_mainnet_alloy_provider.eth.call({
            "to": WETH_ADDRESS,
            "data": "0x70a08231000000000000000000000000C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        })

        assert isinstance(result, FastHexBytes)
        assert len(result) == 32  # uint256 return value

    def test_get_block_returns_hexbytes_for_hash_fields(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that get_block returns FastHexBytes for hash fields.
        """

        block = ethereum_mainnet_alloy_provider.eth.get_block(12_369_621)

        assert block is not None

        # Verify hash fields are FastHexBytes
        assert isinstance(block["hash"], FastHexBytes)
        assert isinstance(block["parent_hash"], FastHexBytes)
        assert isinstance(block["state_root"], FastHexBytes)
        assert isinstance(block["transactions_root"], FastHexBytes)
        assert isinstance(block["receipts_root"], FastHexBytes)

    def test_get_block_returns_int_for_numeric_fields(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that get_block returns int for numeric fields.
        """

        block = ethereum_mainnet_alloy_provider.eth.get_block(12_369_621)

        assert block is not None

        # Verify numeric fields are int
        assert isinstance(block["number"], int)
        assert isinstance(block["timestamp"], int)
        assert isinstance(block["gas_used"], int)
        assert isinstance(block["gas_limit"], int)

    def test_get_code_returns_hexbytes(self, ethereum_mainnet_alloy_provider: AlloyProvider):
        """
        Test that get_code returns FastHexBytes."""

        # Get code for WETH contract
        code = ethereum_mainnet_alloy_provider.eth.get_code(WETH_ADDRESS)

        assert isinstance(code, FastHexBytes)
        assert len(code) > 0


class TestHexBytesBehavior:
    """
    Test FastHexBytes objects have expected behavior.
    """

    def test_hexbytes_can_be_compared_with_bytes(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that FastHexBytes can be compared with bytes and converted to hex strings.
        """

        logs = ethereum_mainnet_alloy_provider.get_logs(
            from_block=12_369_621,
            to_block=12_369_621,
            addresses=[UNISWAP_V3_FACTORY],
        )

        assert len(logs) > 0
        address = logs[0]["address"]

        # Should be able to compare with bytes (HexBytes equality works with bytes)
        expected_bytes = bytes.fromhex(UNISWAP_V3_FACTORY[2:])  # Remove 0x prefix for fromhex
        assert address == expected_bytes

        # Convert FastHexBytes to hex string
        assert address.to_0x_hex().lower() == UNISWAP_V3_FACTORY.lower()

    def test_hexbytes_has_hex_method(self, ethereum_mainnet_alloy_provider: AlloyProvider):
        """
        Test that FastHexBytes has hex() method that returns lowercase hex string.
        """

        logs = ethereum_mainnet_alloy_provider.get_logs(
            from_block=12_369_621,
            to_block=12_369_621,
            addresses=[UNISWAP_V3_FACTORY],
        )

        assert len(logs) > 0
        address = logs[0]["address"]

        # Should have hex() method that returns lowercase hex string (without 0x prefix)
        hex_str = address.hex()
        assert hex_str == UNISWAP_V3_FACTORY.lower()  # Remove 0x for comparison
        assert len(hex_str) == 42
