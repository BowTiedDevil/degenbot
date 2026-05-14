import dataclasses
from enum import Enum, auto
from typing import Protocol

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

# ── Fetcher Protocols ──
# These protocols define the interface for callbacks that fetch on-chain data
# for Curve pools. They are injected by Bot.build_curve_pool() to enable
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
