"""Fixtures for Uniswap V2 offline tests.

These fixtures provide offline-compatible pool objects that can be used without requiring a live
RPC connection.
"""

from pathlib import Path

import pytest

from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider import OfflineProvider, ProviderAdapter
from degenbot.provider.call_helpers import encode_function_calldata, raw_call
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from tests.constants import (
    UNISWAP_V2_FACTORY_ETH,
    UNISWAP_V2_WBTC_WETH_POOL,
    WBTC_ETH,
    WETH_ETH,
)

# Path to recorded chain data
CHAIN_DATA_PATH = Path(__file__).parent.parent.parent / "fixtures" / "chain_data"

UNISWAP_V2_FACTORY_POOL_INIT_HASH = (
    "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
)

V2_BLOCK_NUMBER = 24945920


@pytest.fixture
def offline_provider() -> OfflineProvider:
    """Provide an offline provider with recorded chain data."""
    data_file = CHAIN_DATA_PATH / "1" / f"block_{V2_BLOCK_NUMBER}.json"

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
    # Construct I/O-free tokens
    wbtc = Erc20Token(
        WBTC_ETH,
        name="Wrapped BTC",
        symbol="WBTC",
        decimals=8,
        chain_id=1,
    )
    weth = Erc20Token(
        WETH_ETH,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
        chain_id=1,
    )

    # Fetch reserves from offline provider
    (reserves0, reserves1, *_) = raw_call(
        offline_adapter,
        address=UNISWAP_V2_WBTC_WETH_POOL,
        calldata=encode_function_calldata(
            function_prototype="getReserves()",
            function_arguments=None,
        ),
        return_types=["uint112", "uint112", "uint32"],
        block_identifier=V2_BLOCK_NUMBER,
    )

    return UniswapV2Pool(
        address=UNISWAP_V2_WBTC_WETH_POOL,
        chain_id=1,
        state_block=V2_BLOCK_NUMBER,
        init_hash=UNISWAP_V2_FACTORY_POOL_INIT_HASH,
        token0=wbtc,
        token1=weth,
        factory=UNISWAP_V2_FACTORY_ETH,
        fee_token0=UniswapV2Pool.FEE,
        fee_token1=UniswapV2Pool.FEE,
        reserves_token0=reserves0,
        reserves_token1=reserves1,
    )
