"""Stub for the dynamically-created ``degenbot._ffi.balancer_math`` submodule.

The submodule is created at runtime by ``add_balancer_math_module`` in the
PyO3 wrapper crate (``degenbot-python/src/balancer_math/lib.rs``) — it calls
``PyModule::new`` + ``sys.modules`` registration. This stub gives the type
checker the function signatures.

The functions are thin PyO3 wrappers over the pure-Rust ``degenbot-balancer-math``
core crate. The ``version`` discriminant (1=V1, 2=V2) is the bytecode-detected
PowVersion; ``round_up`` is the stable-invariant V1(always-roundDown)/V2(roundUp)
axis. Reverts surface as ValueError/OverflowError carrying the Solidity revert tag.
"""

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
