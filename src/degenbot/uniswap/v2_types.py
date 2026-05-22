"""Uniswap V2-specific data types and state definitions."""
import dataclasses

from degenbot.types.abstract import AbstractPoolState, AbstractSimulationResult
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PoolStateMessage


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapSimulationResult(AbstractSimulationResult):
    """Common attributes for Uniswap V2 & V3 simulations."""


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class UniswapV2PoolState(AbstractPoolState):
    """UniswapV2PoolState class."""

    reserves_token0: int
    reserves_token1: int


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolSimulationResult(UniswapSimulationResult):
    """UniswapV2PoolSimulationResult class."""

    initial_state: UniswapV2PoolState
    final_state: UniswapV2PoolState


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolExternalUpdate:
    """UniswapV2PoolExternalUpdate class."""

    block_number: BlockNumber
    reserves_token0: int
    reserves_token1: int


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolStateUpdated(PoolStateMessage):
    """UniswapV2PoolStateUpdated class."""

    state: UniswapV2PoolState
