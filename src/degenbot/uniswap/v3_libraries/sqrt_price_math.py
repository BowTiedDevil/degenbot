from typing import Optional

from ...constants import MAX_UINT128, MIN_UINT128, MIN_UINT160
from ...exceptions import EVMRevertError
from . import full_math as FullMath
from . import unsafe_math as UnsafeMath
from .constants import Q96, Q96_RESOLUTION
from .functions import to_int256, to_uint160


def getAmount0Delta(
    sqrtRatioAX96: int,
    sqrtRatioBX96: int,
    liquidity: int,
    roundUp: Optional[bool] = None,
) -> int:
    if roundUp is not None or MIN_UINT128 <= liquidity <= MAX_UINT128:
        if sqrtRatioAX96 > sqrtRatioBX96:
            (sqrtRatioAX96, sqrtRatioBX96) = (sqrtRatioBX96, sqrtRatioAX96)

        numerator1 = liquidity << Q96_RESOLUTION
        numerator2 = sqrtRatioBX96 - sqrtRatioAX96

        if not (sqrtRatioAX96 > 0):
            raise EVMRevertError("require sqrtRatioAX96 > 0")

        return (
            UnsafeMath.divRoundingUp(
                FullMath.mulDivRoundingUp(numerator1, numerator2, sqrtRatioBX96),
                sqrtRatioAX96,
            )
            if roundUp
            else (FullMath.mulDiv(numerator1, numerator2, sqrtRatioBX96)) // sqrtRatioAX96
        )
    else:
        return to_int256(
            -getAmount0Delta(sqrtRatioAX96, sqrtRatioBX96, -liquidity, False)
            if liquidity < 0
            else to_int256(getAmount0Delta(sqrtRatioAX96, sqrtRatioBX96, liquidity, True))
        )


def getAmount1Delta(
    sqrtRatioAX96: int,
    sqrtRatioBX96: int,
    liquidity: int,
    roundUp: Optional[bool] = None,
) -> int:
    if roundUp is not None or MIN_UINT128 <= liquidity <= MAX_UINT128:
        if sqrtRatioAX96 > sqrtRatioBX96:
            sqrtRatioAX96, sqrtRatioBX96 = sqrtRatioBX96, sqrtRatioAX96

        return (
            FullMath.mulDivRoundingUp(liquidity, sqrtRatioBX96 - sqrtRatioAX96, Q96)
            if roundUp
            else FullMath.mulDiv(liquidity, sqrtRatioBX96 - sqrtRatioAX96, Q96)
        )
    else:
        return to_int256(
            -getAmount1Delta(sqrtRatioAX96, sqrtRatioBX96, -liquidity, False)
            if liquidity < 0
            else to_int256(getAmount1Delta(sqrtRatioAX96, sqrtRatioBX96, liquidity, True))
        )


def getNextSqrtPriceFromAmount0RoundingUp(
    sqrtPX96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    # we short circuit amount == 0 because the result is otherwise not guaranteed to equal the input price
    if amount == 0:
        return sqrtPX96

    numerator1 = liquidity << Q96_RESOLUTION

    if add:
        product = amount * sqrtPX96
        if product // amount == sqrtPX96:
            denominator = numerator1 + product
            if denominator >= numerator1:
                # always fits in 160 bits
                return FullMath.mulDivRoundingUp(numerator1, sqrtPX96, denominator)
        return UnsafeMath.divRoundingUp(numerator1, numerator1 // sqrtPX96 + amount)
    else:
        product = amount * sqrtPX96
        # if the product overflows, we know the denominator underflows
        # in addition, we must check that the denominator does not underflow

        if not (product // amount == sqrtPX96 and numerator1 > product):
            raise EVMRevertError("product / amount == sqrtPX96 && numerator1 > product")

        denominator = numerator1 - product
        return to_uint160(FullMath.mulDivRoundingUp(numerator1, sqrtPX96, denominator))


def getNextSqrtPriceFromAmount1RoundingDown(
    sqrtPX96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:
    if add:
        quotient = (
            (amount << Q96_RESOLUTION) // liquidity
            if amount <= 2**160 - 1
            else FullMath.mulDiv(amount, Q96, liquidity)
        )
        return to_uint160(sqrtPX96 + quotient)
    else:
        quotient = (
            UnsafeMath.divRoundingUp(amount << Q96_RESOLUTION, liquidity)
            if amount <= (2**160) - 1
            else FullMath.mulDivRoundingUp(amount, Q96, liquidity)
        )

        if not (sqrtPX96 > quotient):
            raise EVMRevertError("require sqrtPX96 > quotient")

        # always fits 160 bits
        return sqrtPX96 - quotient


def getNextSqrtPriceFromInput(
    sqrtPX96: int,
    liquidity: int,
    amountIn: int,
    zeroForOne: bool,
) -> int:
    if not (sqrtPX96 > MIN_UINT160):
        raise EVMRevertError("sqrtPX96 must be greater than 0")

    if not (liquidity > MIN_UINT160):
        raise EVMRevertError("liquidity must be greater than 0")

    # round to make sure that we don't pass the target price
    return (
        getNextSqrtPriceFromAmount0RoundingUp(sqrtPX96, liquidity, amountIn, True)
        if zeroForOne
        else getNextSqrtPriceFromAmount1RoundingDown(sqrtPX96, liquidity, amountIn, True)
    )


def getNextSqrtPriceFromOutput(
    sqrtPX96: int,
    liquidity: int,
    amountOut: int,
    zeroForOne: bool,
):
    if not (sqrtPX96 > 0):
        raise EVMRevertError

    if not (liquidity > 0):
        raise EVMRevertError

    # round to make sure that we pass the target price
    return (
        getNextSqrtPriceFromAmount1RoundingDown(sqrtPX96, liquidity, amountOut, False)
        if zeroForOne
        else getNextSqrtPriceFromAmount0RoundingUp(sqrtPX96, liquidity, amountOut, False)
    )
