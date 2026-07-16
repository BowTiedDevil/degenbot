"""Aerodrome-specific data types and state definitions."""

from dataclasses import dataclass

from degenbot.types.abstract import AbstractPoolState
from degenbot.types.concrete import PoolStateMessage
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.uniswap.v3_types import UniswapV3PoolState


class AerodromeV2PoolExternalUpdate(UniswapV2PoolExternalUpdate):
    """AerodromeV2PoolExternalUpdate class."""


@dataclass(slots=True, frozen=True)
class AerodromeV2PoolState(AbstractPoolState):
    """AerodromeV2PoolState class."""

    reserves_token0: int
    reserves_token1: int


@dataclass(slots=True, frozen=True)
class AerodromeV2PoolStateUpdated(PoolStateMessage):
    """AerodromeV2PoolStateUpdated class."""

    state: AerodromeV2PoolState


class AerodromeV3PoolState(UniswapV3PoolState):
    """AerodromeV3PoolState class."""


@dataclass(slots=True, frozen=True)
class AerodromeV3PoolStateUpdated(PoolStateMessage):
    """AerodromeV3PoolStateUpdated class."""

    state: AerodromeV3PoolState
