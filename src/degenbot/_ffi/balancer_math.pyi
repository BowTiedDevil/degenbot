def weighted_calculate_invariant(
    normalized_weights: list[int],
    balances: list[int],
    version: int,
) -> int: ...
def weighted_calc_out_given_in(
    balance_in: int,
    weight_in: int,
    balance_out: int,
    weight_out: int,
    amount_in: int,
    version: int,
) -> int: ...
def weighted_calc_in_given_out(
    balance_in: int,
    weight_in: int,
    balance_out: int,
    weight_out: int,
    amount_out: int,
    version: int,
) -> int: ...
def weighted_subtract_swap_fee_amount(amount: int, fee_percentage: int) -> int: ...
def weighted_add_swap_fee_amount(amount: int, fee_percentage: int) -> int: ...
def fixed_point_mul_down(a: int, b: int) -> int: ...
def fixed_point_div_down(a: int, b: int) -> int: ...
def fixed_point_div_up(a: int, b: int) -> int: ...
def stable_calculate_invariant(amp: int, balances: list[int]) -> int: ...
def stable_calculate_invariant_deployed(
    amp: int,
    balances: list[int],
    round_up: bool,
) -> int: ...
def stable_calc_out_given_in(
    amp: int,
    balances: list[int],
    token_index_in: int,
    token_index_out: int,
    token_amount_in: int,
    invariant: int,
) -> int: ...
def stable_calc_in_given_out(
    amp: int,
    balances: list[int],
    token_index_in: int,
    token_index_out: int,
    token_amount_out: int,
    invariant: int,
) -> int: ...

__all__ = [
    "fixed_point_div_down",
    "fixed_point_div_up",
    "fixed_point_mul_down",
    "stable_calc_in_given_out",
    "stable_calc_out_given_in",
    "stable_calculate_invariant",
    "stable_calculate_invariant_deployed",
    "weighted_add_swap_fee_amount",
    "weighted_calc_in_given_out",
    "weighted_calc_out_given_in",
    "weighted_calculate_invariant",
    "weighted_subtract_swap_fee_amount",
]
