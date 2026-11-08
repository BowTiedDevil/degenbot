"""Curve-specific type definitions (swap style, metapool enums).

The member VALUES below are discriminants a pool identity stores — a contract
shared with the Rust core, not a local choice. The core owns them: each enum
mirrors the Rust declaration of the same name in ``degenbot_math::curve``
(``DVariant`` / ``YVariant`` / ``YDVariant`` in its ``stableswap`` module, the
rest in ``curve_dy_calculator``), and
``degenbot_pools::sim::curve_strategies`` resolves a pool address onto those
discriminants. The numbers are written out instead of left to ``auto()`` so an
insertion cannot silently renumber a deployed pool's strategy;
``tests/curve/test_curve_discriminant_mirror.py`` pins every member.
"""

from __future__ import annotations

import dataclasses
from enum import Enum
from typing import TYPE_CHECKING, Final, Protocol, runtime_checkable

from degenbot.checksum_cache import get_checksum_address
from degenbot.types.abstract import AbstractPoolState

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot.erc20 import Erc20Token, NativeToken
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.types.rpc_types import BlockIdentifier


#: The all-Es address the old Vyper Curve pools spell a native coin with —
#: Curve's PRIVATE sentinel, mirroring
#: ``degenbot_pools::NATIVE_SENTINEL_ALL_ES`` (ADR-097 D3). It is never a token
#: contract address and never generic input; only Curve call sites translate
#: it to the conceptual native token. Zero is NOT a Curve native spelling (it
#: remains the coin-enumeration terminator) and is deliberately not
#: recognized here.
NATIVE_COIN_ALL_ES: Final = get_checksum_address("0x" + "EE" * 20)


def is_curve_native_coin(address: str) -> bool:
    """Whether `address` is Curve's all-Es native-coin sentinel.

    Returns:
        Whether `address` is Curve's all-Es native-coin sentinel.

    """
    return get_checksum_address(address) == NATIVE_COIN_ALL_ES


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

    STANDARD = 1  # calc_d + calc_dp
    VARIANT_ALPHA = 2  # calc_d_variant_alpha + calc_dp
    VARIANT_ALPHA_DP_ALPHA = 3  # calc_d_variant_alpha + calc_dp_variant_alpha
    VARIANT_DP_ALPHA = 4  # calc_d + calc_dp_variant_alpha
    VARIANT_BETA_DP = 5  # calc_d + calc_dp_variant_beta
    VARIANT_GAMMA_DP = 6  # calc_d + calc_dp_variant_gamma


class YVariant(Enum):
    """Which Y-calculation formula to use in _get_y.

    The original code had two overlapping address sets controlling independent
    behaviours: Y_VARIANT_GROUP_0 (amp divisor) and Y_VARIANT_GROUP_1 (c/b formula).
    Since Y_VARIANT_GROUP_0 ⊂ Y_VARIANT_GROUP_1, there are exactly 3 observed
    combinations, yielding these variants:
    """

    STANDARD = 1  # amp WITH A_PRECISION divisor + standard c/b
    VARIANT_0 = 2  # amp WITHOUT A_PRECISION divisor + standard c/b
    VARIANT_1 = 3  # amp WITHOUT A_PRECISION divisor + c/b without A_PRECISION


class YDVariant(Enum):
    """Which Y_D-calculation formula to use in _get_y_d."""

    STANDARD = 1
    VARIANT_0 = 2  # A_PRECISION in b/c formulas


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

    STANDARD = 1  # dy = xp[j] - y - 1, fee, then rate convert
    RATE_ADJUSTED = 2  # dy = (xp[j] - y - 1) * PRECISION // rates[j], fee on converted dy
    RAW_BALANCE = 3  # raw balances, no rate conversion, direct fee
    CRYPTO = 4  # Newton's method, dynamic fee
    LIVE_ADMIN = 5  # live balances minus admin, dy = xp[j] - y - 1, fee, rate convert
    LIVE_ADMIN_DYNAMIC = 6  # live balances minus admin, dynamic offpeg fee
    LIVE_ADMIN_DYNAMIC_PRECISION = 7  # live-admin + precision multipliers + dynamic offpeg fee
    LIVE_ADMIN_ORACLE = 8  # live-admin + oracle rates, dy = xp[j] - y - 1
    NO_ONE_FEE_RATE = 9  # dy = xp[j] - y (no -1), fee, rate convert
    CYTOKEN = 10  # dy = xp[j] - y - 1, then (dy-fee)*PRECISION//rates[j]
    RATE_ADJUSTED_NO_ONE = 11  # dy = (xp[j]-y)*PRECISION//rates[j], fee on converted dy


class MetapoolRateStyle(Enum):
    """Which rates to use for the metapool branch in get_dy."""

    STANDARD = 1  # (rate_multipliers[0], virtual_price)
    PRECISION_VP = 2  # (PRECISION, virtual_price)
    REDEMPTION_VP = 3  # (redemption_price, virtual_price)


class MetapoolUnderlyingStyle(Enum):
    """Which computation path to use in _get_dy_underlying."""

    STANDARD = 1  # rate_multipliers with VP for base pool LP token
    REDEMPTION = 2  # redemption_price for first coin, VP for second
    PRECISION_VP = 3  # (PRECISION, virtual_price) — no rate multiplier for first coin


class LendingRateStyle(Enum):
    """Which rate-fetching method to use for lending tokens.

    Used by get_dy() to select which stored-rate resolution path to call
    via CurveDataProvider.lending_rates().
    """

    NONE = 1  # No lending tokens — use rate_multipliers directly
    CTOKEN = 2  # Exchange rate with supply rate accrual
    YTOKEN = 3  # Price per full share
    CYTOKEN = 4  # cToken + yToken combined accrual
    AETH = 5  # Lido aETH ratio inversion
    RETH = 6  # Rocket Pool exchange rate
    ORACLE = 7  # On-chain oracle bitmask


@runtime_checkable
class BasePoolPort(Protocol):
    """The slice of the base-pool surface the metapool ``DyCalculator`` needs.

    Names the *real* interface behind the lazy go-between (ADR-005): a
    metapool's calc paths call exactly these six members on its base pool —
    ``tokens`` / ``balances`` / ``fee`` for metadata, and
    ``calc_token_amount`` / ``get_dy`` / ``calc_withdraw_one_coin`` for
    delegated computation. Two adapters satisfy it: the production
    ``_LazyBasePool`` (handle → base companion, memoised) and a canned
    ``StubBasePool`` for calculator unit tests (which previously couldn't
    exercise ``.calculate()`` without standing up a full pool).
    """

    @property
    def tokens(self) -> tuple[Erc20Token | NativeToken, ...]:
        """Base-pool coin companions."""
        ...

    @property
    def balances(self) -> tuple[int, ...]:
        """Base-pool balances."""
        ...

    @property
    def fee(self) -> int:
        """Base-pool swap fee (``FEE_DENOMINATOR`` units)."""
        ...

    def calc_token_amount(
        self,
        *,
        amounts: Sequence[int],
        deposit: bool,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        """Deposit/withdraw token amount (slippage-adjusted)."""
        ...

    def get_dy(
        self,
        i: int,
        j: int,
        dx: int,
        block_identifier: BlockIdentifier | None = None,
        override_state: CurveStableswapPoolState | None = None,
    ) -> int:
        """Output ``dy`` for swapping ``dx`` of coin ``i`` → coin ``j``."""
        ...

    def calc_withdraw_one_coin(
        self,
        _token_amount: int,
        i: int,
        block_identifier: BlockIdentifier | None = None,
    ) -> tuple[int, ...]:
        """Withdraw a single coin from a deposit."""
        ...


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
    address: ChecksummedAddress

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
    base_pool: BasePoolPort | None = None

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
