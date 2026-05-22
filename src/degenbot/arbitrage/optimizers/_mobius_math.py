"""
Pure-math Möbius transformation functions for constant product AMM paths.

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

This module is an internal implementation detail of MobiusSolver and
PiecewiseMobiusSolver. The public seam is the Solver protocol.

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
from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class MobiusCoefficients:
    """
    The three scalar coefficients that fully describe an n-hop constant.

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
class MobiusFloatHop:
    """
    Reserve and fee state for a single pool hop in float space.

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
    V3/V4 tick range data needed to build a Möbius MobiusFloatHop.

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

    def to_hop_state(self) -> MobiusFloatHop:
        """
        Convert this V3 tick range to a MobiusFloatHop with effective reserves.

        The swap function for a bounded product CFMM is:
            y = gamma*(R1+beta)*x / ((R0+alpha) + gamma*x)

        where R0 = L/sqrt_p - alpha, R1 = L*sqrt_p - beta are the REAL reserves.
        So R0+alpha = L/sqrt_p and R1+beta = L*sqrt_p are the VIRTUAL reserves.

        These virtual reserves are the effective reserves for the Möbius
        formula: r_eff = L/sqrt_p, s_eff = L*sqrt_p.

        Returns
        -------
        MobiusFloatHop
            Hop with effective reserves for Möbius composition.

        """
        if self.zero_for_one:
            r_eff = self.liquidity / self.sqrt_price_current
            s_eff = self.liquidity * self.sqrt_price_current
        else:
            r_eff = self.liquidity * self.sqrt_price_current
            s_eff = self.liquidity / self.sqrt_price_current

        return MobiusFloatHop(
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
    Pre-computed crossing data for a V3 swap that crosses tick boundaries.

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


def compute_mobius_coefficients(hops: list[MobiusFloatHop]) -> MobiusCoefficients:
    """
    Compute the Möbius transformation coefficients K, M, N for an n-hop.

    constant product path via a single forward pass.

    The recurrence is derived from 2x2 matrix multiplication where each
    swap is encoded as:

        M_i = [[f_i * s_i, 0], [f_i, r_i]]

    and the product M_1 * M_2 * ... * M_n yields the composite
    transformation l(x) = K * x / (M + N * x).

    Parameters
    ----------
    hops : list[MobiusFloatHop]
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
    k = first.gamma * first.reserve_out
    m = first.reserve_in
    n = first.gamma

    # Update for each subsequent hop
    # Note: N update uses K before it is updated in this step
    for hop in hops[1:]:
        old_k = k
        k = old_k * hop.gamma * hop.reserve_out
        m *= hop.reserve_in
        n = n * hop.reserve_in + old_k * hop.gamma

    is_profitable = k > m

    return MobiusCoefficients(K=k, M=m, N=n, is_profitable=is_profitable)


def simulate_path(x: float, hops: list[MobiusFloatHop]) -> float:
    """
    Simulate a swap through all hops for verification.

    Parameters
    ----------
    x : float
        Input amount to the first pool.
    hops : list[MobiusFloatHop]
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
    hops: list[MobiusFloatHop],
    max_input: float | None = None,
) -> tuple[float, float, int]:
    """
    Solve for optimal arbitrage input using the Möbius transformation approach.

    Parameters
    ----------
    hops : list[MobiusFloatHop]
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
