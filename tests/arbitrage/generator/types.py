"""
Configuration types for synthetic pool state generation.
"""

from dataclasses import dataclass
from fractions import Fraction
from typing import Literal


@dataclass(frozen=True, slots=True)
class PoolGenerationConfig:
    """
    Base configuration for generating a synthetic pool state.

    Attributes
    ----------
    fee : Fraction
        The pool fee as a fraction (e.g., Fraction(3, 1000) for 0.3%).
    token0_decimals : int
        Decimals for token0 (default: 18).
    token1_decimals : int
        Decimals for token1 (default: 18).
    seed : int | None
        Random seed for deterministic generation.
    """

    fee: Fraction
    token0_decimals: int = 18
    token1_decimals: int = 18
    seed: int | None = None


@dataclass(frozen=True, slots=True)
class V3PoolGenerationConfig(PoolGenerationConfig):
    """
    Configuration for generating a Uniswap V3 pool state.

    Extends the base config with V3-specific parameters for tick spacing,
    liquidity distribution, and tick range.

    Attributes
    ----------
    tick_spacing : int
        The tick spacing for the pool (determines fee tier).
    liquidity_depth : int
        Total liquidity to distribute across tick range.
    tick_range : tuple[int, int] | None
        (tick_lower, tick_upper) range for liquidity distribution.
        If None, liquidity is concentrated at current tick.
    """

    tick_spacing: int = 60
    liquidity_depth: int = 10**18
    tick_range: tuple[int, int] | None = None

    def __post_init__(self) -> None:
        if self.tick_range is not None:
            tick_lower, tick_upper = self.tick_range
            msg: str
            if tick_lower >= tick_upper:
                msg = "tick_lower must be less than tick_upper"
                raise ValueError(msg)
            if tick_lower % self.tick_spacing != 0:
                msg = "tick_lower must be a multiple of tick_spacing"
                raise ValueError(msg)
            if tick_upper % self.tick_spacing != 0:
                msg = "tick_upper must be a multiple of tick_spacing"
                raise ValueError(msg)


@dataclass(frozen=True, slots=True)
class V4PoolGenerationConfig(V3PoolGenerationConfig):
    """
    Configuration for generating a Uniswap V4 pool state.

    Extends V3 config with V4-specific pool identification.

    Attributes
    ----------
    hooks_address : str
        The hooks contract address (default: zero address for no hooks).
    """

    hooks_address: str = "0x0000000000000000000000000000000000000000"


@dataclass(frozen=True, slots=True)
class PriceDiscrepancyConfig:
    """
    Configuration for injecting price discrepancy between pools.

    Used to create guaranteed arbitrage opportunities by setting up
    pools with different effective exchange rates.

    Attributes
    ----------
    price_ratio : float
        Target price ratio between pools. Value > 1.0 means pool B
        has a worse price for token0 (cheaper to buy token1).
        e.g., 1.02 = 2% price difference.
    direction : Literal["token0_to_token1", "token1_to_token0"]
        Which direction the price ratio applies.
    min_profit_wei : int
        Minimum profit threshold to ensure (default: 0).
    """

    price_ratio: float
    direction: Literal["token0_to_token1", "token1_to_token0"] = "token0_to_token1"
    min_profit_wei: int = 0

    def __post_init__(self) -> None:
        msg: str
        if self.price_ratio <= 0:
            msg = "price_ratio must be positive"
            raise ValueError(msg)
        if abs(self.price_ratio - 1.0) < 1e-9:
            msg = "price_ratio of 1.0 creates no arbitrage opportunity"
            raise ValueError(msg)


@dataclass(frozen=True, slots=True)
class ArbitrageFixtureConfig:
    """
    Configuration for generating a complete arbitrage test fixture.

    Defines the scenario including pool types, profit target, and
    randomization parameters.

    Attributes
    ----------
    cycle_type : Literal["v2_v2", "v2_v3", "v3_v3", "v2_v4", "v3_v4", "v4_v4"]
        The type of pools in the arbitrage cycle.
    profit_target_wei : int
        Target profit amount in wei.
    liquidity_depth : Literal["shallow", "medium", "deep"]
        Relative liquidity depth for generated pools.
        - shallow: ~1-10 ETH equivalent
        - medium: ~100-1000 ETH equivalent
        - deep: ~10000+ ETH equivalent
    seed : int
        Random seed for deterministic generation.
    """

    cycle_type: Literal["v2_v2", "v2_v3", "v3_v3", "v2_v4", "v3_v4", "v4_v4"]
    profit_target_wei: int
    liquidity_depth: Literal["shallow", "medium", "deep"] = "medium"
    seed: int = 0

    def __post_init__(self) -> None:
        if self.profit_target_wei < 0:
            msg = "profit_target_wei must be non-negative"
            raise ValueError(msg)


# Fee tier constants for convenience
FEE_TIER_0_01_PERCENT = Fraction(1, 10000)  # 0.01%
FEE_TIER_0_05_PERCENT = Fraction(5, 10000)  # 0.05%
FEE_TIER_0_30_PERCENT = Fraction(3, 1000)  # 0.3%
FEE_TIER_1_PERCENT = Fraction(1, 100)  # 1%

# Standard tick spacings for V3/V4 fee tiers
TICK_SPACING_0_01_PERCENT = 1
TICK_SPACING_0_05_PERCENT = 10
TICK_SPACING_0_30_PERCENT = 60
TICK_SPACING_1_PERCENT = 200

# Liquidity depth multipliers
LIQUIDITY_MULTIPLIERS: dict[Literal["shallow", "medium", "deep"], int] = {
    "shallow": 10**18,  # ~1 ETH
    "medium": 10**21,  # ~1000 ETH
    "deep": 10**24,  # ~1000000 ETH
}
