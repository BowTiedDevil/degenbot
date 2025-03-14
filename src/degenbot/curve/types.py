# ruff: noqa: A005

import dataclasses

from eth_typing import HexAddress

from degenbot.types import AbstractPoolState, PoolStateMessage


@dataclasses.dataclass(slots=True, frozen=True, kw_only=True)
class CurveStableswapPoolState(AbstractPoolState):
    balances: tuple[int, ...]
    base: "CurveStableswapPoolState | None" = None


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
