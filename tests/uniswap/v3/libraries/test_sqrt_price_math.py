from decimal import Decimal, getcontext

import pytest

from degenbot.constants import MAX_UINT128, MAX_UINT256, MIN_UINT128
from degenbot.exceptions import EVMRevertError
from degenbot.uniswap.v3_libraries.sqrt_price_math import (
    get_amount0_delta,
    get_amount1_delta,
    get_next_sqrt_price_from_input,
    get_next_sqrt_price_from_output,
)

# Adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/sqrt_priceMath.spec.ts


getcontext().prec = (
    40
    # Match the decimal places value specified in Uniswap tests
    # ref: https://github.com/Uniswap/v3-core/blob/d8b1c635c275d2a9450bd6a78f3fa2484fef73eb/test/shared/utilities.ts#L60
)

getcontext().rounding = (
    # Change the rounding method to match the BigNumber rounding mode "3",
    # which is 'ROUND_FLOOR' per https://mikemcl.github.io/bignumber.js/#bignumber
    # ref: https://github.com/Uniswap/v3-core/blob/d8b1c635c275d2a9450bd6a78f3fa2484fef73eb/test/shared/utilities.ts#L69
    "ROUND_FLOOR"
)


def expand_to_18_decimals(x: int):
    return x * 10**18


def encode_price_sqrt(reserve1: int, reserve0: int) -> int:
    """
    Returns the sqrt price as a Q64.96 value
    """
    return int((Decimal(reserve1) / Decimal(reserve0)).sqrt() * Decimal(2**96))


def test_get_next_sqrt_price_from_input():
    # fails if price is zero
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_input(0, 0, expand_to_18_decimals(1) // 10, False)

    # fails if liquidity is zero
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_input(1, 0, expand_to_18_decimals(1) // 10, True)

    # fails if input amount overflows the price
    price = 2**160 - 1
    liquidity = 1024
    amount_in = 1024
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_input(price, liquidity, amount_in, False)

    # any input amount cannot underflow the price
    price = 1
    liquidity = 1
    amount_in = 2**255
    assert get_next_sqrt_price_from_input(price, liquidity, amount_in, True) == 1

    # returns input price if amount in is zero and zeroForOne = true
    price = encode_price_sqrt(1, 1)
    assert get_next_sqrt_price_from_input(price, expand_to_18_decimals(1) // 10, 0, True) == price

    # returns input price if amount in is zero and zeroForOne = false
    price = encode_price_sqrt(1, 1)
    assert get_next_sqrt_price_from_input(price, expand_to_18_decimals(1) // 10, 0, False) == price

    # returns the minimum price for max inputs
    sqrt_p = 2**160 - 1
    liquidity = MAX_UINT128
    max_amount_no_overflow = MAX_UINT256 - ((liquidity << 96) // sqrt_p)
    assert get_next_sqrt_price_from_input(sqrt_p, liquidity, max_amount_no_overflow, True) == 1

    # input amount of 0.1 token1
    sqrt_q = get_next_sqrt_price_from_input(
        encode_price_sqrt(1, 1),
        expand_to_18_decimals(1),
        expand_to_18_decimals(1) // 10,
        False,
    )
    assert sqrt_q == 87150978765690771352898345369

    # input amount of 0.1 token0
    sqrt_q = get_next_sqrt_price_from_input(
        encode_price_sqrt(1, 1),
        expand_to_18_decimals(1),
        expand_to_18_decimals(1) // 10,
        True,
    )
    assert sqrt_q == 72025602285694852357767227579

    # amount_in > type(uint96).max and zeroForOne = true
    assert (
        get_next_sqrt_price_from_input(
            encode_price_sqrt(1, 1), expand_to_18_decimals(10), 2**100, True
        )
        == 624999999995069620
    )
    # perfect answer: https://www.wolframalpha.com/input/?i=624999999995069620+-+%28%281e19+*+1+%2F+%281e19+%2B+2%5E100+*+1%29%29+*+2%5E96%29

    # can return 1 with enough amount_in and zeroForOne = true
    assert get_next_sqrt_price_from_input(encode_price_sqrt(1, 1), 1, MAX_UINT256 // 2, True) == 1


def test_get_next_sqrt_price_from_output():
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(0, 0, expand_to_18_decimals(1) // 10, False)

    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(1, 0, expand_to_18_decimals(1) // 10, True)

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 4
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(price, liquidity, amount_out, False)

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 5
    with pytest.raises(EVMRevertError):
        # this test should fail
        assert get_next_sqrt_price_from_output(price, liquidity, amount_out, False)

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 262145
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(price, liquidity, amount_out, True)

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 262144
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(price, liquidity, amount_out, True)

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 262143
    sqrt_q = get_next_sqrt_price_from_output(price, liquidity, amount_out, True)
    assert sqrt_q == 77371252455336267181195264

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 4

    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(price, liquidity, amount_out, False)

    price = encode_price_sqrt(1, 1)
    assert get_next_sqrt_price_from_output(price, expand_to_18_decimals(1) // 10, 0, True) == price

    price = encode_price_sqrt(1, 1)
    assert get_next_sqrt_price_from_output(price, expand_to_18_decimals(1) // 10, 0, False) == price

    sqrt_q = get_next_sqrt_price_from_output(
        encode_price_sqrt(1, 1),
        expand_to_18_decimals(1),
        expand_to_18_decimals(1) // 10,
        False,
    )
    assert sqrt_q == 88031291682515930659493278152

    sqrt_q = get_next_sqrt_price_from_output(
        encode_price_sqrt(1, 1),
        expand_to_18_decimals(1),
        expand_to_18_decimals(1) // 10,
        True,
    )
    assert sqrt_q == 71305346262837903834189555302

    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(encode_price_sqrt(1, 1), 1, MAX_UINT256, True)

    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(encode_price_sqrt(1, 1), 1, MAX_UINT256, False)


def test_get_amount_0_delta():
    with pytest.raises(EVMRevertError):
        get_amount0_delta(0, 0, 0, True)

    with pytest.raises(EVMRevertError):
        get_amount0_delta(1, 0, 0, True)

    with pytest.raises(EVMRevertError):
        get_amount0_delta(1, 0, MAX_UINT128 + 1)

    amount0 = get_amount0_delta(encode_price_sqrt(1, 1), encode_price_sqrt(2, 1), 0, True)
    assert amount0 == 0

    amount0 = get_amount0_delta(encode_price_sqrt(1, 1), encode_price_sqrt(1, 1), 0, True)
    assert amount0 == 0

    amount0 = get_amount0_delta(
        encode_price_sqrt(1, 1),
        encode_price_sqrt(121, 100),
        expand_to_18_decimals(1),
        True,
    )
    assert amount0 == 90909090909090910

    amount_0_rounded_down = get_amount0_delta(
        encode_price_sqrt(1, 1),
        encode_price_sqrt(121, 100),
        expand_to_18_decimals(1),
        False,
    )
    assert amount_0_rounded_down == amount0 - 1

    amount_0_up = get_amount0_delta(
        encode_price_sqrt(2**90, 1),
        encode_price_sqrt(2**96, 1),
        expand_to_18_decimals(1),
        True,
    )
    amount_0_down = get_amount0_delta(
        encode_price_sqrt(2**90, 1),
        encode_price_sqrt(2**96, 1),
        expand_to_18_decimals(1),
        False,
    )
    assert amount_0_up == amount_0_down + 1


def test_get_amount1_delta():
    get_amount1_delta(0, 1, MAX_UINT128 - 1, False)
    get_amount1_delta(1, 0, MAX_UINT128 - 1, False)
    get_amount1_delta(0, 1, MAX_UINT128 - 1, True)
    get_amount1_delta(1, 0, MAX_UINT128 - 1, True)

    get_amount1_delta(0, 0, MIN_UINT128 - 1)
    get_amount1_delta(0, 0, MIN_UINT128 - 1)

    amount_1 = get_amount1_delta(encode_price_sqrt(1, 1), encode_price_sqrt(2, 1), 0, True)
    assert amount_1 == 0

    amount_1 = get_amount0_delta(encode_price_sqrt(1, 1), encode_price_sqrt(1, 1), 0, True)
    assert amount_1 == 0

    # returns 0.1 amount_1 for price of 1 to 1.21
    amount_1 = get_amount1_delta(
        encode_price_sqrt(1, 1),
        encode_price_sqrt(121, 100),
        expand_to_18_decimals(1),
        True,
    )
    assert amount_1 == 100000000000000000

    amount_1_rounded_down = get_amount1_delta(
        encode_price_sqrt(1, 1),
        encode_price_sqrt(121, 100),
        expand_to_18_decimals(1),
        False,
    )
    assert amount_1_rounded_down == amount_1 - 1


def test_swap_computation():
    sqrt_p = 1025574284609383690408304870162715216695788925244
    liquidity = 50015962439936049619261659728067971248
    zero_for_one = True
    amount_in = 406

    sqrt_q = get_next_sqrt_price_from_input(sqrt_p, liquidity, amount_in, zero_for_one)
    assert sqrt_q == 1025574284609383582644711336373707553698163132913

    amount_0_delta = get_amount0_delta(sqrt_q, sqrt_p, liquidity, True)
    assert amount_0_delta == 406
