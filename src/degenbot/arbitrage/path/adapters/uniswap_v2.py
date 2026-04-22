from fractions import Fraction

from degenbot.arbitrage.path.pool_adapter import register_pool_adapter
from degenbot.arbitrage.path.types import SwapVector
from degenbot.arbitrage.solver.types import HopState, MobiusHopState
from degenbot.arbitrage.types import AbstractSwapAmounts, UniswapV2PoolSwapAmounts
from degenbot.types.abstract import AbstractUniswapV2Pool
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolState


class UniswapV2PoolAdapter:
    def extract_fee(self, pool: UniswapV2Pool, *, zero_for_one: bool) -> Fraction:
        if zero_for_one:
            return pool.fee_token0
        return pool.fee_token1

    def to_hop_state(
        self,
        pool: UniswapV2Pool,
        *,
        zero_for_one: bool,
        state_override: UniswapV2PoolState | None = None,
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
        pool: UniswapV2Pool,
        swap_vector: SwapVector,
        amount_in: int,
        amount_out: int,
    ) -> AbstractSwapAmounts:
        zfo = swap_vector.zero_for_one
        return UniswapV2PoolSwapAmounts(
            pool=pool.address,
            amounts_in=(amount_in, 0) if zfo else (0, amount_in),
            amounts_out=(0, amount_out) if zfo else (amount_out, 0),
        )


register_pool_adapter(AbstractUniswapV2Pool, UniswapV2PoolAdapter())
