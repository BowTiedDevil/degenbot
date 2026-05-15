import dataclasses

from .deployment import AbstractExchangeDeployment
from .erc20_token import AbstractErc20Token
from .liquidity_pool import AbstractLiquidityPool
from .pool_state import AbstractPoolState
from .pool_tracker import AbstractPoolTracker


@dataclasses.dataclass(slots=True, frozen=True)
class AbstractSimulationResult:
    amount0_delta: int
    amount1_delta: int
    initial_state: AbstractPoolState
    final_state: AbstractPoolState


class AbstractRegistry: ...


__all__ = (
    "AbstractErc20Token",
    "AbstractExchangeDeployment",
    "AbstractLiquidityPool",
    "AbstractPoolState",
    "AbstractPoolTracker",
    "AbstractRegistry",
    "AbstractSimulationResult",
)
