"""Uniswap V3 SqrtPriceMath: price movement with liquidity constraints.

Rust-accelerated implementations used by default.  Input validation
runs in Python so that V3-specific revert messages match the Solidity
contract exactly.

See: contract_reference/uniswap/V3/UniswapV3Factory.sol
(SqrtPriceMath library)
"""

import functools
from collections.abc import Callable
from typing import Any, overload

from degenbot.degenbot_rs import (
    cl_get_amount0_delta as _rs_get_amount0_delta,
)
from degenbot.degenbot_rs import (
    cl_get_amount1_delta as _rs_get_amount1_delta,
)
from degenbot.degenbot_rs import (
    cl_get_next_sqrt_price_from_amount0_rounding_up as _rs_get_next_sqrt_price_from_amount0_rounding_up,  # noqa: E501
)
from degenbot.degenbot_rs import (
    cl_get_next_sqrt_price_from_amount1_rounding_down as _rs_get_next_sqrt_price_from_amount1_rounding_down,  # noqa: E501
)
from degenbot.degenbot_rs import (
    cl_get_next_sqrt_price_from_input as _rs_get_next_sqrt_price_from_input,
)
from degenbot.degenbot_rs import (
    cl_get_next_sqrt_price_from_output as _rs_get_next_sqrt_price_from_output,
)
from degenbot.exceptions.pool import EVMRevertError
from degenbot.uniswap.v3_libraries._config import V3_LIB_CACHE_SIZE


def _rs(fn: Callable[..., Any]) -> Callable[..., Any]:
    """Wrap a Rust function to convert ValueError/OverflowError → EVMRevertError.

    Returns:
        A wrapper function that re-raises as EVMRevertError.

    """

    @functools.wraps(fn)
    def wrapper(*args: Any, **kwargs: Any) -> Any:  # noqa: ANN401
        try:
            return fn(*args, **kwargs)
        except (ValueError, OverflowError) as e:
            raise EVMRevertError(error=str(e)) from e

    return wrapper


@overload
def get_amount0_delta(
    *,
    sqrt_ratio_a_x96: int,
    sqrt_ratio_b_x96: int,
    liquidity: int,
    round_up: bool,
) -> int: ...


@overload
def get_amount0_delta(
    *,
    sqrt_ratio_a_x96: int,
    sqrt_ratio_b_x96: int,
    liquidity: int,
    round_up: None,
) -> int: ...


@functools.lru_cache(maxsize=V3_LIB_CACHE_SIZE)
def get_amount0_delta(
    *,
    sqrt_ratio_a_x96: int,
    sqrt_ratio_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Return amount0 delta.

    Delegates to Rust implementation.

    Returns:
        The amount of token0 between two sqrt prices.

    Raises:
        EVMRevertError: If inputs are invalid.

    """
    if sqrt_ratio_a_x96 <= 0:
        raise EVMRevertError(error="required: sqrt_ratio_a_x96 > 0")
    if liquidity < 0:
        raise EVMRevertError(error="required: liquidity >= 0")
    return _rs(_rs_get_amount0_delta)(
        sqrt_ratio_a_x96,
        sqrt_ratio_b_x96,
        liquidity,
        round_up,
    )


@overload
def get_amount1_delta(
    *,
    sqrt_ratio_a_x96: int,
    sqrt_ratio_b_x96: int,
    liquidity: int,
    round_up: bool,
) -> int: ...


@overload
def get_amount1_delta(
    *,
    sqrt_ratio_a_x96: int,
    sqrt_ratio_b_x96: int,
    liquidity: int,
    round_up: None,
) -> int: ...


@functools.lru_cache(maxsize=V3_LIB_CACHE_SIZE)
def get_amount1_delta(
    *,
    sqrt_ratio_a_x96: int,
    sqrt_ratio_b_x96: int,
    liquidity: int,
    round_up: bool | None = None,
) -> int:
    """Return amount1 delta.

    Delegates to Rust implementation.

    Returns:
        The amount of token1 between two sqrt prices.

    Raises:
        EVMRevertError: If inputs are invalid.

    """
    if sqrt_ratio_a_x96 <= 0:
        raise EVMRevertError(error="required: sqrt_ratio_a_x96 > 0")
    if liquidity < 0:
        raise EVMRevertError(error="required: liquidity >= 0")
    return _rs(_rs_get_amount1_delta)(
        sqrt_ratio_a_x96,
        sqrt_ratio_b_x96,
        liquidity,
        round_up,
    )


@functools.lru_cache(maxsize=V3_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_amount0_rounding_up(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Return next sqrt price from amount0 rounding up.

    Delegates to Rust implementation.

    Returns:
        The next sqrt price after applying an amount0 delta.

    """
    return _rs(_rs_get_next_sqrt_price_from_amount0_rounding_up)(
        sqrt_price_x96,
        liquidity,
        amount,
        add,
    )


@functools.lru_cache(maxsize=V3_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_amount1_rounding_down(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    """Return next sqrt price from amount1 rounding down.

    Delegates to Rust implementation.

    Returns:
        The next sqrt price after applying an amount1 delta.

    """
    return _rs(_rs_get_next_sqrt_price_from_amount1_rounding_down)(
        sqrt_price_x96,
        liquidity,
        amount,
        add,
    )


@functools.lru_cache(maxsize=V3_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_input(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount_in: int,
    zero_for_one: bool,
) -> int:
    """Return next sqrt price from input.

    Delegates to Rust implementation.

    Returns:
        The next sqrt price given an input amount.

    Raises:
        EVMRevertError: If inputs are invalid.

    """
    if sqrt_price_x96 <= 0:
        raise EVMRevertError(error="required: sqrt_price_x96 > 0")
    if liquidity <= 0:
        raise EVMRevertError(error="required: liquidity > 0")
    return _rs(_rs_get_next_sqrt_price_from_input)(
        sqrt_price_x96,
        liquidity,
        amount_in,
        zero_for_one,
    )


@functools.lru_cache(maxsize=V3_LIB_CACHE_SIZE)
def get_next_sqrt_price_from_output(
    *,
    sqrt_price_x96: int,
    liquidity: int,
    amount_out: int,
    zero_for_one: bool,
) -> int:
    """Return next sqrt price from output.

    Delegates to Rust implementation.

    Returns:
        The next sqrt price given an output amount.

    Raises:
        EVMRevertError: If inputs are invalid.

    """
    if sqrt_price_x96 <= 0:
        raise EVMRevertError(error="required: sqrt_price_x96 > 0")
    if liquidity <= 0:
        raise EVMRevertError(error="required: liquidity must be > 0")
    return _rs(_rs_get_next_sqrt_price_from_output)(
        sqrt_price_x96,
        liquidity,
        amount_out,
        zero_for_one,
    )
