"""V2 constant-product (x*y=k) swap math.

Thin `PyO3` wrappers over the `degenbot-v2-math` pure-Rust core
(`v2_swap_exact_in` / `v2_swap_exact_out`) — the EVM-exact V2
constant-product swap primitive shared by every V2-family pool. The
volatile swap calcs on the Python side (Uniswap V2 family,
Aerodrome volatile) delegate here so Python and the Rust solver round
identically (RH3L24).
"""

def calc_exact_in_v2(
    reserves_in: int, reserves_out: int, amount_in: int, fee_numer: int, fee_denom: int
) -> int: ...
def calc_exact_out_v2(
    reserves_in: int, reserves_out: int, amount_out: int, fee_numer: int, fee_denom: int
) -> int: ...

__all__ = [
    "calc_exact_in_v2",
    "calc_exact_out_v2",
]
