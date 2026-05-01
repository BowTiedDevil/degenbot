"""Cross-cutting math utilities used by multiple solvers."""

import math
from dataclasses import dataclass
from typing import Any

from degenbot.degenbot_rs import mobius as _rs_mobius
from degenbot.types.hop_types import BoundedProductHop, HopType
from degenbot.uniswap.v3_libraries.constants import Q96


def _infer_zero_for_one(v3_hop: BoundedProductHop) -> bool:
    """Infer swap direction from BoundedProductHop data.

    Uses the stored zero_for_one if available. Otherwise computes it
    from the reserve ratio vs the expected ratio from L/sqrt_price.

    For properly-constructed reserves (from _v3_virtual_reserves):
    - zero_for_one=True: reserve_in/reserve_out = R0/R1 = 1/sqrt_p²
    - zero_for_one=False: reserve_in/reserve_out = R1/R0 = sqrt_p²

    The ratio comparison is scale-invariant (works regardless of Q96 scaling).
    """
    if v3_hop.zero_for_one is not None:
        return v3_hop.zero_for_one

    sqrt_p = float(v3_hop.sqrt_price) / Q96
    price = sqrt_p * sqrt_p
    reserve_ratio = float(v3_hop.reserve_in) / float(v3_hop.reserve_out)
    return abs(reserve_ratio - 1.0 / price) < abs(reserve_ratio - price)


def _hop_to_float_state(hop: HopType) -> tuple[float, float, float]:
    """Convert any Hop variant to (reserve_in, reserve_out, gamma) as floats."""
    return float(hop.reserve_in), float(hop.reserve_out), hop.gamma


@dataclass(frozen=True, slots=True)
class _MobiusCoefficients:
    """
    Internal Möbius coefficients l(x) = K*x / (M + N*x).

    Computed from Hop data via an O(n) recurrence.
    """

    K: float
    M: float
    N: float
    is_profitable: bool

    def optimal_input(self) -> float:
        if not self.is_profitable:
            return 0.0
        return (math.sqrt(self.K * self.M) - self.M) / self.N

    def path_output(self, x: float) -> float:
        denom = self.M + self.N * x
        if denom <= 0:
            return 0.0
        return self.K * x / denom

    def profit_at(self, x: float) -> float:
        return self.path_output(x) - x


def _compute_mobius_coefficients(hops: tuple[HopType, ...]) -> _MobiusCoefficients:
    """
    Compute Möbius transformation coefficients from hops.

    The recurrence:
        Initialize: K = gamma_1 * s_1, M = r_1, N = gamma_1
        Per hop i (i >= 2):
            K_new = K * gamma_i * s_i
            M_new = M * r_i
            N_new = N * r_i + K * gamma_i   (uses K before update)
    """
    if not hops:
        return _MobiusCoefficients(K=0.0, M=1.0, N=0.0, is_profitable=False)

    r0, s0, g0 = _hop_to_float_state(hops[0])
    K = g0 * s0
    M = r0
    N = g0

    for hop in hops[1:]:
        r_i, s_i, g_i = _hop_to_float_state(hop)
        old_K = K
        K = old_K * g_i * s_i
        M *= r_i
        N = N * r_i + old_K * g_i

    is_profitable = K > M
    return _MobiusCoefficients(K=K, M=M, N=N, is_profitable=is_profitable)


def _simulate_path(x: float, hops: tuple[HopType, ...]) -> float:
    """Simulate a swap through all hops for verification.

    Supports ConstantProduct, BoundedProduct, SolidlyStable (with swap_fn),
    and CurveStableswap (with swap_fn). Falls back to constant-product
    formula when no exact swap_fn is available.
    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0.0

        # Prefer exact callable if available (Solidly, Curve, etc.)
        swap_fn = getattr(hop, "swap_fn", None)
        if swap_fn is not None:
            amount = float(swap_fn(int(amount)))
            continue

        # Default: constant-product on virtual/actual reserves
        r_i, s_i, g_i = _hop_to_float_state(hop)
        denom = r_i + amount * g_i
        if denom <= 0:
            return 0.0
        amount = amount * g_i * s_i / denom
    return amount


def _rust_integer_refinement(
    x_opt: float,
    hops: tuple[HopType, ...],
    max_input: int | None,
) -> tuple[int, int]:
    """Integer refinement in Rust using EVM-exact U256 arithmetic.

    Converts Python hops to RustIntHopState, calls py_mobius_refine_int
    to search around the float optimum with U256 simulation, and
    returns the best integer result.
    """
    if _rs_mobius is None:
        return 0, 0

    rust_int_hops: list[Any] = []
    for hop in hops:
        fee_numer = hop.fee.numerator
        fee_denom = hop.fee.denominator
        gamma_numer = fee_denom - fee_numer
        gamma_denom = fee_denom
        rust_int_hops.append(
            _rs_mobius.RustIntHopState(hop.reserve_in, hop.reserve_out, gamma_numer, gamma_denom)
        )

    max_input_float = float(max_input) if max_input is not None else None
    result = _rs_mobius.py_mobius_refine_int(x_opt, rust_int_hops, max_input_float)

    if result.success:
        return int(result.optimal_input), int(result.profit)
    return 0, 0
