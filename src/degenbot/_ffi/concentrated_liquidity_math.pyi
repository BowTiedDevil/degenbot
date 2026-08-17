from typing import Any, overload

def most_significant_bit(x: int) -> int:
    """Find the index of the most significant bit set in x.

    Args:
        x: A non-negative integer

    Returns:
        The index (0-255) of the highest set bit

    Raises:
        ValueError: If x is zero

    """

def least_significant_bit(x: int) -> int:
    """Find the index of the least significant bit set in x.

    Args:
        x: A non-negative integer

    Returns:
        The index (0-255) of the lowest set bit

    Raises:
        ValueError: If x is zero

    """

def muldiv(a: int, b: int, denominator: int) -> int:
    """Compute floor(a * b / denominator) with full 512-bit precision.

    Args:
        a: First multiplicand
        b: Second multiplicand
        denominator: Divisor

    Returns:
        The floored result as a Python int

    Raises:
        ValueError: On division by zero or overflow

    """

def muldiv_rounding_up(a: int, b: int, denominator: int) -> int:
    """Compute ceil(a * b / denominator) with full 512-bit precision.

    Args:
        a: First multiplicand
        b: Second multiplicand
        denominator: Divisor

    Returns:
        The ceiling result as a Python int

    Raises:
        ValueError: On division by zero or overflow

    """

def div_rounding_up(x: int, y: int) -> int:
    """Compute ceil(x / y) without overflow checking.

    Args:
        x: Dividend
        y: Divisor

    Returns:
        The ceiling result as a Python int

    Raises:
        ValueError: If y is zero

    """

def simple_mul_div(a: int, b: int, denominator: int) -> int:
    """Compute (a * b) / denominator without overflow checking.

    Args:
        a: First multiplicand
        b: Second multiplicand
        denominator: Divisor

    Returns:
        The result as a Python int

    Raises:
        ValueError: If denominator is zero

    """

def add_delta(x: int, y: int) -> int:
    """Add a signed delta y to x, checking that the result fits in uint128.

    Args:
        x: Base value (must fit in uint128)
        y: Signed delta (must fit in int128)

    Returns:
        The result as a Python int

    Raises:
        ValueError: If the result overflows or inputs are out of range

    """

def get_amount0_delta(
    sqrt_price_a: int,
    sqrt_price_b: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Get the amount0 delta between two prices for a given liquidity.

    Args:
        sqrt_price_a: First sqrt price (X96)
        sqrt_price_b: Second sqrt price (X96)
        liquidity: Liquidity value
        round_up: Whether to round up

    Returns:
        The token0 amount delta as a Python int

    Raises:
        ValueError: On invalid input (zero price, overflow, etc.)

    """

def get_amount1_delta(
    sqrt_price_a: int,
    sqrt_price_b: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Get the amount1 delta between two prices for a given liquidity.

    Args:
        sqrt_price_a: First sqrt price (X96)
        sqrt_price_b: Second sqrt price (X96)
        liquidity: Liquidity value
        round_up: Whether to round up

    Returns:
        The token1 amount delta as a Python int

    Raises:
        ValueError: On invalid input (negative liquidity, overflow, etc.)

    """

def get_next_sqrt_price_from_amount0_rounding_up(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Get the next sqrt price given a delta of token0, rounding up.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount: Token0 amount
        add: Whether to add (True) or remove (False)

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On overflow or insufficient liquidity

    """

def get_next_sqrt_price_from_amount1_rounding_down(
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Get the next sqrt price given a delta of token1, rounding down.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount: Token1 amount
        add: Whether to add (True) or remove (False)

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On overflow or insufficient liquidity

    """

def get_next_sqrt_price_from_input(
    sqrt_price_x96: int,
    liquidity: int,
    amount_in: int,
    zero_for_one: bool,
) -> int:
    """Get the next sqrt price given an input amount.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount_in: Input amount
        zero_for_one: Direction flag

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On invalid price/liquidity or overflow

    """

def get_next_sqrt_price_from_output(
    sqrt_price_x96: int,
    liquidity: int,
    amount_out: int,
    zero_for_one: bool,
) -> int:
    """Get the next sqrt price given an output amount.

    Args:
        sqrt_price_x96: Current sqrt price (X96)
        liquidity: Liquidity value
        amount_out: Output amount
        zero_for_one: Direction flag

    Returns:
        The next sqrt price (X96) as a Python int

    Raises:
        ValueError: On invalid price/liquidity or overflow

    """

def compute_swap_step_v3(
    sqrt_price_current: int,
    sqrt_price_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[int, int, int, int]:
    """Compute a V3-style swap step.

    Args:
        sqrt_price_current: Current sqrt price (X96)
        sqrt_price_target: Target sqrt price (X96)
        liquidity: Liquidity value
        amount_remaining: Remaining amount (signed)
        fee_pips: Fee in pips

    Returns:
        Tuple of (sqrt_price_next, amount_in, amount_out, fee_amount)

    Raises:
        ValueError: On invalid input, overflow, or if liquidity exceeds int128

    """

def compute_swap_step_v4(
    sqrt_price_current: int,
    sqrt_price_target: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
) -> tuple[int, int, int, int]:
    """Compute a V4-style swap step.

    Args:
        sqrt_price_current: Current sqrt price (X96)
        sqrt_price_target: Target sqrt price (X96)
        liquidity: Liquidity value
        amount_remaining: Remaining amount (signed)
        fee_pips: Fee in pips

    Returns:
        Tuple of (sqrt_price_next, amount_in, amount_out, fee_amount)

    Raises:
        ValueError: On invalid input, overflow, or if liquidity exceeds int128

    """

def max_usable_tick(tick_spacing: int) -> int:
    """Compute the maximum usable tick for a given tick spacing.

    Args:
        tick_spacing: The tick spacing value

    Returns:
        The maximum usable tick as an int

    """

def min_usable_tick(tick_spacing: int) -> int:
    """Compute the minimum usable tick for a given tick spacing.

    Args:
        tick_spacing: The tick spacing value

    Returns:
        The minimum usable tick as an int

    """

def get_tick_word_and_bit_position(tick: int, tick_spacing: int) -> tuple[int, int]:
    """Compute the tick word and bit position for a compressed tick.

    Args:
        tick: The tick value
        tick_spacing: The tick spacing value

    Returns:
        A ``(word, bit)`` tuple where ``word`` is the bitmap mapping key
        and ``bit`` is in ``0..=255``.

    """

def apply_liquidity_mapping_update(
    tick_bitmap: dict[int, Any],
    tick_data: dict[int, Any],
    tick_spacing: int,
    tick: int,
    liquidity: int,
    initial_state_block: int,
    update_block: int,
    tick_lower: int,
    tick_upper: int,
    liquidity_delta: int,
) -> dict[str, Any]:
    """Apply a liquidity-mapping update (mint/burn) to the tick bitmap & data.

    A thin PyO3 wrapper over the pure-Rust
    ``degenbot_concentrated_liquidity_math::lib::liquidity_mapping::apply_liquidity_mapping_update``.
    Mirrors ``degenbot.calculations.concentrated_liquidity.apply_liquidity_mapping_update``.
    Values may be plain dicts or pydantic ``BitmapAtWord`` / ``LiquidityAtTick``
    models (the seam reads by key or attribute). ``initial_state_block`` values
    exceeding ``u64::MAX`` (e.g. ``MAX_UINT256`` used to disable the in-range
    adjustment) are clamped to ``u64::MAX``, preserving the skip behavior.

    Args:
        tick_bitmap: Word → ``{"bitmap": int, "block": int}`` (or models)
        tick_data: Tick → ``{"liquidity_net": int,
            "liquidity_gross": int, "block": int}`` (or models)
        tick_spacing: Tick spacing
        tick: Active tick (for the in-range check)
        liquidity: Active in-range liquidity (uint128)
        initial_state_block: State block at which the active liquidity was last settled
        update_block: Block of this liquidity event
        tick_lower: Position lower tick
        tick_upper: Position upper tick
        liquidity_delta: Signed delta (mint positive, burn negative)

    Returns:
        ``{"tick_bitmap": {...}, "tick_data": {...}, "liquidity": int}``

    Raises:
        ValueError: On invalid input types, non-uint128 liquidity, or non-i32 ticks

    """

def get_sqrt_ratio_at_tick(tick: int) -> int:
    """Convert a tick value to its corresponding sqrt price (X96 format).

    Args:
        tick: The tick value in range [-887272, 887272]

    Returns:
        A Python int representing the sqrt price X96 value

    Raises:
        ValueError: If the tick value is invalid (out of range)

    """

#: Minimum Uniswap V3 tick. Canonical source: ``degenbot-concentrated-liquidity-math`` core.
MIN_TICK: int
#: Maximum Uniswap V3 tick. Canonical source: ``degenbot-concentrated-liquidity-math`` core.
MAX_TICK: int
#: Minimum sqrt price ratio (X96). Canonical source: ``degenbot-concentrated-liquidity-math`` core.
MIN_SQRT_RATIO: int
#: Maximum sqrt price ratio (X96). Canonical source: ``degenbot-concentrated-liquidity-math`` core.
MAX_SQRT_RATIO: int

@overload
def get_tick_at_sqrt_ratio(sqrt_price_x96: int) -> int: ...
@overload
def get_tick_at_sqrt_ratio(sqrt_price_x96: bytes) -> int: ...

__all__ = [
    "MAX_SQRT_RATIO",
    "MAX_TICK",
    "MIN_SQRT_RATIO",
    "MIN_TICK",
    "add_delta",
    "apply_liquidity_mapping_update",
    "compute_swap_step_v3",
    "compute_swap_step_v4",
    "div_rounding_up",
    "get_amount0_delta",
    "get_amount1_delta",
    "get_next_sqrt_price_from_amount0_rounding_up",
    "get_next_sqrt_price_from_amount1_rounding_down",
    "get_next_sqrt_price_from_input",
    "get_next_sqrt_price_from_output",
    "get_sqrt_ratio_at_tick",
    "get_tick_at_sqrt_ratio",
    "get_tick_word_and_bit_position",
    "least_significant_bit",
    "max_usable_tick",
    "min_usable_tick",
    "most_significant_bit",
    "muldiv",
    "muldiv_rounding_up",
    "simple_mul_div",
]
