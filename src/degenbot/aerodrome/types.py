"""Aerodrome-specific data types and state definitions."""

from dataclasses import dataclass

from degenbot.types.abstract import AbstractPoolState
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate
from degenbot.uniswap.v3_types import UniswapV3PoolState


class AerodromeV2PoolExternalUpdate(UniswapV2PoolExternalUpdate):
    """AerodromeV2PoolExternalUpdate class."""


@dataclass(slots=True, frozen=True)
class AerodromeV2PoolState(AbstractPoolState):
    """AerodromeV2PoolState class."""

    reserves_token0: int
    reserves_token1: int


class AerodromeV3PoolState(UniswapV3PoolState):
    """AerodromeV3PoolState class."""
