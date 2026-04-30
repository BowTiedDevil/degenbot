"""
Hop state types for arbitrage solvers.

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
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class V3TickRangeInfo:
    """
    Information about a V3/V4 tick range for multi-range support.
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
    def is_v2(self) -> bool:
        return False

    @property
    def is_v3(self) -> bool:
        return True

    @property
    def gamma(self) -> float:
        return 1.0 - float(self.fee)

    @property
    def has_multi_range(self) -> bool:
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
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class BalancerWeightedHop:
    """
    A Balancer weighted pool (∏xᵂⁱ ≥ k) hop.

    Not a Möbius transformation — the swap function uses power-law exponents.
    A 50/50 pool reduces to constant product.
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
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class CurveStableswapHop:
    """
    A Curve stableswap pool hop.

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
    def is_v2(self) -> bool:
        return False

    @property
    def is_v3(self) -> bool:
        return False

    @property
    def gamma(self) -> float:
        return 1.0 - float(self.fee)


@dataclass(frozen=True, slots=True)
class BalancerMultiTokenHop:
    """
    An N-token Balancer weighted pool for multi-token basket arbitrage.

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
        return len(self.reserves)

    @property
    def is_v2(self) -> bool:
        return False

    @property
    def is_v3(self) -> bool:
        return False

    @property
    def gamma(self) -> float:
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
    zero_for_one: bool | None = None,
) -> HopType:
    """
    Backward-compatible Hop constructor.

    Returns the correct hop variant based on the arguments:
    - With liquidity/sqrt_price/tick fields -> BoundedProductHop
    - Without V3 fields -> ConstantProductHop
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
            zero_for_one=zero_for_one,
        )
    return ConstantProductHop(
        reserve_in=reserve_in,
        reserve_out=reserve_out,
        fee=fee,
    )


Hop = hop_factory
