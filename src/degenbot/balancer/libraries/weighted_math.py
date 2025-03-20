from degenbot.balancer.libraries.constants import ONE
from degenbot.balancer.libraries.fixed_point import (
    complement,
    div_down,
    div_up,
    mul_down,
    mul_up,
    pow_down,
    pow_up,
)
from degenbot.exceptions import EVMRevertError

_MIN_WEIGHT = int(0.01 * 10**18)
# Having a minimum normalized weight imposes a limit on the maximum number of tokens;
# i.e., the largest possible pool is one where all tokens have exactly the minimum weight.
_MAX_WEIGHTED_TOKENS = 100

# Pool limits that arise from limitations in the fixed point power function (and the imposed 1:100
# maximum weight ratio).

# Swap limits: amounts swapped may not be larger than this percentage of total balance.
_MAX_IN_RATIO = int(0.3 * 10**18)
_MAX_OUT_RATIO = int(0.3 * 10**18)


# Invariant growth limit: non-proportional joins cannot cause the invariant to increase by more than
# this ratio.
_MAX_INVARIANT_RATIO = 3 * 10**18
# Invariant shrink limit: non-proportional exits cannot cause the invariant to decrease by less than
# this ratio.
_MIN_INVARIANT_RATIO = int(0.7 * 10**18)


def calculate_invariant(normalized_weights: list[int], balances: list[int]) -> int:
    invariant = ONE

    for i in range(len(normalized_weights)):
        invariant = mul_down(invariant, pow_down(balances[i], normalized_weights[i]))

    if invariant <= 0:
        raise EVMRevertError(error="ZERO_INVARIANT")

    return invariant


def _calc_out_given_in(
    balance_in: int,
    weight_in: int,
    balance_out: int,
    weight_out: int,
    amount_in: int,
) -> int:
    """
    Computes how many tokens can be taken out of a pool if `amountIn` are sent, given the
    current balances and weights.
    """

    # ********************************************************************************************
    # outGivenIn                                                                                //
    # aO = amountOut                                                                            //
    # bO = balanceOut                                                                           //
    # bI = balanceIn              /      /            bI             \    (wI / wO) \           //
    # aI = amountIn    aO = bO * |  1 - | --------------------------  | ^            |          //
    # wI = weightIn               \      \       ( bI + aI )         /              /           //
    # wO = weightOut                                                                            //
    # *******************************************************************************************/

    # Amount out, so we round down overall.

    # The multiplication rounds down, and the subtrahend (power) rounds up (so the base rounds up
    # too). Because bI / (bI + aI) <= 1, the exponent rounds down.

    # // Cannot exceed maximum in ratio
    if amount_in > mul_down(
        balance_in,
        _MAX_IN_RATIO,
    ):
        raise EVMRevertError(error="MAX_IN_RATIO")

    denominator = balance_in + amount_in
    base = div_up(balance_in, denominator)
    exponent = div_down(weight_in, weight_out)
    power = pow_up(base, exponent)
    return int(mul_down(balance_out, complement(power)))


def _calc_in_given_out(
    balance_in: int,
    weight_in: int,
    balance_out: int,
    weight_out: int,
    amount_out: int,
) -> int:
    """
    Computes how many tokens must be sent to a pool in order to take `amountOut`, given the
    current balances and weights.
    """

    # ********************************************************************************************
    # inGivenOut                                                                                //
    # aO = amountOut                                                                            //
    # bO = balanceOut                                                                           //
    # bI = balanceIn              /  /            bO             \    (wO / wI)      \          //
    # aI = amountIn    aI = bI * |  | --------------------------  | ^            - 1  |         //
    # wI = weightIn               \  \       ( bO - aO )         /                   /          //
    # wO = weightOut                                                                            //
    # *******************************************************************************************/

    # Amount in, so we round up overall.

    # The multiplication rounds up, and the power rounds up (so the base rounds up too).
    # Because b0 / (b0 - a0) >= 1, the exponent rounds up.

    _balance_in = balance_in
    _weight_in = weight_in
    _balance_out = balance_out
    _weight_out = weight_out
    _amount_out = amount_out

    # Cannot exceed maximum out ratio
    if not (_amount_out <= mul_down(_balance_out, _MAX_OUT_RATIO)):
        raise EVMRevertError(error="MAX_OUT_RATIO")

    _base = div_up(_balance_out, _balance_out - _amount_out)
    _exponent = div_up(_weight_out, _weight_in)
    _power = pow_up(_base, _exponent)
    # Because the base is larger than one (and the power rounds up), the power should always be
    # larger than one, so the following subtraction should never revert.
    _ratio = _power - ONE
    return mul_up(_balance_in, _ratio)


def _subtract_swap_fee_amount(amount: int, fee_percentage: int) -> int:
    """
    Subtracts swap fee amount from `amount`, returning a lower value.
    """

    # This returns amount - fee amount, so we round up (favoring a higher fee amount).
    fee_amount = mul_up(amount, fee_percentage)
    return amount - fee_amount
