"""
Port of Balancer V2 StableMath library.

Solidity source:
https://github.com/balancer/balancer-v2-monorepo/blob/master/pkg/pool-stable/contracts/StableMath.sol

Key differences from Solidity:
- Uses Python's arbitrary-precision integers (no overflow checks needed)
- All arithmetic is plain integer math — NOT 18-decimal fixed-point, except where
  explicitly using mul_down/mul_up/div_down/div_up from FixedPoint (in the BPT/join/exit functions)
- The .add/.sub calls in Solidity are SafeMath (checked overflow), equivalent to plain +/- here
- Math.mul/Math.divDown/Math.divUp are plain integer operations (no 18-decimal scaling)
- FixedPoint.mulDown/mulUp/divDown/divUp ARE 18-decimal operations used in BPT calculations
- All rounding directions are preserved (down for outGivenIn, up for inGivenOut)
- Newton-Raphson iteration matches Solidity's convergence logic exactly

Two invariant implementations are provided:

1. _calculate_invariant (monorepo version): Always rounds down. Uses D_P accumulation
   (D^(n+1) / (n^n * P)). Robust for all balance configurations. Used by default.

2. _calculate_invariant_deployed (deployed contract version): Takes a round_up parameter.
   Uses P_D accumulation (n^n * P / D^(n-1)). Matches the deployed MetaStablePool and
   ComposableStablePool contracts exactly. Called with round_up=True for swaps (per the
   deployed contract code) and round_up=False for joins/exits.

The difference between these implementations produces a small (~0.01%) discrepancy in
swap amounts: the monorepo version is off by ~0.01% vs on-chain, while the deployed version
matches exactly. Both are mathematically correct -- the discrepancy is purely due to rounding
direction in the Newton-Raphson iteration."""

from degenbot.balancer.libraries.fixed_point import (
    ONE,
    complement,
    div_down,
    div_up,
    mul_down,
    mul_up,
)
from degenbot.exceptions.pool import EVMRevertError

# Amplification parameter bounds (raw, before multiplying by AMP_PRECISION)
MIN_AMP = 1
MAX_AMP = 5000

# The amplification parameter is stored scaled by this value
AMP_PRECISION = 1000

MAX_STABLE_TOKENS = 5


def _calculate_invariant(
    amplification_parameter: int,
    balances: list[int],
) -> int:
    """
    Computes the invariant given the current balances, using Newton-Raphson approximation.

    The amplification parameter equals: A * n^(n-1)

    Always rounds down, to match Vyper's arithmetic (which always truncates).
    """
    sum_ = 0
    num_tokens = len(balances)
    for i in range(num_tokens):
        sum_ += balances[i]
    if sum_ == 0:
        return 0

    prev_invariant = 0
    invariant = sum_
    amp_times_total = amplification_parameter * num_tokens

    for _ in range(255):
        # D_P accumulation: D^(n+1) / (n^n * P)
        # built as D_P *= D / (balance * n)
        d_p = invariant
        for j in range(num_tokens):
            d_p = _math_div_down(d_p * invariant, balances[j] * num_tokens)

        prev_invariant = invariant

        invariant = _math_div_down(
            (_math_div_down(amp_times_total * sum_, AMP_PRECISION) + d_p * num_tokens) * invariant,
            _math_div_down((amp_times_total - AMP_PRECISION) * invariant, AMP_PRECISION)
            + (num_tokens + 1) * d_p,
        )

        if invariant > prev_invariant:
            if invariant - prev_invariant <= 1:
                return invariant
        elif prev_invariant - invariant <= 1:
            return invariant

    msg = "STABLE_INVARIANT_DIDNT_CONVERGE"
    raise EVMRevertError(error=msg)


# ---------- Deployed-contract invariant ----------


def _calculate_invariant_deployed(
    amplification_parameter: int,
    balances: list[int],
    *,
    round_up: bool,
) -> int:
    """
    Computes the invariant using the DEPLOYED contract's implementation.

    This matches the deployed MetaStablePool and ComposableStablePool contracts exactly.
    The deployed contracts use a `roundUp` parameter and P_D accumulation
    (n^n * P / D^(n-1)), as opposed to the monorepo's D_P accumulation with
    always-round-down.

    The deployed MetaStablePool calls this with round_up=True for swaps.
    The deployed ComposableStablePool also calls with round_up=True for swaps.
    """
    sum_ = 0
    num_tokens = len(balances)
    for i in range(num_tokens):
        sum_ += balances[i]
    if sum_ == 0:
        return 0

    prev_invariant = 0
    invariant = sum_
    amp_times_total = amplification_parameter * num_tokens

    for _ in range(255):
        # P_D accumulation: n^n * P / D^(n-1)
        # built as P_D *= balance * n / D with round_up rounding
        p_d = balances[0] * num_tokens
        for j in range(1, num_tokens):
            p_d = _math_div(
                _math_mul(p_d * balances[j], num_tokens),
                invariant,
                round_up,
            )

        prev_invariant = invariant

        invariant = _math_div(
            _math_mul(num_tokens * invariant, invariant)
            + _math_div(_math_mul(amp_times_total, sum_) * p_d, AMP_PRECISION, round_up),
            _math_mul((num_tokens + 1), invariant)
            + _math_div(
                _math_mul(amp_times_total - AMP_PRECISION, p_d),
                AMP_PRECISION,
                not round_up,
            ),
            round_up,
        )

        if invariant > prev_invariant:
            if invariant - prev_invariant <= 1:
                return invariant
        elif prev_invariant - invariant <= 1:
            return invariant

    msg = "STABLE_INVARIANT_DIDNT_CONVERGE"
    raise EVMRevertError(error=msg)


def _calc_out_given_in(  # noqa: PLR0917
    amplification_parameter: int,
    balances: list[int],
    token_index_in: int,
    token_index_out: int,
    token_amount_in: int,
    invariant: int,
) -> int:
    """
    Computes how many tokens can be taken out of a pool if tokenAmountIn are sent,
    given the current balances and invariant.

    Amount out, so we round down overall.
    """
    # Add tokenAmountIn to the input balance
    balances[token_index_in] += token_amount_in

    final_balance_out = _get_token_balance_given_invariant_and_all_other_balances(
        amplification_parameter,
        balances,
        invariant,
        token_index_out,
    )

    # Restore the balance (no checked arithmetic needed since we just added)
    balances[token_index_in] -= token_amount_in

    # _getTokenBalanceGivenInvariantAndAllOtherBalances rounds up, so the balance out
    # is rounded down. We subtract 1 to compensate for the overall rounding direction.
    return balances[token_index_out] - final_balance_out - 1


def _calc_in_given_out(  # noqa: PLR0917
    amplification_parameter: int,
    balances: list[int],
    token_index_in: int,
    token_index_out: int,
    token_amount_out: int,
    invariant: int,
) -> int:
    """
    Computes how many tokens must be sent to a pool if tokenAmountOut are sent,
    given the current balances and invariant.

    Amount in, so we round up overall.
    """
    # Subtract tokenAmountOut from the output balance
    balances[token_index_out] -= token_amount_out

    final_balance_in = _get_token_balance_given_invariant_and_all_other_balances(
        amplification_parameter,
        balances,
        invariant,
        token_index_in,
    )

    # Restore the balance
    balances[token_index_out] += token_amount_out

    # _getTokenBalanceGivenInvariantAndAllOtherBalances rounds up, so the balance in
    # is rounded up. We add 1 to compensate for the overall rounding direction.
    return final_balance_in - balances[token_index_in] + 1


def _get_token_balance_given_invariant_and_all_other_balances(
    amplification_parameter: int,
    balances: list[int],
    invariant: int,
    token_index: int,
) -> int:
    """
    Calculates the balance of a given token (tokenIndex) given all the other
    balances and the invariant, using Newton-Raphson iteration.

    Rounds result up overall.
    """
    n = len(balances)
    amp_times_total = amplification_parameter * n

    sum_ = balances[0]
    p_d = balances[0] * n
    for j in range(1, n):
        p_d = _math_div_down(p_d * balances[j] * n, invariant)
        sum_ += balances[j]
    # No need for safe math — sum >= balances[tokenIndex]
    sum_ -= balances[token_index]

    inv2 = invariant * invariant
    # Remove the balance from c by multiplying it
    c = _math_div_up(inv2, amp_times_total * p_d) * AMP_PRECISION * balances[token_index]
    b = sum_ + _math_div_down(invariant, amp_times_total) * AMP_PRECISION

    # Iterate to find the balance
    prev_token_balance = 0
    # Multiply the first iteration outside the loop with the invariant to set the
    # initial approximation value
    token_balance = _math_div_up(inv2 + c, invariant + b)

    for _ in range(255):
        prev_token_balance = token_balance

        token_balance = _math_div_up(
            token_balance * token_balance + c,
            token_balance * 2 + b - invariant,
        )

        if token_balance > prev_token_balance:
            if token_balance - prev_token_balance <= 1:
                return token_balance
        elif prev_token_balance - token_balance <= 1:
            return token_balance

    msg = "STABLE_GET_BALANCE_DIDNT_CONVERGE"
    raise EVMRevertError(error=msg)


def _calc_bpt_out_given_exact_tokens_in(  # noqa: PLR0917
    amp: int,
    balances: list[int],
    amounts_in: list[int],
    bpt_total_supply: int,
    current_invariant: int,
    swap_fee_percentage: int,
) -> int:
    """Calculate BPT out for exact tokens in, rounding down (FixedPoint operations)."""
    sum_balances = 0
    for i in range(len(balances)):
        sum_balances += balances[i]

    # Calculate the weighted balance ratio without considering fees
    balance_ratios_with_fee: list[int] = [0] * len(amounts_in)
    invariant_ratio_with_fees = 0
    for i in range(len(balances)):
        current_weight = div_down(balances[i], sum_balances)
        balance_ratios_with_fee[i] = div_down(balances[i] + amounts_in[i], balances[i])
        invariant_ratio_with_fees += mul_down(balance_ratios_with_fee[i], current_weight)

    # Second loop: calculate new amounts in, taking into account the fee
    new_balances: list[int] = [0] * len(balances)
    for i in range(len(balances)):
        if balance_ratios_with_fee[i] > invariant_ratio_with_fees:
            non_taxable_amount = mul_down(
                balances[i],
                invariant_ratio_with_fees - ONE,
            )
            taxable_amount = amounts_in[i] - non_taxable_amount
            # Swap fee is guaranteed to be lower than 50%
            amount_in_without_fee = non_taxable_amount + mul_down(
                taxable_amount, ONE - swap_fee_percentage
            )
        else:
            amount_in_without_fee = amounts_in[i]

        new_balances[i] = balances[i] + amount_in_without_fee

    # Get new invariant
    new_invariant = _calculate_invariant(amp, new_balances)
    invariant_ratio = div_down(new_invariant, current_invariant)

    if invariant_ratio > ONE:
        return mul_down(bpt_total_supply, invariant_ratio - ONE)
    return 0


def _calc_token_in_given_exact_bpt_out(  # noqa: PLR0917
    amp: int,
    balances: list[int],
    token_index: int,
    bpt_amount_out: int,
    bpt_total_supply: int,
    current_invariant: int,
    swap_fee_percentage: int,
) -> int:
    """Calculate token in for exact BPT out, rounding up (FixedPoint operations)."""
    new_invariant = mul_up(
        div_up(bpt_total_supply + bpt_amount_out, bpt_total_supply),
        current_invariant,
    )

    new_balance_token_index = _get_token_balance_given_invariant_and_all_other_balances(
        amp,
        balances,
        new_invariant,
        token_index,
    )
    amount_in_without_fee = new_balance_token_index - balances[token_index]

    sum_balances = 0
    for i in range(len(balances)):
        sum_balances += balances[i]

    current_weight = div_down(balances[token_index], sum_balances)
    taxable_percentage = complement(current_weight)
    taxable_amount = mul_up(amount_in_without_fee, taxable_percentage)
    non_taxable_amount = amount_in_without_fee - taxable_amount

    # Swap fee is guaranteed to be lower than 50%
    return non_taxable_amount + div_up(taxable_amount, ONE - swap_fee_percentage)


def _calc_bpt_in_given_exact_tokens_out(  # noqa: PLR0917
    amp: int,
    balances: list[int],
    amounts_out: list[int],
    bpt_total_supply: int,
    current_invariant: int,
    swap_fee_percentage: int,
) -> int:
    """Calculate BPT in for exact tokens out, rounding up (FixedPoint operations)."""
    sum_balances = 0
    for i in range(len(balances)):
        sum_balances += balances[i]

    # Calculate the weighted balance ratio without considering fees
    balance_ratios_without_fee: list[int] = [0] * len(amounts_out)
    invariant_ratio_without_fees = 0
    for i in range(len(balances)):
        current_weight = div_up(balances[i], sum_balances)
        balance_ratios_without_fee[i] = div_up(
            balances[i] - amounts_out[i],
            balances[i],
        )
        invariant_ratio_without_fees += mul_up(balance_ratios_without_fee[i], current_weight)

    # Second loop: calculate new amounts, taking into account the fee
    new_balances: list[int] = [0] * len(balances)
    for i in range(len(balances)):
        amount_out_with_fee: int
        if invariant_ratio_without_fees > balance_ratios_without_fee[i]:
            non_taxable_amount = mul_down(
                balances[i],
                complement(invariant_ratio_without_fees),
            )
            taxable_amount = amounts_out[i] - non_taxable_amount
            # Swap fee is guaranteed to be lower than 50%
            amount_out_with_fee = non_taxable_amount + div_up(
                taxable_amount, ONE - swap_fee_percentage
            )
        else:
            amount_out_with_fee = amounts_out[i]

        new_balances[i] = balances[i] - amount_out_with_fee

    # Get current and new invariants
    new_invariant = _calculate_invariant(amp, new_balances)
    invariant_ratio = div_down(new_invariant, current_invariant)

    # Return amountBPTIn
    return mul_up(bpt_total_supply, complement(invariant_ratio))


def _calc_token_out_given_exact_bpt_in(  # noqa: PLR0917
    amp: int,
    balances: list[int],
    token_index: int,
    bpt_amount_in: int,
    bpt_total_supply: int,
    current_invariant: int,
    swap_fee_percentage: int,
) -> int:
    """Calculate token out for exact BPT in, rounding down (FixedPoint operations)."""
    new_invariant = mul_up(
        div_up(bpt_total_supply - bpt_amount_in, bpt_total_supply),
        current_invariant,
    )

    new_balance_token_index = _get_token_balance_given_invariant_and_all_other_balances(
        amp,
        balances,
        new_invariant,
        token_index,
    )
    amount_out_without_fee = balances[token_index] - new_balance_token_index

    sum_balances = 0
    for i in range(len(balances)):
        sum_balances += balances[i]

    current_weight = div_down(balances[token_index], sum_balances)
    taxable_percentage = complement(current_weight)

    # Swap fees applied to 'token out' — results in slightly larger price impact
    taxable_amount = mul_up(amount_out_without_fee, taxable_percentage)
    non_taxable_amount = amount_out_without_fee - taxable_amount

    # Swap fee is guaranteed to be lower than 50%
    return non_taxable_amount + mul_down(taxable_amount, ONE - swap_fee_percentage)


def _calc_tokens_out_given_exact_bpt_in(
    balances: list[int],
    bpt_amount_in: int,
    bpt_total_supply: int,
) -> list[int]:
    """Calculate tokens out for exact BPT in, proportionally (FixedPoint operations)."""
    bpt_ratio = div_down(bpt_amount_in, bpt_total_supply)

    return [mul_down(balances[i], bpt_ratio) for i in range(len(balances))]


def _calc_due_token_protocol_swap_fee_amount(
    amplification_parameter: int,
    balances: list[int],
    last_invariant: int,
    token_index: int,
    protocol_swap_fee_percentage: int,
) -> int:
    """Calculate due token protocol swap fee amount, rounding down."""
    final_balance_fee_token = _get_token_balance_given_invariant_and_all_other_balances(
        amplification_parameter,
        balances,
        last_invariant,
        token_index,
    )

    if balances[token_index] <= final_balance_fee_token:
        # This shouldn't happen outside of rounding errors, but have this safeguard
        return 0

    # Result is rounded down
    accumulated_token_swap_fees = balances[token_index] - final_balance_fee_token
    return mul_down(accumulated_token_swap_fees, protocol_swap_fee_percentage)


def _get_rate(
    balances: list[int],
    amp: int,
    supply: int,
) -> int:
    """Get the BPT rate, rounding down."""
    invariant = _calculate_invariant(amp, balances)
    return div_down(invariant, supply)


# ---------- Internal helpers matching Solidity's Math library ----------


def _math_div_down(a: int, b: int) -> int:
    """Plain integer division rounding down, matching Solidity's Math.divDown."""
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    return a // b


def _math_div_up(a: int, b: int) -> int:
    """Plain integer division rounding up, matching Solidity's Math.divUp."""
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    if a == 0:
        return 0
    return 1 + (a - 1) // b


def _math_mul(a: int, b: int) -> int:
    """Plain integer multiplication, matching Solidity's Math.mul."""
    return a * b


def _math_div(a: int, b: int, round_up: bool) -> int:  # noqa: FBT001
    """
    Plain integer division with rounding direction, matching Solidity's Math.div.

    When round_up is True, rounds up (Math.divUp). When False, rounds down (Math.divDown).
    """
    if b == 0:
        raise EVMRevertError(error="ZERO_DIVISION")
    if round_up:
        return _math_div_up(a, b)
    return _math_div_down(a, b)
