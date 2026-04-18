"""
Synthetic pool state generator for arbitrage testing.

Generates V2, V3, and V4 pool states with configurable parameters
and guaranteed arbitrage opportunities.
"""

import math
from fractions import Fraction
from typing import TYPE_CHECKING

from eth_typing import ChecksumAddress
from hexbytes import HexBytes

from degenbot.types.abstract import AbstractPoolState
from degenbot.uniswap.v2_types import UniswapV2PoolState
from degenbot.uniswap.v3_libraries.tick_math import get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_types import (
    Liquidity,
    SqrtPriceX96,
    Tick,
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolState,
)
from degenbot.uniswap.v4_types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
    UniswapV4PoolState,
)

from .types import (
    PoolGenerationConfig,
    PriceDiscrepancyConfig,
    V3PoolGenerationConfig,
)

if TYPE_CHECKING:
    from degenbot.uniswap.v3_types import InitializedTickMap, LiquidityMap


class PoolStateGenerator:
    """
    Generate synthetic pool states for testing arbitrage calculations.

    Two modes of operation:
    1. Simple cases: Manually constructed with exact values
    2. Stress tests: Randomly generated with constraints

    Examples
    --------
    >>> generator = PoolStateGenerator()
    >>> v2_state = generator.generate_v2_pool_state(
    ...     address="0x" + "1" * 40,
    ...     reserves_token0=1000000000000000000,  # 1 ETH
    ...     reserves_token1=2000000000,  # 2000 USDC
    ... )
    """

    def generate_v2_pool_state(
        self,
        address: ChecksumAddress,
        reserves_token0: int,
        reserves_token1: int,
        block: int = 0,
    ) -> UniswapV2PoolState:
        """
        Generate a V2 pool state with the given reserves.

        Parameters
        ----------
        address : ChecksumAddress
            The pool address.
        reserves_token0 : int
            Reserve amount for token0.
        reserves_token1 : int
            Reserve amount for token1.
        block : int
            Block number for the state (default: 0).

        Returns
        -------
        UniswapV2PoolState
            The generated pool state.
        """
        return UniswapV2PoolState(
            address=address,
            block=block,
            reserves_token0=reserves_token0,
            reserves_token1=reserves_token1,
        )

    def generate_v3_pool_state(
        self,
        address: ChecksumAddress,
        sqrt_price_x96: SqrtPriceX96,
        liquidity: Liquidity,
        tick: Tick,
        *,
        tick_spacing: int = 60,
        tick_bitmap: "InitializedTickMap | None" = None,
        tick_data: "LiquidityMap | None" = None,
        block: int = 0,
    ) -> UniswapV3PoolState:
        """
        Generate a V3 pool state with the given parameters.

        If tick_bitmap and tick_data are not provided, generates minimal
        tick structures that support swaps around the current price.

        Parameters
        ----------
        address : ChecksumAddress
            The pool address.
        sqrt_price_x96 : SqrtPriceX96
            The current sqrt price in Q128.96 format.
        liquidity : Liquidity
            The current liquidity.
        tick : Tick
            The current tick.
        tick_spacing : int
            The tick spacing for the pool (default: 60).
        tick_bitmap : InitializedTickMap | None
            Pre-built tick bitmap. If None, generates minimal bitmap.
        tick_data : LiquidityMap | None
            Pre-built tick data. If None, generates minimal tick data.
        block : int
            Block number for the state (default: 0).

        Returns
        -------
        UniswapV3PoolState
            The generated pool state.
        """
        if tick_bitmap is None or tick_data is None:
            tick_bitmap, tick_data = self._generate_minimal_tick_structures(
                tick=tick,
                tick_spacing=tick_spacing,
                liquidity=liquidity,
            )

        return UniswapV3PoolState(
            address=address,
            block=block,
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
        )

    def generate_v4_pool_state(
        self,
        address: ChecksumAddress,
        pool_id: HexBytes,
        sqrt_price_x96: SqrtPriceX96,
        liquidity: Liquidity,
        tick: Tick,
        *,
        tick_spacing: int = 60,
        tick_bitmap: "dict[int, UniswapV4BitmapAtWord] | None" = None,
        tick_data: "dict[int, UniswapV4LiquidityAtTick] | None" = None,
        block: int = 0,
    ) -> UniswapV4PoolState:
        """
        Generate a V4 pool state with the given parameters.

        V4 pools are similar to V3 but include a pool ID for identification
        within the PoolManager contract.

        Parameters
        ----------
        address : ChecksumAddress
            The PoolManager address.
        pool_id : HexBytes
            The pool identifier.
        sqrt_price_x96 : SqrtPriceX96
            The current sqrt price in Q128.96 format.
        liquidity : Liquidity
            The current liquidity.
        tick : Tick
            The current tick.
        tick_spacing : int
            The tick spacing for the pool (default: 60).
        tick_bitmap : dict[int, UniswapV4BitmapAtWord] | None
            Pre-built tick bitmap. If None, generates minimal bitmap.
        tick_data : dict[int, UniswapV4LiquidityAtTick] | None
            Pre-built tick data. If None, generates minimal tick data.
        block : int
            Block number for the state (default: 0).

        Returns
        -------
        UniswapV4PoolState
            The generated pool state.
        """
        if tick_bitmap is None or tick_data is None:
            tick_bitmap_v3, tick_data_v3 = self._generate_minimal_tick_structures(
                tick=tick,
                tick_spacing=tick_spacing,
                liquidity=liquidity,
            )
            # Convert V3 types to V4 types
            tick_bitmap = {
                word: UniswapV4BitmapAtWord(bitmap=val.bitmap, block=val.block)
                for word, val in tick_bitmap_v3.items()
            }
            tick_data = {
                tick_val: UniswapV4LiquidityAtTick(
                    liquidity_net=val.liquidity_net,
                    liquidity_gross=val.liquidity_gross,
                    block=val.block,
                )
                for tick_val, val in tick_data_v3.items()
            }

        return UniswapV4PoolState(
            address=address,
            block=block,
            id=pool_id,
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
        )

    def _generate_minimal_tick_structures(
        self,
        tick: Tick,
        tick_spacing: int,
        liquidity: Liquidity,
    ) -> tuple["InitializedTickMap", "LiquidityMap"]:
        """
        Generate minimal tick bitmap and tick data for a pool.

        Creates boundary ticks that allow swaps in both directions
        without crossing into uninitialized regions.

        Parameters
        ----------
        tick : Tick
            The current tick.
        tick_spacing : int
            The tick spacing.
        liquidity : Liquidity
            The liquidity at the current tick.

        Returns
        -------
        tuple[InitializedTickMap, LiquidityMap]
            The tick bitmap and tick data.
        """
        # Find nearest initialized ticks on either side of current tick
        tick_lower = (tick // tick_spacing) * tick_spacing
        tick_upper = tick_lower + tick_spacing

        # Ensure we have at least one spacing on each side
        if tick_lower == tick:
            tick_lower -= tick_spacing

        # Compute bitmap positions
        # Each bitmap word covers 256 ticks
        word_lower = tick_lower // 256
        word_upper = tick_upper // 256

        # Compute bit positions within words
        bit_lower = abs(tick_lower % 256)
        bit_upper = abs(tick_upper % 256)

        tick_bitmap: InitializedTickMap = {}
        tick_data: LiquidityMap = {}

        # Add lower tick
        if word_lower not in tick_bitmap:
            tick_bitmap[word_lower] = UniswapV3BitmapAtWord(bitmap=0, block=0)
        bitmap_value = tick_bitmap[word_lower].bitmap | (1 << bit_lower)
        tick_bitmap[word_lower] = UniswapV3BitmapAtWord(bitmap=bitmap_value, block=0)

        # Liquidity comes in at lower tick (positive net)
        tick_data[tick_lower] = UniswapV3LiquidityAtTick(
            liquidity_net=liquidity,
            liquidity_gross=liquidity,
            block=0,
        )

        # Add upper tick
        if word_upper not in tick_bitmap:
            tick_bitmap[word_upper] = UniswapV3BitmapAtWord(bitmap=0, block=0)
        bitmap_value = tick_bitmap[word_upper].bitmap | (1 << bit_upper)
        tick_bitmap[word_upper] = UniswapV3BitmapAtWord(bitmap=bitmap_value, block=0)

        # Liquidity goes out at upper tick (negative net)
        tick_data[tick_upper] = UniswapV3LiquidityAtTick(
            liquidity_net=-liquidity,
            liquidity_gross=liquidity,
            block=0,
        )

        return tick_bitmap, tick_data

    def generate_v2_pool_state_from_price(
        self,
        address: ChecksumAddress,
        price_token1_per_token0: float,
        liquidity_base: int,
        config: PoolGenerationConfig,
    ) -> UniswapV2PoolState:
        """
        Generate a V2 pool state from a target price and liquidity.

        Parameters
        ----------
        address : ChecksumAddress
            The pool address.
        price_token1_per_token0 : float
            Target price (how many token1 per token0).
        liquidity_base : int
            Base liquidity amount for reserve calculation.
        config : PoolGenerationConfig
            Pool configuration.

        Returns
        -------
        UniswapV2PoolState
            The generated pool state.
        """
        # For V2: price = reserve1 / reserve0
        # reserve0 * reserve1 = k (liquidity)
        # Solving: reserve0 = sqrt(k / price), reserve1 = sqrt(k * price)

        # Account for decimals
        decimal_adjustment = 10 ** (config.token1_decimals - config.token0_decimals)
        adjusted_price = price_token1_per_token0 * decimal_adjustment

        reserve0 = int(math.sqrt(liquidity_base / adjusted_price))
        reserve1 = int(math.sqrt(liquidity_base * adjusted_price))

        return self.generate_v2_pool_state(
            address=address,
            reserves_token0=reserve0,
            reserves_token1=reserve1,
        )

    def generate_v3_pool_state_from_price(
        self,
        address: ChecksumAddress,
        price_token1_per_token0: float,
        liquidity: Liquidity,
        config: V3PoolGenerationConfig,
    ) -> UniswapV3PoolState:
        """
        Generate a V3 pool state from a target price and liquidity.

        Parameters
        ----------
        address : ChecksumAddress
            The pool address.
        price_token1_per_token0 : float
            Target price (how many token1 per token0).
        liquidity : Liquidity
            The liquidity amount.
        config : V3PoolGenerationConfig
            Pool configuration.

        Returns
        -------
        UniswapV3PoolState
            The generated pool state.
        """
        decimal_adjustment = 10 ** (config.token1_decimals - config.token0_decimals)
        adjusted_price = price_token1_per_token0 * decimal_adjustment

        tick = int(math.log(adjusted_price) / math.log(1.0001))

        # Round to nearest valid tick
        tick = round(tick / config.tick_spacing) * config.tick_spacing

        # Calculate sqrt price from tick
        sqrt_price_x96 = get_sqrt_ratio_at_tick(tick)

        return self.generate_v3_pool_state(
            address=address,
            sqrt_price_x96=sqrt_price_x96,
            liquidity=liquidity,
            tick=tick,
            tick_spacing=config.tick_spacing,
        )

    def inject_price_discrepancy(
        self,
        pool_a_state: UniswapV2PoolState,
        pool_b_address: ChecksumAddress,
        discrepancy: PriceDiscrepancyConfig,
    ) -> UniswapV2PoolState:
        """
        Create a second V2 pool state that creates an arbitrage opportunity.

        Given an existing pool A state, generates pool B reserves such that
        swapping through both pools yields profit.

        Parameters
        ----------
        pool_a_state : UniswapV2PoolState
            State of the first pool.
        pool_b_address : ChecksumAddress
            Address for the second pool.
        discrepancy : PriceDiscrepancyConfig
            Price discrepancy configuration.

        Returns
        -------
        UniswapV2PoolState
            State for pool B with the price discrepancy applied.
        """
        # Calculate effective price in pool A
        price_a = pool_a_state.reserves_token1 / pool_a_state.reserves_token0

        # Calculate target price for pool B based on discrepancy
        # If price_ratio > 1.0, pool B has worse price (more expensive to buy token1)
        if discrepancy.direction == "token0_to_token1":
            # Pool B gives fewer token1 per token0
            price_b = price_a / discrepancy.price_ratio
        else:
            # Pool B gives fewer token0 per token1
            price_b = price_a * discrepancy.price_ratio

        # Calculate reserves for pool B maintaining same liquidity base
        liquidity_base = pool_a_state.reserves_token0 * pool_a_state.reserves_token1

        reserve0_b = int(math.sqrt(liquidity_base / price_b))
        reserve1_b = int(math.sqrt(liquidity_base * price_b))

        return self.generate_v2_pool_state(
            address=pool_b_address,
            reserves_token0=reserve0_b,
            reserves_token1=reserve1_b,
        )

    def generate_profitable_v2_pair(
        self,
        pool_a_address: ChecksumAddress,
        pool_b_address: ChecksumAddress,
        fee_a: Fraction,
        fee_b: Fraction,
        price_ratio: float,
        liquidity_base: int,
        *,
        base_price: float = 1.0,
    ) -> tuple[UniswapV2PoolState, UniswapV2PoolState]:
        """
        Generate two V2 pool states with a guaranteed arbitrage opportunity.

        Parameters
        ----------
        pool_a_address : ChecksumAddress
            Address for pool A.
        pool_b_address : ChecksumAddress
            Address for pool B.
        fee_a : Fraction
            Fee for pool A.
        fee_b : Fraction
            Fee for pool B.
        price_ratio : float
            Price ratio between pools (> 1.0 creates arb).
        liquidity_base : int
            Base liquidity for reserve calculation.
        base_price : float
            Base price for pool A (default: 1.0).

        Returns
        -------
        tuple[UniswapV2PoolState, UniswapV2PoolState]
            States for pool A and pool B.
        """
        _ = fee_a  # Reserved for future fee-aware calculations
        _ = fee_b  # Reserved for future fee-aware calculations

        config = PoolGenerationConfig(fee=fee_a)

        pool_a_state = self.generate_v2_pool_state_from_price(
            address=pool_a_address,
            price_token1_per_token0=base_price,
            liquidity_base=liquidity_base,
            config=config,
        )

        discrepancy = PriceDiscrepancyConfig(price_ratio=price_ratio)

        pool_b_state = self.inject_price_discrepancy(
            pool_a_state=pool_a_state,
            pool_b_address=pool_b_address,
            discrepancy=discrepancy,
        )

        return pool_a_state, pool_b_state

    def generate_profitable_v3_pair(
        self,
        pool_a_address: ChecksumAddress,
        pool_b_address: ChecksumAddress,
        tick_spacing: int,
        price_ratio: float,
        liquidity: Liquidity,
        *,
        base_price: float = 1.0,
    ) -> tuple[UniswapV3PoolState, UniswapV3PoolState]:
        """
        Generate two V3 pool states with a guaranteed arbitrage opportunity.

        Parameters
        ----------
        pool_a_address : ChecksumAddress
            Address for pool A.
        pool_b_address : ChecksumAddress
            Address for pool B.
        tick_spacing : int
            Tick spacing for both pools.
        price_ratio : float
            Price ratio between pools (> 1.0 creates arb).
        liquidity : Liquidity
            Liquidity for both pools.
        base_price : float
            Base price for pool A (default: 1.0).

        Returns
        -------
        tuple[UniswapV3PoolState, UniswapV3PoolState]
            States for pool A and pool B.
        """
        config = V3PoolGenerationConfig(
            fee=Fraction(3, 1000),
            tick_spacing=tick_spacing,
            liquidity_depth=liquidity,
        )

        pool_a_state = self.generate_v3_pool_state_from_price(
            address=pool_a_address,
            price_token1_per_token0=base_price,
            liquidity=liquidity,
            config=config,
        )

        # Generate pool B with different price
        pool_b_state = self.generate_v3_pool_state_from_price(
            address=pool_b_address,
            price_token1_per_token0=base_price / price_ratio,
            liquidity=liquidity,
            config=config,
        )

        return pool_a_state, pool_b_state

    def generate_profitable_v4_pair(
        self,
        pool_a_address: ChecksumAddress,
        pool_b_address: ChecksumAddress,
        pool_a_id: HexBytes,
        pool_b_id: HexBytes,
        tick_spacing: int,
        price_ratio: float,
        liquidity: Liquidity,
        *,
        base_price: float = 1.0,
    ) -> tuple[UniswapV4PoolState, UniswapV4PoolState]:
        """
        Generate two V4 pool states with a guaranteed arbitrage opportunity.

        Parameters
        ----------
        pool_a_address : ChecksumAddress
            PoolManager address.
        pool_b_address : ChecksumAddress
            PoolManager address (same as pool_a for V4).
        pool_a_id : HexBytes
            Pool ID for pool A.
        pool_b_id : HexBytes
            Pool ID for pool B.
        tick_spacing : int
            Tick spacing for both pools.
        price_ratio : float
            Price ratio between pools (> 1.0 creates arb).
        liquidity : Liquidity
            Liquidity for both pools.
        base_price : float
            Base price for pool A (default: 1.0).

        Returns
        -------
        tuple[UniswapV4PoolState, UniswapV4PoolState]
            States for pool A and pool B.
        """
        config = V3PoolGenerationConfig(
            fee=Fraction(3, 1000),
            tick_spacing=tick_spacing,
            liquidity_depth=liquidity,
        )

        # Generate tick from base price
        decimal_adjustment = 10 ** (config.token1_decimals - config.token0_decimals)
        adjusted_price_a = base_price * decimal_adjustment
        tick_a = int(math.log(adjusted_price_a) / math.log(1.0001))
        tick_a = round(tick_a / config.tick_spacing) * config.tick_spacing
        sqrt_price_x96_a = get_sqrt_ratio_at_tick(tick_a)

        pool_a_state = self.generate_v4_pool_state(
            address=pool_a_address,
            pool_id=pool_a_id,
            sqrt_price_x96=sqrt_price_x96_a,
            liquidity=liquidity,
            tick=tick_a,
            tick_spacing=tick_spacing,
        )

        # Generate pool B with different price
        adjusted_price_b = (base_price / price_ratio) * decimal_adjustment
        tick_b = int(math.log(adjusted_price_b) / math.log(1.0001))
        tick_b = round(tick_b / config.tick_spacing) * config.tick_spacing
        sqrt_price_x96_b = get_sqrt_ratio_at_tick(tick_b)

        pool_b_state = self.generate_v4_pool_state(
            address=pool_b_address,
            pool_id=pool_b_id,
            sqrt_price_x96=sqrt_price_x96_b,
            liquidity=liquidity,
            tick=tick_b,
            tick_spacing=tick_spacing,
        )

        return pool_a_state, pool_b_state

    def generate_profitable_mixed_pair(
        self,
        v2_pool_address: ChecksumAddress,
        v3_pool_address: ChecksumAddress,
        v2_fee: Fraction,
        v3_tick_spacing: int,
        price_ratio: float,
        liquidity_base: int,
        v3_liquidity: Liquidity,
        *,
        base_price: float = 1.0,
    ) -> tuple[UniswapV2PoolState, UniswapV3PoolState]:
        """
        Generate a V2 and V3 pool pair with a guaranteed arbitrage opportunity.

        Parameters
        ----------
        v2_pool_address : ChecksumAddress
            Address for the V2 pool.
        v3_pool_address : ChecksumAddress
            Address for the V3 pool.
        v2_fee : Fraction
            Fee for the V2 pool.
        v3_tick_spacing : int
            Tick spacing for the V3 pool.
        price_ratio : float
            Price ratio between pools (> 1.0 creates arb).
        liquidity_base : int
            Base liquidity for V2 reserve calculation.
        v3_liquidity : Liquidity
            Liquidity for the V3 pool.
        base_price : float
            Base price for V2 pool (default: 1.0).

        Returns
        -------
        tuple[UniswapV2PoolState, UniswapV3PoolState]
            States for V2 pool and V3 pool.
        """
        v2_config = PoolGenerationConfig(fee=v2_fee)
        v3_config = V3PoolGenerationConfig(
            fee=Fraction(3, 1000),
            tick_spacing=v3_tick_spacing,
            liquidity_depth=v3_liquidity,
        )

        v2_pool_state = self.generate_v2_pool_state_from_price(
            address=v2_pool_address,
            price_token1_per_token0=base_price,
            liquidity_base=liquidity_base,
            config=v2_config,
        )

        v3_pool_state = self.generate_v3_pool_state_from_price(
            address=v3_pool_address,
            price_token1_per_token0=base_price / price_ratio,
            liquidity=v3_liquidity,
            config=v3_config,
        )

        return v2_pool_state, v3_pool_state

    def validate_arbitrage_opportunity(
        self,
        pool_a_state: AbstractPoolState,
        pool_b_state: AbstractPoolState,
        pool_a_fee: Fraction,
        pool_b_fee: Fraction,
        *,
        min_profit_ratio: float = 0.001,
    ) -> bool:
        """
        Validate that two pool states create a profitable arbitrage opportunity.

        Checks if the price difference is large enough to overcome fees
        and yield profit.

        Parameters
        ----------
        pool_a_state : AbstractPoolState
            State of the first pool.
        pool_b_state : AbstractPoolState
            State of the second pool.
        pool_a_fee : Fraction
            Fee for pool A.
        pool_b_fee : Fraction
            Fee for pool B.
        min_profit_ratio : float
            Minimum profit ratio required (default: 0.001 = 0.1%).

        Returns
        -------
        bool
            True if arbitrage opportunity exists and is profitable.
        """
        if isinstance(pool_a_state, UniswapV2PoolState) and isinstance(
            pool_b_state, UniswapV2PoolState
        ):
            # Calculate effective prices
            price_a = pool_a_state.reserves_token1 / pool_a_state.reserves_token0
            price_b = pool_b_state.reserves_token1 / pool_b_state.reserves_token0

            # Check if there's a profitable arb after fees
            # Buy in cheaper pool, sell in expensive pool
            if price_a > price_b:
                # Buy token0 in pool B (cheaper), sell in pool A
                effective_price_b = price_b / (1 - float(pool_b_fee))
                effective_price_a = price_a * (1 - float(pool_a_fee))
                return effective_price_a > effective_price_b * (1 + min_profit_ratio)
            # Buy token0 in pool A (cheaper), sell in pool B
            effective_price_a = price_a / (1 - float(pool_a_fee))
            effective_price_b = price_b * (1 - float(pool_b_fee))
            return effective_price_b > effective_price_a * (1 + min_profit_ratio)

        # For V3/V4, use sqrt price to compare
        if isinstance(pool_a_state, UniswapV3PoolState) and isinstance(
            pool_b_state, UniswapV3PoolState
        ):
            # Compare sqrt prices (higher sqrt_price_x96 = more token1 per token0)
            sqrt_price_ratio = pool_a_state.sqrt_price_x96 / pool_b_state.sqrt_price_x96
            # Account for fees: need price ratio > 1/(1-fee_a)/(1-fee_b)
            fee_multiplier = 1 / ((1 - float(pool_a_fee)) * (1 - float(pool_b_fee)))
            return abs(sqrt_price_ratio - 1.0) > (fee_multiplier - 1.0) + min_profit_ratio

        if isinstance(pool_a_state, UniswapV4PoolState) and isinstance(
            pool_b_state, UniswapV4PoolState
        ):
            sqrt_price_ratio = pool_a_state.sqrt_price_x96 / pool_b_state.sqrt_price_x96
            fee_multiplier = 1 / ((1 - float(pool_a_fee)) * (1 - float(pool_b_fee)))
            return abs(sqrt_price_ratio - 1.0) > (fee_multiplier - 1.0) + min_profit_ratio

        return False
