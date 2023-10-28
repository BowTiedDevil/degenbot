import pytest
import web3

from degenbot import set_web3
from degenbot.exceptions import ManagerError
from degenbot.uniswap.managers import (
    UniswapV2LiquidityPoolManager,
    UniswapV3LiquidityPoolManager,
)


def test_create_managers():
    UNISWAP_V2_FACTORY_ADDRESS = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
    UNISWAP_V3_FACTORY_ADDRESS = "0x1F98431c8aD98523631AE4a59f267346ea31F984"

    SUSHISWAP_V2_FACTORY_ADDRESS = "0xC0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac"

    WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
    WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"

    UNISWAPV2_WETH_WBTC_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
    UNISWAPV3_WETH_WBTC_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"

    # Test with mainnet addresses
    w3 = web3.Web3(web3.HTTPProvider("https://rpc.ankr.com/eth"))
    set_web3(w3)

    uniswap_v2_pool_manager = UniswapV2LiquidityPoolManager(
        factory_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
    )

    uniswap_v3_pool_manager = UniswapV3LiquidityPoolManager(
        factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984"
    )

    # Get known pairs
    uniswap_v2_lp = uniswap_v2_pool_manager.get_pool(
        token_addresses=(
            WETH_ADDRESS,
            WBTC_ADDRESS,
        )
    )
    uniswap_v3_lp = uniswap_v3_pool_manager.get_pool(
        token_addresses=(
            WETH_ADDRESS,
            WBTC_ADDRESS,
        ),
        pool_fee=3000,
    )

    assert uniswap_v2_lp.address == UNISWAPV2_WETH_WBTC_ADDRESS
    assert uniswap_v3_lp.address == UNISWAPV3_WETH_WBTC_ADDRESS

    # Create one-off pool managers and verify they return the same object
    assert (
        UniswapV2LiquidityPoolManager(
            factory_address=UNISWAP_V2_FACTORY_ADDRESS
        ).get_pool(
            token_addresses=(
                WETH_ADDRESS,
                WBTC_ADDRESS,
            )
        )
        is uniswap_v2_lp
    )
    assert (
        UniswapV3LiquidityPoolManager(
            factory_address=UNISWAP_V3_FACTORY_ADDRESS
        ).get_pool(
            token_addresses=(
                WETH_ADDRESS,
                WBTC_ADDRESS,
            ),
            pool_fee=3000,
        )
        is uniswap_v3_lp
    )

    sushiswap_v2_lp = UniswapV2LiquidityPoolManager(
        factory_address=SUSHISWAP_V2_FACTORY_ADDRESS
    ).get_pool(
        token_addresses=(
            WETH_ADDRESS,
            WBTC_ADDRESS,
        )
    )

    assert uniswap_v2_lp is not sushiswap_v2_lp

    # Calling get_pool at the wrong pool manager should raise an exception
    with pytest.raises(ManagerError):
        UniswapV2LiquidityPoolManager(
            factory_address=SUSHISWAP_V2_FACTORY_ADDRESS
        ).get_pool(pool_address=uniswap_v2_lp.address)

    with pytest.raises(ManagerError):
        UniswapV2LiquidityPoolManager(
            factory_address=UNISWAP_V2_FACTORY_ADDRESS
        ).get_pool(pool_address=sushiswap_v2_lp.address)
