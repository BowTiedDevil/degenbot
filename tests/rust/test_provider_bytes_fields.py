"""Tests for bytes and address conversion in AlloyProvider against a standalone anvil.

These verify the provider returns bytes for hash/data fields and checksummed
strings for address fields, validated against a real transaction + `Ping` log
emitted on the seeded standalone chain (no upstream RPC).
"""

from collections.abc import Iterator
from dataclasses import dataclass

import pytest
import web3

from degenbot.abi import encode as abi_encode
from degenbot.crypto import keccak256
from degenbot.fork import AnvilFork
from degenbot.provider import AlloyProvider
from tests.standalone_anvil import seed as seed_catalog


@dataclass
class EmittedTx:
    """A real emitted transaction: its block number + hash (for shape assertions)."""

    block: int
    tx_hash: str


def _ping_calldata() -> bytes:
    selector = keccak256(b"ping(uint256,bytes32)")[:4]
    return selector + abi_encode(["uint256", "bytes32"], [42, b"\x00" * 32])


@pytest.fixture
def alloy_provider(standalone_anvil: AnvilFork) -> Iterator[AlloyProvider]:
    """Create an AlloyProvider from the seeded standalone anvil."""
    provider = AlloyProvider(standalone_anvil.http_url)
    try:
        yield provider
    finally:
        provider.close()


@pytest.fixture
def emitted_tx(standalone_anvil: AnvilFork) -> EmittedTx:
    """Emit a real ``Ping`` log (a funded anvil tx) and return its block + hash.

    Sets a mixed-case coinbase so the mined block's ``miner`` is a checksummed
    address (satisfies the Kasto address-shape assertions).
    """
    w3 = web3.Web3(web3.HTTPProvider(standalone_anvil.http_url))
    w3.provider.make_request("anvil_setCoinbase", [seed_catalog.FUNDED_EOA])
    sender = w3.eth.accounts[0]
    tx = w3.eth.send_transaction({
        "from": sender,
        "to": seed_catalog.EVENT_EMITTER,
        "data": _ping_calldata(),
        "chainId": seed_catalog.CHAIN_ID,
    })
    receipt = w3.eth.wait_for_transaction_receipt(tx, timeout=10)
    return EmittedTx(block=receipt["blockNumber"], tx_hash=tx.hex())


class TestbytesConversion:
    """Test that appropriate fields are converted to bytes or checksummed strings."""

    def test_get_logs_returns_checksummed_address(
        self,
        alloy_provider: AlloyProvider,
        emitted_tx: EmittedTx,
    ):
        """Test that get_logs returns checksummed address strings for the address field."""
        logs = alloy_provider.get_logs(
            from_block=0,
            to_block=emitted_tx.block,
            addresses=[seed_catalog.EVENT_EMITTER],
        )

        assert len(logs) > 0
        log = logs[0]

        # address should be a checksummed string
        assert isinstance(log["address"], str)
        assert log["address"] == seed_catalog.EVENT_EMITTER
        # Verify it's checksummed (has mixed case)
        assert log["address"] != log["address"].lower()
        assert log["address"] != log["address"].upper()

    def test_get_logs_returns_bytes_for_hash_fields(
        self,
        alloy_provider: AlloyProvider,
        emitted_tx: EmittedTx,
    ):
        """Test that get_logs returns bytes for topics, blockHash, and transactionHash."""
        logs = alloy_provider.get_logs(
            from_block=0,
            to_block=emitted_tx.block,
            addresses=[seed_catalog.EVENT_EMITTER],
        )

        assert len(logs) > 0
        log = logs[0]

        assert isinstance(log["topics"], list)
        for topic in log["topics"]:
            assert isinstance(topic, bytes)

        assert isinstance(log["data"], bytes)

        if log.get("blockHash"):
            assert isinstance(log["blockHash"], bytes)

        if log.get("transactionHash"):
            assert isinstance(log["transactionHash"], bytes)

    def test_get_logs_returns_int_for_numeric_fields(
        self,
        alloy_provider: AlloyProvider,
        emitted_tx: EmittedTx,
    ):
        """Test that get_logs returns int for numeric fields."""
        logs = alloy_provider.get_logs(
            from_block=0,
            to_block=emitted_tx.block,
            addresses=[seed_catalog.EVENT_EMITTER],
        )

        assert len(logs) > 0
        log = logs[0]

        # Verify blockNumber is int
        assert isinstance(log["blockNumber"], int)

        # Verify logIndex is int
        assert isinstance(log["logIndex"], int)

    def test_eth_call_returns_bytes(self, alloy_provider: AlloyProvider):
        """Test that call returns bytes (ABI-decodable raw bytes)."""
        # Call balanceOf for the seeded token (uint256 read; same selector as ERC20).
        result = alloy_provider.call(
            to=seed_catalog.TOKEN,
            data=bytes.fromhex(
                "70a082310000000000000000000000000000000000000000000000000000000000000000"
            ),
        )

        assert isinstance(result, bytes)
        assert len(result) == 32  # uint256 return value

    def test_get_block_returns_checksummed_address_for_miner(
        self,
        alloy_provider: AlloyProvider,
        emitted_tx: EmittedTx,
    ):
        """Test that get_block returns checksummed address string for miner field."""
        block = alloy_provider.get_block(emitted_tx.block)

        assert block is not None

        # miner should be a checksummed string
        assert isinstance(block["miner"], str)
        # Verify it's checksummed (has mixed case)
        assert block["miner"] != block["miner"].lower()
        assert block["miner"] != block["miner"].upper()

    def test_get_block_returns_bytes_for_hash_fields(
        self,
        alloy_provider: AlloyProvider,
        emitted_tx: EmittedTx,
    ):
        """Test that get_block returns bytes for hash fields."""
        block = alloy_provider.get_block(emitted_tx.block)

        assert block is not None

        # Verify hash fields are bytes
        assert isinstance(block["hash"], bytes)
        assert isinstance(block["parent_hash"], bytes)
        assert isinstance(block["state_root"], bytes)
        assert isinstance(block["transactions_root"], bytes)
        assert isinstance(block["receipts_root"], bytes)

    def test_get_block_returns_int_for_numeric_fields(
        self,
        alloy_provider: AlloyProvider,
        emitted_tx: EmittedTx,
    ):
        """Test that get_block returns int for numeric fields."""
        block = alloy_provider.get_block(emitted_tx.block)

        assert block is not None

        # Verify numeric fields are int
        assert isinstance(block["number"], int)
        assert isinstance(block["timestamp"], int)
        assert isinstance(block["gas_used"], int)
        assert isinstance(block["gas_limit"], int)

    def test_get_code_returns_bytes(self, alloy_provider: AlloyProvider):
        """Test that get_code returns bytes (ABI-decodable raw bytes)."""
        # Get code for the seeded token contract
        code = alloy_provider.get_code(seed_catalog.TOKEN)

        assert isinstance(code, bytes)
        assert len(code) > 0

    def test_transaction_has_checksummed_addresses(
        self,
        alloy_provider: AlloyProvider,
        emitted_tx: EmittedTx,
    ):
        """Test that transactions have checksummed address strings for from/to fields."""
        # get_block lists tx hashes; fetch the full tx via get_transaction.
        tx = alloy_provider.get_transaction(emitted_tx.tx_hash)

        assert tx is not None
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
    """Test address string behavior."""

    def test_address_is_checksummed(
        self,
        alloy_provider: AlloyProvider,
        emitted_tx: EmittedTx,
    ):
        """Test that addresses are returned as checksummed strings."""
        logs = alloy_provider.get_logs(
            from_block=0,
            to_block=emitted_tx.block,
            addresses=[seed_catalog.EVENT_EMITTER],
        )

        assert len(logs) > 0
        address = logs[0]["address"]

        # Should be a string
        assert isinstance(address, str)

        # Should match the expected checksummed address
        assert address == seed_catalog.EVENT_EMITTER

        # Should be 42 characters (0x + 40 hex chars)
        assert len(address) == 42
        assert address.startswith("0x")

    def test_result_has_hex_method(
        self,
        alloy_provider: AlloyProvider,
        emitted_tx: EmittedTx,
    ):
        """Test that bytes has hex() method that returns hex string."""
        logs = alloy_provider.get_logs(
            from_block=0,
            to_block=emitted_tx.block,
            addresses=[seed_catalog.EVENT_EMITTER],
        )

        assert len(logs) > 0
        topic = logs[0]["topics"][0]

        # Topics should be bytes
        assert isinstance(topic, bytes)

        # bytes.hex() returns hex string without 0x prefix
        hex_str = topic.hex()
        assert len(hex_str) == 64  # 32 bytes = 64 hex chars
