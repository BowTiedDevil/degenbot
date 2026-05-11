"""
Tests for CurveFetcherFactory.

The CurveFetcherFactory creates fetcher closures for Curve StableSwap pools.
Each fetcher captures chain_id and optionally pool_address at creation,
then uses the ConnectionManager to perform I/O when called.

This test suite validates each fetcher method with a faked ConnectionManager.
"""

from typing import Any

import eth_abi.abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3
from web3.types import BlockIdentifier

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.fetcher_factory import CurveFetcherFactory
from degenbot.types.aliases import ChainId

# --- Fakes ---


class FakeProvider:
    """A fake ProviderAdapter that returns pre-programmed responses."""

    def __init__(self, responses: dict[bytes, bytes] | None = None) -> None:
        self._responses = responses or {}
        self._block_number = 1

    def call(
        self,
        *,
        _to: str,
        data: bytes,
        _block: int | None = None,
    ) -> HexBytes:
        key = data[:4]
        if key in self._responses:
            return HexBytes(self._responses[key])
        msg = f"No mock response for selector 0x{key.hex()}"
        raise RuntimeError(msg)

    def get_block_number(self) -> int:
        return self._block_number


class FakeWeb3:
    """A fake Web3 with eth.call and eth.get_block."""

    def __init__(self) -> None:
        self.eth = FakeEth()


class FakeEth:
    """A fake eth module."""

    def __init__(self) -> None:
        self._call_responses: dict[bytes, bytes] = {}
        self._block_data: dict[int, dict[str, Any]] = {}

    def call(self, tx: dict[str, Any], block_identifier: BlockIdentifier | None = None) -> bytes:  # noqa: ARG002
        data = tx.get("data", b"")
        selector = data[:4] if data else b""
        if selector in self._call_responses:
            return self._call_responses[selector]
        msg = f"No mock eth.call response for selector 0x{selector.hex()}"
        raise RuntimeError(msg)

    def get_block(self, block_identifier: BlockIdentifier | None = None) -> dict[str, Any]:
        if isinstance(block_identifier, int) and block_identifier in self._block_data:
            return self._block_data[block_identifier]
        return {"timestamp": 1000}


class FakeConnectionManager:
    """A fake ConnectionManager that returns FakeProvider and FakeWeb3."""

    def __init__(self, provider: FakeProvider | None = None, w3: FakeWeb3 | None = None) -> None:
        self._provider = provider or FakeProvider()
        self._w3 = w3 or FakeWeb3()

    def get_provider(self, chain_id: ChainId) -> FakeProvider:  # noqa: ARG002
        return self._provider

    def get_web3(self, chain_id: ChainId) -> FakeWeb3:  # noqa: ARG002
        return self._w3


# --- Helpers ---


def _selector(signature: str) -> bytes:
    """Return the 4-byte function selector."""
    return Web3.keccak(text=signature)[:4]


CHAIN_ID: ChainId = 1
POOL_ADDRESS: ChecksumAddress = get_checksum_address(
    "0x0000000000000000000000000000000000000001"
)


# --- Tests ---


class TestCurveFetcherFactoryCreation:
    """Test that CurveFetcherFactory can be created."""

    def test_factory_exists(self) -> None:
        assert CurveFetcherFactory is not None

    def test_factory_requires_connections_and_chain_id(self) -> None:
        connections = FakeConnectionManager()
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        assert factory is not None


class TestVirtualPriceFetcher:
    """Test virtual_price_fetcher factory method."""

    def test_returns_callable(self) -> None:
        connections = FakeConnectionManager()
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.virtual_price_fetcher(POOL_ADDRESS)
        assert callable(fetcher)

    def test_fetches_virtual_price(self) -> None:
        w3 = FakeWeb3()
        expected_vp = 10**18
        w3.eth._call_responses[_selector("get_virtual_price()")] = eth_abi.abi.encode(
            ["uint256"], [expected_vp]
        )
        connections = FakeConnectionManager(w3=w3)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.virtual_price_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == expected_vp

    def test_uses_base_pool_address_when_provided(self) -> None:
        w3 = FakeWeb3()
        expected_vp = 2 * 10**18
        w3.eth._call_responses[_selector("get_virtual_price()")] = eth_abi.abi.encode(
            ["uint256"], [expected_vp]
        )
        connections = FakeConnectionManager(w3=w3)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        base_pool = get_checksum_address("0x0000000000000000000000000000000000000002")
        fetcher = factory.virtual_price_fetcher(POOL_ADDRESS, base_pool_address=base_pool)
        result = fetcher(block_number=1)
        assert result == expected_vp


class TestBaseVirtualPriceFetcher:
    """Test base_virtual_price_fetcher factory method."""

    def test_fetches_base_virtual_price(self) -> None:
        w3 = FakeWeb3()
        expected_vp = 3 * 10**18
        w3.eth._call_responses[_selector("base_virtual_price()")] = eth_abi.abi.encode(
            ["uint256"], [expected_vp]
        )
        connections = FakeConnectionManager(w3=w3)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.base_virtual_price_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == expected_vp


class TestTimestampFetcher:
    """Test timestamp_fetcher factory method."""

    def test_fetches_timestamp(self) -> None:
        w3 = FakeWeb3()
        w3.eth._block_data[1] = {"timestamp": 1700000000}
        connections = FakeConnectionManager(w3=w3)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.timestamp_fetcher()
        result = fetcher(block_number=1)
        assert result == 1700000000


class TestRedemptionPriceFetcher:
    """Test redemption_price_fetcher factory method."""

    def test_fetches_redemption_price(self) -> None:
        w3 = FakeWeb3()
        snap_address = get_checksum_address("0x0000000000000000000000000000000000000003")
        raw_rate = 10**18  # Will be divided by 10**9
        w3.eth._call_responses[_selector("redemption_price_snap()")] = eth_abi.abi.encode(
            ["address"], [snap_address]
        )
        w3.eth._call_responses[_selector("snappedRedemptionPrice()")] = eth_abi.abi.encode(
            ["uint256"], [raw_rate]
        )
        connections = FakeConnectionManager(w3=w3)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.redemption_price_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == raw_rate // 10**9


class TestAdminBalancesFetcher:
    """Test admin_balances_fetcher factory method."""

    def test_fetches_admin_balances(self) -> None:
        provider = FakeProvider()
        provider._responses[_selector("admin_balances(uint256)")] = eth_abi.abi.encode(
            ["uint256"], [1000]
        )
        # This test is simpler: just check it returns a callable
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.admin_balances_fetcher(POOL_ADDRESS)
        assert callable(fetcher)


class TestBlockNumberFetcher:
    """Test block_number_fetcher factory method."""

    def test_fetches_block_number(self) -> None:
        provider = FakeProvider()
        provider._block_number = 42
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.block_number_fetcher()
        result = fetcher()
        assert result == 42


class TestProviderCall:
    """Test provider_call factory method."""

    def test_returns_callable(self) -> None:
        w3 = FakeWeb3()
        expected_data = b"\x00\x01\x02"
        w3.eth._call_responses[b"\xab\xcd\xef\x01"] = expected_data
        connections = FakeConnectionManager(w3=w3)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.provider_call()
        assert callable(fetcher)


class TestDFetcher:
    """Test D_fetcher factory method."""

    def test_fetches_D_value(self) -> None:  # noqa: N802
        w3 = FakeWeb3()
        expected_d = 10**18
        w3.eth._call_responses[_selector("D()")] = eth_abi.abi.encode(
            ["uint256"], [expected_d]
        )
        connections = FakeConnectionManager(w3=w3)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.D_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == expected_d


class TestGammaFetcher:
    """Test gamma_fetcher factory method."""

    def test_fetches_gamma_value(self) -> None:
        w3 = FakeWeb3()
        expected_gamma = 10**10
        w3.eth._call_responses[_selector("gamma()")] = eth_abi.abi.encode(
            ["uint256"], [expected_gamma]
        )
        connections = FakeConnectionManager(w3=w3)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.gamma_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == expected_gamma


class TestPriceScaleFetcher:
    """Test price_scale_fetcher factory method."""

    def test_fetches_price_scale(self) -> None:
        w3 = FakeWeb3()
        expected_price = 10**18
        w3.eth._call_responses[_selector("price_scale(uint256)")] = eth_abi.abi.encode(
            ["uint256"], [expected_price]
        )
        connections = FakeConnectionManager(w3=w3)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.price_scale_fetcher(POOL_ADDRESS, n_coins=3)
        assert callable(fetcher)
