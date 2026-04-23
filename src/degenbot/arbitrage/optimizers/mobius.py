"""
Möbius transformation optimizer for constant product AMM arbitrage,
including V3/V4 bounded liquidity pools.

Every constant product swap y = (f·s·x)/(r + f·x) is a Möbius transformation
that fixes the origin. This includes V3/V4 tick ranges, which are bounded
product CFMMs with effective reserves:

    y = gamma*(R1+beta)*x / ((R0+alpha) + gamma*x)

which is the same Möbius form with r_eff = R0+alpha, s_eff = R1+beta.

Möbius transformations form a group under composition, so any n-hop path
(whether V2 or V3 within a single tick range) reduces to a single rational
function:

    l(x) = K·x / (M + N·x)

The coefficients K, M, N are computed via an O(n) recurrence (three scalar
updates per hop). The optimal input follows from d(l(x) - x)/dx = 0:

    x_opt = (sqrt(K·M) - M) / N

This is exact and requires zero iterations regardless of path length.

Profitability check (free from the same recurrence): K / M > 1

For V3/V4 paths that may cross tick boundaries, the swap function is
piecewise-Möbius. We handle this by checking a small number of candidate
"stopping ranges" (typically 1-3), each yielding a closed-form solution.

Limitations:
- V3/V4 optimization assumes swap stays within a single tick range
  (validation rejects solutions that cross boundaries)
- Overflow risk for very long paths with large reserves (K·M product)

References:
    Hartigan, J. (2026). "The Geometry of Arbitrage: Generalizing Multi-Hop
    DEX Paths via Möbius Transformations."

    Angeris, G., Chitra, T., Diamandis, T., Evans, A., Kulkarni, K. (2023).
    "The Geometry of Constant Function Market Makers." — Shows that V3 tick
    ranges are bounded product CFMMs (Eq. 11).

    Diamandis, T., Resnick, M., Chitra, T., Angeris, G. (2023).
    "An Efficient Algorithm for Optimal Routing Through CFMMs."
"""

import math
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from degenbot.arbitrage.optimizers.base import (
    OptimizerResult,
    OptimizerType,
)
from degenbot.exceptions import OptimizationError

if TYPE_CHECKING:
    from degenbot.erc20.erc20 import Erc20Token


@dataclass(frozen=True, slots=True)
class MobiusCoefficients:
    """
    The three scalar coefficients that fully describe an n-hop constant
    product path as a single Möbius transformation l(x) = K·x / (M + N·x).

    Attributes
    ----------
    K : float
        Numerator scaling coefficient.
    M : float
        Constant term in denominator.
    N : float
        Linear term in denominator.
    is_profitable : bool
        True when K/M > 1 (initial marginal rate exceeds 1).
    """

    K: float
    M: float
    N: float
    is_profitable: bool

    def path_output(self, x: float) -> float:
        """
        Compute the path output for input x.

        Parameters
        ----------
        x : float
            Input amount.

        Returns
        -------
        float
            Output amount from the full path.
        """
        denom = self.M + self.N * x
        if denom <= 0:
            return 0.0
        return self.K * x / denom

    def optimal_input(self) -> float:
        """
        Compute the exact optimal input that maximizes profit.

        Returns
        -------
        float
            Optimal input amount. Returns 0.0 if not profitable.
        """
        if not self.is_profitable:
            return 0.0
        return (math.sqrt(self.K * self.M) - self.M) / self.N

    def profit_at(self, x: float) -> float:
        """
        Compute profit l(x) - x for input x.

        Parameters
        ----------
        x : float
            Input amount.

        Returns
        -------
        float
            Profit (output minus input).
        """
        return self.path_output(x) - x


@dataclass(frozen=True, slots=True)
class HopState:
    """
    Reserve and fee state for a single pool hop.

    For V2 pools, reserve_in and reserve_out are the raw reserves.
    For V3 tick ranges, they are the effective reserves:
        reserve_in  = R_current + bound_parameter  (e.g. R0 + alpha)
        reserve_out = S_current + bound_parameter  (e.g. R1 + beta)

    Attributes
    ----------
    reserve_in : float
        Reserve of the token being deposited (input reserve).
        For V3: R0 + alpha (zero_for_one) or R1 + beta (one_for_zero).
    reserve_out : float
        Reserve of the token being received (output reserve).
        For V3: R1 + beta (zero_for_one) or R0 + alpha (one_for_zero).
    fee : float
        Fee fraction (e.g. 0.003 for 0.3%).
    """

    reserve_in: float
    reserve_out: float
    fee: float

    @property
    def gamma(self) -> float:
        """Fee multiplier (1 - fee)."""
        return 1.0 - self.fee


@dataclass(frozen=True, slots=True)
class V3TickRangeHop:
    """
    V3/V4 tick range data needed to build a Möbius HopState.

    Stores the bounded product CFMM parameters for a single V3 tick range
    along with the current pool state, so that we can construct effective
    reserves (R0+alpha, R1+beta) and validate that a solution stays in range.

    Attributes
    ----------
    liquidity : float
        Liquidity in this tick range.
    sqrt_price_current : float
        Current sqrt price of the pool (not X96 — plain float).
    sqrt_price_lower : float
        Lower sqrt price bound of the tick range.
    sqrt_price_upper : float
        Upper sqrt price bound of the tick range.
    fee : float
        Fee fraction for this pool.
    zero_for_one : bool
        True if the swap direction is token0 → token1 through this hop.
    """

    liquidity: float
    sqrt_price_current: float
    sqrt_price_lower: float
    sqrt_price_upper: float
    fee: float
    zero_for_one: bool

    @property
    def alpha(self) -> float:
        """Lower bound on R0: L / sqrt(P_upper)."""
        return self.liquidity / self.sqrt_price_upper

    @property
    def beta(self) -> float:
        """Lower bound on R1: L * sqrt(P_lower)."""
        return self.liquidity * self.sqrt_price_lower

    def to_hop_state(self) -> HopState:
        """
        Convert this V3 tick range to a Möbius HopState with effective reserves.

        The swap function for a bounded product CFMM is:
            y = gamma*(R1+beta)*x / ((R0+alpha) + gamma*x)

        where R0 = L/sqrt_p - alpha, R1 = L*sqrt_p - beta are the REAL reserves.
        So R0+alpha = L/sqrt_p and R1+beta = L*sqrt_p are the VIRTUAL reserves.

        These virtual reserves are the effective reserves for the Möbius
        formula: r_eff = L/sqrt_p, s_eff = L*sqrt_p.

        Returns
        -------
        HopState
            Hop with effective reserves for Möbius composition.
        """
        if self.zero_for_one:
            r_eff = self.liquidity / self.sqrt_price_current
            s_eff = self.liquidity * self.sqrt_price_current
        else:
            r_eff = self.liquidity * self.sqrt_price_current
            s_eff = self.liquidity / self.sqrt_price_current

        return HopState(
            reserve_in=r_eff,
            reserve_out=s_eff,
            fee=self.fee,
        )

    def contains_sqrt_price(self, sqrt_price: float) -> bool:
        """Check if a sqrt price is within this tick range."""
        return self.sqrt_price_lower <= sqrt_price <= self.sqrt_price_upper


@dataclass(frozen=True, slots=True)
class TickRangeCrossing:
    """
    Pre-computed crossing data for a V3 swap that crosses tick boundaries
    and ends in a specific range.

    When a V3 swap crosses ranges 0..K-1 and ends in range K:
        total_output = crossing_output + mobius(remaining_input, range_K)
        remaining_input = gross_input - crossing_gross_input

    Attributes
    ----------
    crossing_gross_input : float
        Total gross input (including fees) consumed by crossed ranges.
    crossing_output : float
        Total output from crossed ranges.
    ending_range : V3TickRangeHop
        The ending range with sqrt_price_current set to the entry boundary.
    """

    crossing_gross_input: float
    crossing_output: float
    ending_range: V3TickRangeHop


@dataclass(frozen=True, slots=True)
class V3TickRangeSequence:
    """
    Ordered sequence of V3 tick ranges in the swap direction.

    ranges[0] contains the current price. ranges[1], ranges[2], ... are
    adjacent ranges in the swap direction.

    All ranges must share the same fee and zero_for_one direction.
    """

    ranges: tuple[V3TickRangeHop, ...]

    def __post_init__(self) -> None:
        err_empty = "At least one range required"
        err_fee = "All ranges must have same fee"
        err_dir = "All ranges must have same direction"
        if not self.ranges:
            raise ValueError(err_empty)
        fee = self.ranges[0].fee
        zfo = self.ranges[0].zero_for_one
        for r in self.ranges:
            if r.fee != fee:
                raise ValueError(err_fee)
            if r.zero_for_one != zfo:
                raise ValueError(err_dir)

    @property
    def fee(self) -> float:
        return self.ranges[0].fee

    @property
    def zero_for_one(self) -> bool:
        return self.ranges[0].zero_for_one

    def compute_crossing(self, k: int) -> TickRangeCrossing:
        """
        Compute crossing data to reach range k (0-indexed).

        k=0: no crossing (swap stays in first range).
        k=1: cross range 0, end in range 1.
        k=2: cross ranges 0-1, end in range 2.

        The ending range's sqrt_price_current is set to the entry boundary
        price (the boundary with the previous range), so that to_hop_state()
        gives the correct effective reserves.

        Parameters
        ----------
        k : int
            Index of the ending range.

        Returns
        -------
        TickRangeCrossing
            Crossing data including fixed input/output and ending range.
        """
        if k < 0 or k >= len(self.ranges):
            err_bounds = f"k={k} out of range for {len(self.ranges)} ranges"
            raise IndexError(err_bounds)

        if k == 0:
            return TickRangeCrossing(
                crossing_gross_input=0.0,
                crossing_output=0.0,
                ending_range=self.ranges[0],
            )

        gamma = 1.0 - self.fee
        crossing_gross_input = 0.0
        crossing_output = 0.0

        for i in range(k):
            r = self.ranges[i]
            if i == 0:
                sqrt_p_start = r.sqrt_price_current
            elif self.zero_for_one:
                sqrt_p_start = self.ranges[i - 1].sqrt_price_lower
            else:
                sqrt_p_start = self.ranges[i - 1].sqrt_price_upper

            if self.zero_for_one:
                sqrt_p_end = r.sqrt_price_lower
                net_input = r.liquidity * (1.0 / sqrt_p_end - 1.0 / sqrt_p_start)
                output = r.liquidity * (sqrt_p_start - sqrt_p_end)
            else:
                sqrt_p_end = r.sqrt_price_upper
                net_input = r.liquidity * (sqrt_p_end - sqrt_p_start)
                output = r.liquidity * (1.0 / sqrt_p_start - 1.0 / sqrt_p_end)

            gross_input = net_input / gamma
            crossing_gross_input += gross_input
            crossing_output += output

        # Construct ending range with entry price at boundary
        ending = self.ranges[k]
        if self.zero_for_one:
            entry_sqrt_price = self.ranges[k - 1].sqrt_price_lower
        else:
            entry_sqrt_price = self.ranges[k - 1].sqrt_price_upper

        ending_range = V3TickRangeHop(
            liquidity=ending.liquidity,
            sqrt_price_current=entry_sqrt_price,
            sqrt_price_lower=ending.sqrt_price_lower,
            sqrt_price_upper=ending.sqrt_price_upper,
            fee=ending.fee,
            zero_for_one=ending.zero_for_one,
        )

        return TickRangeCrossing(
            crossing_gross_input=crossing_gross_input,
            crossing_output=crossing_output,
            ending_range=ending_range,
        )


def piecewise_v3_swap(
    gross_input: float,
    crossing: TickRangeCrossing,
) -> tuple[float, bool]:
    """
    Compute V3 swap output including tick crossings.

    For a V3 swap that crosses ranges 0..K-1 and ends in range K:
        total_output = crossing_output + mobius(remaining, range_K)

    Parameters
    ----------
    gross_input : float
        Total gross input (including fees) to the V3 pool.
    crossing : TickRangeCrossing
        Pre-computed crossing data.

    Returns
    -------
    tuple[float, bool]
        (output, valid) where valid=True if the input covers the crossing
        and the result stays within the ending range.
    """
    if gross_input < crossing.crossing_gross_input:
        return 0.0, False

    remaining = gross_input - crossing.crossing_gross_input
    ending_hop = crossing.ending_range.to_hop_state()
    variable_output = simulate_path(remaining, [ending_hop])

    total_output = crossing.crossing_output + variable_output

    # Validate that the remaining input stays in the ending range
    final_sqrt_price = estimate_v3_final_sqrt_price(remaining, crossing.ending_range)
    if not crossing.ending_range.contains_sqrt_price(final_sqrt_price):
        return total_output, False

    return total_output, True


def compute_mobius_coefficients(hops: list[HopState]) -> MobiusCoefficients:
    """
    Compute the Möbius transformation coefficients K, M, N for an n-hop
    constant product path via a single forward pass.

    The recurrence is derived from 2x2 matrix multiplication where each
    swap is encoded as:

        M_i = [[f_i * s_i, 0], [f_i, r_i]]

    and the product M_1 * M_2 * ... * M_n yields the composite
    transformation l(x) = K * x / (M + N * x).

    Parameters
    ----------
    hops : list[HopState]
        Pool states ordered along the arbitrage path.

    Returns
    -------
    MobiusCoefficients
        The three coefficients plus profitability flag.
    """
    if not hops:
        return MobiusCoefficients(K=0.0, M=1.0, N=0.0, is_profitable=False)

    # Initialize from first hop
    first = hops[0]
    K = first.gamma * first.reserve_out  # noqa: N806
    M = first.reserve_in  # noqa: N806
    N = first.gamma  # noqa: N806

    # Update for each subsequent hop
    # Note: N update uses K before it is updated in this step
    for hop in hops[1:]:
        old_K = K  # noqa: N806
        K = old_K * hop.gamma * hop.reserve_out  # noqa: N806
        M *= hop.reserve_in  # noqa: N806
        N = N * hop.reserve_in + old_K * hop.gamma  # noqa: N806

    is_profitable = K > M

    return MobiusCoefficients(K=K, M=M, N=N, is_profitable=is_profitable)


def simulate_path(x: float, hops: list[HopState]) -> float:
    """
    Simulate a swap through all hops for verification.

    Parameters
    ----------
    x : float
        Input amount to the first pool.
    hops : list[HopState]
        Pool states along the path.

    Returns
    -------
    float
        Final output amount.
    """
    amount = x
    for hop in hops:
        if amount <= 0:
            return 0.0
        denom = hop.reserve_in + amount * hop.gamma
        if denom <= 0:
            return 0.0
        amount = amount * hop.gamma * hop.reserve_out / denom
    return amount


def mobius_solve(
    hops: list[HopState],
    max_input: float | None = None,
) -> tuple[float, float, int]:
    """
    Solve for optimal arbitrage input using the Möbius transformation approach.

    Parameters
    ----------
    hops : list[HopState]
        Pool states along the arbitrage path.
    max_input : float | None
        Optional upper bound on input amount.

    Returns
    -------
    tuple[float, float, int]
        (optimal_input, profit, iterations) where iterations is always 0
        for the closed-form solution.
    """
    coeffs = compute_mobius_coefficients(hops)

    if not coeffs.is_profitable:
        return 0.0, 0.0, 0

    x_opt = coeffs.optimal_input()

    if x_opt <= 0:
        return 0.0, 0.0, 0

    # Apply max_input constraint
    if max_input is not None and x_opt > max_input:
        x_opt = max_input

    # Compute exact profit via path simulation (avoids floating-point drift)
    output = simulate_path(x_opt, hops)
    profit = output - x_opt

    return x_opt, profit, 0


def estimate_v3_final_sqrt_price(
    amount_in: float,
    v3_hop: V3TickRangeHop,
) -> float:
    """
    Estimate the final sqrt price after a V3 swap.

    Uses the V3 swap formula for price impact estimation.

    Parameters
    ----------
    amount_in : float
        Input amount to the V3 pool.
    v3_hop : V3TickRangeHop
        V3 tick range hop data.

    Returns
    -------
    float
        Estimated final sqrt price.
    """
    liquidity = v3_hop.liquidity
    gamma = 1.0 - v3_hop.fee
    sqrt_p = v3_hop.sqrt_price_current

    if liquidity <= 0:
        return sqrt_p

    if v3_hop.zero_for_one:
        denom = liquidity + amount_in * gamma * sqrt_p
        if denom <= 0:
            return sqrt_p
        return sqrt_p * liquidity / denom

    return sqrt_p + amount_in * gamma / liquidity


class MobiusOptimizer:
    """
    Arbitrage optimizer for constant product AMM paths using Möbius
    transformation composition, supporting both V2 and V3/V4 pools.

    V2 pools use raw reserves. V3/V4 tick ranges use effective reserves
    (R0+alpha, R1+beta) from the bounded product CFMM representation. Both
    produce the same Möbius form, so the O(n) recurrence and closed-form
    optimal input apply uniformly.

    For V3/V4 hops, you may pass either:
    - V3TickRangeHop objects (automatically converted to HopState), or
    - Pre-built HopState objects with effective reserves.

    For multi-range V3 swaps (tick crossing), use solve_v3_candidates()
    which checks multiple candidate tick ranges.

    Performance:
    - Pure V2: ~1-5μs (zero iterations)
    - V2 + V3 single range: ~1-5μs (zero iterations)
    - V2 + V3 multi-range (3 candidates): ~5-15μs

    Usage:
    -----
    >>> optimizer = MobiusOptimizer()
    >>> result = optimizer.solve([pool_a, pool_b, pool_c], input_token)
    >>> print(f"Optimal: {result.optimal_input}, Profit: {result.profit}")
    """

    @property
    def optimizer_type(self) -> OptimizerType:
        return OptimizerType.MOBIUS

    def solve(
        self,
        pools: list[Any],
        input_token: "Erc20Token",
        max_input: int | None = None,
    ) -> OptimizerResult:
        """
        Find optimal arbitrage for a constant product path.

        Accepts V2 pools and/or V3TickRangeHop objects for V3 ranges.
        For V2 pools, builds HopState from raw reserves. For V3 tick
        ranges, builds HopState from effective reserves (R0+alpha, R1+beta).

        Parameters
        ----------
        pools : list[Any]
            Ordered list of pools forming the arbitrage cycle.
            V2 pools are UniswapV2Pool-like objects.
            V3 tick ranges are V3TickRangeHop objects.
        input_token : Erc20Token
            The input (profit) token.
        max_input : int | None
            Maximum input amount constraint.

        Returns
        -------
        OptimizerResult
            Optimization result with optimal input and profit.
        """
        start_time = time.perf_counter_ns()

        min_pools = 2
        if len(pools) < min_pools:
            raise OptimizationError(
                "Möbius optimizer requires 2+ pools",
                iterations=0,
                method="mobius",
            )

        v2_pool_types = {"UniswapV2Pool", "MockV2Pool"}

        hops: list[HopState] = []
        v3_hops: list[tuple[int, V3TickRangeHop]] = []  # (index, V3Hop) for validation
        current_token = input_token

        for i, pool in enumerate(pools):
            if isinstance(pool, V3TickRangeHop):
                # V3/V4 tick range hop
                hop_state = pool.to_hop_state()
                hops.append(hop_state)
                v3_hops.append((i, pool))
                # V3 pools don't change the current token tracking here;
                # the direction is already encoded in zero_for_one.
                # We still need to track the token, but V3TickRangeHop
                # doesn't carry token references — the caller is responsible
                # for correct ordering.
                continue

            pool_type = type(pool).__name__
            if pool_type not in v2_pool_types:
                raise OptimizationError(
                    f"Unsupported pool type: {pool_type}. Use V2 pools or V3TickRangeHop objects.",
                    iterations=0,
                    method="mobius",
                )

            if current_token == pool.token0:
                reserve_in = float(pool.state.reserves_token0)
                reserve_out = float(pool.state.reserves_token1)
                next_token = pool.token1
            elif current_token == pool.token1:
                reserve_in = float(pool.state.reserves_token1)
                reserve_out = float(pool.state.reserves_token0)
                next_token = pool.token0
            else:
                raise OptimizationError(
                    f"Token {current_token} not in pool",
                    iterations=0,
                    method="mobius",
                )

            hops.append(
                HopState(
                    reserve_in=reserve_in,
                    reserve_out=reserve_out,
                    fee=float(pool.fee),
                )
            )
            current_token = next_token

        # Run closed-form solver
        max_input_float = float(max_input) if max_input is not None else None
        x_opt, profit, iterations = mobius_solve(hops, max_input=max_input_float)

        optimal_input = int(x_opt)

        if optimal_input <= 0 or profit <= 0:
            raise OptimizationError(
                "No profitable arbitrage",
                iterations=iterations,
                method="mobius",
            )

        # Validate V3 hops: check that the swap stays within assumed tick range
        if v3_hops:
            amount = float(optimal_input)
            for hop_idx, v3_hop in v3_hops:
                # Simulate through hops up to and including this V3 hop
                # to get the amount entering this V3 pool
                amt = float(optimal_input)
                for j in range(hop_idx + 1):
                    if j == hop_idx:
                        # This is the V3 hop — estimate final sqrt price
                        final_sqrt_price = estimate_v3_final_sqrt_price(amt, v3_hop)
                        if not v3_hop.contains_sqrt_price(final_sqrt_price):
                            raise OptimizationError(
                                f"V3 swap at hop {hop_idx} crosses tick "
                                f"boundary (sqrt_price={final_sqrt_price:.6f} "
                                f"outside [{v3_hop.sqrt_price_lower:.6f}, "
                                f"{v3_hop.sqrt_price_upper:.6f}])",
                                iterations=iterations,
                                method="mobius",
                            )
                    else:
                        h = hops[j]
                        if amt <= 0:
                            break
                        denom = h.reserve_in + amt * h.gamma
                        if denom <= 0:
                            amt = 0.0
                            break
                        amt = amt * h.gamma * h.reserve_out / denom

        # Verify with integer simulation through hop states
        amount = optimal_input
        for ps in hops:
            gamma = ps.gamma
            denom = ps.reserve_in + amount * gamma
            amount = int(amount * gamma * ps.reserve_out / denom)

        actual_profit = amount - optimal_input

        elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000

        if actual_profit <= 0:
            raise OptimizationError(
                "No profitable arbitrage (integer verification failed)",
                iterations=iterations,
                method="mobius",
            )

        return OptimizerResult(
            optimal_input=optimal_input,
            profit=actual_profit,
            solve_time_ms=elapsed_ms,
            iterations=iterations,
            optimizer_type=self.optimizer_type,
        )

    def solve_v3_candidates(
        self,
        base_hops: list[HopState],
        v3_hop_index: int,
        v3_candidates: list[V3TickRangeHop],
        max_input: int | None = None,
    ) -> OptimizerResult:
        """
        Solve arbitrage with multiple candidate V3 tick ranges.

        For V3 pools where the optimal swap may cross tick boundaries,
        this method checks each candidate range independently. Each
        candidate yields a closed-form O(1) solution.

        Parameters
        ----------
        base_hops : list[HopState]
            V2 (or other) hops in the path, excluding the V3 hop.
        v3_hop_index : int
            Index in the full path where the V3 hop sits.
        v3_candidates : list[V3TickRangeHop]
            Candidate V3 tick ranges to check, sorted by likelihood
            (closest to equilibrium first).
        max_input : int | None
            Maximum input constraint.

        Returns
        -------
        OptimizerResult
            Best result across all valid candidates.
        """
        start_time = time.perf_counter_ns()

        best_result: OptimizerResult | None = None

        for v3_candidate in v3_candidates:
            v3_hop_state = v3_candidate.to_hop_state()

            # Build full hop list: insert V3 hop at the right position
            full_hops = list(base_hops)
            full_hops.insert(v3_hop_index, v3_hop_state)

            # Solve with this candidate
            max_input_float = float(max_input) if max_input is not None else None
            x_opt, profit, iterations = mobius_solve(full_hops, max_input=max_input_float)

            if x_opt <= 0 or profit <= 0:
                continue

            # Validate V3 range
            # Compute amount entering the V3 hop
            amt = x_opt
            for j in range(v3_hop_index + 1):
                if j == v3_hop_index:
                    final_sqrt_price = estimate_v3_final_sqrt_price(amt, v3_candidate)
                    if not v3_candidate.contains_sqrt_price(final_sqrt_price):
                        break  # Solution invalid for this range
                else:
                    h = full_hops[j]
                    if amt <= 0:
                        break
                    denom = h.reserve_in + amt * h.gamma
                    if denom <= 0:
                        amt = 0.0
                        break
                    amt = amt * h.gamma * h.reserve_out / denom
            else:
                # All hops validated (loop completed without break)
                optimal_input = int(x_opt)

                # Integer verification
                amount = optimal_input
                for ps in full_hops:
                    gamma = ps.gamma
                    denom = ps.reserve_in + amount * gamma
                    amount = int(amount * gamma * ps.reserve_out / denom)

                actual_profit = amount - optimal_input

                if actual_profit > 0 and (
                    best_result is None or actual_profit > best_result.profit
                ):
                    elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
                    best_result = OptimizerResult(
                        optimal_input=optimal_input,
                        profit=actual_profit,
                        solve_time_ms=elapsed_ms,
                        iterations=iterations,
                        optimizer_type=self.optimizer_type,
                    )

        if best_result is not None:
            return best_result

        elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
        raise OptimizationError(
            message="No valid V3 candidate range found",
            iterations=0,
            method="mobius",
        )

    def solve_piecewise(
        self,
        hops: list[HopState],
        v3_hop_index: int,
        v3_crossings: list[TickRangeCrossing],
        max_input: int | None = None,
    ) -> OptimizerResult:
        """
        Solve arbitrage with piecewise-Mobius for V3 tick crossings.

        For each candidate ending range (via TickRangeCrossing), the V3
        swap is decomposed into:
        - Fixed crossing output from ranges 0..K-1
        - Variable Mobius output from the ending range K

        The profit function is NOT a pure Mobius composition (due to the
        additive crossing constant), so we use golden section search on
        a well-bracketed interval starting from the single-range Mobius
        solution.

        Parameters
        ----------
        hops : list[HopState]
            Full path hops with the V3 hop at v3_hop_index.
            The V3 hop entry should be the first range's HopState
            (used as starting point for the search bracket).
        v3_hop_index : int
            Index of the V3 hop in the path.
        v3_crossings : list[TickRangeCrossing]
            Candidate crossing data, ordered by likelihood.
        max_input : int | None
            Maximum input constraint.

        Returns
        -------
        OptimizerResult
            Best result across all valid candidates.
        """
        start_time = time.perf_counter_ns()

        best_result: OptimizerResult | None = None

        for crossing in v3_crossings:
            # Build the full hop list with the ending range's HopState
            full_hops = list(hops)
            full_hops[v3_hop_index] = crossing.ending_range.to_hop_state()

            # Single-range Mobius solve as starting point
            max_input_float = float(max_input) if max_input is not None else None
            x_mobius, _, _ = mobius_solve(full_hops, max_input=max_input_float)

            # Split hops into before/after V3
            hops_before = full_hops[:v3_hop_index]
            hops_after = full_hops[v3_hop_index + 1 :]

            # Compute minimum input to cover crossing
            if crossing.crossing_gross_input > 0 and hops_before:
                coeffs_before = compute_mobius_coefficients(hops_before)
                target = crossing.crossing_gross_input
                if target >= coeffs_before.K / coeffs_before.N:
                    continue  # Crossing requires more than the path can deliver
                x_min = target * coeffs_before.M / (coeffs_before.K - target * coeffs_before.N)
            elif crossing.crossing_gross_input > 0:
                x_min = crossing.crossing_gross_input
            else:
                x_min = 0.0

            if x_min <= 0:
                x_min = 0.0

            # Bind loop variables for closure (avoids B023)
            hb = hops_before
            ha = hops_after
            cr = crossing

            def eval_profit(
                x: float,
                _hops_before: list[HopState] = hb,
                _hops_after: list[HopState] = ha,
                _crossing: TickRangeCrossing = cr,
            ) -> float:
                if x <= 0:
                    return -x
                amt_v3 = simulate_path(x, _hops_before) if _hops_before else x
                v3_out, valid = piecewise_v3_swap(amt_v3, _crossing)
                if not valid:
                    return -x
                final_out = simulate_path(v3_out, _hops_after) if _hops_after else v3_out
                return final_out - x

            # Bracket the search
            x_low = x_min
            if x_mobius > x_min:
                x_high = max(x_mobius * 3, x_min + 1.0)
            else:
                x_high = max(x_min * 5, x_min + 1.0)
            if max_input is not None:
                x_high = min(x_high, float(max_input))

            if x_low >= x_high:
                continue

            # Golden section search for maximum profit
            phi = (math.sqrt(5) - 1) / 2  # ~0.618
            n_iterations = 25

            x1 = x_high - phi * (x_high - x_low)
            x2 = x_low + phi * (x_high - x_low)
            p1 = eval_profit(x1)
            p2 = eval_profit(x2)

            for _ in range(n_iterations):
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

            x_opt = (x_low + x_high) / 2
            profit = eval_profit(x_opt)

            if profit <= 0:
                continue

            optimal_input = int(x_opt)

            # Final validation: compute actual path output
            amt_v3 = (
                simulate_path(float(optimal_input), hops_before)
                if hops_before
                else float(optimal_input)
            )
            v3_out, valid = piecewise_v3_swap(amt_v3, crossing)
            if not valid:
                continue
            final_out = simulate_path(v3_out, hops_after) if hops_after else v3_out
            actual_profit = int(final_out) - optimal_input

            if actual_profit > 0 and (best_result is None or actual_profit > best_result.profit):
                elapsed_ms = (time.perf_counter_ns() - start_time) / 1_000_000
                best_result = OptimizerResult(
                    optimal_input=optimal_input,
                    profit=actual_profit,
                    solve_time_ms=elapsed_ms,
                    iterations=n_iterations,
                    optimizer_type=self.optimizer_type,
                )

        if best_result is not None:
            return best_result

        raise OptimizationError(
            "No valid piecewise-Mobius solution found",
            iterations=0,
            method="mobius",
        )


# Backward-compatible alias
MobiusV2Optimizer = MobiusOptimizer
