import random

from degenbot.exchanges.uniswap.types import (
    UniswapFactoryDeployment,
    UniswapRouterDeployment,
    UniswapTickLensDeployment,
    UniswapV2ExchangeDeployment,
    UniswapV3ExchangeDeployment,
)
from degenbot.exchanges.uniswap.deployments import (
    FACTORY_DEPLOYMENTS,
    ROUTER_DEPLOYMENTS,
    TICKLENS_DEPLOYMENTS,
)
from degenbot.exchanges.uniswap.register import register_exchange, register_router
from degenbot.uniswap.abi import UNISWAP_V2_POOL_ABI, UNISWAP_V3_POOL_ABI
from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address


def _generate_random_address() -> ChecksumAddress:
    return to_checksum_address(random.randbytes(20))


def test_register_v2_exchange() -> None:
    DEPLOYMENT_CHAIN = 69
    FACTORY_DEPLOYMENT_ADDRESS = to_checksum_address(_generate_random_address())

    exchange = UniswapV2ExchangeDeployment(
        name="V2 DEX",
        chain_id=DEPLOYMENT_CHAIN,
        factory=UniswapFactoryDeployment(
            address=FACTORY_DEPLOYMENT_ADDRESS,
            deployer=None,
            pool_init_hash="0x0420",
            pool_abi=UNISWAP_V2_POOL_ABI,
        ),
    )

    register_exchange(exchange)
    assert DEPLOYMENT_CHAIN in FACTORY_DEPLOYMENTS
    assert FACTORY_DEPLOYMENT_ADDRESS in FACTORY_DEPLOYMENTS[DEPLOYMENT_CHAIN]
    assert FACTORY_DEPLOYMENTS[DEPLOYMENT_CHAIN][FACTORY_DEPLOYMENT_ADDRESS] is exchange.factory


def test_register_v3_exchange() -> None:
    DEPLOYMENT_CHAIN = 69
    FACTORY_DEPLOYMENT_ADDRESS = to_checksum_address(_generate_random_address())
    TICKLENS_DEPLOYMENT_ADDRESS = to_checksum_address(_generate_random_address())

    exchange = UniswapV3ExchangeDeployment(
        name="V3 DEX",
        chain_id=DEPLOYMENT_CHAIN,
        factory=UniswapFactoryDeployment(
            address=FACTORY_DEPLOYMENT_ADDRESS,
            deployer=None,
            pool_init_hash="0x0420",
            pool_abi=UNISWAP_V3_POOL_ABI,
        ),
        tick_lens=UniswapTickLensDeployment(
            address=TICKLENS_DEPLOYMENT_ADDRESS,
            abi=[],
        ),
    )

    register_exchange(exchange)
    assert DEPLOYMENT_CHAIN in FACTORY_DEPLOYMENTS
    assert FACTORY_DEPLOYMENT_ADDRESS in FACTORY_DEPLOYMENTS[DEPLOYMENT_CHAIN]
    assert FACTORY_DEPLOYMENTS[DEPLOYMENT_CHAIN][FACTORY_DEPLOYMENT_ADDRESS] is exchange.factory
    assert TICKLENS_DEPLOYMENTS[DEPLOYMENT_CHAIN][FACTORY_DEPLOYMENT_ADDRESS] is exchange.tick_lens


def test_register_router() -> None:
    DEPLOYMENT_CHAIN = 69
    FACTORY_DEPLOYMENT_ADDRESS = to_checksum_address(_generate_random_address())
    TICKLENS_DEPLOYMENT_ADDRESS = to_checksum_address(_generate_random_address())

    exchange = UniswapV3ExchangeDeployment(
        name="V3 DEX",
        chain_id=DEPLOYMENT_CHAIN,
        factory=UniswapFactoryDeployment(
            address=FACTORY_DEPLOYMENT_ADDRESS,
            deployer=None,
            pool_init_hash="0x0420",
            pool_abi=UNISWAP_V3_POOL_ABI,
        ),
        tick_lens=UniswapTickLensDeployment(
            address=TICKLENS_DEPLOYMENT_ADDRESS,
            abi=[],
        ),
    )

    ROUTER_DEPLOYMENT_ADDRESS = to_checksum_address(_generate_random_address())

    router = UniswapRouterDeployment(
        address=ROUTER_DEPLOYMENT_ADDRESS,
        chain_id=DEPLOYMENT_CHAIN,
        name="Router",
        exchanges=[exchange],
    )

    register_router(router)
    assert DEPLOYMENT_CHAIN in ROUTER_DEPLOYMENTS
    assert router.address in ROUTER_DEPLOYMENTS[DEPLOYMENT_CHAIN]
    assert ROUTER_DEPLOYMENTS[DEPLOYMENT_CHAIN][router.address] is router
