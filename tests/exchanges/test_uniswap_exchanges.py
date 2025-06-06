import random

import pytest
from eth_typing import ChecksumAddress

from degenbot.cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.uniswap.deployments import (
    FACTORY_DEPLOYMENTS,
    ROUTER_DEPLOYMENTS,
    UniswapFactoryDeployment,
    UniswapRouterDeployment,
    UniswapV2ExchangeDeployment,
    UniswapV3ExchangeDeployment,
    register_exchange,
    register_router,
)


def _generate_random_address() -> ChecksumAddress:
    return get_checksum_address(random.randbytes(20))


def test_register_v2_exchange() -> None:
    deployment_chain = 69
    factory_deployment_address = get_checksum_address(_generate_random_address())

    exchange = UniswapV2ExchangeDeployment(
        name="V2 DEX",
        chain_id=deployment_chain,
        factory=UniswapFactoryDeployment(
            address=factory_deployment_address,
            deployer=None,
            pool_init_hash="0x0420",
        ),
    )

    register_exchange(exchange)
    with pytest.raises(DegenbotValueError):
        register_exchange(exchange)
    assert deployment_chain in FACTORY_DEPLOYMENTS
    assert factory_deployment_address in FACTORY_DEPLOYMENTS[deployment_chain]
    assert FACTORY_DEPLOYMENTS[deployment_chain][factory_deployment_address] is exchange.factory

    del FACTORY_DEPLOYMENTS[deployment_chain][factory_deployment_address]
    del FACTORY_DEPLOYMENTS[deployment_chain]


def test_register_v3_exchange() -> None:
    deployment_chain = 69
    factory_deployment_address = get_checksum_address(_generate_random_address())

    exchange = UniswapV3ExchangeDeployment(
        name="V3 DEX",
        chain_id=deployment_chain,
        factory=UniswapFactoryDeployment(
            address=factory_deployment_address,
            deployer=None,
            pool_init_hash="0x0420",
        ),
    )

    register_exchange(exchange)
    with pytest.raises(DegenbotValueError):
        register_exchange(exchange)

    assert deployment_chain in FACTORY_DEPLOYMENTS
    assert factory_deployment_address in FACTORY_DEPLOYMENTS[deployment_chain]
    assert FACTORY_DEPLOYMENTS[deployment_chain][factory_deployment_address] is exchange.factory

    del FACTORY_DEPLOYMENTS[deployment_chain][factory_deployment_address]
    del FACTORY_DEPLOYMENTS[deployment_chain]


def test_register_router() -> None:
    deployment_chain = 69
    factory_deployment_address = get_checksum_address(_generate_random_address())

    exchange = UniswapV3ExchangeDeployment(
        name="V3 DEX",
        chain_id=deployment_chain,
        factory=UniswapFactoryDeployment(
            address=factory_deployment_address,
            deployer=None,
            pool_init_hash="0x0420",
        ),
    )

    router_deployment_address = get_checksum_address(_generate_random_address())

    router = UniswapRouterDeployment(
        address=router_deployment_address,
        chain_id=deployment_chain,
        name="Router",
        exchanges=[exchange],
    )

    register_router(router)
    with pytest.raises(DegenbotValueError):
        register_router(router)

    assert deployment_chain in ROUTER_DEPLOYMENTS
    assert router.address in ROUTER_DEPLOYMENTS[deployment_chain]
    assert ROUTER_DEPLOYMENTS[deployment_chain][router.address] is router

    del ROUTER_DEPLOYMENTS[deployment_chain][router.address]
    del ROUTER_DEPLOYMENTS[deployment_chain]
