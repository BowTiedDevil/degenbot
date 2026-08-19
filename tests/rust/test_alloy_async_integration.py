"""Async integration tests for the Alloy-based Ethereum RPC provider.

These tests use the Rust async provider with proper tokio runtime support via
pyo3-async-runtimes, run against the seeded standalone anvil (no upstream RPC).
"""

import eth_abi
import pytest
import web3

from degenbot._ffi.provider import AsyncAlloyProvider
from degenbot.crypto import keccak256
from degenbot.fork import AnvilFork
from tests.standalone_anvil import seed as seed_catalog

WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"


def _ping_calldata() -> bytes:
    selector = keccak256(b"ping(uint256,bytes32)")[:4]
    return selector + eth_abi.encode(["uint256", "bytes32"], [42, b"\x00" * 32])


@pytest.fixture
async def async_provider(standalone_anvil: AnvilFork) -> AsyncAlloyProvider:
    """An AsyncAlloyProvider over the seeded standalone anvil."""
    provider = await AsyncAlloyProvider.create(standalone_anvil.http_url)
    yield provider
    provider.close()


@pytest.fixture
def emitted_block(standalone_anvil: AnvilFork) -> int:
    """Emit a real ``Ping`` log (a funded anvil tx) and return its block number."""
    w3 = web3.Web3(web3.HTTPProvider(standalone_anvil.http_url))
    sender = w3.eth.accounts[0]
    tx = w3.eth.send_transaction({
        "from": sender,
        "to": seed_catalog.EVENT_EMITTER,
        "data": _ping_calldata(),
        "chainId": seed_catalog.CHAIN_ID,
    })
    return w3.eth.wait_for_transaction_receipt(tx, timeout=10)["blockNumber"]


@pytest.mark.asyncio
class TestAsyncProviderWithConnection:
    """Async tests against the standalone anvil connection (no upstream RPC)."""

    async def test_async_get_block_number(self, async_provider: AsyncAlloyProvider):
        """Test fetching block number asynchronously."""
        block_number = await async_provider.get_block_number()
        assert isinstance(block_number, int)
        assert block_number > 0

    async def test_async_get_chain_id(self, async_provider: AsyncAlloyProvider):
        """Test fetching chain ID asynchronously."""
        chain_id = await async_provider.get_chain_id()
        assert chain_id == seed_catalog.CHAIN_ID

    async def test_async_get_logs(
        self,
        async_provider: AsyncAlloyProvider,
        emitted_block: int,
    ):
        """Test fetching logs with filter asynchronously from a real emitted event."""
        logs = await async_provider.get_logs(
            from_block=0,
            to_block=emitted_block,
            addresses=[seed_catalog.EVENT_EMITTER],
        )
        assert isinstance(logs, list)
        assert len(logs) > 0

    async def test_async_provider_rpc_url(
        self,
        async_provider: AsyncAlloyProvider,
        standalone_anvil: AnvilFork,
    ):
        """Test async provider exposes rpc_url getter."""
        assert async_provider.rpc_url == standalone_anvil.http_url

    async def test_async_get_gas_price_returns_int(self, async_provider: AsyncAlloyProvider):
        """Async get_gas_price should return int (matches sync return type)."""
        result = await async_provider.get_gas_price()
        assert isinstance(result, int), f"Expected int, got {type(result)}"
        assert result >= 0

    async def test_async_call_returns_bytes(self, async_provider: AsyncAlloyProvider):
        """Async call should return bytes."""
        # SimpleToken.totalSupply() (matches the ERC20 totalSupply selector 0x18160ddd).
        result = await async_provider.call(
            to=seed_catalog.TOKEN,
            data=bytes.fromhex("18160ddd"),
        )
        assert isinstance(result, bytes)
        assert len(result) == 32

    async def test_async_get_code_returns_bytes(self, async_provider: AsyncAlloyProvider):
        """Async get_code should return bytes."""
        code = await async_provider.get_code(seed_catalog.TOKEN)
        assert isinstance(code, bytes)
        assert len(code) > 0

    async def test_async_get_balance_of(self, async_provider: AsyncAlloyProvider):
        """Async eth_call to balanceOf should decode correctly."""
        # balanceOf(address) selector 0x70a08231 + seeded waiter padded to 32 bytes.
        calldata = bytes.fromhex("70a08231" + "00" * 12 + seed_catalog.FUNDED_EOA[2:])
        result = await async_provider.call(
            to=seed_catalog.TOKEN,
            data=calldata,
        )
        assert isinstance(result, bytes)
        # Should be able to decode as int
        balance = int.from_bytes(result, "big")
        assert balance >= 0
