"""
Unified solver interface for arbitrage optimization.

All optimizers accept the same `SolveInput` (a sequence of `Hop` objects)
and return the same `SolveResult`. The `ArbSolver` dispatcher automatically
selects the best method based on the hop types.

Quick Start:
===========

>>> from degenbot.arbitrage.optimizers.solver import ArbSolver, Hop, SolveInput
>>> solver = ArbSolver()
>>> hops = (
...     Hop(reserve_in=2_000_000_000_000, reserve_out=1_000_000_000_000_000_000, fee=Fraction(3, 1000)),
...     Hop(reserve_in=1_500_000_000_000, reserve_out=800_000_000_000_000_000, fee=Fraction(3, 1000)),
... )
>>> result = solver.solve(SolveInput(hops=hops))
>>> if result.success:
...     print(f"Optimal: {result.optimal_input}, Profit: {result.profit}, Method: {result.method}")

Performance:
============

| Method | Time | Use Case |
|--------|------|----------|
| Mobius | 0.86μs (Py), 0.19μs (Rust) | All V2, V3 single-range (zero iterations) |
| Newton | 7.5μs | V2-V2 fallback |
| PiecewiseMobius | ~25μs | V3 multi-range with tick crossing |
| Brent | ~194μs | V3-V3 complex fallback |
"""

import math
import os
import time
from abc import ABC, abstractmethod
from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum
from fractions import Fraction
from typing import Any

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.arbitrage.optimizers.balancer_weighted import (
    BalancerMultiTokenState as _BalancerMultiTokenState,
)
from degenbot.arbitrage.optimizers.balancer_weighted import (
    BalancerWeightedPoolSolver as _BalancerWeightedPoolSolver,
)
from degenbot.arbitrage.optimizers.mobius import (
    HopState as _MobiusHopState,
)
from degenbot.arbitrage.optimizers.mobius import (
    V3TickRangeHop as _V3TickRangeHop,
)
from degenbot.arbitrage.optimizers.mobius import (
    V3TickRangeSequence as _V3TickRangeSequence,
)
from degenbot.arbitrage.optimizers.mobius import (
    compute_mobius_coefficients as _compute_mobius_coefficients,
)
from degenbot.arbitrage.optimizers.mobius import (
    mobius_solve as _mobius_solve,
)
from degenbot.arbitrage.optimizers.mobius import (
    simulate_path as _mobius_simulate_path,
)
from degenbot.camelot.pools import CamelotLiquidityPool
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_libraries.tick_bitmap import gen_ticks
from degenbot.uniswap.v3_libraries.tick_math import MAX_TICK, MIN_TICK, get_sqrt_ratio_at_tick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

# Feature flag: when True, RustArbSolver.solve() receives RustIntHopState objects
# and does float solve + U256 integer refinement in a single Rust call.
# When False, falls back to the old two-step approach (float tuples then
# py_mobius_refine_int separately).
USE_MERGED_INT_REFINEMENT = bool(os.environ.get("DEGENBOT_MERGED_INT_REFINEMENT", "1"))

# Feature flag: when True, pass a flat int array to RustArbSolver.solve_raw()
# instead of creating RustIntHopState Python objects. This eliminates
# ~2.5μs of Python object construction overhead per solve call.
# When False, falls back to RustIntHopState objects.
USE_RAW_ARRAY_MARSHALLING = bool(os.environ.get("DEGENBOT_RAW_ARRAY_MARSHALLING", "1"))

# Feature flag: when True, ArbSolver.solve() delegates to the generalized
# MobiusSolver from degenbot.arbitrage.solver. When False, uses the existing
# inline dispatch code.
USE_GENERALIZED_SOLVER = bool(os.environ.get("DEGENBOT_GENERALIZED_SOLVER", ""))


# ---------------------------------------------------------------------------
# Core Types
# ---------------------------------------------------------------------------


class SolverMethod(Enum):
    """Solver algorithm used to produce a result."""

    MOBIUS = "mobius"
    NEWTON = "newton"
    PIECEWISE_MOBIUS = "piecewise_mobius"
    SOLIDLY_STABLE = "solidly_stable"
    BALANCER_MULTI_TOKEN = "balancer_multi_token"
    BRENT = "brent"


class PoolInvariant(Enum):
    """Pool invariant type for a hop."""

    CONSTANT_PRODUCT = "constant_product"
    BOUNDED_PRODUCT = "bounded_product"
    SOLIDLY_STABLE = "solidly_stable"
    BALANCER_WEIGHTED = "balancer_weighted"
    BALANCER_MULTI_TOKEN = "balancer_multi_token"
    CURVE_STABLESWAP = "curve_stableswap"


@dataclass(frozen=True, slots=True)
class ConstantProductHop:
    """
    A constant-product (x*y=k) pool hop.

    For V2 pools: UniswapV2Pool, AerodromeV2Pool (volatile), CamelotLiquidityPool.
    Supports asymmetric fees via fee_out (Camelot has different fees per direction).

    Attributes
    ----------
    reserve_in : int
        Input reserve in wei.
    reserve_out : int
        Output reserve in wei.
    fee : Fraction
        Fee for the input direction as an exact fraction.
    fee_out : Fraction | None
        Fee for the output direction (None if same as fee). Used by
        Camelot and other pools with asymmetric fees.
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction
    fee_out: Fraction | None = None
    invariant: PoolInvariant = PoolInvariant.CONSTANT_PRODUCT

    @property
    def is_v2(self) -> bool:
        return True

    @property
    def is_v3(self) -> bool:
        return False

    @property
    def gamma(self) -> float:
        """Fee multiplier (1 - fee) as float."""
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class V3TickRangeInfo:
    """
    Information about a V3/V4 tick range for multi-range support.

    Attributes
    ----------
    tick_lower : int
        Lower tick bound of this range.
    tick_upper : int
        Upper tick bound of this range.
    liquidity : int
        Liquidity in this range.
    sqrt_price_lower : int
        Lower sqrt price bound (X96).
    sqrt_price_upper : int
        Upper sqrt price bound (X96).
    """

    tick_lower: int
    tick_upper: int
    liquidity: int
    sqrt_price_lower: int
    sqrt_price_upper: int


@dataclass(frozen=True, slots=True)
class BoundedProductHop:
    """
    A bounded-product (concentrated liquidity) pool hop for V3/V4.

    V3/V4 tick ranges are bounded product CFMMs with effective reserves
    (R0+alpha, R1+beta) that follow the same Möbius form.

    For multi-range support (tick crossings), tick_ranges contains adjacent
    ranges and current_range_index indicates which range contains the current
    price. When tick_ranges is None, the hop represents a single range.

    Attributes
    ----------
    reserve_in : int
        Effective input reserve in wei.
    reserve_out : int
        Effective output reserve in wei.
    fee : Fraction
        Fee as an exact fraction.
    liquidity : int
        V3/V4 liquidity in the current tick range.
    sqrt_price : int
        V3/V4 current sqrt price as X96.
    tick_lower : int
        V3/V4 lower tick of the current range.
    tick_upper : int
        V3/V4 upper tick of the current range.
    tick_ranges : tuple[V3TickRangeInfo, ...] | None
        Optional adjacent tick ranges for multi-range (tick crossing) support.
        When provided, includes all ranges that might be crossed in a swap.
    current_range_index : int
        Index into tick_ranges indicating which range contains current price.
        Ignored when tick_ranges is None.
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction
    liquidity: int
    sqrt_price: int
    tick_lower: int
    tick_upper: int
    tick_ranges: tuple[V3TickRangeInfo, ...] | None = None
    current_range_index: int = 0
    invariant: PoolInvariant = PoolInvariant.BOUNDED_PRODUCT

    @property
    def is_v2(self) -> bool:
        return False

    @property
    def is_v3(self) -> bool:
        return True

    @property
    def gamma(self) -> float:
        """Fee multiplier (1 - fee) as float."""
        return 1.0 - float(self.fee)

    @property
    def has_multi_range(self) -> bool:
        """True if this hop has adjacent tick ranges for crossing support."""
        return self.tick_ranges is not None and len(self.tick_ranges) > 1


@dataclass(frozen=True, slots=True)
class SolidlyStableHop:
    """
    A Solidly stable (x³y + xy³ ≥ k) pool hop.

    Used by AerodromeV2Pool (stable=True) and CamelotLiquidityPool (stable_swap=True).
    Not a Möbius transformation — the swap function comes from solving a cubic.

    The optional ``swap_fn`` provides an integer-accurate swap simulation
    (e.g. wrapping ``calc_exact_in_stable``). When provided, the solver
    uses it for exact path evaluation. When absent, a float approximation
    is used (less accurate for extreme decimal differences).

    Attributes
    ----------
    reserve_in : int
        Input reserve in wei.
    reserve_out : int
        Output reserve in wei.
    fee : Fraction
        Fee as an exact fraction.
    decimals_in : int
        Decimal places of the input token (e.g. 6 for USDC, 18 for WETH).
    decimals_out : int
        Decimal places of the output token.
    swap_fn : Callable[[int], int] | None
        Integer swap function: ``swap_fn(amount_in) -> amount_out``.
        When provided, the solver uses this for exact evaluation.
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction
    decimals_in: int
    decimals_out: int
    swap_fn: Callable[[int], int] | None = field(default=None, compare=False, hash=False)
    invariant: PoolInvariant = PoolInvariant.SOLIDLY_STABLE

    @property
    def is_v2(self) -> bool:
        return False

    @property
    def is_v3(self) -> bool:
        return False

    @property
    def gamma(self) -> float:
        """Fee multiplier (1 - fee) as float."""
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class BalancerWeightedHop:
    """
    A Balancer weighted pool (∏xᵂⁱ ≥ k) hop.

    Not a Möbius transformation — the swap function uses power-law exponents.
    A 50/50 pool reduces to constant product.

    Attributes
    ----------
    reserve_in : int
        Input reserve in wei.
    reserve_out : int
        Output reserve in wei.
    fee : Fraction
        Fee as an exact fraction.
    weight_in : int
        Input token weight as 18-decimal fixed point (0.5 = 5e17).
    weight_out : int
        Output token weight as 18-decimal fixed point.
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction
    weight_in: int
    weight_out: int
    invariant: PoolInvariant = PoolInvariant.BALANCER_WEIGHTED

    @property
    def is_v2(self) -> bool:
        return False

    @property
    def is_v3(self) -> bool:
        return False

    @property
    def gamma(self) -> float:
        """Fee multiplier (1 - fee) as float."""
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class CurveStableswapHop:
    """
    A Curve stableswap pool hop.

    Uses the invariant: A*n^n*Σx + D = A*n^n*D + (D^(n+1) / n^n / ∏x)
    The swap function is inherently iterative (Newton's method for get_y).

    Attributes
    ----------
    reserve_in : int
        Input reserve in wei.
    reserve_out : int
        Output reserve in wei.
    fee : Fraction
        Fee as an exact fraction.
    curve_a: int
        Amplification coefficient (named A in Curve docs).
    curve_n_coins : int
        Number of coins in the pool.
    curve_d : int
        Current invariant value D (named D in Curve docs).
    token_index_in : int
        Index of the input token in the pool.
    token_index_out : int
        Index of the output token in the pool.
    precisions : tuple[int, ...]
        Decimal scaling per token (10^decimals for each coin).
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction
    curve_a: int
    curve_n_coins: int
    curve_d: int
    token_index_in: int
    token_index_out: int
    precisions: tuple[int, ...]
    invariant: PoolInvariant = PoolInvariant.CURVE_STABLESWAP

    @property
    def is_v2(self) -> bool:
        return False

    @property
    def is_v3(self) -> bool:
        return False

    @property
    def gamma(self) -> float:
        """Fee multiplier (1 - fee) as float."""
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class BalancerMultiTokenHop:
    """
    An N-token Balancer weighted pool for multi-token basket arbitrage.

    Unlike pairwise hops, this represents the entire pool state and
    enables closed-form basket trade optimization.

    Attributes
    ----------
    reserves : tuple[int, ...]
        Token reserves in wei, ordered by token index.
    weights : tuple[int, ...]
        Normalized weights as 18-decimal fixed point (sum = 1e18).
    fee : Fraction
        Swap fee as an exact fraction.
    decimals : tuple[int, ...]
        Decimal places for each token (e.g. 18 for ETH, 6 for USDC).
        Required for proper scaling in the closed-form formula.
    market_prices : tuple[float, ...] | None
        Market prices for each token in a common numéraire.
        Required for multi-token arbitrage optimization.
    """

    reserves: tuple[int, ...]
    weights: tuple[int, ...]
    fee: Fraction
    decimals: tuple[int, ...] = ()
    market_prices: tuple[float, ...] | None = None
    invariant: PoolInvariant = PoolInvariant.BALANCER_MULTI_TOKEN

    @property
    def n_tokens(self) -> int:
        return len(self.reserves)

    @property
    def is_v2(self) -> bool:
        return False

    @property
    def is_v3(self) -> bool:
        return False

    @property
    def gamma(self) -> float:
        """Fee multiplier (1 - fee) as float."""
        return 1.0 - float(self.fee)


# HopType is the union of all hop variants for type annotations.
# Use specific types (ConstantProductHop, BoundedProductHop, etc.) for construction.
HopType = (
    ConstantProductHop
    | BoundedProductHop
    | SolidlyStableHop
    | BalancerWeightedHop
    | CurveStableswapHop
    | BalancerMultiTokenHop
)


def hop_factory(
    *,
    reserve_in: int,
    reserve_out: int,
    fee: Fraction,
    liquidity: int | None = None,
    sqrt_price: int | None = None,
    tick_lower: int | None = None,
    tick_upper: int | None = None,
) -> HopType:
    """
    Backward-compatible Hop constructor.

    Returns the correct hop variant based on the arguments:
    - With liquidity/sqrt_price/tick fields -> BoundedProductHop
    - Without V3 fields -> ConstantProductHop

    This preserves the old ``Hop(...)`` API while routing to the new
    tagged union types.
    """
    has_v3 = (
        liquidity is not None
        and sqrt_price is not None
        and tick_lower is not None
        and tick_upper is not None
    )
    if has_v3:
        assert liquidity is not None
        assert sqrt_price is not None
        assert tick_lower is not None
        assert tick_upper is not None
        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
            liquidity=liquidity,
            sqrt_price=sqrt_price,
            tick_lower=tick_lower,
            tick_upper=tick_upper,
        )
    return ConstantProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=fee,
    )


# Backward-compatible alias: Hop(...) calls hop_factory(...)
Hop = hop_factory


@dataclass(frozen=True, slots=True)
class SolveInput:
    """
    Unified input for all solvers.

    Attributes
    ----------
    hops : tuple[Hop, ...]
        Ordered pool hops forming the arbitrage path.
    max_input : int | None
        Optional upper bound on input amount in wei.
    """

    hops: tuple[HopType, ...]
    max_input: int | None = None

    @property
    def num_hops(self) -> int:
        return len(self.hops)

    @property
    def has_v3(self) -> bool:
        """True if any hop has V3/V4 bounded-liquidity data."""
        return any(h.is_v3 for h in self.hops)

    @property
    def all_v2(self) -> bool:
        """True if no hop has V3/V4 data (pure V2 path)."""
        return not self.has_v3

    @property
    def all_constant_product(self) -> bool:
        """True if all hops are constant product (pure V2 path)."""
        return all(h.invariant == PoolInvariant.CONSTANT_PRODUCT for h in self.hops)

    @property
    def has_solidly_stable(self) -> bool:
        """True if any hop is a Solidly stable invariant."""
        return any(h.invariant == PoolInvariant.SOLIDLY_STABLE for h in self.hops)

    @property
    def has_balancer_weighted(self) -> bool:
        """True if any hop is a Balancer weighted invariant."""
        return any(h.invariant == PoolInvariant.BALANCER_WEIGHTED for h in self.hops)

    @property
    def has_curve_stableswap(self) -> bool:
        """True if any hop is a Curve stableswap invariant."""
        return any(h.invariant == PoolInvariant.CURVE_STABLESWAP for h in self.hops)

    @property
    def has_balancer_multi_token(self) -> bool:
        """True if any hop is a Balancer multi-token invariant."""
        return any(h.invariant == PoolInvariant.BALANCER_MULTI_TOKEN for h in self.hops)

    @property
    def v3_indices(self) -> tuple[int, ...]:
        """Indices of hops with V3/V4 data."""
        return tuple(i for i, h in enumerate(self.hops) if h.is_v3)


@dataclass(frozen=True, slots=True)
class SolveResult:
    """
    Unified output from all solvers.

    Attributes
    ----------
    optimal_input : int
        Optimal input amount in wei.
    profit : int
        Expected profit in wei (output - input).
    success : bool
        Whether optimization found a profitable solution.
    iterations : int
        Number of iterations taken (0 for closed-form).
    method : SolverMethod
        Which solver algorithm was used.
    error : str | None
        Error message if unsuccessful.
    solve_time_ns : int
        Solve time in nanoseconds.
    """

    optimal_input: int
    profit: int
    success: bool
    iterations: int
    method: SolverMethod
    error: str | None = None
    solve_time_ns: int = 0


# ---------------------------------------------------------------------------
# Solver ABC
# ---------------------------------------------------------------------------


class Solver(ABC):
    """
    Abstract base class for arbitrage solvers.

    Every solver accepts a `SolveInput` and returns a `SolveResult`.
    The `supports()` method indicates whether a solver can handle a
    given input (used by `ArbSolver` for dispatch).
    """

    @abstractmethod
    def solve(self, solve_input: SolveInput) -> SolveResult:
        """
        Find optimal arbitrage input.

        Parameters
        ----------
        solve_input : SolveInput
            The arbitrage path and constraints.

        Returns
        -------
        SolveResult
            Optimization result.
        """
        ...

    @abstractmethod
    def supports(self, solve_input: SolveInput) -> bool:
        """
        Whether this solver can handle the given input.

        Parameters
        ----------
        solve_input : SolveInput
            The arbitrage path to check.

        Returns
        -------
        bool
            True if this solver supports the input.
        """
        ...


# ---------------------------------------------------------------------------
# Möbius Solver
# ---------------------------------------------------------------------------


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


def _hop_to_float_state(hop: HopType) -> tuple[float, float, float]:
    """Convert any Hop variant to (reserve_in, reserve_out, gamma) as floats."""
    return float(hop.reserve_in), float(hop.reserve_out), hop.gamma


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
    """Simulate a swap through all hops for verification."""
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0.0
        r_i, s_i, g_i = _hop_to_float_state(hop)
        denom = r_i + amount * g_i
        if denom <= 0:
            return 0.0
        amount = amount * g_i * s_i / denom
    return amount


class MobiusSolver(Solver):
    """
    Möbius transformation solver for constant product AMM paths.

    Zero-iteration closed-form solution. Works for V2 paths and V3
    single-range paths (where the swap stays within one tick range).

    Performance: ~0.86μs (Python), ~0.19μs (Rust)
    """

    def supports(self, solve_input: SolveInput) -> bool:
        if solve_input.num_hops < 2:
            return False
        # Supports V2-only paths and V3 single-range paths
        # Does NOT support Solidly, Balancer, Curve (those need other solvers)
        # Does NOT support V3 with tick crossings (PiecewiseMobiusSolver handles those)
        for hop in solve_input.hops:
            if hop.invariant not in (
                PoolInvariant.CONSTANT_PRODUCT,
                PoolInvariant.BOUNDED_PRODUCT,
            ):
                return False
            # Multi-range V3 requires PiecewiseMobiusSolver
            if (
                hop.invariant == PoolInvariant.BOUNDED_PRODUCT
                and isinstance(hop, BoundedProductHop)
                and hop.has_multi_range
            ):
                return False
        return True

    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if solve_input.num_hops < 2:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.MOBIUS,
                error="Möbius solver requires 2+ hops",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        coeffs = _compute_mobius_coefficients(solve_input.hops)

        if not coeffs.is_profitable:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.MOBIUS,
                error="Not profitable (K/M <= 1)",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        x_opt = coeffs.optimal_input()

        if x_opt <= 0:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.MOBIUS,
                error="Optimal input <= 0",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        # Apply max_input constraint
        if solve_input.max_input is not None and x_opt > float(solve_input.max_input):
            x_opt = float(solve_input.max_input)

        # Integer refinement: the Möbius float result is very close
        # to the true optimum. For 2-hop paths, the best integer is
        # within ±1 of the float optimum. For multi-hop paths, the
        # composition of multiple constant-product functions can cause
        # slightly wider integer deviation. We check a small neighborhood
        # proportional to path length.
        num_hops = solve_input.num_hops
        search_radius = 1 if num_hops <= 2 else min(num_hops, 5)

        x_floor = int(x_opt)
        best_input = x_floor
        best_profit = -1

        for candidate in range(max(1, x_floor - search_radius), x_floor + search_radius + 2):
            if solve_input.max_input is not None and candidate > solve_input.max_input:
                continue
            output = _simulate_path(float(candidate), solve_input.hops)
            profit = int(output) - candidate
            if profit > best_profit:
                best_profit = profit
                best_input = candidate

        elapsed_ns = time.perf_counter_ns() - start_ns

        if best_profit <= 0:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.MOBIUS,
                error="Not profitable (integer verification failed)",
                solve_time_ns=elapsed_ns,
            )

        return SolveResult(
            optimal_input=best_input,
            profit=best_profit,
            success=True,
            iterations=0,
            method=SolverMethod.MOBIUS,
            solve_time_ns=elapsed_ns,
        )


# ---------------------------------------------------------------------------
# Newton Solver (2-hop V2 fallback)
# ---------------------------------------------------------------------------


class NewtonSolver(Solver):
    """
    Newton's method solver for 2-hop V2-V2 arbitrage.

    Converges in 3-4 iterations. Useful as a fallback when Möbius
    is not available or for validation.

    Performance: ~7.5μs
    """

    MAX_ITERATIONS = 10
    TOLERANCE = 1e-9

    def supports(self, solve_input: SolveInput) -> bool:
        return solve_input.num_hops == 2 and solve_input.all_constant_product

    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if not self.supports(solve_input):
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.NEWTON,
                error="Newton solver requires exactly 2 V2 hops",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        r0_buy, s0_buy, g0_buy = _hop_to_float_state(solve_input.hops[0])
        r0_sell, s0_sell, g0_sell = _hop_to_float_state(solve_input.hops[1])

        # Initial guess from Möbius closed-form (best available estimate)
        # This ensures Newton starts near the true optimum and converges
        # in 1-2 refinement iterations.
        mobius_coeffs = _compute_mobius_coefficients(solve_input.hops)
        if mobius_coeffs.is_profitable:
            x = mobius_coeffs.optimal_input()
            if x <= 0:
                elapsed_ns = time.perf_counter_ns() - start_ns
                return SolveResult(
                    optimal_input=0,
                    profit=0,
                    success=False,
                    iterations=0,
                    method=SolverMethod.NEWTON,
                    error="Möbius optimal <= 0",
                    solve_time_ns=elapsed_ns,
                )
        else:
            elapsed_ns = time.perf_counter_ns() - start_ns
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.NEWTON,
                error="Not profitable (Möbius check failed)",
                solve_time_ns=elapsed_ns,
            )

        iterations = 0
        for i in range(self.MAX_ITERATIONS):
            # Hop 0: input x → forward amount y
            # y = x * gamma * reserve_out / (reserve_in + x * gamma) # noqa: ERA001
            denom_buy = r0_buy + x * g0_buy
            if denom_buy <= 0:
                break
            y = x * g0_buy * s0_buy / denom_buy

            # Hop 1: forward y → output z
            # z = y * gamma * reserve_out / (reserve_in + y * gamma) # noqa: ERA001
            denom_sell = r0_sell + y * g0_sell
            if denom_sell <= 0:
                break
            z = y * g0_sell * s0_sell / denom_sell

            # Chain rule: dP/dx = dz/dy * dy/dx - 1
            dy_dx = g0_buy * s0_buy * r0_buy / (denom_buy**2)
            dz_dy = g0_sell * s0_sell * r0_sell / (denom_sell**2)
            dprofit_dx = dz_dy * dy_dx - 1.0

            if abs(dprofit_dx) < self.TOLERANCE:
                iterations = i + 1
                break

            # Second derivative: d²P/dx² = d²z/dy² * (dy/dx)² + dz/dy * d²y/dx²
            d2y_dx2 = -2.0 * g0_buy**2 * s0_buy * r0_buy / (denom_buy**3)
            d2z_dy2 = -2.0 * g0_sell**2 * s0_sell * r0_sell / (denom_sell**3)
            d2profit_dx2 = d2z_dy2 * dy_dx**2 + dz_dy * d2y_dx2

            if abs(d2profit_dx2) < 1e-30:
                iterations = i + 1
                break

            # Newton step
            x_new = x - dprofit_dx / d2profit_dx2
            if x_new <= 0:
                x_new = x / 2.0
            x = x_new
            iterations = i + 1

        if x <= 0:
            elapsed_ns = time.perf_counter_ns() - start_ns
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=iterations,
                method=SolverMethod.NEWTON,
                error="Newton did not converge to positive input",
                solve_time_ns=elapsed_ns,
            )

        # Apply max_input constraint
        if solve_input.max_input is not None and x > float(solve_input.max_input):
            x = float(solve_input.max_input)

        # Integer verification
        optimal_input = int(x)
        output = _simulate_path(float(optimal_input), solve_input.hops)
        actual_profit = int(output) - optimal_input

        elapsed_ns = time.perf_counter_ns() - start_ns

        if actual_profit <= 0:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=iterations,
                method=SolverMethod.NEWTON,
                error="Not profitable (integer verification failed)",
                solve_time_ns=elapsed_ns,
            )

        return SolveResult(
            optimal_input=optimal_input,
            profit=actual_profit,
            success=True,
            iterations=iterations,
            method=SolverMethod.NEWTON,
            solve_time_ns=elapsed_ns,
        )


# ---------------------------------------------------------------------------
# Brent Solver (fallback)
# ---------------------------------------------------------------------------


class BrentSolver(Solver):
    """
    Brent's method solver via scipy. Handles all pool types including
    V3-V3 with tick crossings.

    Performance: ~194μs (V2-V2), ~390μs (V3-V3)
    """

    def supports(self, solve_input: SolveInput) -> bool:
        return solve_input.num_hops >= 2

    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if solve_input.num_hops < 2:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.BRENT,
                error="Brent solver requires 2+ hops",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        try:
            from scipy.optimize import minimize_scalar
        except ImportError:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.BRENT,
                error="scipy not available",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        def neg_profit(x: float) -> float:
            """Negative profit for minimization."""
            if x <= 0:
                return 0.0
            output = _simulate_path(x, solve_input.hops)
            return -(output - x)

        # Estimate upper bound from largest input reserve
        max_reserve = max(h.reserve_in for h in solve_input.hops)
        upper = float(max_reserve)

        if solve_input.max_input is not None:
            upper = min(upper, float(solve_input.max_input))

        if upper <= 0:
            elapsed_ns = time.perf_counter_ns() - start_ns
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.BRENT,
                error="Upper bound is zero or negative",
                solve_time_ns=elapsed_ns,
            )

        result = minimize_scalar(
            neg_profit,
            method="bounded",
            bounds=(0, upper),
            options={"xatol": 1.0},
        )

        elapsed_ns = time.perf_counter_ns() - start_ns

        if not result.success and result.fun >= 0:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=result.nfev if hasattr(result, "nfev") else 0,
                method=SolverMethod.BRENT,
                error="No profitable solution found",
                solve_time_ns=elapsed_ns,
            )

        x_opt = result.x
        optimal_input = int(x_opt)
        output = _simulate_path(float(optimal_input), solve_input.hops)
        actual_profit = int(output) - optimal_input

        if actual_profit <= 0:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=result.nfev if hasattr(result, "nfev") else 0,
                method=SolverMethod.BRENT,
                error="Not profitable (integer verification failed)",
                solve_time_ns=elapsed_ns,
            )

        return SolveResult(
            optimal_input=optimal_input,
            profit=actual_profit,
            success=True,
            iterations=result.nfev if hasattr(result, "nfev") else 0,
            method=SolverMethod.BRENT,
            solve_time_ns=elapsed_ns,
        )


# ---------------------------------------------------------------------------
# PiecewiseMobiusSolver — V3 multi-range with tick crossing
# ---------------------------------------------------------------------------


class PiecewiseMobiusSolver(Solver):
    """
    Piecewise-Möbius solver for V3 paths with tick crossings.

    For V3 swaps that cross tick boundaries, the swap function is
    piecewise-Möbius: fixed crossing output from crossed ranges plus
    variable Möbius output from the ending range. Uses golden section
    search on the bracketed profit function.

    Performance:
    - Python implementation: ~50μs (with all optimizations)
    - Rust implementation: ~9μs end-to-end (~1μs raw computation)
    """

    GOLDEN_SECTION_ITERATIONS = 25
    PHI = (math.sqrt(5) - 1) / 2  # ~0.618

    def __init__(self) -> None:
        self._rust_optimizer = None
        self._mobius_solver = None
        # Cache for Rust objects to avoid recreation
        # Key: hash of (reserve_in, reserve_out, fee) tuple
        self._rust_hop_cache: dict[int, list] = {}
        # Key: (range_ids_tuple, current_range_index, zero_for_one) # noqa: ERA001
        self._rust_sequence_cache: dict[tuple[tuple[int, ...], int, bool], Any] = {}
        self._try_load_rust()

    def _try_load_rust(self) -> None:
        """Try to load the Rust Möbius optimizer for faster solving."""
        try:
            from degenbot.degenbot_rs import mobius

            self._rust_optimizer = mobius.RustMobiusOptimizer()
        except ImportError:
            self._rust_optimizer = None

    def supports(self, solve_input: SolveInput) -> bool:
        if solve_input.num_hops < 2:
            return False
        # Supports paths with V3 bounded-product hops
        # (handles both single-range and multi-range V3)
        for hop in solve_input.hops:
            if hop.invariant not in (
                PoolInvariant.CONSTANT_PRODUCT,
                PoolInvariant.BOUNDED_PRODUCT,
            ):
                return False
        return solve_input.has_v3

    def _has_multi_range(self, solve_input: SolveInput) -> bool:
        """Check if any V3 hop has multi-range data for tick crossing."""
        for hop in solve_input.hops:
            if (
                hop.invariant == PoolInvariant.BOUNDED_PRODUCT
                and isinstance(hop, BoundedProductHop)
                and hop.has_multi_range
            ):
                return True
        return False

    def _find_v3_hop_index(self, solve_input: SolveInput) -> tuple[int, BoundedProductHop] | None:
        """Find the first V3 hop with multi-range data."""
        for i, hop in enumerate(solve_input.hops):
            if (
                hop.invariant == PoolInvariant.BOUNDED_PRODUCT
                and isinstance(hop, BoundedProductHop)
                and hop.has_multi_range
            ):
                return i, hop
        return None

    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        # Fast path: check we have 2+ hops with V3 data
        if solve_input.num_hops < 2:
            return self._error_result("Need 2+ hops", start_ns)

        # V3-V3 fast path: 2-hop path with both V3 multi-range
        if solve_input.num_hops == 2 and self._rust_optimizer is not None:
            v3_v3_result = self._try_rust_v3_v3(solve_input, start_ns)
            if v3_v3_result is not None and v3_v3_result.success:
                return v3_v3_result

        # Find V3 hop (combines supports, has_multi_range, find_v3_hop_index)
        v3_result = self._find_v3_hop_index(solve_input)

        if v3_result is None:
            # No multi-range V3 found - try single-range fallback
            return self._try_single_range_fallback(solve_input, start_ns)

        v3_hop_index, v3_hop = v3_result

        # Multi-range V3: try Rust first with caching
        if self._rust_optimizer is not None:
            rust_result = self._try_rust_multi_range(solve_input, v3_hop_index, v3_hop, start_ns)
            if rust_result is not None and rust_result.success:
                return rust_result

        # Fall back to Python implementation
        return self._solve_multi_range(solve_input, start_ns)

    def _error_result(self, error: str, start_ns: int) -> SolveResult:
        """Return error result with timing."""
        return SolveResult(
            optimal_input=0,
            profit=0,
            success=False,
            iterations=0,
            method=SolverMethod.PIECEWISE_MOBIUS,
            error=error,
            solve_time_ns=time.perf_counter_ns() - start_ns,
        )

    def _try_single_range_fallback(self, solve_input: SolveInput, start_ns: int) -> SolveResult:
        """Try MobiusSolver for single-range V3."""
        if self._mobius_solver is None:
            self._mobius_solver = MobiusSolver()
        result = self._mobius_solver.solve(solve_input)
        if result.success:
            return SolveResult(
                optimal_input=result.optimal_input,
                profit=result.profit,
                success=True,
                iterations=result.iterations,
                method=SolverMethod.PIECEWISE_MOBIUS,
                solve_time_ns=result.solve_time_ns,
            )
        return self._error_result("Single-range fallback failed", start_ns)

    def _solve_multi_range(self, solve_input: SolveInput, start_ns: int) -> SolveResult:
        """Solve using Python piecewise-Möbius for tick crossings."""
        # Find the V3 hop with multi-range data (already found in solve(), but refind for safety)
        v3_result = self._find_v3_hop_index(solve_input)
        if v3_result is None:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS,
                error="No multi-range V3 hop found",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        v3_hop_index, v3_hop = v3_result

        # Python-only implementation (Rust is tried first in solve())
        return self._solve_multi_range_python(solve_input, v3_hop_index, v3_hop, start_ns)

    def _solve_multi_range_python(
        self, solve_input: SolveInput, v3_hop_index: int, v3_hop: BoundedProductHop, start_ns: int
    ) -> SolveResult:
        """Python-only multi-range solver (fallback when Rust fails)."""
        # Build candidate crossings from tick_ranges with lazy evaluation
        best_result: SolveResult | None = None
        best_profit = -1

        assert v3_hop.tick_ranges is not None
        current_idx = v3_hop.current_range_index

        # Collect plausible candidates first
        plausible_candidates: list[int] = []
        for end_idx in range(current_idx, min(current_idx + 3, len(v3_hop.tick_ranges))):
            if self._is_candidate_plausible(solve_input, v3_hop, current_idx, end_idx, best_profit):
                plausible_candidates.append(end_idx)

        if not plausible_candidates:
            elapsed_ns = time.perf_counter_ns() - start_ns
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS,
                error="No plausible candidate ranges found",
                solve_time_ns=elapsed_ns,
            )

        # Evaluate candidates (parallel if multiple, sequential if single)
        if len(plausible_candidates) > 1:
            # Parallel evaluation for multiple candidates
            results = self._evaluate_candidates_parallel(
                solve_input, v3_hop_index, v3_hop, current_idx, plausible_candidates
            )
        else:
            # Sequential for single candidate (avoids thread overhead)
            results = [
                self._try_candidate_range(
                    solve_input, v3_hop_index, v3_hop, current_idx, plausible_candidates[0]
                )
            ]

        # Find best result
        for result in results:
            if result.success and result.profit > best_profit:
                best_profit = result.profit
                best_result = result

        if best_result is None:
            elapsed_ns = time.perf_counter_ns() - start_ns
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS,
                error="No profitable candidate range found",
                solve_time_ns=elapsed_ns,
            )

        # Return best result with updated timing
        elapsed_ns = time.perf_counter_ns() - start_ns
        return SolveResult(
            optimal_input=best_result.optimal_input,
            profit=best_result.profit,
            success=True,
            iterations=best_result.iterations,
            method=SolverMethod.PIECEWISE_MOBIUS,
            solve_time_ns=elapsed_ns,
        )

    def _evaluate_candidates_parallel(
        self,
        solve_input: SolveInput,
        v3_hop_index: int,
        v3_hop: BoundedProductHop,
        current_idx: int,
        candidates: list[int],
    ) -> list[SolveResult]:
        """Evaluate multiple candidate ranges.

        Uses sequential evaluation for 1-2 candidates (avoids thread overhead).
        Only uses threads for 3+ candidates where parallelism pays off.
        """
        results: list[SolveResult] = []

        # Sequential evaluation for small number of candidates
        # Thread overhead exceeds benefit for 1-2 candidates
        if len(candidates) <= 2:
            for end_idx in candidates:
                try:
                    result = self._try_candidate_range(
                        solve_input, v3_hop_index, v3_hop, current_idx, end_idx
                    )
                    results.append(result)
                except Exception:
                    pass
            return results

        # Parallel evaluation only for 3+ candidates
        from concurrent.futures import ThreadPoolExecutor, as_completed

        with ThreadPoolExecutor(max_workers=min(len(candidates), 3)) as executor:
            future_to_idx = {
                executor.submit(
                    self._try_candidate_range,
                    solve_input,
                    v3_hop_index,
                    v3_hop,
                    current_idx,
                    end_idx,
                ): end_idx
                for end_idx in candidates
            }

            for future in as_completed(future_to_idx):
                try:
                    result = future.result()
                    results.append(result)
                except Exception:
                    pass

        return results

    def _is_candidate_plausible(
        self,
        solve_input: SolveInput,
        v3_hop: BoundedProductHop,
        start_idx: int,
        end_idx: int,
        current_best_profit: int,
    ) -> bool:
        """
        Cheap check to filter out candidates that can't be profitable.

        Returns False if this candidate can be skipped (saves expensive evaluation).
        """
        assert v3_hop.tick_ranges is not None

        # Current range is always plausible (no crossing cost)
        if end_idx == start_idx:
            return True

        # Check if crossing cost exceeds max_input
        ending_range = v3_hop.tick_ranges[end_idx]
        gamma = 1.0 - float(v3_hop.fee)

        # Rough estimate of crossing cost (linear approximation)
        crossing_input = 0.0
        for i in range(start_idx, end_idx):
            range_info = v3_hop.tick_ranges[i]
            # Simple estimate: price movement across range
            sqrt_p_lower = float(range_info.sqrt_price_lower) / Q96
            sqrt_p_upper = float(range_info.sqrt_price_upper) / Q96
            liq = float(range_info.liquidity)

            # Token0->Token1: price goes down, so we go from upper to lower
            if v3_hop.reserve_in > v3_hop.reserve_out:
                # Approximate input to cross this range
                net_input = liq * (1.0 / sqrt_p_lower - 1.0 / sqrt_p_upper)
            else:
                net_input = liq * (sqrt_p_upper - sqrt_p_lower)

            gross_input = net_input / gamma if gamma > 0 else net_input
            crossing_input += gross_input

        # If crossing cost alone exceeds max_input, skip this candidate
        if solve_input.max_input is not None:
            if crossing_input > float(solve_input.max_input):
                return False

        # If we already have a good profit, skip distant ranges unless they might be better
        if current_best_profit > 0 and end_idx > start_idx + 1:
            # More distant ranges have higher crossing costs, unlikely to beat current best
            # unless there's significantly more liquidity
            current_liq = float(v3_hop.tick_ranges[start_idx].liquidity)
            ending_liq = float(ending_range.liquidity)
            if ending_liq < current_liq * 2:  # Need 2x liquidity to justify 2-range crossing
                return False

        # Price impact pruning: use quick estimate to check if swap stays in ending range
        # This is cheaper than full golden section search
        if end_idx > start_idx:
            from degenbot.arbitrage.optimizers.v3_tick_predictor import estimate_price_impact

            # Estimate price after crossing (roughly the input needed for crossing)
            estimated_sqrt_price = estimate_price_impact(
                amount_in=crossing_input * 1.1,  # 10% buffer for safety
                liquidity=float(ending_range.liquidity),
                current_sqrt_price=float(ending_range.sqrt_price_lower) / Q96,
                fee=float(v3_hop.fee),
                zero_for_one=v3_hop.reserve_in > v3_hop.reserve_out,
            )

            # Check if estimated price stays within ending range bounds
            sqrt_p_lower = float(ending_range.sqrt_price_lower) / Q96
            sqrt_p_upper = float(ending_range.sqrt_price_upper) / Q96

            if not (sqrt_p_lower <= estimated_sqrt_price <= sqrt_p_upper):
                # Price would go out of range even with just crossing amount
                # This candidate is not viable
                return False

        return True

    def _try_candidate_range(
        self,
        solve_input: SolveInput,
        v3_hop_index: int,
        v3_hop: BoundedProductHop,
        start_idx: int,
        end_idx: int,
    ) -> SolveResult:
        """
        Try a candidate ending range using proper V3 tick crossing math.

        Uses the exact V3 swap formulas from mobius.py:
        - Computes TickRangeCrossing with proper fee handling
        - Pre-computes Möbius coefficients for before/after hops
        - Uses golden section search for piecewise profit maximization

        Falls back to Rust implementation when available for ~5x speedup.
        """
        assert v3_hop.tick_ranges is not None

        # Try Rust implementation first for speed
        if self._rust_optimizer is not None:
            rust_result = self._try_rust_candidate_range(
                solve_input, v3_hop_index, v3_hop, start_idx, end_idx
            )
            if rust_result is not None and rust_result.success:
                return rust_result
            # If Rust fails, fall through to Python implementation

        # Convert V3TickRangeInfo to _V3TickRangeHop
        # Determine swap direction from hop reserves
        zero_for_one = v3_hop.reserve_in > v3_hop.reserve_out

        v3_ranges: list[_V3TickRangeHop] = []
        for i, range_info in enumerate(v3_hop.tick_ranges):
            if i == v3_hop.current_range_index:
                sqrt_price_current = float(v3_hop.sqrt_price) / Q96
            elif i < v3_hop.current_range_index:
                sqrt_price_current = float(range_info.sqrt_price_upper) / Q96
            else:
                sqrt_price_current = float(range_info.sqrt_price_lower) / Q96

            v3_ranges.append(
                _V3TickRangeHop(
                    liquidity=float(range_info.liquidity),
                    sqrt_price_current=sqrt_price_current,
                    sqrt_price_lower=float(range_info.sqrt_price_lower) / Q96,
                    sqrt_price_upper=float(range_info.sqrt_price_upper) / Q96,
                    fee=float(v3_hop.fee),
                    zero_for_one=zero_for_one,
                )
            )

        sequence = _V3TickRangeSequence(tuple(v3_ranges))

        # Compute crossing data for this candidate
        try:
            crossing = sequence.compute_crossing(end_idx)
        except (IndexError, ValueError):
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS,
                error=f"Invalid crossing range index {end_idx}",
                solve_time_ns=0,
            )

        # Build _MobiusHopState lists for before/after V3
        hops_before: list[_MobiusHopState] = []
        hops_after: list[_MobiusHopState] = []

        # Convert hops before V3 to _MobiusHopState
        for i, hop in enumerate(solve_input.hops[:v3_hop_index]):
            if isinstance(hop, ConstantProductHop) or isinstance(hop, BoundedProductHop):
                hops_before.append(
                    _MobiusHopState(
                        reserve_in=float(hop.reserve_in),
                        reserve_out=float(hop.reserve_out),
                        fee=float(hop.fee),
                    )
                )

        # Convert hops after V3 to _MobiusHopState
        for i, hop in enumerate(solve_input.hops[v3_hop_index + 1 :]):
            if isinstance(hop, ConstantProductHop) or isinstance(hop, BoundedProductHop):
                hops_after.append(
                    _MobiusHopState(
                        reserve_in=float(hop.reserve_in),
                        reserve_out=float(hop.reserve_out),
                        fee=float(hop.fee),
                    )
                )

        # Pre-compute Möbius coefficients
        coeffs_before = _compute_mobius_coefficients(hops_before) if hops_before else None
        coeffs_after = _compute_mobius_coefficients(hops_after) if hops_after else None

        # Get ending range's HopState
        ending_hop_state = crossing.ending_range.to_hop_state()

        # Compute minimum input to cover crossing
        if crossing.crossing_gross_input > 0 and coeffs_before is not None:
            target = crossing.crossing_gross_input
            if target >= coeffs_before.K / coeffs_before.N:
                return SolveResult(
                    optimal_input=0,
                    profit=0,
                    success=False,
                    iterations=0,
                    method=SolverMethod.PIECEWISE_MOBIUS,
                    error="Crossing requires more than path can deliver",
                    solve_time_ns=0,
                )
            x_min = target * coeffs_before.M / (coeffs_before.K - target * coeffs_before.N)
        elif crossing.crossing_gross_input > 0:
            x_min = crossing.crossing_gross_input
        else:
            x_min = 0.0

        # Single-range Möbius solve as starting point for bracket
        full_hops = hops_before + [ending_hop_state] + hops_after
        max_input_float = (
            float(solve_input.max_input) if solve_input.max_input is not None else None
        )

        try:
            x_mobius, _, _ = _mobius_solve(full_hops, max_input=max_input_float)
        except (ZeroDivisionError, ValueError):
            x_mobius = x_min + 1.0

        # Build bracket for golden section search
        x_low = max(x_min, 0.0)
        if x_mobius > x_low:
            x_high = max(x_mobius * 3, x_low + 1.0)
        else:
            x_high = max(x_low * 5, x_low + 1.0)

        if max_input_float is not None:
            x_high = min(x_high, max_input_float)

        if x_low >= x_high:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.PIECEWISE_MOBIUS,
                error="Invalid bracket for golden section search",
                solve_time_ns=0,
            )

        # Define profit evaluation functions (scalar and vectorized)
        def eval_profit(x: float) -> float:
            """Evaluate profit for input x using piecewise V3 swap."""
            if x <= 0:
                return -x

            # Input to V3 (after hops_before)
            if coeffs_before is not None:
                # Apply Möbius transformation from hops_before
                amt_v3 = coeffs_before.K * x / (coeffs_before.M + coeffs_before.N * x)
            else:
                amt_v3 = x

            # Apply piecewise V3 swap
            if amt_v3 < crossing.crossing_gross_input:
                return -x  # Can't cover crossing

            remaining = amt_v3 - crossing.crossing_gross_input
            variable_out = _mobius_simulate_path(remaining, [ending_hop_state])
            v3_out = crossing.crossing_output + variable_out

            # Validate ending range
            final_sqrt_p = self._estimate_final_sqrt_price(remaining, crossing.ending_range)
            if not crossing.ending_range.contains_sqrt_price(final_sqrt_p):
                return -x  # Out of range

            # Apply hops_after
            if coeffs_after is not None:
                final_out = coeffs_after.K * v3_out / (coeffs_after.M + coeffs_after.N * v3_out)
            else:
                final_out = v3_out

            return final_out - x

        # Try vectorized batch evaluation for initial bracket refinement
        # This uses NumPy to evaluate multiple points simultaneously
        try:
            import numpy as np

            vectorized_result = self._vectorized_bracket_search(
                x_low, x_high, coeffs_before, coeffs_after, crossing, ending_hop_state, eval_profit
            )
            if vectorized_result is not None:
                return vectorized_result
        except ImportError:
            pass  # Fall back to scalar golden section

        # Golden section search with adaptive convergence
        phi = self.PHI
        max_iterations = self.GOLDEN_SECTION_ITERATIONS
        # Convergence tolerance: stop when profit improvement < 0.01% of current profit
        profit_tolerance = 1e-4

        x1 = x_high - phi * (x_high - x_low)
        x2 = x_low + phi * (x_high - x_low)
        p1 = eval_profit(x1)
        p2 = eval_profit(x2)

        best_profit = max(p1, p2)
        iterations = 0

        for i in range(max_iterations):
            iterations = i + 1
            prev_best = best_profit

            if p1 < p2:
                x_low = x1
                x1 = x2
                p1 = p2
                x2 = x_low + phi * (x_high - x_low)
                p2 = eval_profit(x2)
            else:
                x_high = x2
                x2 = x1
                p2 = p1
                x1 = x_high - phi * (x_high - x_low)
                p1 = eval_profit(x1)

            best_profit = max(p1, p2)

            # Early termination: converged if improvement is tiny
            if best_profit > 0 and prev_best > 0:
                improvement = abs(best_profit - prev_best) / prev_best
                if improvement < profit_tolerance:
                    break

        # Best point
        if p1 > p2:
            x_opt, p_opt = x1, p1
        else:
            x_opt, p_opt = x2, p2

        if p_opt <= 0:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=iterations,
                method=SolverMethod.PIECEWISE_MOBIUS,
                error="No profitable solution in candidate range",
                solve_time_ns=0,
            )

        optimal_input = int(x_opt)
        actual_profit = int(p_opt)

        return SolveResult(
            optimal_input=optimal_input,
            profit=actual_profit,
            success=True,
            iterations=iterations,
            method=SolverMethod.PIECEWISE_MOBIUS,
            solve_time_ns=0,  # Will be set by caller
        )

    def _vectorized_bracket_search(
        self,
        x_low: float,
        x_high: float,
        coeffs_before,
        coeffs_after,
        crossing,
        ending_hop_state,
        eval_profit_scalar,
    ):
        """
        Vectorized bracket search using NumPy for parallel evaluation.

        Evaluates profit at multiple points simultaneously to quickly
        narrow down the optimal region before golden section refinement.

        Returns SolveResult if successful, None to fall back to scalar search.
        """
        try:
            import numpy as np
        except ImportError:
            return None

        # Number of points for initial vectorized evaluation
        # Reduced from 20 to 10 to minimize overhead
        n_points = 10

        # Create log-spaced points in the bracket
        log_x = np.linspace(np.log1p(x_low), np.log1p(x_high), n_points)
        x_points = np.expm1(log_x)

        # Ensure x_low and x_high are included
        x_points[0] = x_low
        x_points[-1] = x_high

        # Vectorized profit evaluation
        profits = np.array([eval_profit_scalar(x) for x in x_points])

        # Find best point
        best_idx = np.argmax(profits)
        best_profit = profits[best_idx]
        best_x = x_points[best_idx]

        if best_profit <= 0:
            return None  # No profitable solution in bracket

        # Refine around best point with smaller bracket
        # Reduced from 10 to 5 points
        idx_low = max(0, best_idx - 1)
        idx_high = min(n_points - 1, best_idx + 1)
        x_refined = np.linspace(x_points[idx_low], x_points[idx_high], 5)
        profits_refined = np.array([eval_profit_scalar(x) for x in x_refined])

        best_idx_refined = np.argmax(profits_refined)
        best_profit_refined = profits_refined[best_idx_refined]
        best_x_refined = x_refined[best_idx_refined]

        if best_profit_refined <= 0:
            return None

        # Return result without full golden section convergence
        # This is faster but slightly less precise (acceptable for many cases)
        return SolveResult(
            optimal_input=int(best_x_refined),
            profit=int(best_profit_refined),
            success=True,
            iterations=n_points + 10,  # Total evaluations
            method=SolverMethod.PIECEWISE_MOBIUS,
            solve_time_ns=0,  # Will be set by caller
        )

    def _try_rust_candidate_range(
        self,
        solve_input: SolveInput,
        v3_hop_index: int,
        v3_hop: BoundedProductHop,
        start_idx: int,
        end_idx: int,
    ) -> SolveResult | None:
        """
        Try a candidate ending range using the Rust implementation.

        Returns SolveResult if successful, None to fall back to Python.
        """
        if self._rust_optimizer is None:
            return None

        try:
            from degenbot.degenbot_rs import mobius

            # Convert Python hops to Rust hops
            rust_hops = []
            for hop in solve_input.hops:
                if isinstance(hop, ConstantProductHop) or isinstance(hop, BoundedProductHop):
                    rust_hops.append(
                        mobius.RustHopState(
                            float(hop.reserve_in),
                            float(hop.reserve_out),
                            float(hop.fee),
                        )
                    )

            # Build V3 tick range crossing data
            assert v3_hop.tick_ranges is not None

            # Build the ending range for this candidate
            ending_range_info = v3_hop.tick_ranges[end_idx]
            zero_for_one = v3_hop.reserve_in > v3_hop.reserve_out

            # Compute entry sqrt price (boundary with previous range)
            if end_idx > 0 and v3_hop.tick_ranges:
                prev_range = v3_hop.tick_ranges[end_idx - 1]
                entry_sqrt_price = (
                    float(prev_range.sqrt_price_lower) / Q96
                    if zero_for_one
                    else float(prev_range.sqrt_price_upper) / Q96
                )
            else:
                entry_sqrt_price = float(ending_range_info.sqrt_price_lower) / Q96

            # Build the ending V3TickRangeHop
            rust_ending_range = mobius.RustV3TickRangeHop(
                liquidity=float(ending_range_info.liquidity),
                sqrt_price_current=entry_sqrt_price,
                sqrt_price_lower=float(ending_range_info.sqrt_price_lower) / Q96,
                sqrt_price_upper=float(ending_range_info.sqrt_price_upper) / Q96,
                fee=float(v3_hop.fee),
                zero_for_one=zero_for_one,
            )

            # Compute crossing data (simplified approximation)
            crossing_input = 0.0
            crossing_output = 0.0

            for i in range(start_idx, end_idx):
                range_info = v3_hop.tick_ranges[i]
                liq = float(range_info.liquidity)
                sqrt_p_lower = float(range_info.sqrt_price_lower) / Q96
                sqrt_p_upper = float(range_info.sqrt_price_upper) / Q96
                gamma = 1.0 - float(v3_hop.fee)

                if zero_for_one:
                    net_input = liq * (1.0 / sqrt_p_lower - 1.0 / sqrt_p_upper)
                    output = liq * (sqrt_p_upper - sqrt_p_lower)
                else:
                    net_input = liq * (sqrt_p_upper - sqrt_p_lower)
                    output = liq * (1.0 / sqrt_p_lower - 1.0 / sqrt_p_upper)

                gross_input = net_input / gamma if gamma > 0 else net_input
                crossing_input += gross_input
                crossing_output += output

            # Build TickRangeCrossing for Rust
            rust_crossing = mobius.RustTickRangeCrossing(
                crossing_gross_input=crossing_input,
                crossing_output=crossing_output,
                ending_range=rust_ending_range,
            )

            # Call Rust piecewise solve
            max_input_float = (
                float(solve_input.max_input) if solve_input.max_input is not None else None
            )

            result = self._rust_optimizer.solve_piecewise(
                rust_hops,
                v3_hop_index,
                [rust_crossing],
                max_input_float,
            )

            if result.success:
                return SolveResult(
                    optimal_input=int(result.optimal_input),
                    profit=int(result.profit),
                    success=True,
                    iterations=result.iterations,
                    method=SolverMethod.PIECEWISE_MOBIUS,
                    solve_time_ns=0,  # Will be set by caller
                )
            return None

        except Exception:
            # Any error falls back to Python implementation
            return None

    def _get_cached_rust_hops(self, solve_input: SolveInput) -> list:
        """Get or create cached Rust hop states."""
        from degenbot.degenbot_rs import mobius

        # Create cache key from hop data
        cache_key = hash(
            tuple((hop.reserve_in, hop.reserve_out, float(hop.fee)) for hop in solve_input.hops)
        )

        if cache_key not in self._rust_hop_cache:
            rust_hops = []
            for hop in solve_input.hops:
                if isinstance(hop, (ConstantProductHop, BoundedProductHop)):
                    rust_hops.append(
                        mobius.RustHopState(
                            float(hop.reserve_in),
                            float(hop.reserve_out),
                            float(hop.fee),
                        )
                    )
            self._rust_hop_cache[cache_key] = rust_hops

        return self._rust_hop_cache[cache_key]

    def _get_cached_rust_sequence(
        self,
        v3_hop: BoundedProductHop,
    ):
        """Get or create cached Rust V3 tick range sequence."""
        from degenbot.degenbot_rs import mobius

        assert v3_hop.tick_ranges is not None
        zero_for_one = v3_hop.reserve_in > v3_hop.reserve_out

        # Create cache key from hop identity + tick data
        # Use id() for object identity since V3TickRangeInfo is immutable
        range_ids = tuple(id(r) for r in v3_hop.tick_ranges)
        cache_key = (range_ids, v3_hop.current_range_index, zero_for_one)

        if cache_key not in self._rust_sequence_cache:
            rust_ranges = []
            for i, range_info in enumerate(v3_hop.tick_ranges):
                # Determine current sqrt price for this range
                if i == v3_hop.current_range_index:
                    sqrt_p_current = float(v3_hop.sqrt_price) / Q96
                elif i < v3_hop.current_range_index:
                    sqrt_p_current = float(range_info.sqrt_price_upper) / Q96
                else:
                    sqrt_p_current = float(range_info.sqrt_price_lower) / Q96

                rust_ranges.append(
                    mobius.RustV3TickRangeHop(
                        liquidity=float(range_info.liquidity),
                        sqrt_price_current=sqrt_p_current,
                        sqrt_price_lower=float(range_info.sqrt_price_lower) / Q96,
                        sqrt_price_upper=float(range_info.sqrt_price_upper) / Q96,
                        fee=float(v3_hop.fee),
                        zero_for_one=zero_for_one,
                    )
                )

            self._rust_sequence_cache[cache_key] = mobius.RustV3TickRangeSequence(rust_ranges)

        return self._rust_sequence_cache[cache_key]

    def _try_rust_v3_v3(
        self,
        solve_input: SolveInput,
        start_ns: int,
    ) -> SolveResult | None:
        """
        Try V3-V3 Rust solver for 2-hop paths where both hops are V3.

        Returns None if not applicable (non-V3 hops) or on error.
        """
        if self._rust_optimizer is None:
            return None

        try:
            # Check both hops are V3 with tick range data
            v3_hops: list[BoundedProductHop] = []
            for hop in solve_input.hops:
                if hop.invariant == PoolInvariant.BOUNDED_PRODUCT and isinstance(
                    hop, BoundedProductHop
                ):
                    v3_hops.append(hop)
                else:
                    return None  # Non-V3 hop, not V3-V3

            if len(v3_hops) != 2:
                return None  # Need exactly 2 V3 hops

            # Get cached Rust sequences for both hops
            rust_seq1 = self._get_cached_rust_sequence(v3_hops[0])
            rust_seq2 = self._get_cached_rust_sequence(v3_hops[1])

            max_input_float = (
                float(solve_input.max_input) if solve_input.max_input is not None else None
            )

            result = self._rust_optimizer.solve_v3_v3(
                rust_seq1,
                rust_seq2,
                max_input_float,
                10,
            )

            if result.success:
                elapsed_ns = time.perf_counter_ns() - start_ns
                return SolveResult(
                    optimal_input=int(result.optimal_input),
                    profit=int(result.profit),
                    success=True,
                    iterations=result.iterations,
                    method=SolverMethod.PIECEWISE_MOBIUS,
                    solve_time_ns=elapsed_ns,
                )
            return None

        except Exception:
            return None

    def _try_rust_multi_range(
        self,
        solve_input: SolveInput,
        v3_hop_index: int,
        v3_hop: BoundedProductHop,
        start_ns: int,
    ) -> SolveResult | None:
        """
        Try to solve multi-range V3 using Rust's full sequence solver.

        Uses cached Rust objects to minimize Python-Rust marshalling overhead.
        """
        if self._rust_optimizer is None:
            return None

        try:
            # Get cached Rust objects
            rust_hops = self._get_cached_rust_hops(solve_input)
            rust_sequence = self._get_cached_rust_sequence(v3_hop)

            # Call Rust full sequence solver
            max_input_float = (
                float(solve_input.max_input) if solve_input.max_input is not None else None
            )

            result = self._rust_optimizer.solve_v3_sequence(
                rust_hops,
                v3_hop_index,
                rust_sequence,
                3,  # max_candidates
                max_input_float,
            )

            if result.success:
                elapsed_ns = time.perf_counter_ns() - start_ns
                return SolveResult(
                    optimal_input=int(result.optimal_input),
                    profit=int(result.profit),
                    success=True,
                    iterations=result.iterations,
                    method=SolverMethod.PIECEWISE_MOBIUS,
                    solve_time_ns=elapsed_ns,
                )
            return None

        except Exception:
            # Any error falls back to Python implementation
            return None

    def _estimate_final_sqrt_price(
        self,
        amount_in: float,
        ending_range: _V3TickRangeHop,
    ) -> float:
        """Estimate the final sqrt price after swapping within ending range."""
        if amount_in <= 0:
            return ending_range.sqrt_price_current

        # For constant product, price moves as: 1/sqrt_p_new = 1/sqrt_p + amount/L
        # So: sqrt_p_new = 1 / (1/sqrt_p + amount/L) # noqa: ERA001
        L = ending_range.liquidity
        sqrt_p = ending_range.sqrt_price_current
        gamma = 1.0 - ending_range.fee

        if ending_range.zero_for_one:
            # token0 -> token1: price goes down
            new_sqrt_p = L / (L / sqrt_p + amount_in / gamma)
        else:
            # token1 -> token0: price goes up
            new_sqrt_p = sqrt_p + amount_in * gamma / L

        return new_sqrt_p


# ---------------------------------------------------------------------------
# Solidly Stable Solver (Newton's method for x³y + xy³ ≥ k)
# ---------------------------------------------------------------------------


def _solidly_swap_output_float(
    reserve_in: float,
    reserve_out: float,
    amount_in: float,
    gamma: float,
    decimals_in: int,
    decimals_out: int,
) -> float:
    """
    Float approximation of Solidly stable swap output.

    Solves the Solidly invariant x³y + xy³ ≥ k for output given input.
    Uses Newton's method on the implicit equation f(y) = x³y + xy³ - k = 0.

    The reserves and amounts are scaled to 18-decimal internally, matching
    the Solidity contract behavior.
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

        if abs(f_prime) < 1e-30:
            break

        dy = f_y / f_prime
        y -= dy

        # y must be positive
        if y <= 0:
            y = 1.0
            break

        if abs(dy) < 1e-3:
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
    """
    Simulate a path with mixed hop types using float math.

    For each hop:
    - ConstantProductHop: V2 formula y = gamma*s*x / (r + gamma*x)
    - BoundedProductHop: Same V2 formula (virtual reserves)
    - SolidlyStableHop: float approximation of Solidly swap

    For integer-exact evaluation, use ``_simulate_mixed_path_int`` instead.
    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0.0

        if hop.invariant in (
            PoolInvariant.CONSTANT_PRODUCT,
            PoolInvariant.BOUNDED_PRODUCT,
        ):
            r_i = float(hop.reserve_in)
            s_i = float(hop.reserve_out)
            g_i = hop.gamma
            denom = r_i + amount * g_i
            if denom <= 0:
                return 0.0
            amount = amount * g_i * s_i / denom

        elif hop.invariant == PoolInvariant.SOLIDLY_STABLE:
            # Use integer swap if available for better accuracy
            if hop.swap_fn is not None:
                amount_int = hop.swap_fn(int(amount))
                amount = float(amount_int)
            else:
                amount = _solidly_swap_output_float(
                    reserve_in=float(hop.reserve_in),
                    reserve_out=float(hop.reserve_out),
                    amount_in=amount,
                    gamma=hop.gamma,
                    decimals_in=hop.decimals_in,
                    decimals_out=hop.decimals_out,
                )

        else:
            # Unsupported invariant
            return 0.0

    return amount


def _simulate_mixed_path_int(
    x: int,
    hops: tuple[HopType, ...],
) -> int:
    """
    Simulate a path with mixed hop types using integer math.

    For Solidly hops with ``swap_fn``, uses the integer-accurate callable.
    For V2 hops, uses integer constant-product formula.
    Falls back to float for hops without integer support.
    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0

        if hop.invariant in (
            PoolInvariant.CONSTANT_PRODUCT,
            PoolInvariant.BOUNDED_PRODUCT,
        ):
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

        elif hop.invariant == PoolInvariant.SOLIDLY_STABLE:
            if hop.swap_fn is not None:
                amount = hop.swap_fn(amount)
            else:
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

        else:
            return 0

    return amount


class SolidlyStableSolver(Solver):
    """
    Solver for paths containing Solidly stable pools (x³y + xy³ ≥ k).

    Uses golden section search on the integer profit function when
    ``swap_fn`` is available on Solidly hops (EVM-exact). Falls back
    to Newton's method with float simulation when no swap_fn is set.

    For mixed paths (V2 + Solidly), the initial bracket comes from
    treating all hops as V2-equivalent and running the Möbius solver.

    Performance:
    - With swap_fn (integer): ~180μs (25 golden section iterations)
    - Without swap_fn (float): ~257μs (Newton's method)
    """

    GOLDEN_SECTION_ITERATIONS = 25
    NEWTON_MAX_ITERATIONS = 30
    NEWTON_TOLERANCE = 1e-6

    def _adaptive_step(self, x: float) -> float:
        """Compute an adaptive finite-difference step size."""
        return max(min(x * 1e-4, 1e12), 1e6)

    def supports(self, solve_input: SolveInput) -> bool:
        if solve_input.num_hops < 2:
            return False
        if not solve_input.has_solidly_stable:
            return False
        for hop in solve_input.hops:
            if hop.invariant not in (
                PoolInvariant.CONSTANT_PRODUCT,
                PoolInvariant.BOUNDED_PRODUCT,
                PoolInvariant.SOLIDLY_STABLE,
            ):
                return False
        return True

    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if not self.supports(solve_input):
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.SOLIDLY_STABLE,
                error="SolidlyStableSolver requires 2+ hops with at least one Solidly stable",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        # --- Profitability check via V2-equivalent Möbius ---
        v2_equiv_hops: list[HopType] = []
        for hop in solve_input.hops:
            if hop.invariant == PoolInvariant.SOLIDLY_STABLE:
                v2_equiv_hops.append(
                    ConstantProductHop(
                        reserve_in=hop.reserve_in,
                        reserve_out=hop.reserve_out,
                        fee=hop.fee,
                    )
                )
            else:
                v2_equiv_hops.append(hop)

        mobius_coeffs = _compute_mobius_coefficients(tuple(v2_equiv_hops))

        if not mobius_coeffs.is_profitable:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.SOLIDLY_STABLE,
                error="Not profitable (V2-equivalent Möbius check failed)",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        # Check whether all Solidly hops have swap_fn (integer path)
        has_swap_fn = all(
            hop.swap_fn is not None
            for hop in solve_input.hops
            if hop.invariant == PoolInvariant.SOLIDLY_STABLE
        )

        if has_swap_fn:
            return self._solve_golden_section(solve_input, mobius_coeffs, start_ns)
        return self._solve_newton(solve_input, mobius_coeffs, start_ns)

    def _solve_golden_section(
        self,
        solve_input: SolveInput,
        mobius_coeffs: _MobiusCoefficients,
        start_ns: int,
    ) -> SolveResult:
        """Golden section search using integer path evaluation."""
        hops = solve_input.hops

        # Bracket: [1, max_reserve] # noqa: ERA001
        x_low = 1
        max_reserve = max(h.reserve_in for h in hops)
        x_high = max_reserve
        if solve_input.max_input is not None:
            x_high = min(x_high, solve_input.max_input)

        # Narrow bracket using Möbius initial guess
        x_mobius = mobius_coeffs.optimal_input()
        if x_mobius > 0:
            # Center bracket around Möbius estimate (±5x)
            x_center = min(int(x_mobius), x_high)
            x_low = max(1, x_center // 5)
            x_high = min(x_center * 5, x_high)

        phi = (math.sqrt(5) - 1) / 2  # ~0.618
        n_iterations = self.GOLDEN_SECTION_ITERATIONS

        x1 = int(x_high - phi * (x_high - x_low))
        x2 = int(x_low + phi * (x_high - x_low))
        p1 = _simulate_mixed_path_int(x1, hops) - x1
        p2 = _simulate_mixed_path_int(x2, hops) - x2

        for _ in range(n_iterations):
            if p1 < p2:
                x_low = x1
                x1 = x2
                p1 = p2
                x2 = int(x_low + phi * (x_high - x_low))
                p2 = _simulate_mixed_path_int(x2, hops) - x2
            else:
                x_high = x2
                x2 = x1
                p2 = p1
                x1 = int(x_high - phi * (x_high - x_low))
                p1 = _simulate_mixed_path_int(x1, hops) - x1

        x_opt = int((x_low + x_high) / 2)

        # Integer verification: check a few candidates around the optimum
        best_input = 0
        best_profit = 0
        search_radius = 3

        for candidate in range(max(1, x_opt - search_radius), x_opt + search_radius + 2):
            if solve_input.max_input is not None and candidate > solve_input.max_input:
                continue
            output = _simulate_mixed_path_int(candidate, hops)
            profit = output - candidate
            if profit > best_profit:
                best_profit = profit
                best_input = candidate

        elapsed_ns = time.perf_counter_ns() - start_ns

        if best_profit <= 0:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=n_iterations,
                method=SolverMethod.SOLIDLY_STABLE,
                error="Not profitable (integer verification failed)",
                solve_time_ns=elapsed_ns,
            )

        return SolveResult(
            optimal_input=best_input,
            profit=best_profit,
            success=True,
            iterations=n_iterations,
            method=SolverMethod.SOLIDLY_STABLE,
            solve_time_ns=elapsed_ns,
        )

    def _solve_newton(
        self,
        solve_input: SolveInput,
        mobius_coeffs: _MobiusCoefficients,
        start_ns: int,
    ) -> SolveResult:
        """Newton's method with float simulation (fallback when no swap_fn)."""
        hops = solve_input.hops

        x = mobius_coeffs.optimal_input()
        if x <= 0:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.SOLIDLY_STABLE,
                error="Möbius initial guess <= 0",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        if solve_input.max_input is not None and x > float(solve_input.max_input):
            x = float(solve_input.max_input)

        iterations = 0
        for i in range(self.NEWTON_MAX_ITERATIONS):
            h = self._adaptive_step(x)

            output = _simulate_mixed_path(x, hops)
            profit = output - x

            output_plus = _simulate_mixed_path(x + h, hops)
            output_minus = _simulate_mixed_path(max(x - h, 0.0), hops)
            dprofit = (output_plus - output_minus - 2 * h) / (2 * h)

            iterations = i + 1

            if abs(dprofit) < self.NEWTON_TOLERANCE:
                break

            profit_plus = output_plus - (x + h)
            profit_mid = profit
            profit_minus = output_minus - max(x - h, 0.0)
            d2profit = (profit_plus - 2 * profit_mid + profit_minus) / (h * h)

            if abs(d2profit) < 1e-30:
                if abs(dprofit) > 1e-12:
                    x += dprofit * 0.01
                    if x <= 0:
                        x /= 2.0
                else:
                    break
            else:
                step = dprofit / d2profit
                max_step = x * 0.5
                if abs(step) > max_step:
                    step = max_step if step > 0 else -max_step
                x_new = x - step
                if x_new <= 0:
                    x_new = x / 2.0
                x = x_new

        if x <= 0:
            elapsed_ns = time.perf_counter_ns() - start_ns
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=iterations,
                method=SolverMethod.SOLIDLY_STABLE,
                error="Newton did not converge to positive input",
                solve_time_ns=elapsed_ns,
            )

        if solve_input.max_input is not None and x > float(solve_input.max_input):
            x = float(solve_input.max_input)

        # Integer verification
        x_floor = int(x)
        best_input = 0
        best_profit = 0
        search_radius = 5

        for candidate in range(max(1, x_floor - search_radius), x_floor + search_radius + 2):
            if solve_input.max_input is not None and candidate > solve_input.max_input:
                continue
            output = _simulate_mixed_path(float(candidate), hops)
            profit = int(output) - candidate
            if profit > best_profit:
                best_profit = profit
                best_input = candidate

        elapsed_ns = time.perf_counter_ns() - start_ns

        if best_profit <= 0:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=iterations,
                method=SolverMethod.SOLIDLY_STABLE,
                error="Not profitable (integer verification failed)",
                solve_time_ns=elapsed_ns,
            )

        return SolveResult(
            optimal_input=best_input,
            profit=best_profit,
            success=True,
            iterations=iterations,
            method=SolverMethod.SOLIDLY_STABLE,
            solve_time_ns=elapsed_ns,
        )


# ---------------------------------------------------------------------------
# Balancer Multi-Token Solver (closed-form N-token G3M arbitrage)
# ---------------------------------------------------------------------------


class BalancerMultiTokenSolver(Solver):
    """
    Closed-form solver for N-token Balancer weighted pool basket arbitrage.

    Uses the QuantAMM closed-form solution (Equation 9) from Willetts &
    Harrington's paper for optimal multi-token trades on geometric mean
    market makers.

    Performance:
    - N=3: ~12μs (12 signatures)
    - N=4: ~50μs (50 signatures)
    - N=5: ~180μs (180 signatures)

    Unlike pairwise solvers, this finds optimal basket trades where
    multiple tokens can be deposited/withdrawn simultaneously.

    Usage:
    -----
    >>> from degenbot.arbitrage.optimizers.solver import (
    ...     BalancerMultiTokenHop, BalancerMultiTokenSolver, SolveInput
    ... )
    >>> hop = BalancerMultiTokenHop(
    ...     reserves=(100e18, 2e12, 1e12),  # WETH, USDC, DAI in wei
    ...     weights=(5e17, 25e16, 25e16),   # 50%, 25%, 25%
    ...     fee=Fraction(3, 1000),
    ...     market_prices=(2000.0, 1.0, 1.0),  # In USD
    ... )
    >>> solver = BalancerMultiTokenSolver()
    >>> result = solver.solve(SolveInput(hops=(hop,)))
    """

    def __init__(
        self,
        use_heuristic_pruning: bool = False,
        max_signatures: int = 500,
    ) -> None:
        """
        Initialize the solver.

        Parameters
        ----------
        use_heuristic_pruning
            If True, use price-ratio heuristic to prune signatures.
            Recommended for N >= 6.
        max_signatures
            Maximum signatures to evaluate before forcing pruning.
        """
        self._solver = _BalancerWeightedPoolSolver(
            use_heuristic_pruning=use_heuristic_pruning,
            max_signatures=max_signatures,
        )

    def supports(self, solve_input: SolveInput) -> bool:
        # Only supports single BalancerMultiTokenHop
        return (
            solve_input.num_hops == 1
            and solve_input.hops[0].invariant == PoolInvariant.BALANCER_MULTI_TOKEN
        )

    def solve(self, solve_input: SolveInput) -> SolveResult:
        start_ns = time.perf_counter_ns()

        if not self.supports(solve_input):
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.BALANCER_MULTI_TOKEN,
                error="BalancerMultiTokenSolver requires single BalancerMultiTokenHop",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        hop = solve_input.hops[0]
        assert isinstance(hop, BalancerMultiTokenHop)

        if hop.market_prices is None:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.BALANCER_MULTI_TOKEN,
                error="BalancerMultiTokenHop requires market_prices",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        pool = _BalancerMultiTokenState(
            reserves=hop.reserves,
            weights=hop.weights,
            fee=hop.fee,
            decimals=hop.decimals,
        )

        max_input: float | None = None
        if solve_input.max_input is not None:
            max_input = float(solve_input.max_input)

        result = self._solver.solve(pool, hop.market_prices, max_input=max_input)

        elapsed_ns = time.perf_counter_ns() - start_ns

        if not result.success:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=result.iterations,
                method=SolverMethod.BALANCER_MULTI_TOKEN,
                error="No profitable basket trade found",
                solve_time_ns=elapsed_ns,
            )

        # For basket trades, "optimal_input" is the total deposit value
        # and "profit" is the total withdrawal value minus deposits
        total_deposit = sum(max(0, t) * hop.market_prices[i] for i, t in enumerate(result.trades))
        profit = result.profit

        return SolveResult(
            optimal_input=int(total_deposit),
            profit=int(profit),
            success=True,
            iterations=result.iterations,
            method=SolverMethod.BALANCER_MULTI_TOKEN,
            solve_time_ns=elapsed_ns,
        )


# ---------------------------------------------------------------------------
# ArbSolver — the dispatcher
# ---------------------------------------------------------------------------


class ArbSolver(Solver):
    """
    Top-level solver that dispatches to the best method.

    Delegates to Rust for all supported path types (V2, V3 single-range,
    V3 multi-range, V3-V3). Falls back to Python for unsupported types
    (Solidly stable, Balancer, Curve).

    The Rust dispatch eliminates Python overhead for method selection
    and Möbius computation. V2-V2 paths go through Rust Möbius directly
    (~0.19μs instead of ~5.8μs via Python Möbius).

    Usage:
    -----
    >>> from degenbot.arbitrage.optimizers.solver import ArbSolver, Hop, SolveInput
    >>> solver = ArbSolver()
    >>> result = solver.solve(SolveInput(hops=(
    ...     Hop(reserve_in=2_000_000e6, reserve_out=1_000e18, fee=Fraction(3, 1000)),
    ...     Hop(reserve_in=1_500_000e6, reserve_out=800e18, fee=Fraction(3, 1000)),
    ... )))
    """

    # Method tag mapping from Rust to Python SolverMethod
    _RUST_METHOD_MAP: dict[int, SolverMethod] = {
        0: SolverMethod.MOBIUS,
        1: SolverMethod.PIECEWISE_MOBIUS,
        2: SolverMethod.PIECEWISE_MOBIUS,  # V3-V3 dispatched as piecewise
    }

    def __init__(self) -> None:
        self._rust_solver: Any = None
        self._pool_cache: Any = None
        self._next_pool_id: int = 1
        self._pool_id_map: dict[int, int] = {}  # id(pool) -> pool_id
        self._piecewise = PiecewiseMobiusSolver()
        self._solidly = SolidlyStableSolver()
        self._balancer_multi = BalancerMultiTokenSolver()
        self._brent = BrentSolver()
        self._v3_sequence_cache: dict[tuple, Any] = {}
        self._generalized_solver: Any = None
        self._try_load_rust()

    def _try_load_rust(self) -> None:
        """Try to load the Rust unified solver and pool cache."""
        try:
            from degenbot.degenbot_rs import mobius

            self._rust_solver = mobius.RustArbSolver()
            self._pool_cache = mobius.RustPoolCache()
        except ImportError:
            self._rust_solver = None
            self._pool_cache = None

    def get_pool_cache(self) -> Any:
        """Return the Rust-side pool state cache.

        The cache can be used to register pool states at update time,
        then solve by pool ID reference without any Python object
        construction on the solve path.
        """
        if self._pool_cache is None:
            raise RuntimeError("Pool cache requires the Rust extension (degenbot_rs)")
        return self._pool_cache

    def register_pool(
        self,
        reserve_in: int,
        reserve_out: int,
        fee: Fraction,
        *,
        pool_id: int | None = None,
    ) -> int:
        """Register a pool's state in the Rust cache.

        Call this at pool state update time (once per block). The returned
        pool_id can then be used in `solve_cached()` calls.

        If pool_id is not provided, a new unique ID is assigned.

        Returns the pool_id (useful when auto-assigning).
        """
        cache = self.get_pool_cache()

        if pool_id is None:
            pool_id = self._next_pool_id
            self._next_pool_id += 1

        fee_denom = fee.denominator
        gamma_numer = fee_denom - fee.numerator
        cache.insert(pool_id, reserve_in, reserve_out, gamma_numer, fee_denom)
        return pool_id

    def update_pool(
        self,
        pool_id: int,
        reserve_in: int,
        reserve_out: int,
        fee: Fraction,
    ) -> None:
        """Update a previously registered pool's state in the Rust cache.

        Equivalent to register_pool() with an explicit pool_id.
        """
        cache = self.get_pool_cache()
        fee_denom = fee.denominator
        gamma_numer = fee_denom - fee.numerator
        cache.insert(pool_id, reserve_in, reserve_out, gamma_numer, fee_denom)

    def remove_pool(self, pool_id: int) -> bool:
        """Remove a pool from the Rust cache.

        Returns True if the pool was found and removed.
        """
        cache = self.get_pool_cache()
        return cache.remove(pool_id)

    def solve_cached(
        self,
        path: list[int],
        *,
        max_input: int | None = None,
    ) -> SolveResult:
        """Solve an arbitrage path using cached pool states by ID.

        This is the fastest solve path: no Python object construction,
        no per-item extraction, just a list of integer pool IDs passed
        to Rust. Pool states must have been registered beforehand via
        `register_pool()` or `update_pool()`.

        Parameters
        ----------
        path
            Ordered list of pool IDs along the arbitrage path.
        max_input
            Optional maximum input constraint.

        Returns
        -------
        SolveResult
        """
        start_ns = time.perf_counter_ns()
        cache = self.get_pool_cache()

        max_input_float = float(max_input) if max_input is not None else None

        try:
            result = cache.solve(path, max_input_float)
        except (ValueError, TypeError):
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.MOBIUS,
                error="Pool cache solve failed",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        if not result.supported:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=0,
                method=SolverMethod.MOBIUS,
                error="Not supported by cache",
                solve_time_ns=time.perf_counter_ns() - start_ns,
            )

        elapsed_ns = time.perf_counter_ns() - start_ns
        method = self._RUST_METHOD_MAP.get(result.method, SolverMethod.MOBIUS)

        if not result.success:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=result.iterations,
                method=method,
                error="Not profitable",
                solve_time_ns=elapsed_ns,
            )

        # Integer refinement results from cache
        if result.optimal_input_int is not None and result.profit_int is not None:
            optimal_input = int(result.optimal_input_int)
            profit = int(result.profit_int)
            if profit > 0:
                return SolveResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    success=True,
                    iterations=result.iterations,
                    method=method,
                    solve_time_ns=elapsed_ns,
                )

        return SolveResult(
            optimal_input=0,
            profit=0,
            success=False,
            iterations=result.iterations,
            method=method,
            error="Not profitable",
            solve_time_ns=elapsed_ns,
        )

    def supports(self, solve_input: SolveInput) -> bool:
        return solve_input.num_hops >= 2

    def solve(self, solve_input: SolveInput) -> SolveResult:
        """
        Solve with automatic method selection.

        Tries Rust dispatch first, then Python fallback for unsupported types.
        """
        start_ns = time.perf_counter_ns()

        if USE_GENERALIZED_SOLVER:
            result = self._try_generalized_solve(solve_input, start_ns)
            if result is not None:
                return result

        # Try Rust fast path (handles V2, V3 single-range, V3 multi-range, V3-V3)
        if self._rust_solver is not None:
            result = self._try_rust_solve(solve_input, start_ns)
            if result is not None and result.success:
                return result

        # Python fallback for V3 multi-range (handles scale mismatches between
        # BoundedProductHop reserves and V3TickRangeSequence.to_hop_state())
        if self._piecewise.supports(solve_input):
            result = self._piecewise.solve(solve_input)
            if result.success:
                return result

        # SolidlyStableSolver
        if self._solidly.supports(solve_input):
            result = self._solidly.solve(solve_input)
            if result.success:
                return result

        # BalancerMultiTokenSolver
        if self._balancer_multi.supports(solve_input):
            result = self._balancer_multi.solve(solve_input)
            if result.success:
                return result

        # Brent fallback (handles everything)
        if self._brent.supports(solve_input):
            result = self._brent.solve(solve_input)
            if result.success:
                return result

        # All methods failed
        return SolveResult(
            optimal_input=0,
            profit=0,
            success=False,
            iterations=0,
            method=SolverMethod.MOBIUS,
            error="All solver methods failed to find a profitable solution",
            solve_time_ns=time.perf_counter_ns() - start_ns,
        )

    def _try_rust_solve(self, solve_input: SolveInput, start_ns: int) -> SolveResult | None:
        """
        Convert hops to Rust format and call RustArbSolver.

        When USE_RAW_ARRAY_MARSHALLING is enabled and all hops are constant/
        bounded-product (no multi-range V3), uses solve_raw() with a flat
        int array, avoiding Python object construction overhead.

        Otherwise, falls back to solve() with RustIntHopState objects
        (merged int refinement) or float tuples.

        Returns None if Rust cannot handle the path (unsupported hop types),
        signaling the caller to fall back to Python solvers.
        """
        from degenbot.degenbot_rs import mobius

        max_input_float = (
            float(solve_input.max_input) if solve_input.max_input is not None else None
        )

        # Check if we can use the raw array path:
        # - All hops must be ConstantProduct or single-range BoundedProduct
        # - USE_RAW_ARRAY_MARSHALLING must be enabled
        all_möbius_int = USE_RAW_ARRAY_MARSHALLING
        for hop in solve_input.hops:
            if isinstance(hop, ConstantProductHop | BoundedProductHop):
                if isinstance(hop, BoundedProductHop) and hop.has_multi_range:
                    all_möbius_int = False
            else:
                all_möbius_int = False

        if all_möbius_int:
            return self._try_rust_solve_raw(solve_input, start_ns, max_input_float, mobius)

        # Build hop list for the object-based solve() path
        rust_hops: list[Any] = []
        v3_sequences: list[tuple[int, Any]] = []

        for i, hop in enumerate(solve_input.hops):
            if isinstance(hop, ConstantProductHop):
                if USE_MERGED_INT_REFINEMENT:
                    # Build RustIntHopState for merged int refinement
                    fee_numer = hop.fee.numerator
                    fee_denom = hop.fee.denominator
                    gamma_numer = fee_denom - fee_numer
                    rust_hops.append(
                        mobius.RustIntHopState(
                            hop.reserve_in, hop.reserve_out, gamma_numer, fee_denom
                        )
                    )
                else:
                    rust_hops.append((
                        float(hop.reserve_in),
                        float(hop.reserve_out),
                        float(hop.fee),
                    ))
            elif isinstance(hop, BoundedProductHop):
                if hop.has_multi_range:
                    # Multi-range V3: use float tuple
                    seq = self._build_rust_v3_sequence(hop)
                    if seq is None:
                        return None  # Can't build sequence, fall back
                    v3_sequences.append((i, seq))
                    rust_hops.append((
                        float(hop.reserve_in),
                        float(hop.reserve_out),
                        float(hop.fee),
                    ))
                elif USE_MERGED_INT_REFINEMENT:
                    # Single-range V3: use int hops
                    fee_numer = hop.fee.numerator
                    fee_denom = hop.fee.denominator
                    gamma_numer = fee_denom - fee_numer
                    rust_hops.append(
                        mobius.RustIntHopState(
                            hop.reserve_in, hop.reserve_out, gamma_numer, fee_denom
                        )
                    )
                else:
                    rust_hops.append((
                        float(hop.reserve_in),
                        float(hop.reserve_out),
                        float(hop.fee),
                    ))
            else:
                # Unsupported hop type for Rust (Solidly, Balancer, Curve)
                return None

        result = self._rust_solver.solve(
            rust_hops,
            v3_sequences or None,
            max_input_float,
            10,
        )

        return self._process_rust_result(result, start_ns, solve_input)

    def _try_rust_solve_raw(
        self, solve_input: SolveInput, start_ns: int, max_input_float: float | None, mobius: Any
    ) -> SolveResult | None:
        """Build flat int array and call RustArbSolver.solve_raw().

        This is the fast path: no Python object construction for hops,
        just a flat list of Python ints passed directly to Rust.
        Only works for ConstantProduct and single-range BoundedProduct hops.
        """
        int_hops_flat: list[int] = []
        for hop in solve_input.hops:
            fee_denom = hop.fee.denominator
            gamma_numer = fee_denom - hop.fee.numerator
            int_hops_flat.extend([hop.reserve_in, hop.reserve_out, gamma_numer, fee_denom])

        try:
            result = self._rust_solver.solve_raw(int_hops_flat, max_input_float)
        except (ValueError, TypeError):
            return None

        return self._process_rust_result(result, start_ns, solve_input)

    def _process_rust_result(
        self, result: Any, start_ns: int, solve_input: SolveInput
    ) -> SolveResult | None:
        """Process a RustArbResult into a SolveResult, handling integer
        refinement and unprofitable cases."""
        if not result.supported:
            return None  # Fall back to Python

        elapsed_ns = time.perf_counter_ns() - start_ns
        method = self._RUST_METHOD_MAP.get(result.method, SolverMethod.MOBIUS)

        if not result.success:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=result.iterations,
                method=method,
                error="Not profitable",
                solve_time_ns=elapsed_ns,
            )

        # Check for merged integer refinement results
        if result.optimal_input_int is not None and result.profit_int is not None:
            # Merged int refinement: EVM-exact results already computed in Rust
            optimal_input = int(result.optimal_input_int)
            profit = int(result.profit_int)
            if profit > 0:
                return SolveResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    success=True,
                    iterations=result.iterations,
                    method=method,
                    solve_time_ns=elapsed_ns,
                )
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=result.iterations,
                method=method,
                error="Not profitable (integer verification failed)",
                solve_time_ns=elapsed_ns,
            )

        # Fallback: no merged integer refinement (float tuples only)
        x_opt = result.optimal_input
        if method == SolverMethod.MOBIUS and x_opt > 0:
            optimal_input, profit = self._rust_integer_refinement(
                x_opt, solve_input.hops, solve_input.max_input
            )
            if profit > 0:
                return SolveResult(
                    optimal_input=optimal_input,
                    profit=profit,
                    success=True,
                    iterations=result.iterations,
                    method=method,
                    solve_time_ns=elapsed_ns,
                )
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=result.iterations,
                method=method,
                error="Not profitable (integer verification failed)",
                solve_time_ns=elapsed_ns,
            )

        # For non-Möbius methods (V3 multi-range, V3-V3), use int() conversion
        x_opt = result.optimal_input
        optimal_input = int(x_opt)
        profit = int(result.profit)

        return SolveResult(
            optimal_input=optimal_input,
            profit=profit,
            success=True,
            iterations=result.iterations,
            method=method,
            solve_time_ns=elapsed_ns,
        )

    def _try_generalized_solve(self, solve_input: SolveInput, start_ns: int) -> SolveResult | None:
        """
        Delegate to the generalized MobiusSolver from degenbot.arbitrage.solver.

        Converts SolveInput hops to MobiusHopState/ConcentratedLiquidityHopState,
        calls MobiusSolver.solve(), and converts the result back to SolveResult.
        Returns None if the generalized solver doesn't support the path (e.g.
        Solidly, Balancer, Curve), signaling the caller to fall back.
        """
        # Deferred import: genuine circular dependency with
        # arbitrage/solver/mobius_solver.py (which imports from this module).
        from degenbot.arbitrage.solver.mobius_solver import MobiusSolver
        from degenbot.arbitrage.solver.types import (
            ConcentratedLiquidityHopState,
            MobiusHopState,
            TickRangeState,
        )
        from degenbot.arbitrage.solver.types import (
            SolverMethod as _GenSolverMethod,
        )

        if self._generalized_solver is None:
            self._generalized_solver = MobiusSolver()

        new_hops: list[MobiusHopState | ConcentratedLiquidityHopState] = []
        for hop in solve_input.hops:
            if isinstance(hop, ConstantProductHop):
                new_hops.append(
                    MobiusHopState(
                        reserve_in=hop.reserve_in,
                        reserve_out=hop.reserve_out,
                        fee=hop.fee,
                    )
                )
            elif isinstance(hop, BoundedProductHop):
                tick_ranges = None
                if hop.tick_ranges is not None:
                    tick_ranges = tuple(
                        TickRangeState(
                            tick_lower=tr.tick_lower,
                            tick_upper=tr.tick_upper,
                            liquidity=tr.liquidity,
                            sqrt_price_lower=tr.sqrt_price_lower,
                            sqrt_price_upper=tr.sqrt_price_upper,
                        )
                        for tr in hop.tick_ranges
                    )
                new_hops.append(
                    ConcentratedLiquidityHopState(
                        reserve_in=hop.reserve_in,
                        reserve_out=hop.reserve_out,
                        fee=hop.fee,
                        liquidity=hop.liquidity,
                        sqrt_price=hop.sqrt_price,
                        tick_lower=hop.tick_lower,
                        tick_upper=hop.tick_upper,
                        tick_ranges=tick_ranges,
                        current_range_index=hop.current_range_index,
                    )
                )
            else:
                return None

        gen_result = self._generalized_solver.solve(new_hops, max_input=solve_input.max_input)

        method_map = {
            _GenSolverMethod.MOBIUS: SolverMethod.MOBIUS,
            _GenSolverMethod.PIECEWISE_MOBIUS: SolverMethod.PIECEWISE_MOBIUS,
        }

        elapsed_ns = time.perf_counter_ns() - start_ns

        if not gen_result.is_profitable:
            return SolveResult(
                optimal_input=0,
                profit=0,
                success=False,
                iterations=gen_result.iterations,
                method=method_map.get(gen_result.method, SolverMethod.MOBIUS),
                error=gen_result.error or "Not profitable",
                solve_time_ns=elapsed_ns,
            )

        return SolveResult(
            optimal_input=gen_result.optimal_input,
            profit=gen_result.profit,
            success=True,
            iterations=gen_result.iterations,
            method=method_map.get(gen_result.method, SolverMethod.MOBIUS),
            solve_time_ns=elapsed_ns,
        )

    @staticmethod
    def _integer_refinement(
        x_opt: float,
        hops: tuple[HopType, ...],
        max_input: int | None,
    ) -> tuple[int, int]:
        """
        Integer refinement: check ±1 around float optimum for best integer profit.

        Uses Python's arbitrary-precision integers, so handles values
        exceeding i64 range correctly.
        """
        num_hops = len(hops)
        search_radius = 1 if num_hops <= 2 else min(num_hops, 5)

        x_floor = int(x_opt)
        best_input = x_floor
        best_profit = -1

        for candidate in range(max(1, x_floor - search_radius), x_floor + search_radius + 2):
            if max_input is not None and candidate > max_input:
                continue
            output = _simulate_path(float(candidate), hops)
            profit = int(output) - candidate
            if profit > best_profit:
                best_profit = profit
                best_input = candidate

        return best_input, best_profit

    @staticmethod
    def _rust_integer_refinement(
        x_opt: float,
        hops: tuple[HopType, ...],
        max_input: int | None,
    ) -> tuple[int, int]:
        """
        Integer refinement in Rust using EVM-exact U256 arithmetic.

        Converts Python hops to RustIntHopState, calls py_mobius_refine_int
        to search ±N around the float optimum with U256 simulation, and
        returns the best integer result.

        This replaces the Python _integer_refinement method, eliminating
        3-5 Python float-arithmetic _simulate_path calls (~3.7μs) in favor
        of a single Rust U256 call (~0.2μs).
        """
        try:
            from degenbot.degenbot_rs import mobius
        except ImportError:
            # Rust extension not available, fall back to Python
            return ArbSolver._integer_refinement(x_opt, hops, max_input)

        # Convert hops to RustIntHopState
        rust_int_hops: list[Any] = []
        for hop in hops:
            # Convert fee to gamma = 1 - fee for EVM-exact arithmetic
            # fee is a Fraction (e.g. 3/1000), gamma is (fee_denom - fee_numer) / fee_denom
            # EVM swap: y = gamma_numer * reserve_out * x / (gamma_denom * reserve_in + gamma_numer * x)
            fee_numer = hop.fee.numerator
            fee_denom = hop.fee.denominator
            gamma_numer = fee_denom - fee_numer
            gamma_denom = fee_denom
            rust_int_hops.append(
                mobius.RustIntHopState(hop.reserve_in, hop.reserve_out, gamma_numer, gamma_denom)
            )

        max_input_float = float(max_input) if max_input is not None else None
        result = mobius.py_mobius_refine_int(x_opt, rust_int_hops, max_input_float)

        if result.success:
            return int(result.optimal_input), int(result.profit)
        return 0, 0

    def _build_rust_v3_sequence(self, v3_hop: BoundedProductHop):
        """
        Build a RustV3TickRangeSequence from a BoundedProductHop.

        Uses a cache keyed by tick range object identity to avoid
        repeated construction.
        """
        from degenbot.degenbot_rs import mobius

        assert v3_hop.tick_ranges is not None
        zero_for_one = v3_hop.reserve_in > v3_hop.reserve_out

        # Cache key: object identity of tick ranges + direction
        range_ids = tuple(id(r) for r in v3_hop.tick_ranges)
        cache_key = (range_ids, v3_hop.current_range_index, zero_for_one)

        if cache_key in self._v3_sequence_cache:
            return self._v3_sequence_cache[cache_key]

        try:
            rust_ranges = []
            for i, range_info in enumerate(v3_hop.tick_ranges):
                # Determine current sqrt price for this range
                if i == v3_hop.current_range_index:
                    sqrt_p_current = float(v3_hop.sqrt_price) / Q96
                elif i < v3_hop.current_range_index:
                    sqrt_p_current = float(range_info.sqrt_price_upper) / Q96
                else:
                    sqrt_p_current = float(range_info.sqrt_price_lower) / Q96

                rust_ranges.append(
                    mobius.RustV3TickRangeHop(
                        liquidity=float(range_info.liquidity),
                        sqrt_price_current=sqrt_p_current,
                        sqrt_price_lower=float(range_info.sqrt_price_lower) / Q96,
                        sqrt_price_upper=float(range_info.sqrt_price_upper) / Q96,
                        fee=float(v3_hop.fee),
                        zero_for_one=zero_for_one,
                    )
                )

            sequence = mobius.RustV3TickRangeSequence(rust_ranges)
            self._v3_sequence_cache[cache_key] = sequence
            return sequence
        except Exception:
            return None


# ---------------------------------------------------------------------------
# Conversion Utilities
# ---------------------------------------------------------------------------


Q96 = 2**96


def _v3_virtual_reserves(
    liquidity: int,
    sqrt_price_x96: int,
    zero_for_one: bool,
) -> tuple[int, int]:
    """
    Compute virtual reserves for a V3/V4 tick range.

    For a concentrated-liquidity pool, the effective (virtual) reserves
    within the current tick range are:
        R0_virtual = L / sqrt_price
        R1_virtual = L * sqrt_price

    where sqrt_price = sqrt_price_x96 / 2**96.

    The reserves are returned as integers scaled to match V2 wei-scale
    reserve magnitudes for compatibility with the Möbius solver.

    Parameters
    ----------
    liquidity
        V3/V4 liquidity in this tick range.
    sqrt_price_x96
        Current sqrt price as Q64.96 fixed-point.
    zero_for_one
        True if swapping token0 → token1 (input is token0).

    Returns
    -------
    tuple[int, int]
        (reserve_in, reserve_out) as integers in wei-equivalent scale.
    """
    # Convert X96 to float for virtual reserve computation
    sqrt_price = sqrt_price_x96 / Q96
    L = float(liquidity)

    # Virtual reserves: R0 = L/sqrt_p, R1 = L*sqrt_p
    r0_virtual = L / sqrt_price  # token0 virtual reserves
    r1_virtual = L * sqrt_price  # token1 virtual reserves

    # Scale to integer — multiply by Q96 to preserve precision
    # The Möbius solver uses float internally, so the exact integer
    # scale doesn't matter as long as reserves are in the right ratio.
    # Using int(round()) to avoid drift.
    scale = Q96
    if zero_for_one:
        return int(round(r0_virtual * scale)), int(round(r1_virtual * scale))
    return int(round(r1_virtual * scale)), int(round(r0_virtual * scale))


# Cache for tick range lookups: (pool_address, current_tick, zero_for_one) -> result
_tick_range_cache: dict[tuple[str, int, bool], tuple[tuple[V3TickRangeInfo, ...], int] | None] = {}
_MAX_TICK_RANGE_CACHE_SIZE = 128


def _get_cached_tick_ranges(
    pool: UniswapV3Pool | UniswapV4Pool,
    zero_for_one: bool,
    max_ranges: int = 3,
) -> tuple[tuple[V3TickRangeInfo, ...], int] | None:
    """
    Cached version of _v3_get_adjacent_tick_ranges.

    Uses LRU-style cache keyed by (pool_address, current_tick, zero_for_one).
    Cache is cleared when it exceeds _MAX_TICK_RANGE_CACHE_SIZE entries.
    """
    global _tick_range_cache

    cache_key = (str(pool.address), pool.tick, zero_for_one)

    # Check cache
    if cache_key in _tick_range_cache:
        return _tick_range_cache[cache_key]

    # Compute and cache result
    result = _v3_get_adjacent_tick_ranges(pool, zero_for_one, max_ranges)

    # Simple LRU: clear if too large (simplest approach)
    if len(_tick_range_cache) >= _MAX_TICK_RANGE_CACHE_SIZE:
        _tick_range_cache.clear()

    _tick_range_cache[cache_key] = result
    return result


def _v3_get_adjacent_tick_ranges(
    pool: UniswapV3Pool | UniswapV4Pool,
    zero_for_one: bool,
    max_ranges: int = 3,
) -> tuple[tuple[V3TickRangeInfo, ...], int] | None:
    """
    Fetch adjacent tick ranges from a V3/V4 pool for multi-range support.

    Returns a tuple of (tick_ranges, current_range_index) where current_range_index
    indicates which range contains the current price. Returns None if the pool
    doesn't have full tick data available (sparse liquidity map).

    Parameters
    ----------
    pool
        A UniswapV3Pool or UniswapV4Pool.
    zero_for_one
        True if swapping token0 → token1.
    max_ranges
        Maximum number of ranges to fetch (including current).

    Returns
    -------
    tuple[tuple[V3TickRangeInfo, ...], int] | None
        Adjacent tick ranges and current range index, or None if sparse.
    """

    # Check if pool has full tick data (sparse pools can't provide adjacent ranges)
    if getattr(pool, "sparse_liquidity_map", True):
        return None

    tick_data = getattr(pool, "tick_data", None)
    tick_bitmap = getattr(pool, "tick_bitmap", None)
    tick_spacing = getattr(pool, "tick_spacing", 0)

    if tick_data is None or tick_bitmap is None or tick_spacing == 0:
        return None

    current_tick = pool.tick

    # Generate ticks in swap direction
    less_than_or_equal = not zero_for_one  # token0→token1: price goes down, tick goes down

    try:
        ticks_along_path = gen_ticks(
            tick_data=tick_data,
            starting_tick=current_tick,
            tick_spacing=tick_spacing,
            less_than_or_equal=less_than_or_equal,
        )
    except Exception:
        return None

    # Build list of initialized ticks
    # Clamp ticks to MIN_TICK/MAX_TICK bounds like real V3 pool does
    initialized_ticks: list[int] = []
    try:
        for tick, is_initialized in ticks_along_path:
            # Clamp to valid tick range (like UniswapV3Pool._calculate_swap)
            clamped_tick = (
                max(MIN_TICK, tick)  # descending ticks
                if less_than_or_equal
                else min(MAX_TICK, tick)  # ascending ticks
            )

            # Stop if we've reached the boundary
            if clamped_tick != tick:
                break

            if len(initialized_ticks) >= max_ranges + 1:
                break
            if is_initialized or tick == current_tick:
                initialized_ticks.append(tick)
    except StopIteration:
        pass

    if len(initialized_ticks) < 2:
        # Not enough range boundaries to form meaningful ranges
        return None

    # Build V3TickRangeInfo for each range
    ranges: list[V3TickRangeInfo] = []
    current_idx = 0

    for i in range(len(initialized_ticks) - 1):
        if zero_for_one:
            tick_lower = initialized_ticks[i + 1]
            tick_upper = initialized_ticks[i]
        else:
            tick_lower = initialized_ticks[i]
            tick_upper = initialized_ticks[i + 1]

        # Get liquidity at this tick
        tick_info = tick_data.get(tick_lower if zero_for_one else tick_upper)
        liquidity = tick_info.liquidity_net if tick_info else pool.liquidity

        # Compute sqrt price bounds
        sqrt_price_lower = int(get_sqrt_ratio_at_tick(tick_lower))
        sqrt_price_upper = int(get_sqrt_ratio_at_tick(tick_upper))

        range_info = V3TickRangeInfo(
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity=liquidity,
            sqrt_price_lower=sqrt_price_lower,
            sqrt_price_upper=sqrt_price_upper,
        )
        ranges.append(range_info)

        # Determine if this range contains current price
        if zero_for_one:
            if tick_lower <= current_tick < tick_upper:
                current_idx = i
        elif tick_lower <= current_tick < tick_upper:
            current_idx = i

    if len(ranges) < 1:
        return None

    return (tuple(ranges), current_idx)


def pool_to_hop(
    pool: UniswapV2Pool | AerodromeV2Pool | UniswapV3Pool | UniswapV4Pool | CamelotLiquidityPool,
    input_token: Erc20Token,
) -> HopType:
    """
    Convert a pool object to a Hop for the solver.

    For V2/Aerodrome volatile pools: returns ConstantProductHop with actual reserves.
    For Aerodrome stable pools: returns SolidlyStableHop with decimals.
    For Camelot volatile pools: returns ConstantProductHop with asymmetric fees.
    For Camelot stable pools: returns SolidlyStableHop with decimals.
    For V3/V4 pools: returns BoundedProductHop with virtual reserves.

    Parameters
    ----------
    pool
        A UniswapV2Pool, AerodromeV2Pool, UniswapV3Pool, or UniswapV4Pool.
    input_token
        The token being deposited into this pool.

    Returns
    -------
    Hop
        A Hop with reserves oriented for the swap direction.
    """
    zero_for_one = input_token == pool.token0

    # Camelot stable pool — Solidly invariant
    if isinstance(pool, CamelotLiquidityPool) and getattr(pool, "stable_swap", False):
        from degenbot.camelot.functions import get_y_camelot, k_camelot

        if zero_for_one:
            reserve_in = pool.state.reserves_token0
            reserve_out = pool.state.reserves_token1
            decimals_in = pool.token0.decimals
            decimals_out = pool.token1.decimals
        else:
            reserve_in = pool.state.reserves_token1
            reserve_out = pool.state.reserves_token0
            decimals_in = pool.token1.decimals
            decimals_out = pool.token0.decimals

        # Build swap_fn using Camelot's get_y
        _reserves0 = pool.state.reserves_token0
        _reserves1 = pool.state.reserves_token1
        _decimals0 = 10**pool.token0.decimals
        _decimals1 = 10**pool.token1.decimals
        _fee = pool.fee  # type: ignore[union-attr]
        _token_in = 0 if zero_for_one else 1

        from degenbot.solidly.solidly_functions import general_calc_exact_in_stable

        def _camelot_stable_swap_fn(
            amount_in: int,
            __reserves0: int = _reserves0,
            __reserves1: int = _reserves1,
            __decimals0: int = _decimals0,
            __decimals1: int = _decimals1,
            __fee: Fraction = _fee,
            __token_in: int = _token_in,
        ) -> int:
            return general_calc_exact_in_stable(
                amount_in=amount_in,
                token_in=__token_in,  # type: ignore[arg-type]
                reserves0=__reserves0,
                reserves1=__reserves1,
                decimals0=__decimals0,
                decimals1=__decimals1,
                fee=__fee,
                k_func=k_camelot,
                get_y_func=get_y_camelot,
            )

        return SolidlyStableHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=pool.fee,  # type: ignore[union-attr]
            decimals_in=decimals_in,
            decimals_out=decimals_out,
            swap_fn=_camelot_stable_swap_fn,
        )

    # Camelot volatile pool — constant product with asymmetric fees
    if isinstance(pool, CamelotLiquidityPool):
        # Camelot stores fee as tuple: (Fraction(fee_token0, denom), Fraction(fee_token1, denom))
        # pool.fee is the tuple from super().__init__
        fee_tuple = pool.fee  # type: ignore[union-attr]
        if zero_for_one:
            reserve_in = pool.state.reserves_token0
            reserve_out = pool.state.reserves_token1
            fee_in = fee_tuple[0]  # fee for token0 → token1
            fee_out = fee_tuple[1]  # fee for token1 → token0
        else:
            reserve_in = pool.state.reserves_token1
            reserve_out = pool.state.reserves_token0
            fee_in = fee_tuple[1]  # fee for token1 → token0
            fee_out = fee_tuple[0]  # fee for token0 → token1
        return ConstantProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_in,
            fee_out=fee_out,
        )

    # Aerodrome stable pool — Solidly invariant
    if isinstance(pool, AerodromeV2Pool) and getattr(pool, "stable", False):
        from degenbot.aerodrome.functions import calc_exact_in_stable as _aerodrome_stable_calc

        if zero_for_one:
            reserve_in = pool.state.reserves_token0
            reserve_out = pool.state.reserves_token1
            decimals_in = pool.token0.decimals
            decimals_out = pool.token1.decimals
        else:
            reserve_in = pool.state.reserves_token1
            reserve_out = pool.state.reserves_token0
            decimals_in = pool.token1.decimals
            decimals_out = pool.token0.decimals

        # Build swap_fn using Aerodrome's calc_exact_in_stable
        _reserves0 = pool.state.reserves_token0
        _reserves1 = pool.state.reserves_token1
        _decimals0 = 10**pool.token0.decimals
        _decimals1 = 10**pool.token1.decimals
        _fee = pool.fee
        _token_in = 0 if zero_for_one else 1

        def _aerodrome_stable_swap_fn(
            amount_in: int,
            __reserves0: int = _reserves0,
            __reserves1: int = _reserves1,
            __decimals0: int = _decimals0,
            __decimals1: int = _decimals1,
            __fee: Fraction = _fee,
            __token_in: int = _token_in,
        ) -> int:
            return _aerodrome_stable_calc(
                amount_in=amount_in,
                token_in=__token_in,  # type: ignore[arg-type]
                reserves0=__reserves0,
                reserves1=__reserves1,
                decimals0=__decimals0,
                decimals1=__decimals1,
                fee=__fee,
            )

        return SolidlyStableHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=pool.fee,
            decimals_in=decimals_in,
            decimals_out=decimals_out,
            swap_fn=_aerodrome_stable_swap_fn,
        )

    if isinstance(pool, UniswapV3Pool):
        # V3: virtual reserves from L and sqrt_price_x96
        # Fee is stored as int pip (e.g. 3000 = 0.3%), denominator 1_000_000
        fee_fraction = Fraction(pool.fee, pool.FEE_DENOMINATOR)
        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=pool.liquidity,
            sqrt_price_x96=pool.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        # Try to get adjacent tick ranges for multi-range support (cached)
        tick_ranges_result = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=zero_for_one,
            max_ranges=3,
        )
        if tick_ranges_result is not None:
            tick_ranges, current_range_index = tick_ranges_result
        else:
            tick_ranges = None
            current_range_index = 0

        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_fraction,
            liquidity=pool.liquidity,
            sqrt_price=pool.sqrt_price_x96,
            tick_lower=pool.tick,
            tick_upper=pool.tick,
            tick_ranges=tick_ranges,
            current_range_index=current_range_index,
        )

    if isinstance(pool, UniswapV4Pool):
        # V4: same structure as V3 (concentrated liquidity)
        fee_fraction = Fraction(pool.fee, pool.FEE_DENOMINATOR)
        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=pool.liquidity,
            sqrt_price_x96=pool.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        # Try to get adjacent tick ranges for multi-range support (cached)
        tick_ranges_result = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=zero_for_one,
            max_ranges=3,
        )
        if tick_ranges_result is not None:
            tick_ranges, current_range_index = tick_ranges_result
        else:
            tick_ranges = None
            current_range_index = 0

        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_fraction,
            liquidity=pool.liquidity,
            sqrt_price=pool.sqrt_price_x96,
            tick_lower=pool.tick,
            tick_upper=pool.tick,
            tick_ranges=tick_ranges,
            current_range_index=current_range_index,
        )

    # V2 pool (UniswapV2Pool or AerodromeV2Pool volatile) — actual reserves
    if zero_for_one:
        reserve_in = pool.state.reserves_token0
        reserve_out = pool.state.reserves_token1
    else:
        reserve_in = pool.state.reserves_token1
        reserve_out = pool.state.reserves_token0

    return ConstantProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=pool.fee,
    )


def pool_state_to_hop(
    pool: UniswapV2Pool | AerodromeV2Pool | UniswapV3Pool | UniswapV4Pool | CamelotLiquidityPool,
    input_token: Erc20Token,
    state_override: Any = None,
) -> HopType:
    """
    Convert a pool object to a Hop, with optional state override.

    Like pool_to_hop() but accepts a PoolState override for the pool's
    current state (used when simulating a different reserve configuration).

    Parameters
    ----------
    pool
        A UniswapV2Pool, AerodromeV2Pool, UniswapV3Pool, or UniswapV4Pool.
    input_token
        The token being deposited into this pool.
    state_override
        Optional PoolState to use instead of pool.state.

    Returns
    -------
    Hop
        A Hop with reserves oriented for the swap direction.
    """
    state = state_override or pool.state
    zero_for_one = input_token == pool.token0

    # Aerodrome stable pool — Solidly invariant
    if isinstance(pool, AerodromeV2Pool) and getattr(pool, "stable", False):
        if zero_for_one:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
            decimals_in = pool.token0.decimals
            decimals_out = pool.token1.decimals
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0
            decimals_in = pool.token1.decimals
            decimals_out = pool.token0.decimals
        return SolidlyStableHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=pool.fee,
            decimals_in=decimals_in,
            decimals_out=decimals_out,
        )

    if isinstance(pool, UniswapV3Pool) or isinstance(pool, UniswapV4Pool):
        fee_fraction = Fraction(pool.fee, pool.FEE_DENOMINATOR)
        reserve_in, reserve_out = _v3_virtual_reserves(
            liquidity=state.liquidity,
            sqrt_price_x96=state.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )
        # Try to get adjacent tick ranges for multi-range support (cached)
        tick_ranges_result = _get_cached_tick_ranges(
            pool=pool,
            zero_for_one=zero_for_one,
            max_ranges=3,
        )
        if tick_ranges_result is not None:
            tick_ranges, current_range_index = tick_ranges_result
        else:
            tick_ranges = None
            current_range_index = 0

        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_fraction,
            liquidity=state.liquidity,
            sqrt_price=state.sqrt_price_x96,
            tick_lower=state.tick,
            tick_upper=state.tick,
            tick_ranges=tick_ranges,
            current_range_index=current_range_index,
        )

    # V2 pool (UniswapV2Pool or AerodromeV2Pool volatile)
    if zero_for_one:
        reserve_in = state.reserves_token0
        reserve_out = state.reserves_token1
    else:
        reserve_in = state.reserves_token1
        reserve_out = state.reserves_token0

    return ConstantProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=pool.fee,
    )


def pools_to_solve_input(
    pools: list,
    input_token: Erc20Token,
    max_input: int | None = None,
) -> SolveInput:
    """
    Convert a list of pool objects to a SolveInput.

    Parameters
    ----------
    pools
        Ordered list of pools in the arbitrage path.
    input_token
        The input (profit) token.
    max_input
        Optional maximum input constraint.

    Returns
    -------
    SolveInput
        Solver input with Hop for each pool.
    """
    hops: list[HopType] = []
    current_token = input_token

    for pool in pools:
        hop = pool_to_hop(pool, current_token)
        hops.append(hop)
        # Advance the current token
        if current_token == pool.token0:
            current_token = pool.token1
        else:
            current_token = pool.token0

    return SolveInput(hops=tuple(hops), max_input=max_input)
