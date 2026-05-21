from __future__ import annotations

import dataclasses
from enum import Enum, auto
from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress, HexAddress

    from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
    from degenbot.types.aliases import BlockNumber

from degenbot.types.abstract import AbstractPoolState
from degenbot.types.concrete import PoolStateMessage

# ── Variant Enums ──
# These enums identify calculation variants for Curve StableSwap pools.
# They are resolved at construction time from the pool address and
# injected into the pool, replacing address-based dispatch.


class DVariant(Enum):
    """Which D-calculation formula to use in _get_d.

    The original code had 5 address groups selecting different d_func and dp_func
    pairs. Groups 1 and 3 both use variant_alpha dp but differ on d_func:
    - Group 1: variant_alpha d + variant_alpha dp
    - Group 3: standard d + variant_alpha dp
    """

    STANDARD = auto()  # calc_d + calc_dp
    VARIANT_ALPHA = auto()  # calc_d_variant_alpha + calc_dp
    VARIANT_ALPHA_DP_ALPHA = auto()  # calc_d_variant_alpha + calc_dp_variant_alpha
    VARIANT_DP_ALPHA = auto()  # calc_d + calc_dp_variant_alpha
    VARIANT_BETA_DP = auto()  # calc_d + calc_dp_variant_beta
    VARIANT_GAMMA_DP = auto()  # calc_d + calc_dp_variant_gamma


class YVariant(Enum):
    """Which Y-calculation formula to use in _get_y.

    The original code had two overlapping address sets controlling independent
    behaviours: Y_VARIANT_GROUP_0 (amp divisor) and Y_VARIANT_GROUP_1 (c/b formula).
    Since Y_VARIANT_GROUP_0 ⊂ Y_VARIANT_GROUP_1, there are exactly 3 observed
    combinations, yielding these variants:
    """

    STANDARD = auto()  # amp WITH A_PRECISION divisor + standard c/b
    VARIANT_0 = auto()  # amp WITHOUT A_PRECISION divisor + standard c/b
    VARIANT_1 = auto()  # amp WITHOUT A_PRECISION divisor + c/b without A_PRECISION


class YDVariant(Enum):
    """Which Y_D-calculation formula to use in _get_y_d."""

    STANDARD = auto()
    VARIANT_0 = auto()  # A_PRECISION in b/c formulas


class SwapStyle(Enum):
    """Which computation path to use in get_dy.

    Each value identifies a complete swap calculation path differing in rate source,
    balance source, fee application, and rate conversion. These are not independent
    axes — each path is a coherent unit.

    The variants capture differences in:
    - How dy is computed (with or without the - 1 subtraction)
    - When fee is applied (before or after rate conversion)
    - How rate conversion is applied
    - What balances are used (pool state, live minus admin, raw)
    """

    STANDARD = auto()  # dy = xp[j] - y - 1, fee, then rate convert
    RATE_ADJUSTED = auto()  # dy = (xp[j] - y - 1) * PRECISION // rates[j], fee on converted dy
    RAW_BALANCE = auto()  # raw balances, no rate conversion, direct fee
    CRYPTO = auto()  # Newton's method, dynamic fee
    LIVE_ADMIN = auto()  # live balances minus admin, dy = xp[j] - y - 1, fee, rate convert
    LIVE_ADMIN_DYNAMIC = auto()  # live balances minus admin, dynamic offpeg fee
    LIVE_ADMIN_DYNAMIC_PRECISION = auto()  # live-admin + precision multipliers + dynamic offpeg fee
    LIVE_ADMIN_ORACLE = auto()  # live-admin + oracle rates, dy = xp[j] - y - 1
    NO_ONE_FEE_RATE = auto()  # dy = xp[j] - y (no -1), fee, rate convert
    CYTOKEN = auto()  # dy = xp[j] - y - 1, then (dy-fee)*PRECISION//rates[j]
    RATE_ADJUSTED_NO_ONE = auto()  # dy = (xp[j]-y)*PRECISION//rates[j], fee on converted dy

    def make_calculator(self) -> DyCalculator:
        """Construct the appropriate DyCalculator for this swap style."""
        # Lazy imports to avoid circular dependency: calculator modules
        # import from this module at runtime.
        from degenbot.curve.calculators.crypto import CryptoDyCalculator  # noqa: PLC0415
        from degenbot.curve.calculators.live_admin import (  # noqa: PLC0415
            LiveAdminDyCalculator,
            LiveAdminDynamicDyCalculator,
            LiveAdminDynamicPrecisionDyCalculator,
            LiveAdminOracleDyCalculator,
        )
        from degenbot.curve.calculators.standard import (  # noqa: PLC0415
            CytokenDyCalculator,
            NoOneFeeRateDyCalculator,
            RateAdjustedDyCalculator,
            RateAdjustedNoOneDyCalculator,
            RawBalanceDyCalculator,
            StandardDyCalculator,
        )

        match self:
            case SwapStyle.STANDARD:
                return StandardDyCalculator()
            case SwapStyle.RATE_ADJUSTED:
                return RateAdjustedDyCalculator()
            case SwapStyle.RATE_ADJUSTED_NO_ONE:
                return RateAdjustedNoOneDyCalculator()
            case SwapStyle.RAW_BALANCE:
                return RawBalanceDyCalculator()
            case SwapStyle.CRYPTO:
                return CryptoDyCalculator()
            case SwapStyle.LIVE_ADMIN:
                return LiveAdminDyCalculator()
            case SwapStyle.LIVE_ADMIN_DYNAMIC:
                return LiveAdminDynamicDyCalculator()
            case SwapStyle.LIVE_ADMIN_DYNAMIC_PRECISION:
                return LiveAdminDynamicPrecisionDyCalculator()
            case SwapStyle.LIVE_ADMIN_ORACLE:
                return LiveAdminOracleDyCalculator()
            case SwapStyle.NO_ONE_FEE_RATE:
                return NoOneFeeRateDyCalculator()
            case SwapStyle.CYTOKEN:
                return CytokenDyCalculator()


class MetapoolRateStyle(Enum):
    """Which rates to use for the metapool branch in get_dy."""

    STANDARD = auto()  # (rate_multipliers[0], virtual_price)
    PRECISION_VP = auto()  # (PRECISION, virtual_price)
    REDEMPTION_VP = auto()  # (redemption_price, virtual_price)

    def make_calculator(self) -> DyCalculator:
        """Construct the appropriate metapool DyCalculator for this rate style."""
        from degenbot.curve.calculators.metapool import (  # noqa: PLC0415
            MetapoolPrecisionVpDyCalculator,
            MetapoolRedemptionVpDyCalculator,
            MetapoolStandardDyCalculator,
        )

        match self:
            case MetapoolRateStyle.PRECISION_VP:
                return MetapoolPrecisionVpDyCalculator()
            case MetapoolRateStyle.REDEMPTION_VP:
                return MetapoolRedemptionVpDyCalculator()
            case MetapoolRateStyle.STANDARD:
                return MetapoolStandardDyCalculator()


class MetapoolUnderlyingStyle(Enum):
    """Which computation path to use in _get_dy_underlying."""

    STANDARD = auto()  # rate_multipliers with VP for base pool LP token
    REDEMPTION = auto()  # redemption_price for first coin, VP for second
    PRECISION_VP = auto()  # (PRECISION, virtual_price) — no rate multiplier for first coin

    def make_calculator(self) -> DyCalculator:
        """Construct the appropriate metapool underlying DyCalculator."""
        from degenbot.curve.calculators.metapool import (  # noqa: PLC0415
            MetapoolUnderlyingPrecisionVpDyCalculator,
            MetapoolUnderlyingRedemptionDyCalculator,
            MetapoolUnderlyingStandardDyCalculator,
        )

        match self:
            case MetapoolUnderlyingStyle.PRECISION_VP:
                return MetapoolUnderlyingPrecisionVpDyCalculator()
            case MetapoolUnderlyingStyle.REDEMPTION:
                return MetapoolUnderlyingRedemptionDyCalculator()
            case MetapoolUnderlyingStyle.STANDARD:
                return MetapoolUnderlyingStandardDyCalculator()


class LendingRateStyle(Enum):
    """Which rate-fetching method to use for lending tokens.

    Used by get_dy() to select which stored-rate resolution path to call
    via CurveDataProvider.lending_rates().
    """

    NONE = auto()  # No lending tokens — use rate_multipliers directly
    CTOKEN = auto()  # Exchange rate with supply rate accrual
    YTOKEN = auto()  # Price per full share
    CYTOKEN = auto()  # cToken + yToken combined accrual
    AETH = auto()  # Lido aETH ratio inversion
    RETH = auto()  # Rocket Pool exchange rate
    ORACLE = auto()  # On-chain oracle bitmask


@dataclasses.dataclass(slots=True, frozen=True)
class DyCalculationInputs:
    """Pre-resolved data for a single dy calculation.

    Constructed by CurveStableswapPool.get_dy() before delegating to the
    injected DyCalculator. The calculator reads only from this object —
    never from the pool directly. All I/O, cache lookups, and rate
    resolution happen before this object is created.
    """

    # ── Pool constants ──
    PRECISION: int
    FEE_DENOMINATOR: int
    fee: int
    n_coins: int

    # ── Pool state ──
    balances: tuple[int, ...]
    rate_multipliers: tuple[int, ...]
    precision_multipliers: tuple[int, ...]
    offpeg_fee_multiplier: int
    fee_gamma: int
    mid_fee: int
    out_fee: int
    address: ChecksumAddress

    # ── Pre-resolved rates (after lending-rate I/O) ──
    resolved_rates: tuple[int, ...]

    # ── Pre-computed XP (rate-adjusted balances) ──
    xp: tuple[int, ...]

    # ── Pre-resolved block data ──
    block_number: int
    block_timestamp: int
    amp: int

    # ── I/O results for crypto pools ──
    d: int | None = None
    gamma: int | None = None
    price_scale: tuple[int, ...] | None = None

    # ── I/O results for live-admin pools ──
    live_balances: tuple[int, ...] | None = None
    admin_balances: tuple[int, ...] | None = None
    effective_balances: tuple[int, ...] | None = None

    # ── I/O results for metapool pools ──
    virtual_price: int | None = None
    scaled_redemption_price: int | None = None
    base_pool: CurveStableswapPool | None = None

    # ── Pre-resolved variant enums for pure invariant solving ──
    d_variant: DVariant = DVariant.STANDARD
    y_variant: YVariant = YVariant.STANDARD
    yd_variant: YDVariant = YDVariant.STANDARD
    a_precision: int = 100


class DyCalculator(Protocol):
    """Calculates dy (output amount) for a Curve StableSwap swap.

    Each SwapStyle variant maps to a frozen dataclass implementing this
    protocol. The pool's get_dy() delegates to the injected calculator
    via PoolStrategies.dy_calculator.

    Calculators receive a DyCalculationInputs object carrying all
    pre-resolved data (balances, rates, xp, variant enums for invariant
    solving). All I/O and cache lookups happen before the calculator
    is called — the calculator is pure math.
    """

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        inputs: DyCalculationInputs,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int: ...


@dataclasses.dataclass(slots=True, frozen=True)
class PoolStrategies:
    """Resolved calculation strategies for a Curve pool instance.

    Set at construction time by the builder from the pool address.
    The pool class is address-agnostic — it only reads these strategy values.

    Calculator instances are auto-constructed from the enum values in
    __post_init__ if not explicitly provided. Callers may pass explicit
    calculator instances to override the default factory (e.g., in tests).
    """

    d_variant: DVariant = DVariant.STANDARD
    y_variant: YVariant = YVariant.STANDARD
    yd_variant: YDVariant = YDVariant.STANDARD
    swap_style: SwapStyle = SwapStyle.STANDARD
    metapool_rate_style: MetapoolRateStyle = MetapoolRateStyle.STANDARD
    metapool_underlying_style: MetapoolUnderlyingStyle = MetapoolUnderlyingStyle.STANDARD
    lending_rate_style: LendingRateStyle = LendingRateStyle.NONE

    # Calculator instances — auto-constructed from enum values if not provided.
    # Enum values remain for introspection (e.g., logging).
    dy_calculator: DyCalculator | None = dataclasses.field(default=None)
    metapool_dy_calculator: DyCalculator | None = dataclasses.field(default=None)
    metapool_underlying_dy_calculator: DyCalculator | None = dataclasses.field(default=None)

    def __post_init__(self) -> None:
        # Auto-construct calculators from enum values when not explicitly set.
        # Uses object.__setattr__ because the dataclass is frozen.
        if self.dy_calculator is None:
            object.__setattr__(self, "dy_calculator", self.swap_style.make_calculator())
        if self.metapool_dy_calculator is None:
            object.__setattr__(
                self, "metapool_dy_calculator", self.metapool_rate_style.make_calculator()
            )
        if self.metapool_underlying_dy_calculator is None:
            object.__setattr__(
                self,
                "metapool_underlying_dy_calculator",
                self.metapool_underlying_style.make_calculator(),
            )


# ── Data Provider Protocol ──


@runtime_checkable
class CurveDataProvider(Protocol):
    """On-chain data access for a Curve StableSwap pool.

    Consolidates the 13 individual fetcher callbacks into a single interface.
    The pool checks provider availability before calling; a provider that
    doesn't support a method should raise MissingCurveData.

    All methods accepting `block_number` may use block-specific data.
    """

    # Pool-state fetchers
    def virtual_price(self, block_number: int) -> int: ...
    def base_virtual_price(self, block_number: int) -> int: ...
    def base_cache_updated(self, block_number: int) -> int: ...
    def admin_balances(self, block_number: int) -> tuple[int, ...]: ...
    def D(self, block_number: int) -> int: ...  # crypto only
    def gamma(self, block_number: int) -> int: ...  # crypto only
    def price_scale(self, block_number: int) -> tuple[int, ...]: ...  # crypto only

    # Chain-state fetchers
    def block_timestamp(self, block_number: int) -> int: ...
    def block_number(self) -> int: ...

    # Helper fetchers
    def token_balance(self, token_address: str, holder_address: str, block_number: int) -> int: ...
    def token_total_supply(self, token_address: str, block_number: int) -> int: ...
    def lending_rates(self, block_number: int) -> tuple[int, ...]: ...
    def redemption_price(self, block_number: int) -> int: ...


# ── Provider & State Types ──


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class CurveStableswapPoolState(AbstractPoolState):
    balances: tuple[int, ...]
    base: CurveStableswapPoolState | None = None


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableswapPoolExternalUpdate:
    block_number: BlockNumber
    balances: tuple[int, ...]


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableswapPoolSimulationResult:
    amount0_delta: int
    amount1_delta: int
    current_state: CurveStableswapPoolState
    future_state: CurveStableswapPoolState


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolAttributes:
    address: HexAddress
    lp_token_address: HexAddress
    coin_addresses: list[HexAddress]
    coin_index_type: str
    is_metapool: bool
    underlying_coin_addresses: list[HexAddress] | None = dataclasses.field(default=None)
    base_pool_address: HexAddress | None = dataclasses.field(default=None)


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolStateUpdated(PoolStateMessage):
    state: CurveStableswapPoolState
