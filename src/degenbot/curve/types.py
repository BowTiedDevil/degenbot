import dataclasses
from enum import Enum, auto
from typing import Protocol, runtime_checkable

from eth_typing import HexAddress

from degenbot.types.abstract import AbstractPoolState
from degenbot.types.aliases import BlockNumber
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
    LIVE_ADMIN_DYNAMIC_PRECISION = auto()  # live balances minus admin, precision multipliers for xp, dynamic offpeg fee
    LIVE_ADMIN_ORACLE = auto()  # live balances minus admin, oracle rates, dy = xp[j] - y - 1, fee, rate convert
    NO_ONE_FEE_RATE = auto()  # dy = xp[j] - y (no -1), fee, then rate convert — used by AETH/RETH pools
    CYTOKEN = auto()  # dy = xp[j] - y - 1, then (dy - fee) * PRECISION // rates[j] — fee inside rate conversion
    RATE_ADJUSTED_NO_ONE = auto()  # dy = (xp[j] - y) * PRECISION // rates[j], fee on converted dy — used by some ytoken pools


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

    Used by get_dy() to select which _stored_rates_from_*() method to call.
    Will be replaced by typed fetcher protocols in Plan 027.
    """

    NONE = auto()  # No lending tokens — use rate_multipliers directly
    CTOKEN = auto()  # Exchange rate with supply rate accrual
    YTOKEN = auto()  # Price per full share
    CYTOKEN = auto()  # cToken + yToken combined accrual
    AETH = auto()  # Lido aETH ratio inversion
    RETH = auto()  # Rocket Pool exchange rate
    ORACLE = auto()  # On-chain oracle bitmask


@dataclasses.dataclass(slots=True, frozen=True)
class DyCalculator(Protocol):
    """Calculates dy (output amount) for a Curve StableSwap swap.

    Each SwapStyle variant maps to a frozen dataclass implementing this
    protocol. The pool's get_dy() delegates to the injected calculator
    via PoolStrategies.dy_calculator.

    Calculators resolve data from the pool (amp, balances, rates) in
    the first few lines, then call pure invariant-solver functions for
    the math. The pool parameter is read-only.
    """

    def calculate(
        self,
        i: int,
        j: int,
        dx: int,
        *,
        pool: "CurveStableswapPool",
        block_number: int,
        override_state: "CurveStableswapPoolState | None" = None,
    ) -> int: ...


@dataclasses.dataclass(slots=True, frozen=True)
class PoolStrategies:
    """Resolved calculation strategies for a Curve pool instance.

    Set at construction time by the builder from the pool address.
    The pool class is address-agnostic — it only reads these strategy values.
    """
    d_variant: DVariant = DVariant.STANDARD
    y_variant: YVariant = YVariant.STANDARD
    yd_variant: YDVariant = YDVariant.STANDARD
    swap_style: SwapStyle = SwapStyle.STANDARD
    metapool_rate_style: MetapoolRateStyle = MetapoolRateStyle.STANDARD
    metapool_underlying_style: MetapoolUnderlyingStyle = MetapoolUnderlyingStyle.STANDARD
    lending_rate_style: LendingRateStyle = LendingRateStyle.NONE

    # Calculator instances — carry the actual swap formula implementation.
    # Enum values remain for introspection (e.g., logging).
    dy_calculator: DyCalculator = dataclasses.field(default=None)  # type: ignore[assignment]
    metapool_dy_calculator: DyCalculator | None = None
    metapool_underlying_dy_calculator: DyCalculator | None = None

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


# ── Fetcher Protocols (deprecated — use CurveDataProvider) ──# These protocols define the interface for callbacks that fetch on-chain data
# for Curve pools. They are injected by Bot.build_pool() to enable
# on-demand data fetching while keeping the pool class I/O-free.


class VirtualPriceFetcher(Protocol):
    """Fetch virtual price from a base pool at a given block.

    Used by metapools to get the LP token price of their base pool.
    """

    def __call__(self, block_number: int) -> int: ...


class TimestampFetcher(Protocol):
    """Fetch block timestamp for a given block number.

    Used for A coefficient ramping calculations.
    """

    def __call__(self, block_number: int) -> int: ...


class RedemptionPriceFetcher(Protocol):
    """Fetch scaled redemption price for LSD pools at a given block.

    Used by pools that wrap LSD tokens (e.g., stETH, frxETH).
    """

    def __call__(self, block_number: int) -> int: ...


class AdminBalancesFetcher(Protocol):
    """Fetch admin balances for the pool at a given block.

    Used by pools that track accumulated admin fees separately.
    """

    def __call__(self, block_number: int) -> tuple[int, ...]: ...


class DFetcher(Protocol):
    """Fetch the invariant D for a crypto pool at a given block.

    Used by crypto (volatile) Curve pools that need the on-chain D value
    instead of computing it locally.
    """

    def __call__(self, block_number: int) -> int: ...


class GammaFetcher(Protocol):
    """Fetch the gamma parameter for a crypto pool at a given block.

    Used by crypto (volatile) Curve pools for dynamic fee calculation.
    """

    def __call__(self, block_number: int) -> int: ...


class PriceScaleFetcher(Protocol):
    """Fetch price_scale values for a crypto pool at a given block.

    Returns a tuple of (n_coins - 1) price scale values.
    Used by crypto (volatile) Curve pools for multi-asset price normalization.
    """

    def __call__(self, block_number: int) -> tuple[int, ...]: ...


class LendingRateFetcher(Protocol):
    """Fetch lending rates for all tokens in a Curve pool at a given block.

    Returns per-token rates scaled to PRECISION (10^18).
    Non-lending tokens return PRECISION. Lending tokens return
    their rate (e.g., cToken exchange rate, yToken PPS).
    """

    def __call__(self, block_number: int) -> tuple[int, ...]: ...


# ── Data Classes ──


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class CurveStableswapPoolState(AbstractPoolState):
    balances: tuple[int, ...]
    base: "CurveStableswapPoolState | None" = None


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
