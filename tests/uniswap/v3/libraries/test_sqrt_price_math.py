from decimal import Decimal, getcontext

import pytest

from degenbot.constants import MAX_UINT128, MAX_UINT256
from degenbot.exceptions.evm import EVMRevertError
from degenbot.uniswap.v3_libraries.sqrt_price_math import (
    get_amount0_delta,
    get_amount1_delta,
    get_next_sqrt_price_from_input,
    get_next_sqrt_price_from_output,
)

# Adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/SqrtPriceMath.spec.ts


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
    with pytest.raises(EVMRevertError, match="required: sqrt_price_x96 > 0"):
        # this test should fail
        get_next_sqrt_price_from_input(
            sqrt_price_x96=0,
            liquidity=1,
            amount_in=expand_to_18_decimals(1) // 10,
            zero_for_one=False,
        )

    # fails if liquidity is zero
    with pytest.raises(EVMRevertError, match="required: liquidity > 0"):
        # this test should fail
        get_next_sqrt_price_from_input(
            sqrt_price_x96=1,
            liquidity=0,
            amount_in=expand_to_18_decimals(1) // 10,
            zero_for_one=True,
        )

    # fails if input amount overflows the price
    price = 2**160 - 1
    liquidity = 1024
    amount_in = 1024
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_input(
            sqrt_price_x96=price,
            liquidity=liquidity,
            amount_in=amount_in,
            zero_for_one=False,
        )

    # any input amount cannot underflow the price
    price = 1
    liquidity = 1
    amount_in = 2**255
    assert (
        get_next_sqrt_price_from_input(
            sqrt_price_x96=price,
            liquidity=liquidity,
            amount_in=amount_in,
            zero_for_one=True,
        )
        == 1
    )

    # returns input price if amount in is zero and zeroForOne = true
    price = encode_price_sqrt(1, 1)
    assert (
        get_next_sqrt_price_from_input(
            sqrt_price_x96=price,
            liquidity=expand_to_18_decimals(1) // 10,
            amount_in=0,
            zero_for_one=True,
        )
        == price
    )

    # returns input price if amount in is zero and zeroForOne = false
    price = encode_price_sqrt(1, 1)
    assert (
        get_next_sqrt_price_from_input(
            sqrt_price_x96=price,
            liquidity=expand_to_18_decimals(1) // 10,
            amount_in=0,
            zero_for_one=False,
        )
        == price
    )

    # returns the minimum price for max inputs
    sqrt_p = 2**160 - 1
    liquidity = MAX_UINT128
    max_amount_no_overflow = MAX_UINT256 - ((liquidity << 96) // sqrt_p)
    assert (
        get_next_sqrt_price_from_input(
            sqrt_price_x96=sqrt_p,
            liquidity=liquidity,
            amount_in=max_amount_no_overflow,
            zero_for_one=True,
        )
        == 1
    )

    # input amount of 0.1 token1
    sqrt_q = get_next_sqrt_price_from_input(
        sqrt_price_x96=encode_price_sqrt(1, 1),
        liquidity=expand_to_18_decimals(1),
        amount_in=expand_to_18_decimals(1) // 10,
        zero_for_one=False,
    )
    assert sqrt_q == 87150978765690771352898345369

    # input amount of 0.1 token0
    sqrt_q = get_next_sqrt_price_from_input(
        sqrt_price_x96=encode_price_sqrt(1, 1),
        liquidity=expand_to_18_decimals(1),
        amount_in=expand_to_18_decimals(1) // 10,
        zero_for_one=True,
    )
    assert sqrt_q == 72025602285694852357767227579

    # amount_in > type(uint96).max and zeroForOne = true
    assert (
        get_next_sqrt_price_from_input(
            sqrt_price_x96=encode_price_sqrt(1, 1),
            liquidity=expand_to_18_decimals(10),
            amount_in=2**100,
            zero_for_one=True,
        )
        == 624999999995069620
    )
    # perfect answer: https://www.wolframalpha.com/input/?i=624999999995069620+-+%28%281e19+*+1+%2F+%281e19+%2B+2%5E100+*+1%29%29+*+2%5E96%29

    # can return 1 with enough amount_in and zeroForOne = true
    assert (
        get_next_sqrt_price_from_input(
            sqrt_price_x96=encode_price_sqrt(1, 1),
            liquidity=1,
            amount_in=MAX_UINT256 // 2,
            zero_for_one=True,
        )
        == 1
    )


def test_get_next_sqrt_price_from_output():
    with pytest.raises(EVMRevertError, match="required: sqrt_price_x96 > 0"):
        # this test should fail because liquidity cannot be zero
        get_next_sqrt_price_from_output(
            sqrt_price_x96=0,
            liquidity=1,
            amount_out=expand_to_18_decimals(1) // 10,
            zero_for_one=False,
        )

    with pytest.raises(EVMRevertError, match="required: liquidity must be > 0"):
        # this test should fail because liquidity cannot be zero
        get_next_sqrt_price_from_output(
            sqrt_price_x96=1,
            liquidity=0,
            amount_out=expand_to_18_decimals(1) // 10,
            zero_for_one=True,
        )

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 4
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(
            sqrt_price_x96=price,
            liquidity=liquidity,
            amount_out=amount_out,
            zero_for_one=False,
        )

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 5
    with pytest.raises(EVMRevertError):
        # this test should fail
        assert get_next_sqrt_price_from_output(
            sqrt_price_x96=price,
            liquidity=liquidity,
            amount_out=amount_out,
            zero_for_one=False,
        )

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 262145
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(
            sqrt_price_x96=price,
            liquidity=liquidity,
            amount_out=amount_out,
            zero_for_one=True,
        )

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 262144
    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(
            sqrt_price_x96=price,
            liquidity=liquidity,
            amount_out=amount_out,
            zero_for_one=True,
        )

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 262143
    sqrt_q = get_next_sqrt_price_from_output(
        sqrt_price_x96=price,
        liquidity=liquidity,
        amount_out=amount_out,
        zero_for_one=True,
    )
    assert sqrt_q == 77371252455336267181195264

    price = 20282409603651670423947251286016
    liquidity = 1024
    amount_out = 4

    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(
            sqrt_price_x96=price,
            liquidity=liquidity,
            amount_out=amount_out,
            zero_for_one=False,
        )

    price = encode_price_sqrt(1, 1)
    assert (
        get_next_sqrt_price_from_output(
            sqrt_price_x96=price,
            liquidity=expand_to_18_decimals(1) // 10,
            amount_out=0,
            zero_for_one=True,
        )
        == price
    )

    price = encode_price_sqrt(1, 1)
    assert (
        get_next_sqrt_price_from_output(
            sqrt_price_x96=price,
            liquidity=expand_to_18_decimals(1) // 10,
            amount_out=0,
            zero_for_one=False,
        )
        == price
    )

    sqrt_q = get_next_sqrt_price_from_output(
        sqrt_price_x96=encode_price_sqrt(1, 1),
        liquidity=expand_to_18_decimals(1),
        amount_out=expand_to_18_decimals(1) // 10,
        zero_for_one=False,
    )
    assert sqrt_q == 88031291682515930659493278152

    sqrt_q = get_next_sqrt_price_from_output(
        sqrt_price_x96=encode_price_sqrt(1, 1),
        liquidity=expand_to_18_decimals(1),
        amount_out=expand_to_18_decimals(1) // 10,
        zero_for_one=True,
    )
    assert sqrt_q == 71305346262837903834189555302

    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(
            sqrt_price_x96=encode_price_sqrt(1, 1),
            liquidity=1,
            amount_out=MAX_UINT256,
            zero_for_one=True,
        )

    with pytest.raises(EVMRevertError):
        # this test should fail
        get_next_sqrt_price_from_output(
            sqrt_price_x96=encode_price_sqrt(1, 1),
            liquidity=1,
            amount_out=MAX_UINT256,
            zero_for_one=False,
        )


def test_get_amount_0_delta():
    amount0 = get_amount0_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(1, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(2, 1),
        liquidity=0,
        round_up=True,
    )
    assert amount0 == 0

    amount0 = get_amount0_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(1, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(1, 1),
        liquidity=0,
        round_up=True,
    )
    assert amount0 == 0

    amount0 = get_amount0_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(1, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(121, 100),
        liquidity=expand_to_18_decimals(1),
        round_up=True,
    )
    assert amount0 == 90909090909090910

    amount_0_rounded_down = get_amount0_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(1, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(121, 100),
        liquidity=expand_to_18_decimals(1),
        round_up=False,
    )
    assert amount_0_rounded_down == amount0 - 1

    amount_0_up = get_amount0_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(2**90, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(2**96, 1),
        liquidity=expand_to_18_decimals(1),
        round_up=True,
    )
    amount_0_down = get_amount0_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(2**90, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(2**96, 1),
        liquidity=expand_to_18_decimals(1),
        round_up=False,
    )
    assert amount_0_up == amount_0_down + 1


def test_get_amount1_delta():
    amount_1 = get_amount1_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(1, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(2, 1),
        liquidity=0,
        round_up=True,
    )
    assert amount_1 == 0

    amount_1 = get_amount0_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(1, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(1, 1),
        liquidity=0,
        round_up=True,
    )
    assert amount_1 == 0

    # returns 0.1 amount_1 for price of 1 to 1.21
    amount_1 = get_amount1_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(1, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(121, 100),
        liquidity=expand_to_18_decimals(1),
        round_up=True,
    )
    assert amount_1 == 100000000000000000

    amount_1_rounded_down = get_amount1_delta(
        sqrt_ratio_a_x96=encode_price_sqrt(1, 1),
        sqrt_ratio_b_x96=encode_price_sqrt(121, 100),
        liquidity=expand_to_18_decimals(1),
        round_up=False,
    )
    assert amount_1_rounded_down == amount_1 - 1


def test_swap_computation():
    sqrt_p = 1025574284609383690408304870162715216695788925244
    liquidity = 50015962439936049619261659728067971248
    zero_for_one = True
    amount_in = 406

    sqrt_q = get_next_sqrt_price_from_input(
        sqrt_price_x96=sqrt_p,
        liquidity=liquidity,
        amount_in=amount_in,
        zero_for_one=zero_for_one,
    )
    assert sqrt_q == 1025574284609383582644711336373707553698163132913

    amount_0_delta = get_amount0_delta(
        sqrt_ratio_a_x96=sqrt_q,
        sqrt_ratio_b_x96=sqrt_p,
        liquidity=liquidity,
        round_up=True,
    )
    assert amount_0_delta == 406
