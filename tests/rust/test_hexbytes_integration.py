"""
Tests for HexBytes and address conversion in AlloyProvider.

These tests verify that the provider returns:
- HexBytes for hash and data fields
- Checksummed strings for address fields
"""

import pytest
from hexbytes import HexBytes

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
    Test that appropriate fields are converted to HexBytes or checksummed strings.
    """

    def test_get_logs_returns_checksummed_address(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that get_logs returns checksummed address strings for the address field.
        """

        # Fetch a known log from the Uniswap V3 factory
        logs = ethereum_mainnet_alloy_provider.get_logs(
            from_block=12_369_621,
            to_block=12_369_621,
            addresses=[UNISWAP_V3_FACTORY],
        )

        assert len(logs) > 0
        log = logs[0]

        # address should be a checksummed string
        assert isinstance(log["address"], str)
        assert log["address"] == UNISWAP_V3_FACTORY
        # Verify it's checksummed (has mixed case)
        assert log["address"] != log["address"].lower()
        assert log["address"] != log["address"].upper()

    def test_get_logs_returns_hexbytes_for_hash_fields(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that get_logs returns HexBytes for topics, blockHash, and transactionHash.
        """

        logs = ethereum_mainnet_alloy_provider.get_logs(
            from_block=12_369_621,
            to_block=12_369_621,
            addresses=[UNISWAP_V3_FACTORY],
        )

        assert len(logs) > 0
        log = logs[0]

        assert isinstance(log["topics"], list)
        for topic in log["topics"]:
            assert isinstance(topic, HexBytes)

        assert isinstance(log["data"], HexBytes)

        if log.get("blockHash"):
            assert isinstance(log["blockHash"], HexBytes)

        if log.get("transactionHash"):
            assert isinstance(log["transactionHash"], HexBytes)

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
        Test that call returns HexBytes (for eth_abi compatibility).
        """

        # Call balanceOf for WETH
        # balanceOf selector: 0x70a08231
        result = ethereum_mainnet_alloy_provider.call(
            to=WETH_ADDRESS,
            data=bytes.fromhex("70a08231000000000000000000000000C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        )

        assert isinstance(result, HexBytes)
        assert len(result) == 32  # uint256 return value

    def test_get_block_returns_checksummed_address_for_miner(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that get_block returns checksummed address string for miner field.
        """

        block = ethereum_mainnet_alloy_provider.get_block(12_369_621)

        assert block is not None

        # miner should be a checksummed string
        assert isinstance(block["miner"], str)
        # Verify it's checksummed (has mixed case)
        assert block["miner"] != block["miner"].lower()
        assert block["miner"] != block["miner"].upper()

    def test_get_block_returns_hexbytes_for_hash_fields(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that get_block returns HexBytes for hash fields.
        """

        block = ethereum_mainnet_alloy_provider.get_block(12_369_621)

        assert block is not None

        # Verify hash fields are HexBytes
        assert isinstance(block["hash"], HexBytes)
        assert isinstance(block["parent_hash"], HexBytes)
        assert isinstance(block["state_root"], HexBytes)
        assert isinstance(block["transactions_root"], HexBytes)
        assert isinstance(block["receipts_root"], HexBytes)

    def test_get_block_returns_int_for_numeric_fields(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that get_block returns int for numeric fields.
        """

        block = ethereum_mainnet_alloy_provider.get_block(12_369_621)

        assert block is not None

        # Verify numeric fields are int
        assert isinstance(block["number"], int)
        assert isinstance(block["timestamp"], int)
        assert isinstance(block["gas_used"], int)
        assert isinstance(block["gas_limit"], int)

    def test_get_code_returns_hexbytes(self, ethereum_mainnet_alloy_provider: AlloyProvider):
        """
        Test that get_code returns HexBytes (for eth_abi compatibility).
        """

        # Get code for WETH contract
        code = ethereum_mainnet_alloy_provider.get_code(WETH_ADDRESS)

        assert isinstance(code, HexBytes)
        assert len(code) > 0

    def test_transaction_has_checksummed_addresses(
        self, ethereum_mainnet_alloy_provider: AlloyProvider
    ):
        """
        Test that transactions have checksummed address strings for from/to fields.
        """

        block = ethereum_mainnet_alloy_provider.get_block(12_369_621)

        assert block is not None
        transactions = block.get("transactions", [])
        if transactions and isinstance(transactions, list) and len(transactions) > 0:
            tx = transactions[0]
            if isinstance(tx, dict):
                # from should be a checksummed string
                assert isinstance(tx["from"], str)
                assert tx["from"] != tx["from"].lower()
                assert tx["from"] != tx["from"].upper()

                # to can be None (contract creation) or checksummed string
                if tx.get("to") is not None:
                    assert isinstance(tx["to"], str)
                    assert tx["to"] != tx["to"].lower()
                    assert tx["to"] != tx["to"].upper()


class TestAddressBehavior:
    """
    Test address string behavior.
    """

    def test_address_is_checksummed(self, ethereum_mainnet_alloy_provider: AlloyProvider):
        """
        Test that addresses are returned as checksummed strings.
        """

        logs = ethereum_mainnet_alloy_provider.get_logs(
            from_block=12_369_621,
            to_block=12_369_621,
            addresses=[UNISWAP_V3_FACTORY],
        )

        assert len(logs) > 0
        address = logs[0]["address"]

        # Should be a string
        assert isinstance(address, str)

        # Should match the expected checksummed address
        assert address == UNISWAP_V3_FACTORY

        # Should be 42 characters (0x + 40 hex chars)
        assert len(address) == 42
        assert address.startswith("0x")

    def test_hexbytes_has_hex_method(self, ethereum_mainnet_alloy_provider: AlloyProvider):
        """
        Test that HexBytes has hex() method that returns hex string.
        """

        logs = ethereum_mainnet_alloy_provider.get_logs(
            from_block=12_369_621,
            to_block=12_369_621,
            addresses=[UNISWAP_V3_FACTORY],
        )

        assert len(logs) > 0
        topic = logs[0]["topics"][0]

        # Topics should be HexBytes
        assert isinstance(topic, HexBytes)

        # HexBytes.hex() returns hex string without 0x prefix
        hex_str = topic.hex()
        assert len(hex_str) == 64  # 32 bytes = 64 hex chars
