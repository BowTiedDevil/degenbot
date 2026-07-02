"""Cross-cutting math utilities used by multiple solvers."""

import math
from dataclasses import dataclass

from degenbot.exceptions.arbitrage import OptimizationError
from degenbot.types.hop_types import (
    BalancerMultiTokenHop,
    BoundedProductHop,
    CurveStableswapHop,
    HopType,
    SolidlyStableHop,
)
from degenbot.uniswap.v3_libraries.constants import Q96


def _infer_zero_for_one(v3_hop: BoundedProductHop) -> bool:
    (
        """Infer swap direction from BoundedProductHop data.

    Uses the stored zero_for_one if available. Otherwise computes it
    from the reserve ratio vs the expected ratio from L/sqrt_price.

    For properly-constructed reserves (from _v3_virtual_reserves):
    - zero_for_one=True: reserve_in/reserve_out = R0/R1 = 1/sqrt_p²
    - zero_for_one=False: reserve_in/reserve_out = R1/R0 = sqrt_p²

    The ratio comparison is scale-invariant (works regardless of Q96 scaling).

    Returns:
        The computed value.

    Raises:
        OptimizationError: If reserve_out is zero.

    """
        ""
    )
    if v3_hop.zero_for_one is not None:
        return v3_hop.zero_for_one

    sqrt_p = float(v3_hop.sqrt_price) / Q96
    price = sqrt_p * sqrt_p
    if v3_hop.reserve_out == 0:
        msg = "reserve_out is zero, cannot infer zero_for_one"
        raise OptimizationError(
            message=msg,
            iterations=0,
            method="INFER_ZERO_FOR_ONE",
        )
    reserve_ratio = float(v3_hop.reserve_in) / float(v3_hop.reserve_out)
    return abs(reserve_ratio - 1.0 / price) < abs(reserve_ratio - price)


def _hop_to_float_state(hop: HopType) -> tuple[float, float, float]:
    """Convert any pairwise Hop variant to (reserve_in, reserve_out, gamma) as floats.

    Returns:
        The computed value.

    Raises:
        TypeError: If the operation fails.

    """
    if isinstance(hop, BalancerMultiTokenHop):
        msg = "BalancerMultiTokenHop has no pairwise reserves"
        raise TypeError(msg)
    return float(hop.reserve_in), float(hop.reserve_out), hop.gamma


@dataclass(frozen=True, slots=True)
class _MobiusCoefficients:
    """Internal Möbius coefficients l(x) = K*x / (M + N*x).

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
    """Compute Möbius transformation coefficients from hops.

    The recurrence:
        Initialize: K = gamma_1 * s_1, M = r_1, N = gamma_1
        Per hop i (i >= 2):
            K_new = K * gamma_i * s_i
            M_new = M * r_i
            N_new = N * r_i + K * gamma_i   (uses K before update)

    Returns:
        The computed value.

    """
    if not hops:
        return _MobiusCoefficients(K=0.0, M=1.0, N=0.0, is_profitable=False)

    r0, s0, g0 = _hop_to_float_state(hops[0])
    k = g0 * s0
    m = r0
    n = g0

    for hop in hops[1:]:
        r_i, s_i, g_i = _hop_to_float_state(hop)
        old_k = k
        k = old_k * g_i * s_i
        m *= r_i
        n = n * r_i + old_k * g_i

    is_profitable = k > m
    return _MobiusCoefficients(K=k, M=m, N=n, is_profitable=is_profitable)


def _simulate_path(x: float, hops: tuple[HopType, ...]) -> float:
    """Simulate a swap through all hops for verification.

    Supports ConstantProduct, BoundedProduct, SolidlyStable (with swap_fn),
    and CurveStableswap (with swap_fn). Falls back to constant-product
    formula when no exact swap_fn is available.

    Returns:
        The computed value.

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


# --- Mixed-hop path simulators (moved from the retired solidly_stable.py) ---
# These dispatch across V2 / Solidly / Curve hop types, preferring an exact
# integer ``swap_fn`` when present and falling back to a float approximation.
# Solidly-specific float fallback (``_solidly_swap_output_float``) is kept
# here so the no-swap_fn Solidly branch keeps its prior behavior.

_CURVATURE_THRESHOLD = 1e-30
_NEWTON_CONVERGENCE_TOLERANCE = 1e-3


def _solidly_swap_output_float(
    *,
    reserve_in: float,
    reserve_out: float,
    amount_in: float,
    gamma: float,
    decimals_in: int,
    decimals_out: int,
) -> float:
    """Float approximation of Solidly stable swap output.

    Solves the Solidly invariant x³y + xy³ ≥ k for output given input.
    Uses Newton's method on the implicit equation f(y) = x³y + xy³ - k = 0.

    The reserves and amounts are scaled to 18-decimal internally, matching
    the Solidity contract behavior.

    Returns:
        The computed value.

    """
    if amount_in <= 0:
        return 0.0
    if reserve_in <= 0 or reserve_out <= 0:
        return 0.0

    d_in = 10**decimals_in
    d_out = 10**decimals_out
    scale = 1e18

    # Scale reserves to 18-decimal
    r0_scaled = reserve_in * scale / d_in
    r1_scaled = reserve_out * scale / d_out

    # Apply fee
    amount_after_fee = amount_in * gamma

    # Scale amount to 18-decimal
    a_scaled = amount_after_fee * scale / d_in

    # New x (input reserve after deposit)
    x_new = r0_scaled + a_scaled

    # Compute k at original reserves: k = xy * (x² + y²) / 10^18
    # In 18-decimal space: k = (r0 * r1 / 1e18) * ((r0² / 1e18) + (r1² / 1e18)) / 1e18
    xy = r0_scaled * r1_scaled / scale
    x2_y2 = (r0_scaled**2 / scale) + (r1_scaled**2 / scale)
    k = xy * x2_y2 / scale

    if k <= 0:
        return 0.0

    # Solve for y_new such that: x_new³ * y_new + x_new * y_new³ = k
    # f(y) = (x³/1e36) * y + x * (y³/1e36) - k = 0
    # f'(y) = (x³/1e36) + 3x * (y²/1e36)
    x3 = x_new**3 / (scale * scale)
    y = r1_scaled  # initial guess

    for _ in range(100):
        y3 = y**3 / (scale * scale)
        f_y = x3 * y + x_new * y3 - k
        y2 = y**2 / scale
        f_prime = x3 + 3.0 * x_new * y2

        if abs(f_prime) < _CURVATURE_THRESHOLD:
            break

        dy = f_y / f_prime
        y -= dy

        # y must be positive
        if y <= 0:
            y = 1.0
            break

        if abs(dy) < _NEWTON_CONVERGENCE_TOLERANCE:
            break

    # Output = old reserve - new reserve
    output_scaled = r1_scaled - y
    if output_scaled <= 0:
        return 0.0

    # Descale from 18-decimal
    output = output_scaled * d_out / scale
    return float(max(output, 0.0))


def _simulate_mixed_path(
    x: float,
    hops: tuple[HopType, ...],
) -> float:
    """Simulate a path with mixed hop types using float math.

    For each hop:
    - ConstantProductHop: V2 formula y = gamma*s*x / (r + gamma*x)
    - BoundedProductHop: Same V2 formula (virtual reserves)
    - SolidlyStableHop: float approximation of Solidly swap
    - CurveStableswapHop: uses swap_fn if available

    For integer-exact evaluation, use ``_simulate_mixed_path_int`` instead.

    Returns:
        The computed value.

    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0.0

        if isinstance(hop, SolidlyStableHop):
            # Prefer exact callable if available
            if hop.swap_fn is not None:
                amount = float(hop.swap_fn(int(amount)))
                continue
            amount = _solidly_swap_output_float(
                reserve_in=float(hop.reserve_in),
                reserve_out=float(hop.reserve_out),
                amount_in=amount,
                gamma=hop.gamma,
                decimals_in=hop.decimals_in,
                decimals_out=hop.decimals_out,
            )

        elif isinstance(hop, CurveStableswapHop):
            # Prefer exact callable if available
            if hop.swap_fn is not None:
                amount = float(hop.swap_fn(int(amount)))
                continue
            # Fall back to float approximation
            r_i = float(hop.reserve_in)
            s_i = float(hop.reserve_out)
            g_i = 1.0 - float(hop.fee)
            denom = r_i + amount * g_i
            if denom <= 0:
                return 0.0
            amount = amount * g_i * s_i / denom

        elif not isinstance(hop, BalancerMultiTokenHop):
            r_i = float(hop.reserve_in)
            s_i = float(hop.reserve_out)
            g_i = hop.gamma
            denom = r_i + amount * g_i
            if denom <= 0:
                return 0.0
            amount = amount * g_i * s_i / denom

        else:
            # Unsupported invariant
            return 0.0

    return amount


def _simulate_mixed_path_int(
    x: int,
    hops: tuple[HopType, ...],
) -> int:
    """Simulate a path with mixed hop types using integer math.

    For hops with ``swap_fn`` (Solidly, Curve), uses the integer-accurate callable.
    For V2 hops, uses integer constant-product formula.
    Falls back to float for hops without integer support.

    Returns:
        The computed value.

    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0

        if isinstance(hop, SolidlyStableHop):
            # Prefer exact callable if available
            if hop.swap_fn is not None:
                amount = hop.swap_fn(amount)
                continue
            # Fall back to float (less accurate)
            out = _solidly_swap_output_float(
                reserve_in=float(hop.reserve_in),
                reserve_out=float(hop.reserve_out),
                amount_in=float(amount),
                gamma=hop.gamma,
                decimals_in=hop.decimals_in,
                decimals_out=hop.decimals_out,
            )
            amount = int(out)

        elif isinstance(hop, CurveStableswapHop):
            # Prefer exact callable if available
            if hop.swap_fn is not None:
                amount = hop.swap_fn(amount)
                continue
            # Fall back to float approximation
            r_i = float(hop.reserve_in)
            s_i = float(hop.reserve_out)
            g_i = 1.0 - float(hop.fee)
            denom = r_i + float(amount) * g_i
            if denom <= 0:
                return 0
            amount = int(float(amount) * g_i * s_i / denom)

        elif not isinstance(hop, BalancerMultiTokenHop):
            r_i = hop.reserve_in
            s_i = hop.reserve_out
            g_num = hop.fee.denominator - hop.fee.numerator
            g_den = hop.fee.denominator
            # V2 formula: y = (gamma * s * x) / (r + gamma * x)
            gamma_x = amount * g_num // g_den
            denom = r_i + gamma_x
            if denom <= 0:
                return 0
            amount = gamma_x * s_i // denom

        else:
            return 0

    return amount
