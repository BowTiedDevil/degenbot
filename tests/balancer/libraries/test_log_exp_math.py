import pytest

from degenbot.balancer.libraries import log_exp_math
from degenbot.balancer.libraries.helpers import bn, fp
from degenbot.exceptions import EVMRevertError

SCALING_FACTOR = 1 * 10**18
FP_SCALING_FACTOR = bn(SCALING_FACTOR)
FP_ZERO = fp(0)
FP_ONE = fp(1)
FP_100_PCT = fp(1)

MAX_X = 2**255 - 1
MAX_Y = 2**254 // 10**20 - 1


def test_pow_exponent_zero():
    exponent = 0

    # handles base zero
    base = 0
    assert log_exp_math.pow(base, exponent) == FP_ONE

    base = 1
    assert log_exp_math.pow(base, exponent) == FP_ONE

    # handles base one
    base = 1
    assert log_exp_math.pow(base, exponent) == FP_ONE

    # handles base greater than one
    base = 10
    assert log_exp_math.pow(base, exponent) == FP_ONE


def test_pow_base_zero():
    base = 0

    # handles exponent zero
    exponent = 0
    assert log_exp_math.pow(base, exponent) == FP_ONE

    # handles exponent one
    exponent = 1
    expected_result = 0
    assert log_exp_math.pow(base, exponent) == expected_result

    # handles exponent greater than one
    exponent = 10
    expected_result = 0
    assert log_exp_math.pow(base, exponent) == expected_result


def test_pow_base_one():
    base = 1

    # handles exponent zero
    exponent = 0
    assert log_exp_math.pow(base, exponent) == (FP_ONE)

    # handles exponent one
    exponent = 1
    assert log_exp_math.pow(base, exponent) == pytest.approx(FP_ONE)

    # handles exponent greater than one
    exponent = 10
    assert log_exp_math.pow(base, exponent) == pytest.approx(FP_ONE)


def test_pow_decimals():
    # handles decimals properly
    base = fp(2)
    exponent = fp(4)
    expected_result = fp(2**4)

    result = log_exp_math.pow(base, exponent)
    assert result == pytest.approx(expected_result)


def test_pow_max_values():
    # cannot handle a base greater than 2^255 - 1
    base = MAX_X + 1
    exponent = 1
    with pytest.raises(EVMRevertError, match="X_OUT_OF_BOUNDS"):
        log_exp_math.pow(base, exponent)

    # cannot handle an exponent greater than (2^254/1e20) - 1
    base = 1
    exponent = MAX_Y + 1
    with pytest.raises(EVMRevertError, match="Y_OUT_OF_BOUNDS"):
        log_exp_math.pow(base, exponent)
