import dataclasses

from degenbot.types.abstract import AbstractPoolState, AbstractSimulationResult
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PoolStateMessage


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapSimulationResult(AbstractSimulationResult):
    """
    Common attributes for Uniswap V2 & V3 simulations
    """

    amount0_delta: int
    amount1_delta: int
    initial_state: AbstractPoolState
    final_state: AbstractPoolState


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class UniswapV2PoolState(AbstractPoolState):
    reserves_token0: int
    reserves_token1: int


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolSimulationResult(UniswapSimulationResult):
    initial_state: UniswapV2PoolState
    final_state: UniswapV2PoolState


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolExternalUpdate:
    block_number: BlockNumber
    reserves_token0: int
    reserves_token1: int


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolStateUpdated(PoolStateMessage):
    state: UniswapV2PoolState
