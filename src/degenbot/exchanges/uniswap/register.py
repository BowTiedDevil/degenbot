from .deployments import TICKLENS_DEPLOYMENTS, FACTORY_DEPLOYMENTS, ROUTER_DEPLOYMENTS
from .dataclasses import (
    UniswapV2ExchangeDeployment,
    UniswapV3ExchangeDeployment,
    UniswapRouterDeployment,
)


def register_exchange(exchange: UniswapV2ExchangeDeployment | UniswapV3ExchangeDeployment) -> None:
    if exchange.chain_id not in TICKLENS_DEPLOYMENTS:
        TICKLENS_DEPLOYMENTS[exchange.chain_id] = {}

    if exchange.chain_id not in FACTORY_DEPLOYMENTS:
        FACTORY_DEPLOYMENTS[exchange.chain_id] = {}

    if any(
        [
            exchange.factory.address in TICKLENS_DEPLOYMENTS[exchange.chain_id],
            exchange.factory.address in FACTORY_DEPLOYMENTS[exchange.chain_id],
        ]
    ):
        raise ValueError("Exchange is already registered.")

    FACTORY_DEPLOYMENTS[exchange.chain_id][exchange.factory.address] = exchange.factory

    if isinstance(exchange, UniswapV3ExchangeDeployment):
        TICKLENS_DEPLOYMENTS[exchange.chain_id][exchange.factory.address] = exchange.tick_lens


def register_router(router: UniswapRouterDeployment) -> None:
    if router.chain_id not in ROUTER_DEPLOYMENTS:
        ROUTER_DEPLOYMENTS[router.chain_id] = {}

    if router.address in ROUTER_DEPLOYMENTS[router.chain_id]:
        raise ValueError("Router is already registered.")

    ROUTER_DEPLOYMENTS[router.chain_id][router.address] = router
