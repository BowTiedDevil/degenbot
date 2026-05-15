import dataclasses

from .arbitrage import AbstractArbitrage
from .deployment import AbstractExchangeDeployment
from .erc20_token import AbstractErc20Token
from .liquidity_pool import AbstractLiquidityPool
from .pool_manager import AbstractPoolManager
from .pool_state import AbstractPoolState


@dataclasses.dataclass(slots=True, frozen=True)
class AbstractSimulationResult:
    amount0_delta: int
    amount1_delta: int
    initial_state: AbstractPoolState
    final_state: AbstractPoolState


class AbstractRegistry: ...


__all__ = (
    "AbstractArbitrage",
    "AbstractErc20Token",
    "AbstractExchangeDeployment",
    "AbstractLiquidityPool",
    "AbstractPoolManager",
    "AbstractPoolState",
    "AbstractRegistry",
    "AbstractSimulationResult",
)
