"""Stub for the dynamically-created ``degenbot._ffi.curve_dy`` submodule.

The submodule is created at runtime by ``add_curve_dy_module`` in the PyO3
wrapper crate (``degenbot-python/src/curve_dy/lib.rs``). This stub gives the
type checker the class + function signatures.

``DyCalculationInputs`` is a mutable builder the Python companion
(``CurveStableswapPool._calculate_dy_via_rust``) fills; the calculator is the
pure Rust ``degenbot-curve-math`` core (task ``CNEP47``). Variant discriminants
are 1-based ``auto()`` enum ``.value`` s.
"""

class DyCalculationInputs:
    precision: int
    fee_denominator: int
    fee: int
    n_coins: int
    balances: list[int]
    rate_multipliers: list[int]
    precision_multipliers: list[int]
    offpeg_fee_multiplier: int
    fee_gamma: int
    mid_fee: int
    out_fee: int
    address: str
    resolved_rates: list[int]
    xp: list[int]
    block_number: int
    block_timestamp: int
    amp: int
    d_variant: int
    y_variant: int
    a_precision: int
    swap_style: int
    metapool: bool
    metapool_rate_style: int
    metapool_underlying_style: int
    d: int | None
    gamma: int | None
    price_scale: list[int] | None
    effective_balances: list[int] | None
    virtual_price: int | None
    scaled_redemption_price: int | None

    def __init__(self) -> None: ...

def calculate_dy(i: int, j: int, dx: int, inputs: DyCalculationInputs) -> int: ...
def calculate_dy_underlying(
    i: int,
    j: int,
    dx: int,
    inputs: DyCalculationInputs,
    base: object,
) -> int: ...

__all__ = [
    "DyCalculationInputs",
    "calculate_dy",
    "calculate_dy_underlying",
]
