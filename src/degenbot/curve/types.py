import dataclasses
from typing import Protocol

from eth_typing import HexAddress

from degenbot.types.abstract import AbstractPoolState
from degenbot.types.aliases import BlockNumber
from degenbot.types.concrete import PoolStateMessage

# ── Fetcher Protocols ──
# These protocols define the interface for callbacks that fetch on-chain data
# for Curve pools. They are injected by Bot.build_curve_pool() to enable
# on-demand data fetching while keeping the pool class I/O-free.


class RateFetcher(Protocol):
    """Fetch rates for lending tokens (cTokens, yTokens, etc.) at a given block.

    Returns rates for all pool tokens in the same order as pool.tokens.
    Non-lending tokens return PRECISION (10^18).
    """

    def __call__(self, block_number: int) -> tuple[int, ...]: ...


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
