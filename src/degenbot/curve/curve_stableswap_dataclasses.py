import dataclasses
from typing import List

from eth_typing import ChecksumAddress, HexAddress

from ..baseclasses import BasePoolState, Message


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableswapPoolState(BasePoolState):
    pool: ChecksumAddress
    balances: List[int]
    base: "CurveStableswapPoolState | None" = dataclasses.field(default=None)


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
    coin_addresses: List[HexAddress]
    coin_index_type: str
    fee: int
    admin_fee: int
    is_metapool: bool
    underlying_coin_addresses: List[HexAddress] | None = dataclasses.field(default=None)
    base_pool_address: HexAddress | None = dataclasses.field(default=None)


@dataclasses.dataclass(slots=True, frozen=True)
class CurveStableSwapPoolStateUpdated(Message):
    state: CurveStableswapPoolState
