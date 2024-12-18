from decimal import Decimal

import pytest

from degenbot.balancer.libraries import weighted_math
from degenbot.balancer.libraries.helpers import bn, fp, fromFp, toFp
from degenbot.exceptions import EVMRevertError

MAX_RELATIVE_ERROR = 0.0001


def calculate_invariant(fp_raw_balances: list[int], fp_raw_weights: list[int]) -> int:
    normalized_weights = [fromFp(weight) for weight in fp_raw_weights]
    balances = [Decimal(balance) for balance in fp_raw_balances]

    invariant = 1
    for i, balance in enumerate(balances):
        invariant *= balance ** normalized_weights[i]
    return bn(invariant)


def _calc_out_given_in(
    fpBalanceIn: int,
    fpWeightIn: int,
    fpBalanceOut: int,
    fpWeightOut: int,
    fpAmountIn: int,
) -> Decimal:
    newBalance = fromFp(fpBalanceIn) + fromFp(fpAmountIn)
    base = fromFp(fpBalanceIn) / (newBalance)
    exponent = fromFp(fpWeightIn) / fromFp(fpWeightOut)
    ratio = Decimal(1) - base**exponent
    return toFp(fromFp(fpBalanceOut) * ratio)


def _calc_in_given_out(
    fpBalanceIn: int,
    fpWeightIn: int,
    fpBalanceOut: int,
    fpWeightOut: int,
    fpAmountOut: int,
) -> Decimal:
    newBalance = fromFp(fpBalanceOut) - fromFp(fpAmountOut)
    base = fromFp(fpBalanceOut) / (newBalance)
    exponent = fromFp(fpWeightOut) / fromFp(fpWeightIn)
    ratio = base ** (exponent) - (1)
    return toFp(fromFp(fpBalanceIn) * (ratio))


def test_invariant():
    # zero invariant
    with pytest.raises(EVMRevertError, match="ZERO_INVARIANT"):
        weighted_math.calculate_invariant(normalized_weights=[1], balances=[0])

    # two tokens
    normalized_weights = [bn(0.3e18), bn(0.7e18)]
    balances = [bn(10e18), bn(12e18)]

    result = weighted_math.calculate_invariant(
        normalized_weights=normalized_weights, balances=balances
    )
    expected_invariant = calculate_invariant(
        fp_raw_balances=balances, fp_raw_weights=normalized_weights
    )

    assert result == pytest.approx(bn(expected_invariant), rel=MAX_RELATIVE_ERROR)

    # three tokens
    normalized_weights = [bn(0.3e18), bn(0.2e18), bn(0.5e18)]
    balances = [bn(10e18), bn(12e18), bn(14e18)]
    result = weighted_math.calculate_invariant(
        normalized_weights=normalized_weights, balances=balances
    )
    expected_invariant = calculate_invariant(
        fp_raw_balances=balances, fp_raw_weights=normalized_weights
    )
    assert result == pytest.approx(bn(expected_invariant), rel=MAX_RELATIVE_ERROR)


def test_swap():
    # simple swap
    token_balance_in = bn(100e18)
    token_weight_in = bn(50e18)
    token_balance_out = bn(100e18)
    token_weight_out = bn(40e18)
    token_amount_in = bn(15e18)

    out_amount_math = _calc_out_given_in(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )

    out_amount_pool = weighted_math._calcOutGivenIn(
        balanceIn=token_balance_in,
        weightIn=token_weight_in,
        balanceOut=token_balance_out,
        weightOut=token_weight_out,
        amountIn=token_amount_in,
    )

    assert out_amount_pool == pytest.approx(expected=bn(out_amount_math), rel=MAX_RELATIVE_ERROR)

    # out, given in
    token_balance_in = bn(100e18)
    token_weight_in = bn(50e18)
    token_balance_out = bn(100e18)
    token_weight_out = bn(40e18)
    token_amount_out = bn(15e18)
    in_amount_math = _calc_in_given_out(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )
    in_amount_pool = weighted_math._calcInGivenOut(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )

    assert in_amount_pool == pytest.approx(expected=bn(in_amount_math), rel=MAX_RELATIVE_ERROR)

    # in, given out
    token_balance_in = bn(100e18)
    token_weight_in = bn(50e18)
    token_balance_out = bn(100e18)
    token_weight_out = bn(40e18)
    token_amount_out = bn(15e18)
    in_amount_math = _calc_in_given_out(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )
    in_amount_pool = weighted_math._calcInGivenOut(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )

    assert in_amount_pool == pytest.approx(expected=bn(in_amount_math), rel=MAX_RELATIVE_ERROR)


def test_extreme_amount_swaps():
    # outGivenIn - min amount in
    token_balance_in = bn(100e18)
    token_weight_in = bn(50e18)
    token_balance_out = bn(100e18)
    token_weight_out = bn(40e18)
    token_amount_in = bn(10e6)  # (MIN AMOUNT = 0.00000000001)

    out_amount_math = _calc_out_given_in(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )
    out_amount_pool = weighted_math._calcOutGivenIn(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )

    assert out_amount_pool == pytest.approx(expected=bn(out_amount_math), rel=0.1)

    # inGivenOut - min amount out
    token_balance_in = bn(100e18)
    token_weight_in = bn(50e18)
    token_balance_out = bn(100e18)
    token_weight_out = bn(40e18)
    token_amount_out = bn(10e6)  # (MIN AMOUNT = 0.00000000001)

    in_amount_math = _calc_in_given_out(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )
    in_amount_pool = weighted_math._calcInGivenOut(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )

    assert in_amount_pool == pytest.approx(expected=bn(in_amount_math), rel=0.5)


def test_extreme_weights():
    # outGivenIn - max weights relation
    tokenBalanceIn = bn(100e18)
    tokenWeightIn = bn(130.7e18)
    tokenBalanceOut = bn(100e18)
    tokenWeightOut = bn(1e18)
    tokenAmountIn = bn(15e18)
    outAmountMath = _calc_out_given_in(
        tokenBalanceIn, tokenWeightIn, tokenBalanceOut, tokenWeightOut, tokenAmountIn
    )
    outAmountPool = weighted_math._calcOutGivenIn(
        tokenBalanceIn, tokenWeightIn, tokenBalanceOut, tokenWeightOut, tokenAmountIn
    )

    assert outAmountPool == pytest.approx(expected=bn(outAmountMath), rel=MAX_RELATIVE_ERROR)

    # outGivenIn - min weights relation
    # Weight relation = 0.00769

    tokenBalanceIn = bn(100e18)
    tokenWeightIn = bn(0.00769e18)
    tokenBalanceOut = bn(100e18)
    tokenWeightOut = bn(1e18)
    tokenAmountIn = bn(15e18)
    outAmountMath = _calc_out_given_in(
        tokenBalanceIn, tokenWeightIn, tokenBalanceOut, tokenWeightOut, tokenAmountIn
    )
    outAmountPool = weighted_math._calcOutGivenIn(
        tokenBalanceIn, tokenWeightIn, tokenBalanceOut, tokenWeightOut, tokenAmountIn
    )

    assert outAmountPool == pytest.approx(expected=bn(outAmountMath), rel=MAX_RELATIVE_ERROR)
