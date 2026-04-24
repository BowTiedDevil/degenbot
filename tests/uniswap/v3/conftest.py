"""
Fixtures for Uniswap V3 offline tests.

These fixtures provide offline-compatible V3 pool objects with complete tick data.
"""

from pathlib import Path

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider import OfflineProvider, ProviderAdapter
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

# Path to recorded chain data
CHAIN_DATA_PATH = Path(__file__).parent.parent.parent / "fixtures" / "chain_data"

# WBTC-WETH V3 pool (block 24947230 has complete tick data recorded)
UNISWAP_V3_WBTC_WETH_POOL = get_checksum_address("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD")
UNISWAP_V3_WBTC_WETH_TICK_SPACING = 60
UNISWAP_V3_WBTC_WETH_BLOCK = 24947230

# Token addresses
WBTC_CONTRACT_ADDRESS = get_checksum_address("0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599")
WETH_CONTRACT_ADDRESS = get_checksum_address("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2")


def load_v3_liquidity_data(data_file: Path, pool_address: str) -> dict | None:
    """Load V3 liquidity data (tick_bitmap, tick_data) from recorded single-block data file."""
    if not data_file.exists():
        return None

    with Path(data_file).open(encoding="utf-8") as f:
        import json

        data = json.load(f)

    # New flattened format: keys are at top level with pool-specific prefixes
    pool_key = f"v3_{pool_address.lower()}"
    tick_spacing_key = f"{pool_key}_tick_spacing"
    tick_bitmap_key = f"{pool_key}_tick_bitmap"
    tick_data_key = f"{pool_key}_tick_data"

    if tick_bitmap_key not in data:
        return None

    return {
        "tick_spacing": data.get(tick_spacing_key),
        "tick_bitmap": data.get(tick_bitmap_key, {}),
        "tick_data": data.get(tick_data_key, {}),
    }


@pytest.fixture
def offline_provider() -> OfflineProvider:
    """Provide an offline provider with recorded chain data."""
    data_file = CHAIN_DATA_PATH / "1" / f"block_{UNISWAP_V3_WBTC_WETH_BLOCK}.json"
    if not data_file.exists():
        pytest.skip(f"Offline data file not found: {data_file}")

    return OfflineProvider.from_json_file(data_file)


@pytest.fixture
def offline_adapter(offline_provider: OfflineProvider) -> ProviderAdapter:
    """Provide a ProviderAdapter wrapping the offline provider."""
    return ProviderAdapter.from_offline(offline_provider)


@pytest.fixture
def offline_wbtc_weth_v3_pool(offline_adapter: ProviderAdapter) -> UniswapV3Pool:
    """
    Provide WBTC-WETH V3 pool using offline provider with complete tick data.
    """
    connection_manager._default_chain_id = 1

    # Load tick data from recorded file
    data_file = CHAIN_DATA_PATH / "1" / f"block_{UNISWAP_V3_WBTC_WETH_BLOCK}.json"
    liquidity_data = load_v3_liquidity_data(data_file, UNISWAP_V3_WBTC_WETH_POOL)

    if not liquidity_data:
        pytest.skip("V3 liquidity data not found in recorded file")

    tick_bitmap = liquidity_data.get("tick_bitmap", {})
    tick_data = liquidity_data.get("tick_data", {})

    # Convert string keys back to integers
    tick_bitmap_int = {
        int(k): {"bitmap": int(v["bitmap"]), "block": v["block"]} for k, v in tick_bitmap.items()
    }
    tick_data_int = {
        int(k): {
            "liquidity_gross": int(v["liquidity_gross"]),
            "liquidity_net": int(v["liquidity_net"]),
            "block": v["block"],
        }
        for k, v in tick_data.items()
    }

    # Import the types needed for tick data
    from degenbot.uniswap.v3_types import UniswapV3BitmapAtWord, UniswapV3LiquidityAtTick

    # Convert to the format expected by UniswapV3Pool
    tick_bitmap_for_pool = {
        k: UniswapV3BitmapAtWord(bitmap=v["bitmap"], block=v["block"])
        for k, v in tick_bitmap_int.items()
    }
    tick_data_for_pool = {
        k: UniswapV3LiquidityAtTick(
            liquidity_gross=v["liquidity_gross"],
            liquidity_net=v["liquidity_net"],
            block=v["block"],
        )
        for k, v in tick_data_int.items()
    }

    return UniswapV3Pool(
        address=UNISWAP_V3_WBTC_WETH_POOL,
        chain_id=1,
        provider=offline_adapter,
        state_block=UNISWAP_V3_WBTC_WETH_BLOCK,
        tick_bitmap=tick_bitmap_for_pool,
        tick_data=tick_data_for_pool,
        silent=True,
    )


@pytest.fixture
def offline_wbtc(offline_wbtc_weth_v3_pool: UniswapV3Pool) -> Erc20Token:
    """Get WBTC token from the offline V3 pool."""
    return offline_wbtc_weth_v3_pool.token0


@pytest.fixture
def offline_weth(offline_wbtc_weth_v3_pool: UniswapV3Pool) -> Erc20Token:
    """Get WETH token from the offline V3 pool."""
    return offline_wbtc_weth_v3_pool.token1
