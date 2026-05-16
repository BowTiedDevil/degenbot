"""
Tests for CurveDataProvider: the consolidated I/O seam for Curve pools.

Validates that _CurveDataProviderImpl (created by CurveFetcherFactory.create_provider())
satisfies the CurveDataProvider protocol and correctly delegates to the underlying
connection manager.
"""

from unittest.mock import MagicMock

import eth_abi.abi
from hexbytes import HexBytes
from web3 import Web3

from degenbot.curve.fetcher_factory import CurveFetcherFactory
from degenbot.curve.types import CurveDataProvider

# --- Fake provider ---


class FakeProvider:
    """A fake provider that returns pre-programmed ABI-encoded responses."""

    def __init__(self, responses: dict[str, bytes]) -> None:
        self._responses = responses

    def call(self, *, to: str, data: bytes, block: int | None = None) -> HexBytes:
        selector = data[:4].hex()
        if selector in self._responses:
            return HexBytes(self._responses[selector])
        msg = f"No mock response for selector 0x{selector}"
        raise ValueError(msg)

    def call_raw(self, tx: dict, block: int | None = None) -> HexBytes:
        return self.call(to=tx["to"], data=tx["data"], block=block)

    def get_block_number(self) -> int:
        return 18_000_000

    def get_block_timestamp(self, block: int | None = None) -> int:
        return 1_700_000_000


def _selector(signature: str) -> str:
    return Web3.keccak(text=signature)[:4].hex()


def _make_factory(provider: FakeProvider) -> CurveFetcherFactory:
    connections = MagicMock()
    connections.get_provider.return_value = provider
    return CurveFetcherFactory(connections=connections, chain_id=1)


POOL_ADDRESS = "0x0000000000000000000000000000000000000001"


class TestCurveDataProviderProtocol:
    """Test that _CurveDataProviderImpl satisfies the CurveDataProvider protocol."""

    def test_provider_satisfies_protocol(self) -> None:
        """create_provider() returns an object that satisfies CurveDataProvider."""
        factory = _make_factory(FakeProvider({}))
        provider = factory.create_provider(POOL_ADDRESS)
        assert isinstance(provider, CurveDataProvider)


class TestVirtualPrice:
    """Test the virtual_price method."""

    def test_virtual_price_fetches_from_pool(self) -> None:
        """virtual_price() calls get_virtual_price() on the pool contract."""
        vp_value = 10**18
        fake = FakeProvider({
            _selector("get_virtual_price()"): eth_abi.abi.encode(["uint256"], [vp_value]),
        })
        factory = _make_factory(fake)
        provider = factory.create_provider(POOL_ADDRESS)
        assert provider.virtual_price(18_000_000) == vp_value

    def test_virtual_price_for_metapool_uses_base_pool(self) -> None:
        """virtual_price() with base_pool_address calls get_virtual_price() on the base pool."""
        vp_value = 10**18
        base_address = "0x0000000000000000000000000000000000000002"
        fake = FakeProvider({
            _selector("get_virtual_price()"): eth_abi.abi.encode(["uint256"], [vp_value]),
        })
        factory = _make_factory(fake)
        provider = factory.create_provider(
            POOL_ADDRESS,
            base_pool_address=base_address,
        )
        assert provider.virtual_price(18_000_000) == vp_value


class TestBaseVirtualPrice:
    """Test the base_virtual_price method."""

    def test_base_virtual_price_fetches_from_pool(self) -> None:
        """base_virtual_price() calls base_virtual_price() on the pool contract."""
        bvp_value = 10**18 + 1
        fake = FakeProvider({
            _selector("base_virtual_price()"): eth_abi.abi.encode(["uint256"], [bvp_value]),
        })
        factory = _make_factory(fake)
        provider = factory.create_provider(POOL_ADDRESS)
        assert provider.base_virtual_price(18_000_000) == bvp_value


class TestBlockTimestamp:
    """Test the block_timestamp method."""

    def test_block_timestamp_delegates_to_provider(self) -> None:
        """block_timestamp() delegates to ProviderAdapter.get_block_timestamp()."""
        fake = FakeProvider({})
        factory = _make_factory(fake)
        provider = factory.create_provider(POOL_ADDRESS)
        assert provider.block_timestamp(18_000_000) == 1_700_000_000


class TestBlockNumber:
    """Test the block_number method."""

    def test_block_number_delegates_to_provider(self) -> None:
        """block_number() delegates to ProviderAdapter.get_block_number()."""
        fake = FakeProvider({})
        factory = _make_factory(fake)
        provider = factory.create_provider(POOL_ADDRESS)
        assert provider.block_number() == 18_000_000


class TestDFetcher:
    """Test the D method for crypto pools."""

    def test_D_fetches_from_pool(self) -> None:
        """D() calls D() on the pool contract (crypto pools only)."""
        d_value = 10**18 * 100
        fake = FakeProvider({
            _selector("D()"): eth_abi.abi.encode(["uint256"], [d_value]),
        })
        factory = _make_factory(fake)
        provider = factory.create_provider(POOL_ADDRESS, is_crypto=True)
        assert provider.D(18_000_000) == d_value


class TestGammaFetcher:
    """Test the gamma method for crypto pools."""

    def test_gamma_fetches_from_pool(self) -> None:
        """gamma() calls gamma() on the pool contract (crypto pools only)."""
        gamma_value = 10**10
        fake = FakeProvider({
            _selector("gamma()"): eth_abi.abi.encode(["uint256"], [gamma_value]),
        })
        factory = _make_factory(fake)
        provider = factory.create_provider(POOL_ADDRESS, is_crypto=True)
        assert provider.gamma(18_000_000) == gamma_value
