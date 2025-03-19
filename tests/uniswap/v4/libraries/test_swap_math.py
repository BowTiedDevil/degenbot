# ruff: noqa: E501

import hypothesis
import hypothesis.strategies

from degenbot.constants import (
    MAX_INT256,
    MAX_UINT128,
    MAX_UINT160,
    MAX_UINT256,
    MIN_INT256,
    MIN_UINT24,
    MIN_UINT128,
    MIN_UINT160,
)
from degenbot.uniswap.v4_libraries.constants import (
    SQRT_PRICE_1_1,
    SQRT_PRICE_1_4,
    SQRT_PRICE_101_100,
    SQRT_PRICE_1000_100,
    SQRT_PRICE_10000_100,
)
from degenbot.uniswap.v4_libraries.sqrt_price_math import (
    get_next_sqrt_price_from_input,
    get_next_sqrt_price_from_output,
)
from degenbot.uniswap.v4_libraries.swap_math import (
    MAX_SWAP_FEE,
    compute_swap_step,
    get_sqrt_price_target,
)

# Tests adapted from Foundry tests in the Uniswap V4 Github repo
# ref: https://github.com/Uniswap/v4-core/blob/main/test/libraries/SwapMath.t.sol


@hypothesis.given(
    zero_for_one=hypothesis.strategies.booleans(),
    sqrt_price_next_x96=hypothesis.strategies.integers(
        min_value=MIN_UINT160, max_value=MAX_UINT160
    ),
    sqrt_price_limit_x96=hypothesis.strategies.integers(
        min_value=MIN_UINT160, max_value=MAX_UINT160
    ),
)
def test_fuzz_get_sqrt_price_target(
    zero_for_one: bool,
    sqrt_price_next_x96: int,
    sqrt_price_limit_x96: int,
):
    assert get_sqrt_price_target(
        zero_for_one=zero_for_one,
        sqrt_price_next_x96=sqrt_price_next_x96,
        sqrt_price_limit_x96=sqrt_price_limit_x96,
    ) == (
        sqrt_price_limit_x96
        if (
            sqrt_price_next_x96 < sqrt_price_limit_x96
            if zero_for_one
            else sqrt_price_next_x96 > sqrt_price_limit_x96
        )
        else sqrt_price_next_x96
    )


def test_compute_swap_step_exact_amount_in_one_for_zero_that_gets_capped_at_price_target_in():
    price_target = SQRT_PRICE_101_100
    price = SQRT_PRICE_1_1
    liquidity = 2 * 10**18
    amount = -(1 * 10**18)
    lp_fee = 600
    zero_for_one = False
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        price, price_target, liquidity, amount, lp_fee
    )
    assert amount_in == 9975124224178055
    assert amount_out == 9925619580021728
    assert fee_amount == 5988667735148
    assert amount_in + fee_amount < -amount
    price_after_whole_input_amount = get_next_sqrt_price_from_input(
        price, liquidity, -amount, zero_for_one
    )
    assert sqrt_q == price_target
    assert sqrt_q < price_after_whole_input_amount


def test_compute_swap_step_exact_amount_out_one_for_zero_that_gets_capped_at_price_target_in():
    price_target = SQRT_PRICE_101_100
    price = SQRT_PRICE_1_1
    liquidity = 2 * 10**18
    amount = 1 * 10**18
    lp_fee = 600
    zero_for_one = False
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        price, price_target, liquidity, amount, lp_fee
    )
    assert amount_in == 9975124224178055
    assert amount_out == 9925619580021728
    assert fee_amount == 5988667735148
    assert amount_out < amount
    price_after_whole_output_amount = get_next_sqrt_price_from_output(
        price, liquidity, amount, zero_for_one
    )
    assert sqrt_q == price_target
    assert sqrt_q < price_after_whole_output_amount


def test_compute_swap_step_exact_amount_in_one_for_zero_that_is_fully_spent_in():
    price_target = SQRT_PRICE_1000_100
    price = SQRT_PRICE_1_1
    liquidity = 2 * 10**18
    amount = -1 * 10**18
    lp_fee = 600
    zero_for_one = False
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        price, price_target, liquidity, amount, lp_fee
    )
    assert amount_in == 999400000000000000
    assert amount_out == 666399946655997866
    assert fee_amount == 600000000000000
    assert amount_in + fee_amount == -amount
    price_after_whole_input_amount_less_fee = get_next_sqrt_price_from_input(
        price, liquidity, -amount - fee_amount, zero_for_one
    )
    assert sqrt_q < price_target
    assert sqrt_q == price_after_whole_input_amount_less_fee


def test_compute_swap_step_exact_amount_out_one_for_zero_that_is_fully_received_in():
    price_target = SQRT_PRICE_10000_100
    price = SQRT_PRICE_1_1
    liquidity = 2 * 10**18
    amount = 1 * 10**18
    lp_fee = 600
    zero_for_one = False
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        price, price_target, liquidity, amount, lp_fee
    )
    assert amount_in == 2000000000000000000
    assert fee_amount == 1200720432259356
    assert amount_out == amount
    price_after_whole_output_amount = get_next_sqrt_price_from_output(
        price, liquidity, amount, zero_for_one
    )
    assert sqrt_q < price_target
    assert sqrt_q == price_after_whole_output_amount


def test_compute_swap_step_amount_out_is_capped_at_the_desired_amount_out():
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        417332158212080721273783715441582,
        1452870262520218020823638996,
        159344665391607089467575320103,
        1,
        1,
    )
    assert amount_in == 1
    assert fee_amount == 1
    assert amount_out == 1  # would be 2 if not capped
    assert sqrt_q == 417332158212080721273783715441581


def test_compute_swap_step_target_price_of1_uses_partial_input_amount():
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        2, 1, 1, -3915081100057732413702495386755767, 1
    )
    assert amount_in == SQRT_PRICE_1_4
    assert fee_amount == 39614120871253040049813
    assert amount_in + fee_amount <= 3915081100057732413702495386755767
    assert amount_out == 0
    assert sqrt_q == 1


def test_compute_swap_step_not_entire_input_amount_taken_as_fee():
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        2413, 79887613182836312, 1985041575832132834610021537970, -10, 1872
    )
    assert amount_in == 9
    assert fee_amount == 1
    assert amount_out == 0
    assert sqrt_q == 2413


def test_compute_swap_step_zero_for_one_handles_intermediate_insufficient_liquidity_in_exact_output_case():
    sqrt_p = 20282409603651670423947251286016
    sqrt_p_target = (sqrt_p * 11) // 10
    liquidity = 1024
    # virtual reserves of one are only 4
    # https://www.wolframalpha.com/input/?i=1024+%2f+%2820282409603651670423947251286016+%2f+2**96%29
    amount_remaining = 4
    fee_pips = 3000
    (sqrt_q, amount_in, amount_out, fee_amount) = compute_swap_step(
        sqrt_p, sqrt_p_target, liquidity, amount_remaining, fee_pips
    )
    assert amount_out == 0
    assert sqrt_q == sqrt_p_target
    assert amount_in == 26215
    assert fee_amount == 79


def test_compute_swap_step_one_for_zero_handles_intermediate_insufficient_liquidity_in_exact_output_case():
    sqrt_p = 20282409603651670423947251286016
    sqrt_p_target = (sqrt_p * 9) // 10
    liquidity = 1024
    # virtual reserves of zero are only 262144
    # https://www.wolframalpha.com/input/?i=1024+*+%2820282409603651670423947251286016+%2f+2**96%29
    amount_remaining = 263000
    fee_pips = 3000
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        sqrt_p, sqrt_p_target, liquidity, amount_remaining, fee_pips
    )
    assert amount_out == 26214
    assert sqrt_q == sqrt_p_target
    assert amount_in == 1
    assert fee_amount == 1


@hypothesis.given(
    sqrt_price_raw=hypothesis.strategies.integers(min_value=MIN_UINT160, max_value=MAX_UINT160),
    sqrt_price_target_raw=hypothesis.strategies.integers(
        min_value=MIN_UINT160, max_value=MAX_UINT160
    ),
    liquidity=hypothesis.strategies.integers(min_value=MIN_UINT128, max_value=MAX_UINT128),
    amount_remaining=hypothesis.strategies.integers(min_value=MIN_INT256, max_value=MAX_INT256),
    fee_pips=hypothesis.strategies.integers(min_value=MIN_UINT24, max_value=MAX_SWAP_FEE),
)
def test_fuzz_compute_swap_step(
    sqrt_price_raw: int,
    sqrt_price_target_raw: int,
    liquidity: int,
    amount_remaining: int,
    fee_pips: int,
):
    hypothesis.assume(sqrt_price_raw > 0)
    hypothesis.assume(sqrt_price_target_raw > 0)

    if amount_remaining >= 0:
        hypothesis.assume(fee_pips < MAX_SWAP_FEE)

    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        sqrt_ratio_x96_current=sqrt_price_raw,
        sqrt_ratio_x96_target=sqrt_price_target_raw,
        liquidity=liquidity,
        amount_remaining=amount_remaining,
        fee_pips=fee_pips,
    )
    assert amount_in <= MAX_UINT256 - fee_amount
    if amount_remaining >= 0:
        assert amount_out <= amount_remaining
    else:
        assert amount_in + fee_amount <= -amount_remaining

    if sqrt_price_raw == sqrt_price_target_raw:
        assert amount_in == 0
        assert amount_out == 0
        assert fee_amount == 0
        assert sqrt_q == sqrt_price_target_raw

    # didn't reach price target, entire amount must be consumed
    if sqrt_q != sqrt_price_target_raw:
        if amount_remaining == MIN_INT256:
            abs_amt_remaining = MAX_UINT256 + 1
        elif amount_remaining < 0:
            abs_amt_remaining = -amount_remaining
        else:
            abs_amt_remaining = amount_remaining

        if amount_remaining > 0:
            assert amount_out == abs_amt_remaining
        else:
            assert amount_in + fee_amount == abs_amt_remaining

    # next price is between price and price target
    if sqrt_price_target_raw <= sqrt_price_raw:
        assert sqrt_q <= sqrt_price_raw
        assert sqrt_q >= sqrt_price_target_raw
    else:
        assert sqrt_q >= sqrt_price_raw
        assert sqrt_q <= sqrt_price_target_raw


# skipped gas tests
