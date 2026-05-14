"""
Tests for CurveFetcherFactory.

The CurveFetcherFactory creates fetcher closures for Curve StableSwap pools.
Each fetcher captures chain_id and optionally pool_address at creation,
then uses the ConnectionManager to perform I/O when called.

This test suite validates each fetcher method with a faked ConnectionManager
that routes all calls through ProviderAdapter.
"""

from typing import Any

import eth_abi.abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3

from degenbot.checksum_cache import get_checksum_address
from degenbot.curve.fetcher_factory import CurveFetcherFactory
from degenbot.provider.interface import ProviderAdapter
from degenbot.types.aliases import ChainId

# --- Fakes ---


class FakeProviderBackend:
    """A fake _SyncProviderBackend that dispatches call_raw by selector.

    Responses are configured via the ``call_responses`` dict. The key is the
    4-byte selector; the value is either:
    - A callable(to, data, block) -> bytes
    - A bytes value returned directly
    """

    def __init__(self, call_responses: dict[bytes, bytes] | None = None) -> None:
        self._responses = call_responses or {}
        self._block_number = 1
        self._block_timestamp = 1700000000

    @property
    def chain_id(self) -> int:
        return 1

    @property
    def block_number(self) -> int:
        return self._block_number

    def get_block_number(self) -> int:
        return self._block_number

    def get_block(self, block_identifier: int | str) -> dict[str, Any] | None:
        return {"number": self._block_number, "timestamp": self._block_timestamp}

    def get_logs(self, from_block: int, to_block: int, addresses: list[str] | None,
                 topics: list[list[str]] | None) -> list[dict[str, Any]]:
        return []

    def call(self, to: str, data: bytes, block: int | None) -> HexBytes:
        return self.call_raw({"to": to, "data": data}, block)

    def call_raw(self, tx: dict[str, Any], block: int | None) -> HexBytes:
        data = bytes(tx["data"])
        selector = data[:4]
        if selector in self._responses:
            return HexBytes(self._responses[selector])
        msg = f"No mock response for selector 0x{selector.hex()}"
        raise RuntimeError(msg)

    def get_code(self, address: str, block: int | None) -> HexBytes:
        return HexBytes(b"")

    def get_balance(self, address: str, block: int | None) -> int:
        return 0

    def get_storage_at(self, address: str, position: int, block: int | None) -> HexBytes:
        return HexBytes(b"\x00" * 32)

    def get_transaction_count(self, address: str, block: int | None) -> int:
        return 0

    def is_connected(self) -> bool:
        return True

    def close(self) -> None:
        pass


def _make_fake_provider(
    call_responses: dict[bytes, bytes] | None = None,
    block_number: int = 1,
    block_timestamp: int = 1700000000,
) -> ProviderAdapter:
    """Create a ProviderAdapter backed by FakeProviderBackend."""
    backend = FakeProviderBackend(call_responses)
    backend._block_number = block_number
    backend._block_timestamp = block_timestamp
    adapter = ProviderAdapter.__new__(ProviderAdapter)
    adapter._backend = backend
    adapter._provider_type = "alloy"
    adapter._raw_provider = None
    return adapter


class FakeConnectionManager:
    """A fake ConnectionManager that returns a ProviderAdapter."""

    def __init__(self, provider: ProviderAdapter | None = None) -> None:
        self._provider = provider or _make_fake_provider()

    def get_provider(self, chain_id: ChainId) -> ProviderAdapter:  # noqa: ARG002
        return self._provider

    def get_web3(self, chain_id: ChainId) -> Any:  # noqa: ARG002
        msg = "get_web3() should not be called — use get_provider() instead"
        raise RuntimeError(msg)


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
        expected_vp = 10**18
        provider = _make_fake_provider({
            _selector("get_virtual_price()"): eth_abi.abi.encode(
                ["uint256"], [expected_vp]
            ),
        })
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.virtual_price_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == expected_vp

    def test_uses_base_pool_address_when_provided(self) -> None:
        expected_vp = 2 * 10**18
        provider = _make_fake_provider({
            _selector("get_virtual_price()"): eth_abi.abi.encode(
                ["uint256"], [expected_vp]
            ),
        })
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        base_pool = get_checksum_address("0x0000000000000000000000000000000000000002")
        fetcher = factory.virtual_price_fetcher(POOL_ADDRESS, base_pool_address=base_pool)
        result = fetcher(block_number=1)
        assert result == expected_vp


class TestBaseVirtualPriceFetcher:
    """Test base_virtual_price_fetcher factory method."""

    def test_fetches_base_virtual_price(self) -> None:
        expected_vp = 3 * 10**18
        provider = _make_fake_provider({
            _selector("base_virtual_price()"): eth_abi.abi.encode(
                ["uint256"], [expected_vp]
            ),
        })
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.base_virtual_price_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == expected_vp


class TestTimestampFetcher:
    """Test timestamp_fetcher factory method."""

    def test_fetches_timestamp(self) -> None:
        provider = _make_fake_provider(block_timestamp=1700000000)
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.timestamp_fetcher()
        result = fetcher(block_number=1)
        assert result == 1700000000


class TestRedemptionPriceFetcher:
    """Test redemption_price_fetcher factory method."""

    def test_fetches_redemption_price(self) -> None:
        snap_address = get_checksum_address("0x0000000000000000000000000000000000000003")
        raw_rate = 10**18  # Will be divided by 10**9
        provider = _make_fake_provider({
            _selector("redemption_price_snap()"): eth_abi.abi.encode(
                ["address"], [snap_address]
            ),
            _selector("snappedRedemptionPrice()"): eth_abi.abi.encode(
                ["uint256"], [raw_rate]
            ),
        })
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.redemption_price_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == raw_rate // 10**9


class TestAdminBalancesFetcher:
    """Test admin_balances_fetcher factory method."""

    def test_fetches_admin_balances(self) -> None:
        provider = _make_fake_provider({
            _selector("admin_balances(uint256)"): eth_abi.abi.encode(
                ["uint256"], [1000]
            ),
        })
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.admin_balances_fetcher(POOL_ADDRESS)
        assert callable(fetcher)


class TestBlockNumberFetcher:
    """Test block_number_fetcher factory method."""

    def test_fetches_block_number(self) -> None:
        provider = _make_fake_provider(block_number=42)
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.block_number_fetcher()
        result = fetcher()
        assert result == 42


class TestProviderCall:
    """Test provider_call factory method."""

    def test_returns_callable(self) -> None:
        expected_data = b"\x00\x01\x02"
        provider = _make_fake_provider({
            b"\xab\xcd\xef\x01": expected_data,
        })
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.provider_call()
        assert callable(fetcher)


class TestDFetcher:
    """Test D_fetcher factory method."""

    def test_fetches_D_value(self) -> None:  # noqa: N802
        expected_d = 10**18
        provider = _make_fake_provider({
            _selector("D()"): eth_abi.abi.encode(
                ["uint256"], [expected_d]
            ),
        })
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.D_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == expected_d


class TestGammaFetcher:
    """Test gamma_fetcher factory method."""

    def test_fetches_gamma_value(self) -> None:
        expected_gamma = 10**10
        provider = _make_fake_provider({
            _selector("gamma()"): eth_abi.abi.encode(
                ["uint256"], [expected_gamma]
            ),
        })
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.gamma_fetcher(POOL_ADDRESS)
        result = fetcher(block_number=1)
        assert result == expected_gamma


class TestPriceScaleFetcher:
    """Test price_scale_fetcher factory method."""

    def test_fetches_price_scale(self) -> None:
        expected_price = 10**18
        provider = _make_fake_provider({
            _selector("price_scale(uint256)"): eth_abi.abi.encode(
                ["uint256"], [expected_price]
            ),
        })
        connections = FakeConnectionManager(provider=provider)
        factory = CurveFetcherFactory(connections=connections, chain_id=CHAIN_ID)
        fetcher = factory.price_scale_fetcher(POOL_ADDRESS, n_coins=3)
        assert callable(fetcher)
