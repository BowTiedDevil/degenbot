"""Uniswap V4 SqrtPriceMath: price movement with liquidity constraints.

Rust-accelerated implementations used by default.

See: contract_reference/uniswap/V4/PoolManager.sol (SqrtPriceMath library)
"""

import functools

from degenbot.degenbot_rs import (
    cl_get_amount0_delta as _rs_get_amount0_delta,
    cl_get_amount1_delta as _rs_get_amount1_delta,
    cl_get_next_sqrt_price_from_amount0_rounding_up as _rs_get_next_sqrt_price_from_amount0_rounding_up,
    cl_get_next_sqrt_price_from_amount1_rounding_down as _rs_get_next_sqrt_price_from_amount1_rounding_down,
    cl_get_next_sqrt_price_from_input as _rs_get_next_sqrt_price_from_input,
    cl_get_next_sqrt_price_from_output as _rs_get_next_sqrt_price_from_output,
)

from degenbot.exceptions.pool import EVMRevertError
from degenbot.uniswap.v4_libraries._config import V4_LIB_CACHE_SIZE
from degenbot.uniswap.v4_libraries.full_math import muldiv, muldiv_rounding_up
from degenbot.uniswap.v4_libraries.functions import mulmod
from degenbot.uniswap.v4_libraries.unsafe_math import div_rounding_up


def _rs(fn):
    """Wrap a Rust function to convert ValueError → EVMRevertError."""
    def wrapper(*args, **kwargs):
        try:
            return fn(*args, **kwargs)
        except (ValueError, OverflowError) as e:
            raise EVMRevertError(error=str(e)) from e
        except OverflowError as e:
            raise EVMRevertError(error=str(e)) from e
    wrapper.__name__ = fn.__name__
    wrapper.__qualname__ = fn.__qualname__
    return wrapper


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_amount0_delta(
    *,
    sqrt_price_a_x96: int,
    sqrt_price_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Get the amount0 delta between two prices.

    Delegates to Rust implementation.
    """
    return _rs(_rs_get_amount0_delta)(sqrt_price_a_x96, sqrt_price_b_x96, liquidity, round_up)


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_amount1_delta(
    *,
    sqrt_price_a_x96: int,
    sqrt_price_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Get the amount1 delta between two prices.

    Delegates to Rust implementation.
    """
    return _rs(_rs_get_amount1_delta)(sqrt_price_a_x96, sqrt_price_b_x96, liquidity, round_up)


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_amount0_rounding_up(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Get the next sqrt price given a delta of currency0.

    Delegates to Rust implementation.
    """
    return _rs(_rs_get_next_sqrt_price_from_amount0_rounding_up)(sqrt_price_x96, liquidity, amount, add)


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_amount1_rounding_down(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Get the next sqrt price given a delta of currency1.

    Delegates to Rust implementation.
    """
    return _rs(_rs_get_next_sqrt_price_from_amount1_rounding_down)(sqrt_price_x96, liquidity, amount, add)


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_input(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount_in: int,
    zero_for_one: bool,
) -> int:
    """Get the next sqrt price given an input amount.

    Delegates to Rust implementation.
    """
    return _rs(_rs_get_next_sqrt_price_from_input)(sqrt_price_x96, liquidity, amount_in, zero_for_one)


@functools.lru_cache(maxsize=V4_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_output(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount_out: int,
    zero_for_one: bool,
) -> int:
    """Get the next sqrt price given an output amount.

    Delegates to Rust implementation.
    """
    return _rs(_rs_get_next_sqrt_price_from_output)(sqrt_price_x96, liquidity, amount_out, zero_for_one)
