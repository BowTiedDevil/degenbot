from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.libraries.fixed_point import (
    complement,
    divDown,
    divUp,
    mulDown,
    mulUp,
    powDown,
    powUp,
)
from degenbot.exceptions import EVMRevertError

_MIN_WEIGHT = 10**16
# Having a minimum normalized weight imposes a limit on the maximum number of tokens;
# i.e., the largest possible pool is one where all tokens have exactly the minimum weight.
_MAX_WEIGHTED_TOKENS = 100

# Pool limits that arise from limitations in the fixed point power function (and the imposed 1:100 maximum weight
# ratio).

# Swap limits: amounts swapped may not be larger than this percentage of total balance.
_MAX_IN_RATIO = _MAX_OUT_RATIO = 3 * 10**17


# Invariant growth limit: non-proportional joins cannot cause the invariant to increase by more than this ratio.
_MAX_INVARIANT_RATIO = 3 * 10**18
# Invariant shrink limit: non-proportional exits cannot cause the invariant to decrease by less than this ratio.
_MIN_INVARIANT_RATIO = 7 * 10**17


def calculate_invariant(normalized_weights: list[int], balances: list[int]) -> int:
    invariant = ONE

    for i in range(len(normalized_weights)):
        invariant = mulDown(invariant, powDown(balances[i], normalized_weights[i]))

    if invariant <= 0:
        raise EVMRevertError(error="ZERO_INVARIANT")

    return invariant


def _calcInGivenOut(
    balanceIn: int,
    weightIn: int,
    balanceOut: int,
    weightOut: int,
    amountOut: int,
) -> int:
    """
    Computes how many tokens must be sent to a pool in order to take `amountOut`, given the
    current balances and weights.
    """
    # /**********************************************************************************************
    # // inGivenOut                                                                                //
    # // aO = amountOut                                                                            //
    # // bO = balanceOut                                                                           //
    # // bI = balanceIn              /  /            bO             \    (wO / wI)      \          //
    # // aI = amountIn    aI = bI * |  | --------------------------  | ^            - 1  |         //
    # // wI = weightIn               \  \       ( bO - aO )         /                   /          //
    # // wO = weightOut                                                                            //
    # **********************************************************************************************/

    # Amount in, so we round up overall.

    # The multiplication rounds up, and the power rounds up (so the base rounds up too).
    # Because b0 / (b0 - a0) >= 1, the exponent rounds up.

    # Cannot exceed maximum out ratio
    if not (amountOut <= mulDown(balanceOut, _MAX_OUT_RATIO)):
        raise EVMRevertError(error="MAX_OUT_RATIO")

    base = divUp(balanceOut, balanceOut - amountOut)
    exponent = divUp(weightOut, weightIn)
    power = powUp(base, exponent)
    # Because the base is larger than one (and the power rounds up), the power should always be larger than one, so
    # the following subtraction should never revert.
    ratio = power - ONE
    return mulUp(balanceIn, ratio)


def _calcOutGivenIn(
    balanceIn: int,
    weightIn: int,
    balanceOut: int,
    weightOut: int,
    amountIn: int,
) -> int:
    """
    Computes how many tokens can be taken out of a pool if `amountIn` are sent, given the
    current balances and weights.
    """

    # /**********************************************************************************************
    # // outGivenIn                                                                                //
    # // aO = amountOut                                                                            //
    # // bO = balanceOut                                                                           //
    # // bI = balanceIn              /      /            bI             \    (wI / wO) \           //
    # // aI = amountIn    aO = bO * |  1 - | --------------------------  | ^            |          //
    # // wI = weightIn               \      \       ( bI + aI )         /              /           //
    # // wO = weightOut                                                                            //
    # **********************************************************************************************/

    # // Amount out, so we round down overall.

    # // The multiplication rounds down, and the subtrahend (power) rounds up (so the base rounds up too).
    # // Because bI / (bI + aI) <= 1, the exponent rounds down.

    # // Cannot exceed maximum in ratio
    if not (amountIn <= mulDown(balanceIn, _MAX_IN_RATIO)):
        raise EVMRevertError(error="MAX_IN_RATIO")

    denominator = balanceIn + amountIn

    base = divUp(balanceIn, denominator)
    exponent = divDown(weightIn, weightOut)
    power = powUp(base, exponent)
    return mulDown(balanceOut, complement(power))
