import pytest
from pydantic import ValidationError

from degenbot.constants import MAX_UINT128, MAX_UINT160, MAX_UINT256
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v4_libraries.constants import (
    SQRT_PRICE_1_1,
    SQRT_PRICE_2_1,
    SQRT_PRICE_121_100,
)
from degenbot.uniswap.v4_libraries.sqrt_price_math import (
    get_amount0_delta,
    get_amount1_delta,
    get_next_sqrt_price_from_input,
    get_next_sqrt_price_from_output,
)

# All tests ported from Foundry tests on Uniswap V4 Github repo
# ref: https://github.com/Uniswap/v4-core/blob/main/test/libraries/SqrtPriceMath.t.sol


def test_get_next_sqrt_price_from_input_reverts_if_price_is_zero():
    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_input(0, 1, int(0.1 * 10**18), False)


def test_get_next_sqrt_price_from_input_reverts_if_liquidity_is_zero():
    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_input(1, 0, int(0.1 * 10**18), True)


def test_get_next_sqrt_price_from_input_reverts_if_input_amount_overflows_the_price():
    price = MAX_UINT160 - 1
    liquidity = 1024
    amount_in = 1024
    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_input(price, liquidity, amount_in, False)


def test_get_next_sqrt_price_from_input_any_input_amount_cannot_underflow_the_price():
    price = 1
    liquidity = 1
    amount_in = 2**255
    sqrt_q = get_next_sqrt_price_from_input(price, liquidity, amount_in, True)
    assert sqrt_q == 1


def test_get_next_sqrt_price_from_input_returns_input_price_if_amount_in_is_zero_and_zero_for_one_equals_true():  # noqa: E501
    price = SQRT_PRICE_1_1
    liquidity = 1
    assert get_next_sqrt_price_from_input(price, liquidity, 0, True) == price


def test_get_next_sqrt_price_from_input_returns_input_price_if_amount_in_is_zero_and_zero_for_one_equals_false():  # noqa: E501
    price = SQRT_PRICE_1_1
    liquidity = 1
    assert get_next_sqrt_price_from_input(price, liquidity, 0, False) == price


def test_get_next_sqrt_price_from_input_returns_the_minimum_price_for_max_inputs():
    sqrt_p = MAX_UINT160 - 1
    liquidity = MAX_UINT128
    max_amount_no_overflow = MAX_UINT256 - (MAX_UINT128 << 96) // sqrt_p
    assert get_next_sqrt_price_from_input(sqrt_p, liquidity, max_amount_no_overflow, True) == 1


def test_get_next_sqrt_price_from_input_input_amount_of0_1_currency1():
    sqrt_p = SQRT_PRICE_1_1
    sqrt_q = get_next_sqrt_price_from_input(sqrt_p, 1 * 10**18, int(0.1 * 10**18), False)
    assert sqrt_q == SQRT_PRICE_121_100


def test_get_next_sqrt_price_from_input_input_amount_of0_1_currency0():
    sqrt_p = SQRT_PRICE_1_1
    sqrt_q = get_next_sqrt_price_from_input(sqrt_p, 1 * 10**18, int(0.1 * 10**18), True)
    assert sqrt_q == 72025602285694852357767227579


def test_get_next_sqrt_price_from_input_amount_in_greater_than_type_uint96_max_and_zero_for_one_equals_true():  # noqa: E501
    sqrt_p = SQRT_PRICE_1_1
    sqrt_q = get_next_sqrt_price_from_input(sqrt_p, (10 * 10**18), 2**100, True)

    # perfect answer:
    # https://www.wolframalpha.com/input/?i=624999999995069620+-+%28%281e19+*+1+%2F+%281e19+%2B+2%5E100+*+1%29%29+*+2%5E96%29
    assert sqrt_q == 624999999995069620


def test_get_next_sqrt_price_from_input_can_return1_with_enough_amount_in_and_zero_for_one_equals_true():  # noqa: E501
    sqrt_p = SQRT_PRICE_1_1
    sqrt_q = get_next_sqrt_price_from_input(sqrt_p, 1, MAX_UINT256 // 2, True)
    assert sqrt_q == 1


# gas snapshot tests skipped


def test_get_next_sqrt_price_from_output_reverts_if_price_is_zero():
    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_output(0, 1, int(0.1 * 10**18), False)


def test_get_next_sqrt_price_from_output_reverts_if_liquidity_is_zero():
    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_output(1, 0, int(0.1 * 10**18), True)


def test_get_next_sqrt_price_from_output_reverts_if_output_amount_is_exactly_the_virtual_reserves_of_currency0():  # noqa: E501
    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 4
    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_output(price, liquidity, amount_out, False)


def test_get_next_sqrt_price_from_output_reverts_if_output_amount_is_greater_than_the_virtual_reserves_of_currency0():  # noqa: E501
    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 5
    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_output(price, liquidity, amount_out, False)


def test_get_next_sqrt_price_from_output_reverts_if_output_amount_is_greater_than_the_virtual_reserves_of_currency1():  # noqa: E501
    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 262145
    with pytest.raises(EVMRevertError):
        get_next_sqrt_price_from_output(price, liquidity, amount_out, True)


def test_get_next_sqrt_price_from_output_reverts_if_output_amount_is_exactly_the_virtual_reserves_of_currency1():  # noqa: E501
    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 262144
    with pytest.raises(EVMRevertError):
        get_next_sqrt_price_from_output(price, liquidity, amount_out, True)


def test_get_next_sqrt_price_from_output_succeeds_if_output_amount_is_just_less_than_the_virtual_reserves_of_currency1():  # noqa: E501
    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 262143
    sqrt_q = get_next_sqrt_price_from_output(price, liquidity, amount_out, True)
    assert sqrt_q == 77371252455336267181195264


def test_get_next_sqrt_price_from_output_puzzling_echidna_test():
    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 4
    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_output(price, liquidity, amount_out, False)


def test_get_next_sqrt_price_from_output_returns_input_price_if_amount_in_is_zero_and_zero_for_one_equals_true():  # noqa: E501
    sqrt_p = SQRT_PRICE_1_1
    sqrt_q = get_next_sqrt_price_from_output(sqrt_p, int(0.1 * 10**18), 0, True)
    assert sqrt_p == sqrt_q


def test_get_next_sqrt_price_from_output_returns_input_price_if_amount_in_is_zero_and_zero_for_one_equals_false():  # noqa: E501
    sqrt_p = SQRT_PRICE_1_1
    sqrt_q = get_next_sqrt_price_from_output(sqrt_p, int(0.1 * 10**18), 0, False)
    assert sqrt_p == sqrt_q


def test_get_next_sqrt_price_from_output_output_amount_of0_1_currency1():
    sqrt_p = SQRT_PRICE_1_1
    sqrt_q = get_next_sqrt_price_from_output(sqrt_p, 1 * 10**18, int(0.1 * 10**18), False)
    assert sqrt_q == 88031291682515930659493278152


def test_get_nextsqrt_price_from_output_output_amount_of0_1_currency0():
    sqrt_p = SQRT_PRICE_1_1
    sqrt_q = get_next_sqrt_price_from_output(sqrt_p, 1 * 10**18, int(0.1 * 10**18), True)
    assert sqrt_q == 71305346262837903834189555302


def test_get_nextsqrt_price_from_output_reverts_if_amount_out_is_impossible_in_zero_for_one_direction():  # noqa: E501
    sqrt_p = SQRT_PRICE_1_1

    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_output(sqrt_p, 1, MAX_UINT256, True)


def test_get_nextsqrt_price_from_output_reverts_if_amount_out_is_impossible_in_one_for_zero_direction():  # noqa: E501
    sqrt_p = SQRT_PRICE_1_1

    with pytest.raises(ValidationError):
        get_next_sqrt_price_from_output(sqrt_p, 1, MAX_UINT256, False)


# skip gas tests


def test_get_amount0_delta_returns0_if_liquidity_is0():
    amount0 = get_amount0_delta(SQRT_PRICE_1_1, SQRT_PRICE_2_1, 0, True)
    assert amount0 == 0


def test_get_amount0_delta_returns0_if_prices_are_equal():
    amount0 = get_amount0_delta(SQRT_PRICE_1_1, SQRT_PRICE_1_1, 0, True)
    assert amount0 == 0


def test_get_amount0_delta_reverts_if_price_is_zero():
    with pytest.raises(ValidationError):
        get_amount0_delta(0, 1, 1, True)


def test_get_amount0_delta_1_amount1_for_price_of1_to1_21():
    amount0 = get_amount0_delta(SQRT_PRICE_1_1, SQRT_PRICE_121_100, 1 * 10**18, True)
    assert amount0 == 90909090909090910
    amount0_rounded_down = get_amount0_delta(SQRT_PRICE_1_1, SQRT_PRICE_121_100, 1 * 10**18, False)
    assert amount0_rounded_down == amount0 - 1


def test_get_amount0_delta_works_for_prices_that_overflow():
    # sqrt_p_1 = encodesqrt_priceX96(2^90, 1)
    sqrt_p_1 = 2787593149816327892691964784081045188247552
    # sqrt_p_2 = encodesqrt_priceX96(2^96, 1)
    sqrt_p_2 = 22300745198530623141535718272648361505980416
    amount0_up = get_amount0_delta(sqrt_p_1, sqrt_p_2, 1 * 10**18, True)
    amount0_down = get_amount0_delta(sqrt_p_1, sqrt_p_2, 1 * 10**18, False)
    assert amount0_up == amount0_down + 1


# skipped gas tests


def test_get_amount1_delta_returns0_if_liquidity_is0():
    amount1 = get_amount1_delta(SQRT_PRICE_1_1, SQRT_PRICE_2_1, 0, True)
    assert amount1 == 0


def test_get_amount1_delta_returns0_if_prices_are_equal():
    amount1 = get_amount1_delta(SQRT_PRICE_1_1, SQRT_PRICE_1_1, 0, True)
    assert amount1 == 0


def test_get_amount1_delta_1_amount1_for_price_of1_to1_21():
    amount1 = get_amount1_delta(SQRT_PRICE_1_1, SQRT_PRICE_121_100, 1 * 10**18, True)
    assert amount1 == 100000000000000000
    amount1_rounded_down = get_amount1_delta(SQRT_PRICE_1_1, SQRT_PRICE_121_100, 1 * 10**18, False)
    assert amount1_rounded_down == amount1 - 1


# skipped gas tests


def test_swap_computation_sqrt_p_times_sqrt_q_overflows():
    sqrt_p = 1025574284609383690408304870162715216695788925244
    liquidity = 50015962439936049619261659728067971248
    zero_for_one = True
    amount_in = 406
    sqrt_q = get_next_sqrt_price_from_input(sqrt_p, liquidity, amount_in, zero_for_one)
    assert sqrt_q == 1025574284609383582644711336373707553698163132913
    amount0_delta = get_amount0_delta(sqrt_q, sqrt_p, liquidity, True)
    assert amount0_delta == 406
