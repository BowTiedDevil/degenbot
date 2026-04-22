from fractions import Fraction
from typing import Any

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.arbitrage.path.pool_adapter import register_pool_adapter
from degenbot.arbitrage.path.types import PoolCompatibility, SwapVector
from degenbot.arbitrage.solver.types import HopState, MobiusHopState
from degenbot.arbitrage.types import AbstractSwapAmounts


class AerodromeV2PoolAdapter:
    def is_compatible(self, pool: Any) -> PoolCompatibility:
        if getattr(pool, "stable", False):
            return PoolCompatibility.INCOMPATIBLE_INVARIANT
        return PoolCompatibility.COMPATIBLE

    def extract_fee(self, pool: Any, *, zero_for_one: bool) -> Fraction:
        return pool.fee

    def to_hop_state(
        self,
        pool: Any,
        *,
        zero_for_one: bool,
        state_override: Any = None,
    ) -> HopState:
        state = state_override or pool.state
        fee = self.extract_fee(pool, zero_for_one=zero_for_one)
        if zero_for_one:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0
        return MobiusHopState(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
        )

    def build_swap_amount(
        self,
        pool: Any,
        swap_vector: SwapVector,
        amount_in: int,
        amount_out: int,
    ) -> AbstractSwapAmounts:
        msg = "Aerodrome swap amount construction not yet supported"
        raise NotImplementedError(msg)


register_pool_adapter(AerodromeV2Pool, AerodromeV2PoolAdapter())
