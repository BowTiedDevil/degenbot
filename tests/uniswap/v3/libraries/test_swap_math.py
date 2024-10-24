from decimal import Decimal, getcontext

from degenbot.uniswap.v3_libraries.sqrt_price_math import (
    get_next_sqrt_price_from_input,
    get_next_sqrt_price_from_output,
)
from degenbot.uniswap.v3_libraries.swap_math import compute_swap_step

# Tests adapted from Typescript tests on Uniswap V3 Github repo
# ref: https://github.com/Uniswap/v3-core/blob/main/test/SwapMath.spec.ts


# Change the rounding method to match the BigNumber unit test at https://github.com/Uniswap/v3-core/blob/main/test/shared/utilities.ts
# which specifies .integerValue(3), the 'ROUND_FLOOR' rounding method per https://mikemcl.github.io/bignumber.js/#bignumber
getcontext().prec = 256
getcontext().rounding = "ROUND_FLOOR"


def expand_to_18_decimals(x: int):
    return x * 10**18


def encode_price_sqrt(reserve1: int, reserve0: int):
    """
    Returns the sqrt price as a Q64.96 value
    """
    return round((Decimal(reserve1) / Decimal(reserve0)).sqrt() * Decimal(2**96))


def test_compute_swap_step():
    # exact amount in that gets capped at price target in one for zero
    price = encode_price_sqrt(1, 1)
    price_target = encode_price_sqrt(101, 100)
    liquidity = expand_to_18_decimals(2)
    amount = expand_to_18_decimals(1)
    fee = 600
    zero_for_one = False

    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        price, price_target, liquidity, amount, fee
    )

    assert amount_in == 9975124224178055
    assert fee_amount == 5988667735148
    assert amount_out == 9925619580021728
    assert amount_in + fee_amount < amount

    price_after_whole_input_amount = get_next_sqrt_price_from_input(
        price, liquidity, amount, zero_for_one
    )

    assert sqrt_q == price_target
    assert sqrt_q < price_after_whole_input_amount

    # exact amount out that gets capped at price target in one for zero
    price = encode_price_sqrt(1, 1)
    price_target = encode_price_sqrt(101, 100)
    liquidity = expand_to_18_decimals(2)
    amount = -expand_to_18_decimals(1)
    fee = 600
    zero_for_one = False

    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        price, price_target, liquidity, amount, fee
    )

    assert amount_in == 9975124224178055
    assert fee_amount == 5988667735148
    assert amount_out == 9925619580021728
    assert amount_out < -amount

    price_after_whole_output_amount = get_next_sqrt_price_from_output(
        price, liquidity, -amount, zero_for_one
    )

    assert sqrt_q == price_target
    assert sqrt_q < price_after_whole_output_amount

    # exact amount in that is fully spent in one for zero
    price = encode_price_sqrt(1, 1)
    price_target = encode_price_sqrt(1000, 100)
    liquidity = expand_to_18_decimals(2)
    amount = expand_to_18_decimals(1)
    fee = 600
    zero_for_one = False

    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        price, price_target, liquidity, amount, fee
    )

    assert amount_in == 999400000000000000
    assert fee_amount == 600000000000000
    assert amount_out == 666399946655997866
    assert amount_in + fee_amount == amount

    price_after_whole_input_amount_less_fee = get_next_sqrt_price_from_input(
        price, liquidity, amount - fee_amount, zero_for_one
    )

    assert sqrt_q < price_target
    assert sqrt_q == price_after_whole_input_amount_less_fee

    # exact amount out that is fully received in one for zero
    price = encode_price_sqrt(1, 1)
    price_target = encode_price_sqrt(10000, 100)
    liquidity = expand_to_18_decimals(2)
    amount = -expand_to_18_decimals(1)
    fee = 600
    zero_for_one = False

    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        price, price_target, liquidity, amount, fee
    )

    assert amount_in == 2000000000000000000
    assert fee_amount == 1200720432259356
    assert amount_out == -amount

    price_after_whole_output_amount = get_next_sqrt_price_from_output(
        price, liquidity, -amount, zero_for_one
    )

    assert sqrt_q < price_target
    assert sqrt_q == price_after_whole_output_amount

    # amount out is capped at the desired amount out
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        417332158212080721273783715441582,
        1452870262520218020823638996,
        159344665391607089467575320103,
        -1,
        1,
    )

    assert amount_in == 1
    assert fee_amount == 1
    assert amount_out == 1  # would be 2 if not capped
    assert sqrt_q == 417332158212080721273783715441581

    # target price of 1 uses partial input amount
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        2,
        1,
        1,
        3915081100057732413702495386755767,
        1,
    )
    assert amount_in == 39614081257132168796771975168
    assert fee_amount == 39614120871253040049813
    assert amount_in + fee_amount <= 3915081100057732413702495386755767
    assert amount_out == 0
    assert sqrt_q == 1

    # entire input amount taken as fee
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        2413,
        79887613182836312,
        1985041575832132834610021537970,
        10,
        1872,
    )
    assert amount_in == 0
    assert fee_amount == 10
    assert amount_out == 0
    assert sqrt_q == 2413

    # handles intermediate insufficient liquidity in zero for one exact output case
    sqrt_p = 20282409603651670423947251286016
    sqrt_p_target = sqrt_p * 11 // 10
    liquidity = 1024
    # virtual reserves of one are only 4
    # https://www.wolframalpha.com/input/?i=1024+%2F+%2820282409603651670423947251286016+%2F+2**96%29
    amount_remaining = -4
    fee_pips = 3000
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        sqrt_p, sqrt_p_target, liquidity, amount_remaining, fee_pips
    )
    assert amount_out == 0
    assert sqrt_q == sqrt_p_target
    assert amount_in == 26215
    assert fee_amount == 79

    # handles intermediate insufficient liquidity in one for zero exact output case
    sqrt_p = 20282409603651670423947251286016
    sqrt_p_target = sqrt_p * 9 // 10
    liquidity = 1024
    # virtual reserves of zero are only 262144
    # https://www.wolframalpha.com/input/?i=1024+*+%2820282409603651670423947251286016+%2F+2**96%29
    amount_remaining = -263000
    fee_pips = 3000
    sqrt_q, amount_in, amount_out, fee_amount = compute_swap_step(
        sqrt_p, sqrt_p_target, liquidity, amount_remaining, fee_pips
    )
    assert amount_out == 26214
    assert sqrt_q == sqrt_p_target
    assert amount_in == 1
    assert fee_amount == 1
