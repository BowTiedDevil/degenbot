from typing import Tuple

from . import full_math as FullMath
from . import sqrt_price_math as SqrtPriceMath


def computeSwapStep(
    sqrtRatioCurrentX96: int,
    sqrtRatioTargetX96: int,
    liquidity: int,
    amountRemaining: int,
    feePips: int,
) -> Tuple[int, int, int, int]:
    zeroForOne: bool = sqrtRatioCurrentX96 >= sqrtRatioTargetX96
    exactIn: bool = amountRemaining >= 0

    if exactIn:
        amountRemainingLessFee: int = FullMath.mulDiv(amountRemaining, 10**6 - feePips, 10**6)
        amountIn = (
            SqrtPriceMath.getAmount0Delta(sqrtRatioTargetX96, sqrtRatioCurrentX96, liquidity, True)
            if zeroForOne
            else SqrtPriceMath.getAmount1Delta(
                sqrtRatioCurrentX96, sqrtRatioTargetX96, liquidity, True
            )
        )
        if amountRemainingLessFee >= amountIn:
            sqrtRatioNextX96 = sqrtRatioTargetX96
        else:
            sqrtRatioNextX96 = SqrtPriceMath.getNextSqrtPriceFromInput(
                sqrtRatioCurrentX96,
                liquidity,
                amountRemainingLessFee,
                zeroForOne,
            )
    else:
        amountOut = (
            SqrtPriceMath.getAmount1Delta(sqrtRatioTargetX96, sqrtRatioCurrentX96, liquidity, False)
            if zeroForOne
            else SqrtPriceMath.getAmount0Delta(
                sqrtRatioCurrentX96, sqrtRatioTargetX96, liquidity, False
            )
        )
        if -amountRemaining >= amountOut:
            sqrtRatioNextX96 = sqrtRatioTargetX96
        else:
            sqrtRatioNextX96 = SqrtPriceMath.getNextSqrtPriceFromOutput(
                sqrtRatioCurrentX96,
                liquidity,
                -amountRemaining,
                zeroForOne,
            )

    max: bool = sqrtRatioTargetX96 == sqrtRatioNextX96
    # get the input/output amounts
    if zeroForOne:
        amountIn = (
            amountIn
            if (max and exactIn)
            else SqrtPriceMath.getAmount0Delta(
                sqrtRatioNextX96, sqrtRatioCurrentX96, liquidity, True
            )
        )
        amountOut = (
            amountOut
            if (max and not exactIn)
            else SqrtPriceMath.getAmount1Delta(
                sqrtRatioNextX96, sqrtRatioCurrentX96, liquidity, False
            )
        )
    else:
        amountIn = (
            amountIn
            if (max and exactIn)
            else SqrtPriceMath.getAmount1Delta(
                sqrtRatioCurrentX96, sqrtRatioNextX96, liquidity, True
            )
        )
        amountOut = (
            amountOut
            if (max and not exactIn)
            else SqrtPriceMath.getAmount0Delta(
                sqrtRatioCurrentX96, sqrtRatioNextX96, liquidity, False
            )
        )

    # cap the output amount to not exceed the remaining output amount
    if not exactIn and (amountOut > -amountRemaining):
        amountOut = -amountRemaining

    if exactIn and (sqrtRatioNextX96 != sqrtRatioTargetX96):
        # we didn't reach the target, so take the remainder of the maximum input as fee
        feeAmount = amountRemaining - amountIn
    else:
        feeAmount = FullMath.mulDivRoundingUp(amountIn, feePips, 10**6 - feePips)

    return (
        sqrtRatioNextX96,
        amountIn,
        amountOut,
        feeAmount,
    )
