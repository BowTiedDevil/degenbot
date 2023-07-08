from decimal import Decimal, localcontext

from degenbot.uniswap.v3.libraries import SqrtPriceMath, SwapMath

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/SwapMath.spec.ts


def expandTo18Decimals(x: int):
    return x * 10**18


def encodePriceSqrt(reserve1: int, reserve0: int):
    """
    Returns the sqrt price as a Q64.96 value
    """
    with localcontext() as ctx:
        # Change the rounding method to match the BigNumber unit test at https://github.com/Uniswap/v3-core/blob/main/test/shared/utilities.ts
        # which specifies .integerValue(3), the 'ROUND_FLOOR' rounding method per https://mikemcl.github.io/bignumber.js/#bignumber
        ctx.rounding = "ROUND_FLOOR"
        return round(
            (Decimal(reserve1) / Decimal(reserve0)).sqrt() * Decimal(2**96)
        )


def test_computeSwapStep():
    # exact amount in that gets capped at price target in one for zero
    price = encodePriceSqrt(1, 1)
    priceTarget = encodePriceSqrt(101, 100)
    liquidity = expandTo18Decimals(2)
    amount = expandTo18Decimals(1)
    fee = 600
    zeroForOne = False

    sqrtQ, amountIn, amountOut, feeAmount = SwapMath.computeSwapStep(
        price, priceTarget, liquidity, amount, fee
    )

    assert amountIn == 9975124224178055
    assert feeAmount == 5988667735148
    assert amountOut == 9925619580021728
    assert amountIn + feeAmount < amount

    priceAfterWholeInputAmount = SqrtPriceMath.getNextSqrtPriceFromInput(
        price, liquidity, amount, zeroForOne
    )

    assert sqrtQ == priceTarget
    assert sqrtQ < priceAfterWholeInputAmount

    # exact amount out that gets capped at price target in one for zero
    price = encodePriceSqrt(1, 1)
    priceTarget = encodePriceSqrt(101, 100)
    liquidity = expandTo18Decimals(2)
    amount = -expandTo18Decimals(1)
    fee = 600
    zeroForOne = False

    sqrtQ, amountIn, amountOut, feeAmount = SwapMath.computeSwapStep(
        price, priceTarget, liquidity, amount, fee
    )

    assert amountIn == 9975124224178055
    assert feeAmount == 5988667735148
    assert amountOut == 9925619580021728
    assert amountOut < -amount

    priceAfterWholeOutputAmount = SqrtPriceMath.getNextSqrtPriceFromOutput(
        price, liquidity, -amount, zeroForOne
    )

    assert sqrtQ == priceTarget
    assert sqrtQ < priceAfterWholeOutputAmount

    # exact amount in that is fully spent in one for zero
    price = encodePriceSqrt(1, 1)
    priceTarget = encodePriceSqrt(1000, 100)
    liquidity = expandTo18Decimals(2)
    amount = expandTo18Decimals(1)
    fee = 600
    zeroForOne = False

    sqrtQ, amountIn, amountOut, feeAmount = SwapMath.computeSwapStep(
        price, priceTarget, liquidity, amount, fee
    )

    assert amountIn == 999400000000000000
    assert feeAmount == 600000000000000
    assert amountOut == 666399946655997866
    assert amountIn + feeAmount == amount

    priceAfterWholeInputAmountLessFee = (
        SqrtPriceMath.getNextSqrtPriceFromInput(
            price, liquidity, amount - feeAmount, zeroForOne
        )
    )

    assert sqrtQ < priceTarget
    assert sqrtQ == priceAfterWholeInputAmountLessFee

    # exact amount out that is fully received in one for zero
    price = encodePriceSqrt(1, 1)
    priceTarget = encodePriceSqrt(10000, 100)
    liquidity = expandTo18Decimals(2)
    amount = -expandTo18Decimals(1)
    fee = 600
    zeroForOne = False

    sqrtQ, amountIn, amountOut, feeAmount = SwapMath.computeSwapStep(
        price, priceTarget, liquidity, amount, fee
    )

    assert amountIn == 2000000000000000000
    assert feeAmount == 1200720432259356
    assert amountOut == -amount

    priceAfterWholeOutputAmount = SqrtPriceMath.getNextSqrtPriceFromOutput(
        price, liquidity, -amount, zeroForOne
    )

    assert sqrtQ < priceTarget
    assert sqrtQ == priceAfterWholeOutputAmount

    # amount out is capped at the desired amount out
    sqrtQ, amountIn, amountOut, feeAmount = SwapMath.computeSwapStep(
        417332158212080721273783715441582,
        1452870262520218020823638996,
        159344665391607089467575320103,
        -1,
        1,
    )

    assert amountIn == 1
    assert feeAmount == 1
    assert amountOut == 1  # would be 2 if not capped
    assert sqrtQ == 417332158212080721273783715441581

    # target price of 1 uses partial input amount
    sqrtQ, amountIn, amountOut, feeAmount = SwapMath.computeSwapStep(
        2,
        1,
        1,
        3915081100057732413702495386755767,
        1,
    )
    assert amountIn == 39614081257132168796771975168
    assert feeAmount == 39614120871253040049813
    assert amountIn + feeAmount <= 3915081100057732413702495386755767
    assert amountOut == 0
    assert sqrtQ == 1

    # entire input amount taken as fee
    sqrtQ, amountIn, amountOut, feeAmount = SwapMath.computeSwapStep(
        2413,
        79887613182836312,
        1985041575832132834610021537970,
        10,
        1872,
    )
    assert amountIn == 0
    assert feeAmount == 10
    assert amountOut == 0
    assert sqrtQ == 2413

    # handles intermediate insufficient liquidity in zero for one exact output case
    sqrtP = 20282409603651670423947251286016
    sqrtPTarget = sqrtP * 11 // 10
    liquidity = 1024
    # virtual reserves of one are only 4
    # https://www.wolframalpha.com/input/?i=1024+%2F+%2820282409603651670423947251286016+%2F+2**96%29
    amountRemaining = -4
    feePips = 3000
    sqrtQ, amountIn, amountOut, feeAmount = SwapMath.computeSwapStep(
        sqrtP, sqrtPTarget, liquidity, amountRemaining, feePips
    )
    assert amountOut == 0
    assert sqrtQ == sqrtPTarget
    assert amountIn == 26215
    assert feeAmount == 79

    # handles intermediate insufficient liquidity in one for zero exact output case
    sqrtP = 20282409603651670423947251286016
    sqrtPTarget = sqrtP * 9 // 10
    liquidity = 1024
    # virtual reserves of zero are only 262144
    # https://www.wolframalpha.com/input/?i=1024+*+%2820282409603651670423947251286016+%2F+2**96%29
    amountRemaining = -263000
    feePips = 3000
    sqrtQ, amountIn, amountOut, feeAmount = SwapMath.computeSwapStep(
        sqrtP, sqrtPTarget, liquidity, amountRemaining, feePips
    )
    assert amountOut == 26214
    assert sqrtQ == sqrtPTarget
    assert amountIn == 1
    assert feeAmount == 1
