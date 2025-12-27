import random

import pytest
from eth_typing import ChecksumAddress

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions import DegenbotValueError
from degenbot.uniswap.deployments import (
    FACTORY_DEPLOYMENTS,
    UniswapFactoryDeployment,
    UniswapV2ExchangeDeployment,
    UniswapV3ExchangeDeployment,
    register_exchange,
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
