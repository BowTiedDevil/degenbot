"""
Core types for arbitrage optimization solvers.

This module contains the data types shared between:
- degenbot.arbitrage.optimizers.solver (ArbSolver and concrete solvers)
- degenbot.arbitrage.solver.mobius_solver (generalized MobiusSolver)

Placing these types in a separate module breaks the circular import between
the solver modules.
"""

from abc import ABC, abstractmethod
from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum, auto
from fractions import Fraction


class SolverMethod(Enum):
    """Solver algorithm used to produce a result."""

    MOBIUS = auto()
    NEWTON = auto()
    PIECEWISE_MOBIUS = auto()
    SOLIDLY_STABLE = auto()
    BALANCER_MULTI_TOKEN = auto()
    BRENT = auto()


class PoolInvariant(Enum):
    """Pool invariant type for a hop."""

    CONSTANT_PRODUCT = auto()
    BOUNDED_PRODUCT = auto()
    SOLIDLY_STABLE = auto()
    BALANCER_WEIGHTED = auto()
    BALANCER_MULTI_TOKEN = auto()
    CURVE_STABLESWAP = auto()


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

    Raises OptimizationError on failure.

    Attributes
    ----------
    optimal_input : int
        Optimal input amount in wei.
    profit : int
        Expected profit in wei (output - input).
    iterations : int
        Number of iterations taken (0 for closed-form).
    method : SolverMethod
        Which solver algorithm was used.
    solve_time_ns : int
        Solve time in nanoseconds.
    """

    optimal_input: int
    profit: int
    iterations: int
    method: SolverMethod
    solve_time_ns: int = 0


class Solver(ABC):
    """
    Abstract base class for arbitrage solvers.

    Every solver accepts a `SolveInput` and returns a `SolveResult`.
    The `supports()` method indicates whether a solver can handle a
    given input (used by `ArbSolver` for dispatch).
    """

    MIN_HOPS = 2

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
