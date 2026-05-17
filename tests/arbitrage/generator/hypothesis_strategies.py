"""
Hypothesis strategies for property-based arbitrage testing.

Provides strategies for generating random pool states and arbitrage
configurations that are guaranteed to have profitable opportunities.
"""

from fractions import Fraction

import hypothesis.strategies as st

from .types import (
    PoolGenerationConfig,
    PriceDiscrepancyConfig,
    V3PoolGenerationConfig,
    V4PoolGenerationConfig,
)

# ==============================================================================
# Numeric Strategies (Bounded)
# ==============================================================================

# Reserve amounts: 0.001 ETH to 1M ETH equivalent (in wei)
reserve_strategy = st.integers(min_value=10**15, max_value=10**24)

# Liquidity values for V3/V4 pools
liquidity_strategy = st.integers(min_value=10**15, max_value=10**24)

# Price ratio that creates arbitrage opportunity (>1% price difference)
# Limited range to ensure profitable arb after fees
price_ratio_strategy = st.floats(
    min_value=1.01,
    max_value=1.10,
    allow_nan=False,
    allow_infinity=False,
)

# Smaller price ratio for tighter spreads
price_ratio_tight_strategy = st.floats(
    min_value=1.001,
    max_value=1.02,
    allow_nan=False,
    allow_infinity=False,
)

# Token decimals (standard ERC20 ranges)
token_decimals_strategy = st.integers(min_value=6, max_value=18)

# Block numbers for state tracking
block_number_strategy = st.integers(min_value=0, max_value=10**9)

# Tick values (within V3 bounds)
tick_strategy = st.integers(min_value=-887272, max_value=887272)

# ==============================================================================
# Fee Strategies
# ==============================================================================

# Standard Uniswap fee tiers
standard_fee_strategy = st.sampled_from([
    Fraction(1, 10000),  # 0.01%
    Fraction(5, 10000),  # 0.05%
    Fraction(3, 1000),  # 0.3%
    Fraction(1, 100),  # 1%
])

# Common V2 fee (0.3%)
v2_fee_strategy = st.just(Fraction(3, 1000))

# Fee range for testing edge cases
fee_range_strategy = st.fractions(
    min_value=Fraction(1, 10000),
    max_value=Fraction(1, 10),
)

# ==============================================================================
# Tick Spacing Strategies
# ==============================================================================

# Standard tick spacings for V3/V4 fee tiers
standard_tick_spacing_strategy = st.sampled_from([1, 10, 60, 200])

# Tick spacing that corresponds to fee tier
tick_spacing_for_fee = {
    Fraction(1, 10000): 1,
    Fraction(5, 10000): 10,
    Fraction(3, 1000): 60,
    Fraction(1, 100): 200,
}

# ==============================================================================
# Liquidity Depth Strategies
# ==============================================================================

# Named liquidity depths
liquidity_depth_strategy = st.sampled_from(["shallow", "medium", "deep"])

# ==============================================================================
# Pool Configuration Strategies
# ==============================================================================

pool_config_strategy = st.builds(
    PoolGenerationConfig,
    fee=standard_fee_strategy,
    token0_decimals=token_decimals_strategy,
    token1_decimals=token_decimals_strategy,
)

v3_pool_config_strategy = st.builds(
    V3PoolGenerationConfig,
    fee=standard_fee_strategy,
    tick_spacing=standard_tick_spacing_strategy,
    liquidity_depth=liquidity_strategy,
)

v4_pool_config_strategy = st.builds(
    V4PoolGenerationConfig,
    fee=standard_fee_strategy,
    tick_spacing=standard_tick_spacing_strategy,
    liquidity_depth=liquidity_strategy,
)

# ==============================================================================
# Price Discrepancy Strategies
# ==============================================================================

price_discrepancy_strategy = st.builds(
    PriceDiscrepancyConfig,
    price_ratio=price_ratio_strategy,
    direction=st.just("token0_to_token1"),
)

price_discrepancy_tight_strategy = st.builds(
    PriceDiscrepancyConfig,
    price_ratio=price_ratio_tight_strategy,
    direction=st.just("token0_to_token1"),
)

# ==============================================================================
# Decimal Pair Strategies
# ==============================================================================

# Any valid decimal pair
decimal_pair_strategy = st.tuples(
    token_decimals_strategy,
    token_decimals_strategy,
)

# Mismatched decimals (tests decimal correction)
mismatched_decimal_pair_strategy = decimal_pair_strategy.filter(lambda d: d[0] != d[1])

# Common decimal pairs in DeFi (e.g., USDC/WETH, WBTC/WETH)
common_decimal_pairs_strategy = st.sampled_from([
    (6, 18),  # USDC/WETH, USDT/WETH
    (8, 18),  # WBTC/WETH
    (6, 8),  # USDC/WBTC
    (6, 6),  # USDC/USDT
    (18, 18),  # WETH/stETH, etc.
])

# ==============================================================================
# Multi-Pool Cycle Strategies
# ==============================================================================

# Number of pools in a cycle (2-6 pools)
num_pools_strategy = st.integers(min_value=2, max_value=6)

# Pool type for multi-pool cycles
pool_type_strategy = st.sampled_from(["v2", "v3", "v4"])

# List of pool types for multi-pool cycles
pool_types_strategy = st.lists(
    pool_type_strategy,
    min_size=2,
    max_size=6,
)

# ==============================================================================
# Seed Strategy (for reproducibility)
# ==============================================================================

seed_strategy = st.integers(min_value=0, max_value=2**31 - 1)

# ==============================================================================
# Composite Strategies
# ==============================================================================


def v2_pair_params_strategy():
    """Strategy for generating V2 pair parameters."""
    return st.tuples(
        price_ratio_strategy,
        liquidity_depth_strategy,
        standard_fee_strategy,
        seed_strategy,
    )


def v3_pair_params_strategy():
    """Strategy for generating V3 pair parameters."""
    return st.tuples(
        price_ratio_strategy,
        liquidity_depth_strategy,
        standard_fee_strategy,
        standard_tick_spacing_strategy,
        seed_strategy,
    )


def multipool_params_strategy():
    """Strategy for generating multi-pool cycle parameters."""
    return st.tuples(
        num_pools_strategy,
        liquidity_depth_strategy,
        st.tuples(
            st.floats(min_value=1.005, max_value=1.02, allow_nan=False, allow_infinity=False),
            st.floats(min_value=1.02, max_value=1.05, allow_nan=False, allow_infinity=False),
        ),
        seed_strategy,
    )


# ==============================================================================
# Sqrt Price Strategies (V3/V4)
# ==============================================================================

# Sqrt price X96 bounds from tick math
MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342

sqrt_price_x96_strategy = st.integers(
    min_value=MIN_SQRT_RATIO + 1,
    max_value=MAX_SQRT_RATIO - 1,
)

# ==============================================================================
# Amount Strategies
# ==============================================================================

# Small swap amounts (for testing precision)
small_amount_strategy = st.integers(min_value=1, max_value=10**6)

# Medium swap amounts
medium_amount_strategy = st.integers(min_value=10**6, max_value=10**18)

# Large swap amounts (near pool capacity)
large_amount_strategy = st.integers(min_value=10**18, max_value=10**24)

# Any valid amount
amount_strategy = st.integers(min_value=1, max_value=10**24)
