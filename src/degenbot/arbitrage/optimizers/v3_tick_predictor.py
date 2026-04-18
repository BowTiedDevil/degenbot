"""
V3 Tick Crossing Predictor and Optimizer.

Key insight: V3 pools iterate through tick ranges during swaps. If we can predict
whether a swap will cross tick boundaries, we can:
1. Use closed-form bounded product CFMM for single-range swaps (O(1))
2. Fall back to Brent for multi-range swaps

This module provides:
1. Tick crossing prediction based on price impact estimation
2. Optimal tick range identification for V2-V3 arbitrage
3. Efficient V3 pool analysis for routing
"""

import math
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool


# =============================================================================
# TICK MATH CONSTANTS
# =============================================================================

MIN_TICK = -887272
MAX_TICK = 887272
Q96 = 2**96


# =============================================================================
# TICK CROSSING PREDICTION
# =============================================================================


@dataclass(frozen=True)
class TickRange:
    """A tick range with its liquidity."""

    tick_lower: int
    tick_upper: int
    liquidity: int
    sqrt_price_lower: float  # sqrt(1.0001^tick_lower)
    sqrt_price_upper: float  # sqrt(1.0001^tick_upper)


@dataclass(frozen=True)
class TickCrossingPrediction:
    """Result of tick crossing prediction."""

    will_cross: bool
    estimated_ticks_crossed: int
    estimated_final_tick: int
    estimated_final_sqrt_price: float
    current_tick: int
    current_sqrt_price: float
    confidence: float  # 0-1, how confident in prediction


def tick_to_sqrt_price(tick: int) -> float:
    """
    Convert tick to sqrt price.

    sqrt_price = sqrt(1.0001^tick) = 1.0001^(tick/2)
    """
    return 1.0001 ** (tick / 2)


def sqrt_price_to_tick(sqrt_price: float) -> int:
    """
    Convert sqrt price to tick.

    tick = 2 * log_{1.0001}(sqrt_price) = 2 * ln(sqrt_price) / ln(1.0001)
    """
    if sqrt_price <= 0:
        return MIN_TICK
    return int(2 * math.log(sqrt_price) / math.log(1.0001))


def estimate_price_impact(
    amount_in: float,
    liquidity: float,
    current_sqrt_price: float,
    fee: float = 0.003,
    zero_for_one: bool = True,
) -> float:
    """
    Estimate price impact from a swap.

    Uses V3 price impact approximation:
    Δ(sqrt_price) ≈ amount_in * (1 - fee) / (2 * L)

    For token0 → token1 (zero_for_one = True):
    - sqrt_price decreases (price of token1 in terms of token0 goes down)

    For token1 → token0 (zero_for_one = False):
    - sqrt_price increases (price of token1 in terms of token0 goes up)

    Parameters
    ----------
    amount_in : float
        Input amount.
    liquidity : float
        Current pool liquidity.
    current_sqrt_price : float
        Current sqrt price (token1/token0).
    fee : float
        Fee rate (e.g., 0.003 for 0.3%).
    zero_for_one : bool
        True if swapping token0 for token1.

    Returns
    -------
    float
        Estimated new sqrt price.
    """
    if liquidity <= 0:
        return current_sqrt_price

    gamma = 1.0 - fee

    # Approximation: Δsqrt_price ≈ amount_in / (2 * L * sqrt_price)
    # This is derived from: amount_out = L * (sqrt_price_new - sqrt_price_old)
    # and the constraint that L = sqrt(R0 * R1)

    # More accurate: use the V3 swap formula
    # For zero_for_one: sqrt_price_new = sqrt_price * L / (L + amount_in * gamma * sqrt_price)
    # For one_for_zero: sqrt_price_new = sqrt_price + amount_in * gamma / L

    if zero_for_one:
        # Token0 in, token1 out
        # sqrt_price decreases
        # New sqrt_price = sqrt_price * L / (L + amount_in * gamma)
        denom = liquidity + amount_in * gamma
        if denom <= 0:
            return current_sqrt_price
        new_sqrt_price = current_sqrt_price * liquidity / denom
    else:
        # Token1 in, token0 out
        # sqrt_price increases
        # New sqrt_price = sqrt_price + amount_in * gamma / L
        new_sqrt_price = current_sqrt_price + amount_in * gamma / liquidity

    return new_sqrt_price


def predict_tick_crossing(
    amount_in: float,
    liquidity: float,
    current_sqrt_price: float,
    current_tick: int,
    tick_spacing: int,
    fee: float = 0.003,
    zero_for_one: bool = True,
    tick_data: dict[int, Any] | None = None,
) -> TickCrossingPrediction:
    """
    Predict whether a swap will cross tick boundaries.

    Parameters
    ----------
    amount_in : float
        Input amount.
    liquidity : float
        Current pool liquidity.
    current_sqrt_price : float
        Current sqrt price.
    current_tick : int
        Current tick index.
    tick_spacing : int
        Tick spacing for the pool.
    fee : float
        Fee rate.
    zero_for_one : bool
        True if swapping token0 for token1.
    tick_data : dict | None
        Tick data for finding initialized ticks.

    Returns
    -------
    TickCrossingPrediction
        Prediction result.
    """
    # Estimate final sqrt price
    estimated_sqrt_price = estimate_price_impact(
        amount_in=amount_in,
        liquidity=liquidity,
        current_sqrt_price=current_sqrt_price,
        fee=fee,
        zero_for_one=zero_for_one,
    )

    # Convert to tick
    estimated_tick = sqrt_price_to_tick(estimated_sqrt_price)

    # Calculate tick boundaries for current range
    current_range_lower = (current_tick // tick_spacing) * tick_spacing
    current_range_upper = current_range_lower + tick_spacing

    # Check if we stay in current range
    will_cross = not (current_range_lower <= estimated_tick < current_range_upper)

    # Count estimated ticks crossed (simplified)
    if will_cross:
        if zero_for_one:
            # Price decreases, moving to lower ticks
            estimated_ticks_crossed = max(0, current_tick - estimated_tick) // tick_spacing
        else:
            # Price increases, moving to upper ticks
            estimated_ticks_crossed = max(0, estimated_tick - current_tick) // tick_spacing
    else:
        estimated_ticks_crossed = 0

    # Confidence is high when we stay in range, decreases with more crossings
    confidence = 1.0 / (1.0 + estimated_ticks_crossed * 0.2)

    return TickCrossingPrediction(
        will_cross=will_cross,
        estimated_ticks_crossed=estimated_ticks_crossed,
        estimated_final_tick=estimated_tick,
        estimated_final_sqrt_price=estimated_sqrt_price,
        current_tick=current_tick,
        current_sqrt_price=current_sqrt_price,
        confidence=confidence,
    )


# =============================================================================
# V3 POOL ANALYSIS
# =============================================================================


@dataclass
class V3PoolState:
    """Extracted V3 pool state for optimization."""

    sqrt_price_x96: int
    sqrt_price: float  # As float for optimization
    tick: int
    liquidity: int
    fee: float
    tick_spacing: int

    # Token info
    token0_address: str
    token1_address: str
    token0_decimals: int
    token1_decimals: int

    # Virtual reserves (derived from L and sqrt_price)
    virtual_reserve0: float
    virtual_reserve1: float


def extract_v3_pool_state(pool: "UniswapV3Pool") -> V3PoolState:
    """
    Extract V3 pool state for optimization.

    Parameters
    ----------
    pool : UniswapV3Pool
        The V3 pool.

    Returns
    -------
    V3PoolState
        Extracted state.
    """
    state = pool.state

    sqrt_price_x96 = state.sqrt_price_x96
    sqrt_price = sqrt_price_x96 / Q96
    tick = state.tick
    liquidity = state.liquidity
    fee = float(pool.fee)
    tick_spacing = pool.tick_spacing

    # Virtual reserves from liquidity and sqrt_price
    # R0 = L / sqrt_price
    # R1 = L * sqrt_price
    virtual_reserve0 = liquidity / sqrt_price
    virtual_reserve1 = liquidity * sqrt_price

    return V3PoolState(
        sqrt_price_x96=sqrt_price_x96,
        sqrt_price=sqrt_price,
        tick=tick,
        liquidity=liquidity,
        fee=fee,
        tick_spacing=tick_spacing,
        token0_address=pool.token0.address,
        token1_address=pool.token1.address,
        token0_decimals=pool.token0.decimals,
        token1_decimals=pool.token1.decimals,
        virtual_reserve0=virtual_reserve0,
        virtual_reserve1=virtual_reserve1,
    )


# =============================================================================
# TICK RANGE FINDER
# =============================================================================


def find_tick_range_at_price(
    tick_ranges: list[TickRange],
    sqrt_price: float,
) -> TickRange | None:
    """
    Find the tick range containing a given sqrt price.

    Parameters
    ----------
    tick_ranges : list[TickRange]
        List of tick ranges (sorted by tick_lower).
    sqrt_price : float
        Target sqrt price.

    Returns
    -------
    TickRange | None
        The containing range, or None if not found.
    """
    for tick_range in tick_ranges:
        if tick_range.sqrt_price_lower <= sqrt_price <= tick_range.sqrt_price_upper:
            return tick_range
    return None


def get_nearest_tick_ranges(
    tick_ranges: list[TickRange],
    sqrt_price: float,
    n: int = 3,
) -> list[TickRange]:
    """
    Get the N nearest tick ranges to a given sqrt price.

    Useful for checking multiple candidate ranges in V2-V3 arbitrage.

    Parameters
    ----------
    tick_ranges : list[TickRange]
        List of tick ranges.
    sqrt_price : float
        Target sqrt price.
    n : int
        Number of ranges to return.

    Returns
    -------
    list[TickRange]
        N nearest ranges.
    """
    if not tick_ranges:
        return []

    # Calculate distance to each range
    distances = []
    for tick_range in tick_ranges:
        if sqrt_price < tick_range.sqrt_price_lower:
            distance = tick_range.sqrt_price_lower - sqrt_price
        elif sqrt_price > tick_range.sqrt_price_upper:
            distance = sqrt_price - tick_range.sqrt_price_upper
        else:
            distance = 0.0  # Inside range
        distances.append((distance, tick_range))

    # Sort by distance and return top N
    distances.sort(key=lambda x: x[0])
    return [tick_range for _, tick_range in distances[:n]]


# =============================================================================
# BOUNDED PRODUCT CFMM (ENHANCED)
# =============================================================================


@dataclass
class BoundedProductCFMM:
    """
    Bounded product CFMM representation of a V3 tick range.

    Trading function: φ(R) = (R₀ + α)(R₁ + β) ≥ L²

    where:
    - α = L / sqrt(P_upper) is the lower bound on R₀
    - β = L * sqrt(P_lower) is the lower bound on R₁

    At optimum, marginal price = external price:
    - R₁_opt + β = L × sqrt(P_external)
    - R₀_opt + α = L / sqrt(P_external)
    """

    tick_lower: int
    tick_upper: int
    liquidity: float
    sqrt_price_lower: float
    sqrt_price_upper: float

    @property
    def alpha(self) -> float:
        """Lower bound on R0: L / sqrt(P_upper)."""
        return self.liquidity / self.sqrt_price_upper

    @property
    def beta(self) -> float:
        """Lower bound on R1: L * sqrt(P_lower)."""
        return self.liquidity * self.sqrt_price_lower

    @property
    def k(self) -> float:
        """Effective constant product: L²."""
        return self.liquidity**2

    def contains_sqrt_price(self, sqrt_price: float) -> bool:
        """Check if sqrt price is within this range."""
        return self.sqrt_price_lower <= sqrt_price <= self.sqrt_price_upper

    def optimal_reserves_at_price(
        self,
        external_price: float,
    ) -> tuple[float, float]:
        """
        Find optimal reserves given external price.

        Parameters
        ----------
        external_price : float
            External market price (token1/token0).

        Returns
        -------
        tuple[float, float]
            (optimal_R0, optimal_R1)
        """
        sqrt_p_ext = math.sqrt(external_price)

        # Closed-form solution
        R1_opt = self.liquidity * sqrt_p_ext - self.beta
        R0_opt = self.liquidity / sqrt_p_ext - self.alpha

        return max(R0_opt, 0.0), max(R1_opt, 0.0)

    def optimal_swap_at_price(
        self,
        external_price: float,
        current_sqrt_price: float,
        zero_for_one: bool = True,
    ) -> tuple[float, float]:
        """
        Find optimal swap amounts given external price.

        Parameters
        ----------
        external_price : float
            External market price.
        current_sqrt_price : float
            Current pool sqrt price.
        zero_for_one : bool
            True if swapping token0 for token1.

        Returns
        -------
        tuple[float, float]
            (amount_in, amount_out)
        """
        R0_opt, R1_opt = self.optimal_reserves_at_price(external_price)

        # Current reserves at current sqrt price
        R0_current = self.liquidity / current_sqrt_price
        R1_current = self.liquidity * current_sqrt_price

        if zero_for_one:
            # Token0 in, token1 out
            amount_in = max(0.0, R0_opt - R0_current)
            amount_out = max(0.0, R1_current - R1_opt)
        else:
            # Token1 in, token0 out
            amount_in = max(0.0, R1_opt - R1_current)
            amount_out = max(0.0, R0_current - R0_opt)

        return amount_in, amount_out


def tick_range_to_bounded_product(
    tick_lower: int,
    tick_upper: int,
    liquidity: float,
) -> BoundedProductCFMM:
    """
    Convert tick range to bounded product CFMM.

    Parameters
    ----------
    tick_lower : int
        Lower tick boundary.
    tick_upper : int
        Upper tick boundary.
    liquidity : float
        Liquidity in the range.

    Returns
    -------
    BoundedProductCFMM
        Bounded product CFMM representation.
    """
    sqrt_price_lower = tick_to_sqrt_price(tick_lower)
    sqrt_price_upper = tick_to_sqrt_price(tick_upper)

    return BoundedProductCFMM(
        tick_lower=tick_lower,
        tick_upper=tick_upper,
        liquidity=liquidity,
        sqrt_price_lower=sqrt_price_lower,
        sqrt_price_upper=sqrt_price_upper,
    )


# =============================================================================
# V2-V3 ARBITRAGE WITH TICK PREDICTION
# =============================================================================


def estimate_v2_equilibrium_price(
    v2_reserve_in: float,
    v2_reserve_out: float,
    v2_fee: float,
) -> float:
    """
    Estimate equilibrium price from V2 pool.

    The equilibrium price after arbitrage will be close to:
    P_eq = sqrt(R_out / R_in) adjusted for fees

    This is the price at which V2 pool's marginal rate equals V3 pool's marginal rate.
    """
    # Simple estimate: pool's current price
    return v2_reserve_out / v2_reserve_in


def predict_v2_v3_optimal_range(
    v2_pool_state: Any,
    v3_pool_state: V3PoolState,
    v3_tick_ranges: list[TickRange],
    input_token_is_token0: bool,
) -> tuple[TickRange, float]:
    """
    Predict the optimal V3 tick range for V2-V3 arbitrage.

    Uses equilibrium price estimation to identify which V3 tick range
    will be active after arbitrage.

    Parameters
    ----------
    v2_pool_state : Any
        V2 pool state (with reserves).
    v3_pool_state : V3PoolState
        V3 pool state.
    v3_tick_ranges : list[TickRange]
        V3 initialized tick ranges.
    input_token_is_token0 : bool
        True if input token is token0 of both pools.

    Returns
    -------
    tuple[TickRange, float]
        (predicted_range, equilibrium_price)
    """
    # Estimate equilibrium price
    v2_price = v2_pool_state.reserves_token1 / v2_pool_state.reserves_token0
    v3_price = v3_pool_state.virtual_reserve1 / v3_pool_state.virtual_reserve0

    # Geometric mean as equilibrium estimate
    equilibrium_price = math.sqrt(v2_price * v3_price)

    # Find tick range containing equilibrium price
    sqrt_eq_price = math.sqrt(equilibrium_price)

    # Get nearest ranges
    nearest_ranges = get_nearest_tick_ranges(v3_tick_ranges, sqrt_eq_price, n=3)

    if nearest_ranges:
        return nearest_ranges[0], equilibrium_price

    # Fallback: return current range
    current_range = TickRange(
        tick_lower=(v3_pool_state.tick // v3_pool_state.tick_spacing) * v3_pool_state.tick_spacing,
        tick_upper=(v3_pool_state.tick // v3_pool_state.tick_spacing + 1) * v3_pool_state.tick_spacing,
        liquidity=v3_pool_state.liquidity,
        sqrt_price_lower=tick_to_sqrt_price(
            (v3_pool_state.tick // v3_pool_state.tick_spacing) * v3_pool_state.tick_spacing
        ),
        sqrt_price_upper=tick_to_sqrt_price(
            (v3_pool_state.tick // v3_pool_state.tick_spacing + 1) * v3_pool_state.tick_spacing
        ),
    )

    return current_range, equilibrium_price
