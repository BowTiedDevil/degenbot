from fractions import Fraction

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.aerodrome.types import AerodromeV2PoolState
from degenbot.arbitrage.optimizers.hop_types import ConstantProductHop, HopType
from degenbot.arbitrage.path.pool_adapter import register_pool_adapter
from degenbot.arbitrage.path.types import PoolCompatibility, SwapVector
from degenbot.arbitrage.types import AbstractSwapAmounts
from degenbot.types.abstract import AbstractAerodromeV2Pool


class AerodromeV2PoolAdapter:
    def extract_fee(self, pool: AerodromeV2Pool, *, zero_for_one: bool) -> Fraction:
        return pool.fee

    def to_hop_state(
        self,
        pool: AerodromeV2Pool,
        *,
        zero_for_one: bool,
        state_override: AerodromeV2PoolState | None = None,
    ) -> HopType:
        state = state_override or pool.state
        fee = self.extract_fee(pool, zero_for_one=zero_for_one)
        if zero_for_one:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0
        return ConstantProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
        )

    def build_swap_amount(
        self,
        pool: AerodromeV2Pool,
        swap_vector: SwapVector,
        amount_in: int,
        amount_out: int,
    ) -> AbstractSwapAmounts:
        msg = "Aerodrome swap amount construction not yet supported"
        raise NotImplementedError(msg)


register_pool_adapter(
    AbstractAerodromeV2Pool,
    AerodromeV2PoolAdapter(),
    is_compatible=lambda pool: (
        PoolCompatibility.INCOMPATIBLE_INVARIANT
        if getattr(pool, "stable", False)
        else PoolCompatibility.COMPATIBLE
    ),
)
