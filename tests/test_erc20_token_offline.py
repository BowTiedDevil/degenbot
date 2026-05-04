"""
Offline tests for ERC20 token comparisons and cache behavior.

Uses offline pools to get real Erc20Token instances without requiring a live RPC.
"""

from pathlib import Path

import pytest
from hexbytes import HexBytes

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.provider import OfflineProvider, ProviderAdapter
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

CHAIN_DATA_PATH = Path(__file__).parent / "fixtures" / "chain_data"
UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
UNISWAP_V2_FACTORY_POOL_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)


@pytest.fixture(autouse=True)
def _reset_registries():
    """Reset singletons before each test so pools/tokens can be recreated."""
    from degenbot.registry import pool_registry, token_registry
    pool_registry._reset()
    token_registry._reset()
    connection_manager._reset()


@pytest.fixture
def offline_adapter():
    """Provide a ProviderAdapter wrapping offline chain data."""
    data_file = CHAIN_DATA_PATH / "1" / "block_24945920.json"
    provider = OfflineProvider.from_json_file(data_file)
    adapter = ProviderAdapter.from_offline(provider)
    connection_manager.register_provider(adapter)
    connection_manager._default_chain_id = 1
    return adapter


@pytest.fixture
def offline_v2_pool(offline_adapter):
    """Construct a V2 pool using offline data."""
    from degenbot.registry import pool_registry, token_registry
    pool_registry._reset()
    token_registry._reset()
    return UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        chain_id=1,
        provider=offline_adapter,
        state_block=24945920,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
        silent=True,
    )


@pytest.fixture
def offline_wbtc(offline_v2_pool):
    return offline_v2_pool.token0


@pytest.fixture
def offline_weth(offline_v2_pool):
    return offline_v2_pool.token1


class TestErc20TokenComparisons:
    """Token comparison tests that need real token objects but no live RPC."""

    def test_equality_and_inequality(self, offline_wbtc, offline_weth):
        """ERC20Token compares to addresses and raises on unsupported types."""
        weth = offline_weth
        wbtc = offline_wbtc

        with pytest.raises(AssertionError):
            assert weth == 69

        with pytest.raises(TypeError):
            assert weth < 69

        with pytest.raises(TypeError):
            assert weth > 69

        assert weth != wbtc

        assert weth == weth.address
        assert weth == weth.address.lower()
        assert weth == weth.address.upper()
        assert weth == get_checksum_address(weth.address)
        assert weth == HexBytes(weth.address)
        assert weth == bytes.fromhex(weth.address[2:])

        assert wbtc == wbtc.address
        assert wbtc == wbtc.address.lower()
        assert wbtc == wbtc.address.upper()
        assert wbtc == get_checksum_address(wbtc.address)
        assert wbtc == HexBytes(wbtc.address)

        assert weth > wbtc
        assert weth > wbtc.address
        assert weth > wbtc.address.lower()
        assert weth > wbtc.address.upper()
        assert weth > get_checksum_address(wbtc.address)
        assert weth > HexBytes(wbtc.address)
        assert weth > bytes.fromhex(wbtc.address[2:])

        assert wbtc < weth
        assert wbtc < weth.address
        assert wbtc < weth.address.lower()
        assert wbtc < weth.address.upper()
        assert wbtc < get_checksum_address(weth.address)
        assert wbtc < HexBytes(weth.address)
        assert wbtc < bytes.fromhex(weth.address[2:])


class TestErc20TokenCaches:
    """ERC20Token cache manipulation tests (no RPC calls needed)."""

    def test_cached_total_supply(self, offline_wbtc):
        """Total supply cache hit and miss behave correctly."""
        # use cached_total_supply heavily, since totalSupply is already recorded
        wbtc = offline_wbtc

        # First call primes the cache
        current_total_supply = wbtc.get_total_supply(24945920)
        assert current_total_supply > 0

        fake_supply = 69_420_000_000
        wbtc._cached_total_supply[24945920] = fake_supply
        assert wbtc.get_total_supply(24945920) == fake_supply

        wbtc._cached_total_supply.clear()
        assert wbtc.get_total_supply(24945920) == current_total_supply

    def test_cached_name_and_symbol(self, offline_wbtc):
        """Name and symbol are loaded once and reused."""
        wbtc = offline_wbtc
        assert wbtc.name == "Wrapped BTC"
        assert wbtc.symbol == "WBTC"

    def test_decimals(self, offline_wbtc, offline_weth):
        """Decimals match known token properties."""
        assert offline_wbtc.decimals == 8
        assert offline_weth.decimals == 18
