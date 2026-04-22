from .arbitrage import AbstractArbitrage
from .deployment import AbstractExchangeDeployment
from .erc20_token import AbstractErc20Token
from .liquidity_pool import (
    AbstractAerodromeV2Pool,
    AbstractConcentratedLiquidityPool,
    AbstractLiquidityPool,
    AbstractUniswapV2Pool,
)
from .pool_manager import AbstractPoolManager
from .pool_state import AbstractPoolState


class AbstractSimulationResult: ...


class AbstractPoolUpdate: ...


class AbstractManager:
    """
    Base class for managers that generate, track and distribute various helper classes
    """


class AbstractRegistry: ...


class AbstractTransaction: ...


__all__ = (
    "AbstractAerodromeV2Pool",
    "AbstractArbitrage",
    "AbstractConcentratedLiquidityPool",
    "AbstractErc20Token",
    "AbstractExchangeDeployment",
    "AbstractLiquidityPool",
    "AbstractManager",
    "AbstractPoolManager",
    "AbstractPoolState",
    "AbstractPoolUpdate",
    "AbstractRegistry",
    "AbstractSimulationResult",
    "AbstractTransaction",
    "AbstractUniswapV2Pool",
)
