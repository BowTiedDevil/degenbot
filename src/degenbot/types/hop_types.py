"""Hop state types for arbitrage solvers.

These data types represent a pool's numerical state in a form suitable for
solver consumption. They are shared across the arbitrage and pool modules,
so they live in the types package to avoid circular imports.

Moved from degenbot.arbitrage.optimizers.hop_types to break the
dependency from pools -> arbitrage.
"""

from collections.abc import Callable
from dataclasses import dataclass, field
from enum import Enum, auto
from fractions import Fraction


class PoolInvariant(Enum):
    """PoolInvariant class."""

    CONSTANT_PRODUCT = auto()
    BOUNDED_PRODUCT = auto()
    SOLIDLY_STABLE = auto()
    BALANCER_WEIGHTED = auto()
    BALANCER_MULTI_TOKEN = auto()
    CURVE_STABLESWAP = auto()
    BALANCER_STABLESWAP = auto()


@dataclass(frozen=True, slots=True)
class ConstantProductHop:
    """A constant-product (x*y=k) pool hop.

    For V2 pools: LiquidityPool, AerodromeV2Pool (volatile), CamelotLiquidityPool.
    Supports asymmetric fees via fee_out (Camelot has different fees per direction).
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction
    fee_out: Fraction | None = None
    invariant: PoolInvariant = PoolInvariant.CONSTANT_PRODUCT

    @property
    def gamma(self) -> float:
        """Gamma."""
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class V3TickRangeInfo:
    """Information about a V3/V4 tick range for multi-range support."""

    tick_lower: int
    tick_upper: int
    liquidity: int
    sqrt_price_lower: int
    sqrt_price_upper: int


@dataclass(frozen=True, slots=True)
class BoundedProductHop:
    """A bounded-product (concentrated liquidity) pool hop for V3/V4.

    V3/V4 tick ranges are bounded product CFMMs with effective reserves
    (R0+alpha, R1+beta) that follow the same Möbius form.

    For multi-range support (tick crossings), tick_ranges contains adjacent
    ranges and current_range_index indicates which range contains the current
    price. When tick_ranges is None, the hop represents a single range.
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
    zero_for_one: bool | None = None
    invariant: PoolInvariant = PoolInvariant.BOUNDED_PRODUCT

    @property
    def gamma(self) -> float:
        """Gamma."""
        return 1.0 - float(self.fee)

    @property
    def has_multi_range(self) -> bool:
        """Check if multi range."""
        return self.tick_ranges is not None and len(self.tick_ranges) > 1


@dataclass(frozen=True, slots=True)
class SolidlyStableHop:
    """A Solidly stable (x³y + xy³ ≥ k) pool hop.

    Used by AerodromeV2Pool (stable=True) and CamelotLiquidityPool (stable_swap=True).
    Not a Möbius transformation — the swap function comes from solving a cubic.

    The optional ``swap_fn`` provides an integer-accurate swap simulation
    (e.g. wrapping ``calc_exact_in_stable``). When provided, the solver
    uses it for exact path evaluation. When absent, a float approximation
    is used (less accurate for extreme decimal differences).
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction
    decimals_in: int
    decimals_out: int
    swap_fn: Callable[[int], int] | None = field(default=None, compare=False, hash=False)
    invariant: PoolInvariant = PoolInvariant.SOLIDLY_STABLE

    @property
    def gamma(self) -> float:
        """Gamma."""
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class BalancerWeightedHop:
    """A Balancer weighted pool (∏xᵂⁱ ≥ k) hop.

    Not a Möbius transformation — the swap function uses power-law exponents.
    A 50/50 pool reduces to constant product.

    The optional ``swap_fn`` provides an integer-accurate swap simulation
    wrapping the pool's calculate_tokens_out_from_tokens_in. When provided,
    the solver uses it for exact path evaluation. When absent, the float
    approximation using weight_in/weight_out is used.
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction
    weight_in: int
    weight_out: int
    swap_fn: Callable[[int], int] | None = field(default=None, compare=False, hash=False)
    invariant: PoolInvariant = PoolInvariant.BALANCER_WEIGHTED

    @property
    def gamma(self) -> float:
        """Gamma."""
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class CurveStableswapHop:
    """A Curve stableswap pool hop.

    Uses the invariant: A*n^n*Σx + D = A*n^n*D + (D^(n+1) / n^n / ∏x)
    The swap function is inherently iterative (Newton's method for get_y).

    The optional ``swap_fn`` provides an integer-accurate swap simulation
    wrapping ``get_dy``. When provided, the solver uses it for exact path
    evaluation. When absent, a float approximation is used (less accurate
    for stable pairs with extreme decimal differences).
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
    swap_fn: "Callable[[int], int] | None" = field(default=None, compare=False, hash=False)
    invariant: PoolInvariant = PoolInvariant.CURVE_STABLESWAP

    @property
    def gamma(self) -> float:
        """Gamma."""
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class BalancerStableHop:
    """A Balancer stable pool hop (StableSwap invariant).

    Not a Möbius transformation — the swap function requires iterative
    invariant computation (Newton's method). Follows the same pattern as
    CurveStableswapHop and SolidlyStableHop.

    The ``invariant`` field carries a pre-computed D value for float
    approximation. For integer-accurate evaluation, ``swap_fn`` calls
    the pool's calculate_tokens_out_from_tokens_in directly.

    ``swap_fn`` and ``StaleRateResult``: when the underlying pool has a
    static rate provider, calculate_tokens_out_from_tokens_in raises
    StaleRateResult. The swap_fn wraps the call and extracts the
    approximate amount from the exception, so the solver can continue
    with best-effort values rather than crashing.

    ``token_index_in`` and ``token_index_out`` are indices in the
    non-BPT token list (BPT-skipped), not the full token list.
    """

    reserve_in: int
    reserve_out: int
    fee: Fraction
    amp: int
    n_tokens: int
    invariant: int  # Pre-computed D invariant for float approximation
    token_index_in: int  # Index in the non-BPT token list (BPT-skipped)
    token_index_out: int  # Index in the non-BPT token list (BPT-skipped)
    swap_fn: Callable[[int], int] | None = field(default=None, compare=False, hash=False)
    pool_invariant: PoolInvariant = PoolInvariant.BALANCER_STABLESWAP

    @property
    def gamma(self) -> float:
        """Gamma."""
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class BalancerMultiTokenHop:
    """An N-token Balancer weighted pool for multi-token basket arbitrage.

    Unlike pairwise hops, this represents the entire pool state and
    enables closed-form basket trade optimization.
    """

    reserves: tuple[int, ...]
    weights: tuple[int, ...]
    fee: Fraction
    decimals: tuple[int, ...] = ()
    market_prices: tuple[float, ...] | None = None
    invariant: PoolInvariant = PoolInvariant.BALANCER_MULTI_TOKEN

    @property
    def n_tokens(self) -> int:
        """Return n tokens."""
        return len(self.reserves)

    @property
    def gamma(self) -> float:
        """Gamma."""
        return 1.0 - float(self.fee)


HopType = (
    ConstantProductHop
    | BoundedProductHop
    | SolidlyStableHop
    | BalancerWeightedHop
    | BalancerStableHop
    | CurveStableswapHop
    | BalancerMultiTokenHop
)
