from decimal import Decimal

import pytest

from degenbot.balancer.libraries import weighted_math
from degenbot.balancer.libraries.helpers import bn, from_fp, to_fp
from degenbot.exceptions import EVMRevertError

MAX_RELATIVE_ERROR = Decimal("0.0001")


def _calculate_invariant(fp_raw_balances: list[int], fp_raw_weights: list[int]) -> int:
    normalized_weights = [from_fp(weight) for weight in fp_raw_weights]
    balances = [Decimal(balance) for balance in fp_raw_balances]

    invariant = 1
    for i, balance in enumerate(balances):
        invariant *= balance ** normalized_weights[i]
    return bn(invariant)


def _calc_out_given_in(
    fp_balance_in: int,
    fp_weight_in: int,
    fp_balance_out: int,
    fp_weight_out: int,
    fp_amount_in: int,
) -> int:
    new_balance = from_fp(fp_balance_in) + from_fp(fp_amount_in)
    base = from_fp(fp_balance_in) / (new_balance)
    exponent = from_fp(fp_weight_in) / from_fp(fp_weight_out)
    ratio = Decimal(1) - base**exponent
    return to_fp(from_fp(fp_balance_out) * ratio)


def _calc_in_given_out(
    fp_balance_in: int,
    fp_weight_in: int,
    fp_balance_out: int,
    fp_weight_out: int,
    fp_amount_out: int,
) -> Decimal:
    new_balance = from_fp(fp_balance_out) - from_fp(fp_amount_out)
    base = from_fp(fp_balance_out) / (new_balance)
    exponent = from_fp(fp_weight_out) / from_fp(fp_weight_in)
    ratio = base ** (exponent) - (1)
    return int(to_fp(from_fp(fp_balance_in) * (ratio)))


def test_invariant():
    # zero invariant
    with pytest.raises(EVMRevertError, match="ZERO_INVARIANT"):
        weighted_math.calculate_invariant(normalized_weights=[1], balances=[0])

    # two tokens
    normalized_weights = [int(0.3 * 10**18), int(0.7 * 10**18)]
    balances = [10**18, 12**18]

    result = weighted_math.calculate_invariant(
        normalized_weights=normalized_weights, balances=balances
    )
    expected_invariant = _calculate_invariant(
        fp_raw_balances=balances, fp_raw_weights=normalized_weights
    )

    assert result == pytest.approx(expected_invariant, rel=MAX_RELATIVE_ERROR)

    # three tokens
    normalized_weights = [int(0.3 * 10**18), int(0.2 * 10**18), int(0.5 * 10**18)]
    balances = [10 * 10**18, 12 * 10**18, 14 * 10**18]
    result = weighted_math.calculate_invariant(
        normalized_weights=normalized_weights, balances=balances
    )
    expected_invariant = _calculate_invariant(
        fp_raw_balances=balances, fp_raw_weights=normalized_weights
    )
    assert result == pytest.approx(expected_invariant, rel=MAX_RELATIVE_ERROR)


def test_swap():
    # simple swap
    token_balance_in = 100 * 10**18
    token_weight_in = 50 * 10**18
    token_balance_out = 100 * 10**18
    token_weight_out = 40 * 10**18
    token_amount_in = 15 * 10**18

    out_amount_math = _calc_out_given_in(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )

    out_amount_pool = weighted_math._calc_out_given_in(
        balance_in=token_balance_in,
        weight_in=token_weight_in,
        balance_out=token_balance_out,
        weight_out=token_weight_out,
        amount_in=token_amount_in,
    )

    assert out_amount_pool == pytest.approx(expected=out_amount_math, rel=MAX_RELATIVE_ERROR)

    # out, given in
    token_balance_in = 100 * 10**18
    token_weight_in = 50 * 10**18
    token_balance_out = 100 * 10**18
    token_weight_out = 40 * 10**18
    token_amount_out = 15 * 10**18
    in_amount_math = _calc_in_given_out(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )
    in_amount_pool = weighted_math._calc_in_given_out(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )

    assert in_amount_pool == pytest.approx(expected=in_amount_math, rel=MAX_RELATIVE_ERROR)

    # in, given out
    token_balance_in = 100 * 10**18
    token_weight_in = 50 * 10**18
    token_balance_out = 100 * 10**18
    token_weight_out = 40 * 10**18
    token_amount_out = 15 * 10**18
    in_amount_math = _calc_in_given_out(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )
    in_amount_pool = weighted_math._calc_in_given_out(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )

    assert in_amount_pool == pytest.approx(expected=in_amount_math, rel=MAX_RELATIVE_ERROR)


def test_extreme_amount_swaps():
    # outGivenIn - min amount in
    token_balance_in = 100 * 10**18
    token_weight_in = 50 * 10**18
    token_balance_out = 100 * 10**18
    token_weight_out = 40 * 10**18
    token_amount_in = 10 * 10**6  # (MIN AMOUNT = 0.00000000001)

    out_amount_math = _calc_out_given_in(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )
    out_amount_pool = weighted_math._calc_out_given_in(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )

    assert out_amount_pool == pytest.approx(expected=out_amount_math, rel=0.1)

    # inGivenOut - min amount out
    token_balance_in = 100 * 10**18
    token_weight_in = 50 * 10**18
    token_balance_out = 100 * 10**18
    token_weight_out = 40 * 10**18
    token_amount_out = 10 * 10**6  # (MIN AMOUNT = 0.00000000001)

    in_amount_math = _calc_in_given_out(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )
    in_amount_pool = weighted_math._calc_in_given_out(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_out
    )

    assert in_amount_pool == pytest.approx(expected=in_amount_math, rel=0.5)


def test_extreme_weights():
    # outGivenIn - max weights relation
    token_balance_in = 100 * 10**18
    token_weight_in = int(130.7 * 10**18)
    token_balance_out = 100 * 10**18
    token_weight_out = 1 * 10**18
    token_amount_in = 15 * 10**18
    out_amount_math = _calc_out_given_in(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )
    out_amount_pool = weighted_math._calc_out_given_in(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )

    assert out_amount_pool == pytest.approx(expected=out_amount_math, rel=MAX_RELATIVE_ERROR)

    # outGivenIn - min weights relation
    # Weight relation = 0.00769

    token_balance_in = 100 * 10**18
    token_weight_in = int(0.00769 * 10**18)
    token_balance_out = 100 * 10**18
    token_weight_out = 1 * 10**18
    token_amount_in = 15 * 10**18
    out_amount_math = _calc_out_given_in(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )
    out_amount_pool = weighted_math._calc_out_given_in(
        token_balance_in, token_weight_in, token_balance_out, token_weight_out, token_amount_in
    )

    assert out_amount_pool == pytest.approx(expected=out_amount_math, rel=MAX_RELATIVE_ERROR)
