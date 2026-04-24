"""
Fixtures for Uniswap V2 offline tests.

These fixtures provide offline-compatible pool objects that can be used without requiring a live
RPC connection.
"""

from pathlib import Path

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider import OfflineProvider, ProviderAdapter
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

# Path to recorded chain data
CHAIN_DATA_PATH = Path(__file__).parent.parent.parent / "fixtures" / "chain_data"

UNISWAP_V2_WBTC_WETH_POOL = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
UNISWAP_V2_FACTORY_POOL_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)

# Token addresses
WBTC_CONTRACT_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
WETH_CONTRACT_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


@pytest.fixture
def offline_provider() -> OfflineProvider:
    """Provide an offline provider with recorded chain data."""
    data_file = CHAIN_DATA_PATH / "1" / "block_24945920.json"
    if not data_file.exists():
        pytest.skip(f"Offline data file not found: {data_file}")

    return OfflineProvider.from_json_file(data_file)


@pytest.fixture
def offline_adapter(offline_provider: OfflineProvider) -> ProviderAdapter:
    """Provide a ProviderAdapter wrapping the offline provider."""
    return ProviderAdapter.from_offline(offline_provider)


@pytest.fixture
def offline_wbtc(offline_wbtc_weth_v2_pool: UniswapV2Pool) -> Erc20Token:
    """Get WBTC token from the offline pool."""
    return offline_wbtc_weth_v2_pool.token0


@pytest.fixture
def offline_weth(offline_wbtc_weth_v2_pool: UniswapV2Pool) -> Erc20Token:
    """Get WETH token from the offline pool."""
    return offline_wbtc_weth_v2_pool.token1


@pytest.fixture
def offline_wbtc_weth_v2_pool(offline_adapter: ProviderAdapter) -> UniswapV2Pool:
    """Provide WBTC-WETH V2 pool using offline provider."""
    connection_manager._default_chain_id = 1
    return UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        chain_id=1,
        provider=offline_adapter,
        state_block=24945920,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
        silent=True,
    )
