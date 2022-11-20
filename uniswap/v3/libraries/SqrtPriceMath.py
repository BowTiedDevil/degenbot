from . import FullMath, FixedPoint96, UnsafeMath
from .Helpers import uint128, uint256


def getNextSqrtPriceFromAmount0RoundingUp(
    sqrtPX96: int, liquidity: int, amount: int, add: bool
) -> int:
    # we short circuit amount == 0 because the result is otherwise not guaranteed to equal the input price
    if amount == 0:
        return sqrtPX96

    numerator1 = uint256(liquidity) << FixedPoint96.RESOLUTION

    if add:
        product = amount * sqrtPX96
        if product // amount == sqrtPX96:
            denominator = numerator1 + product
            if denominator >= numerator1:
                # always fits in 160 bits
                return FullMath.mulDivRoundingUp(
                    numerator1, sqrtPX96, denominator
                )

        return UnsafeMath.divRoundingUp(
            numerator1, numerator1 // sqrtPX96 + amount
        )
    else:
        product = amount * sqrtPX96
        # if the product overflows, we know the denominator underflows
        # in addition, we must check that the denominator does not underflow
        assert product // amount == sqrtPX96 and numerator1 > product, "FAIL!"
        denominator = numerator1 - product
        return FullMath.mulDivRoundingUp(numerator1, sqrtPX96, denominator)


def getNextSqrtPriceFromAmount1RoundingDown(
    sqrtPX96: int,
    liquidity: int,
    amount: int,
    add: bool,
) -> int:

    if add:
        quotient = (
            (amount << FixedPoint96.RESOLUTION) // liquidity
            if amount <= 2**160 - 1
            else FullMath.mulDiv(amount, FixedPoint96.Q96, liquidity)
        )
        return uint256(sqrtPX96) + quotient
    else:
        quotient = (
            UnsafeMath.divRoundingUp(
                amount << FixedPoint96.RESOLUTION, liquidity
            )
            if amount <= (2**160) - 1
            else FullMath.mulDivRoundingUp(amount, FixedPoint96.Q96, liquidity)
        )

        assert sqrtPX96 > quotient, "FAIL!"
        # always fits 160 bits
        return sqrtPX96 - quotient


def getNextSqrtPriceFromInput(
    sqrtPX96: int,
    liquidity: int,
    amountIn: int,
    zeroForOne: bool,
):
    assert sqrtPX96 > 0, "FAIL!"
    assert liquidity > 0, "FAIL!"

    # round to make sure that we don't pass the target price
    return (
        getNextSqrtPriceFromAmount0RoundingUp(
            sqrtPX96, liquidity, amountIn, True
        )
        if zeroForOne
        else getNextSqrtPriceFromAmount1RoundingDown(
            sqrtPX96, liquidity, amountIn, True
        )
    )


def getNextSqrtPriceFromOutput(
    sqrtPX96: int,
    liquidity: int,
    amountOut: int,
    zeroForOne: bool,
):
    assert sqrtPX96 > 0, "FAIL!"
    assert liquidity > 0, "FAIL!"

    # round to make sure that we pass the target price
    return (
        getNextSqrtPriceFromAmount1RoundingDown(
            sqrtPX96, liquidity, amountOut, False
        )
        if zeroForOne
        else getNextSqrtPriceFromAmount0RoundingUp(
            sqrtPX96, liquidity, amountOut, False
        )
    )


def getAmount0Delta(
    sqrtRatioAX96: int,
    sqrtRatioBX96: int,
    liquidity: int,
    roundUp: bool = None,
) -> int:

    if roundUp is not None:
        if sqrtRatioAX96 > sqrtRatioBX96:
            (sqrtRatioAX96, sqrtRatioBX96) = (sqrtRatioBX96, sqrtRatioAX96)

        numerator1 = uint256(liquidity) << FixedPoint96.RESOLUTION
        numerator2 = sqrtRatioBX96 - sqrtRatioAX96

        assert sqrtRatioAX96 > 0, "FAIL!"

        return (
            UnsafeMath.divRoundingUp(
                FullMath.mulDivRoundingUp(
                    numerator1, numerator2, sqrtRatioBX96
                ),
                sqrtRatioAX96,
            )
            if roundUp
            else FullMath.mulDiv(numerator1, numerator2, sqrtRatioBX96)
            // sqrtRatioAX96
        )

    else:
        return (
            -getAmount0Delta(
                sqrtRatioAX96, sqrtRatioBX96, uint128(-liquidity), False
            )
            if liquidity < 0
            else getAmount0Delta(
                sqrtRatioAX96, sqrtRatioBX96, uint128(liquidity), True
            )
        )


def getAmount1Delta(
    sqrtRatioAX96: int,
    sqrtRatioBX96: int,
    liquidity: int,
    roundUp: bool = None,
) -> int:

    if roundUp is not None:
        if sqrtRatioAX96 > sqrtRatioBX96:
            (sqrtRatioAX96, sqrtRatioBX96) = (sqrtRatioBX96, sqrtRatioAX96)

        return (
            FullMath.mulDivRoundingUp(
                liquidity, sqrtRatioBX96 - sqrtRatioAX96, FixedPoint96.Q96
            )
            if roundUp
            else FullMath.mulDiv(
                liquidity, sqrtRatioBX96 - sqrtRatioAX96, FixedPoint96.Q96
            )
        )

    else:
        return (
            -getAmount1Delta(
                sqrtRatioAX96, sqrtRatioBX96, uint128(-liquidity), False
            )
            if liquidity < 0
            else getAmount1Delta(
                sqrtRatioAX96, sqrtRatioBX96, uint128(liquidity), True
            )
        )
