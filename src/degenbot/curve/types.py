"""Curve-specific type definitions (swap style, metapool enums)."""

from __future__ import annotations

import dataclasses
from enum import Enum, auto
from typing import TYPE_CHECKING, Protocol, runtime_checkable

from degenbot.types.abstract import AbstractPoolState
from degenbot.types.concrete import PoolStateMessage

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress, HexAddress

    from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
    from degenbot.types.aliases import BlockNumber


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


class MetapoolRateStyle(Enum):
    """Which rates to use for the metapool branch in get_dy."""

    STANDARD = auto()  # (rate_multipliers[0], virtual_price)
    PRECISION_VP = auto()  # (PRECISION, virtual_price)
    REDEMPTION_VP = auto()  # (redemption_price, virtual_price)


class MetapoolUnderlyingStyle(Enum):
    """Which computation path to use in _get_dy_underlying."""

    STANDARD = auto()  # rate_multipliers with VP for base pool LP token
    REDEMPTION = auto()  # redemption_price for first coin, VP for second
    PRECISION_VP = auto()  # (PRECISION, virtual_price) — no rate multiplier for first coin


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


@runtime_checkable
class CurveDataProvider(Protocol):
    """On-chain data access for a Curve StableSwap pool.

    Consolidates the 13 individual fetcher callbacks into a single interface.
    The pool checks provider availability before calling; a provider that
    doesn't support a method should raise MissingCurveData.

    All methods accepting `block_number` may use block-specific data.
    """

    # Pool-state fetchers
    def virtual_price(self, block_number: int) -> int:
        """Return virtual price."""
        ...

    def base_virtual_price(self, block_number: int) -> int:
        """Return base virtual price."""
        ...

    def base_cache_updated(self, block_number: int) -> int:
        """Return base cache updated."""
        ...

    def admin_balances(self, block_number: int) -> tuple[int, ...]:
        """Return admin balances."""
        ...

    def d(self, block_number: int) -> int:  # crypto only
        """Return the D invariant value."""
        ...

    def gamma(self, block_number: int) -> int:  # crypto only
        """Return the gamma parameter."""
        ...

    def price_scale(self, block_number: int) -> tuple[int, ...]:  # crypto only
        """Return the price scale values."""
        ...

    # Chain-state fetchers
    def block_timestamp(self, block_number: int) -> int:
        """Return the block timestamp."""
        ...

    def block_number(self) -> int:
        """Return block number."""
        ...

    # Helper fetchers
    def token_balance(self, token_address: str, holder_address: str, block_number: int) -> int:
        """Return token balance."""
        ...

    def token_total_supply(self, token_address: str, block_number: int) -> int:
        """Return token total supply."""
        ...

    def lending_rates(self, block_number: int) -> tuple[int, ...]:
        """Return lending rates."""
        ...

    def redemption_price(self, block_number: int) -> int:
        """Return redemption price."""
        ...


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class CurveStableswapPoolState(AbstractPoolState):
    """CurveStableswapPoolState class."""

    balances: tuple[int, ...]
    base: CurveStableswapPoolState | None = None


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableswapPoolExternalUpdate:
    """CurveStableswapPoolExternalUpdate class."""

    block_number: BlockNumber
    balances: tuple[int, ...]


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableswapPoolSimulationResult:
    """CurveStableswapPoolSimulationResult class."""

    amount0_delta: int
    amount1_delta: int
    current_state: CurveStableswapPoolState
    future_state: CurveStableswapPoolState


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolAttributes:
    """CurveStableSwapPoolAttributes class."""

    address: HexAddress
    lp_token_address: HexAddress
    coin_addresses: list[HexAddress]
    coin_index_type: str
    is_metapool: bool
    underlying_coin_addresses: list[HexAddress] | None = dataclasses.field(default=None)
    base_pool_address: HexAddress | None = dataclasses.field(default=None)


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolStateUpdated(PoolStateMessage):
    """CurveStableSwapPoolStateUpdated class."""

    state: CurveStableswapPoolState
