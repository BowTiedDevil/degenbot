"""Stub for the dynamically-created ``degenbot._ffi.solidly_math`` submodule.

Created at runtime by ``add_solidly_math_module`` in the PyO3 wrapper crate
(``degenbot-python/src/solidly_math/lib.rs``). The functions are thin PyO3
wrappers over the pure-Rust ``degenbot-solidly-math`` core crate.
"""

def calc_d(x0: int, y: int) -> int: ...
def calc_k(balance_0: int, balance_1: int, decimals_0: int, decimals_1: int) -> int: ...
def calc_f(x0: int, y: int) -> int: ...
def camelot_f(x0: int, y: int) -> int: ...
def camelot_k(balance_0: int, balance_1: int, decimals_0: int, decimals_1: int) -> int: ...
def get_y_solidly(x0: int, xy: int, y: int, decimals_0: int, decimals_1: int) -> int: ...
def camelot_get_y_camelot(x_0: int, xy: int, y: int) -> int: ...
def calc_exact_in_volatile(
    amount_in: int,
    token_in: int,
    reserves_0: int,
    reserves_1: int,
    fee_numer: int,
    fee_denom: int,
) -> int: ...
def calc_exact_in_stable_solidly(
    amount_in: int,
    token_in: int,
    reserves_0: int,
    reserves_1: int,
    decimals_0: int,
    decimals_1: int,
    fee_numer: int,
    fee_denom: int,
) -> int: ...
def calc_exact_out_stable_solidly(
    amount_out: int,
    token_in: int,
    reserves_0: int,
    reserves_1: int,
    decimals_0: int,
    decimals_1: int,
    fee_numer: int,
    fee_denom: int,
) -> int: ...
def calc_exact_in_stable_camelot(
    amount_in: int,
    token_in: int,
    reserves_0: int,
    reserves_1: int,
    decimals_0: int,
    decimals_1: int,
    fee_numer: int,
    fee_denom: int,
) -> int: ...

__all__ = [
    "calc_d",
    "calc_exact_in_stable_camelot",
    "calc_exact_in_stable_solidly",
    "calc_exact_in_volatile",
    "calc_exact_out_stable_solidly",
    "calc_f",
    "calc_k",
    "camelot_f",
    "camelot_get_y_camelot",
    "camelot_k",
    "get_y_solidly",
]
