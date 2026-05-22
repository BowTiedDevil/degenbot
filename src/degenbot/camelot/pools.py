"""Camelot V2 liquidity pool implementation."""
from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, ClassVar

from degenbot.calculations.camelot import get_y_camelot, k_camelot
from degenbot.calculations.solidly_stable import calc_exact_in_stable
from degenbot.camelot.v2_pool_calc import CamelotPoolCalc
from degenbot.checksum_cache import get_checksum_address
from degenbot.types.hop_types import ConstantProductHop, HopType, SolidlyStableHop
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.types.aliases import ChainId
    from degenbot.uniswap.v2_types import UniswapV2PoolState


class CamelotLiquidityPool(CamelotPoolCalc, UniswapV2Pool):
    """CamelotLiquidityPool class."""

    variant: ClassVar[str | None] = "camelot"

    CAMELOT_ARBITRUM_POOL_INIT_HASH = (
        "0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1"
    )

    def __init__(
        self,
        address: str,
        *,
        token0: Erc20Token,
        token1: Erc20Token,
        factory: str,
        fee_token0: int,
        fee_token1: int,
        fee_denominator: int,
        reserves_token0: int,
        reserves_token1: int,
        stable_swap: bool,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        deployer_address: str | None = None,
    ) -> None:
        """Initialize the instance."""
        address = get_checksum_address(address)
        self.fee_denominator = fee_denominator
        self.stable_swap = stable_swap

        # Wire calculation strategy at construction
        self._wire_camelot_calculations(stable_swap=stable_swap)

        super().__init__(
            address=address,
            chain_id=chain_id if chain_id is not None else token0.chain_id,
            init_hash=self.CAMELOT_ARBITRUM_POOL_INIT_HASH,
            token0=token0,
            token1=token1,
            factory=factory,
            fee_token0=Fraction(fee_token0, fee_denominator),
            fee_token1=Fraction(fee_token1, fee_denominator),
            reserves_token0=reserves_token0,
            reserves_token1=reserves_token1,
            state_block=state_block,
            deployer_address=deployer_address,
        )

    def to_hop_state(
        self,
        *,
        zero_for_one: bool,
        state_override: UniswapV2PoolState | None = None,
        token_in: Erc20Token | None = None,  # ruff: ignore[ARG002]
        token_out: Erc20Token | None = None,  # ruff: ignore[ARG002]
    ) -> HopType:
        """Convert to hop state."""
        # token_in/token_out unused — 2-token pools determine pair from zero_for_one.
        # Callers should ensure these match pool.token0/pool.token1 if provided.
        state = state_override or self.state
        fee_in = self.extract_fee(zero_for_one=zero_for_one)

        if zero_for_one:
            reserve_in = state.reserves_token0
            reserve_out = state.reserves_token1
            decimals_in = self.token0.decimals
            decimals_out = self.token1.decimals
        else:
            reserve_in = state.reserves_token1
            reserve_out = state.reserves_token0
            decimals_in = self.token1.decimals
            decimals_out = self.token0.decimals

        if self.stable_swap:

            def _camelot_stable_swap_fn(
                amount_in: int,
                /,
                _reserves0: int = state.reserves_token0,
                _reserves1: int = state.reserves_token1,
                _decimals0: int = 10**self.token0.decimals,
                _decimals1: int = 10**self.token1.decimals,
                _fee: Fraction = fee_in,
                _token_in: int = 0 if zero_for_one else 1,
            ) -> int:
                return calc_exact_in_stable(
                    amount_in=amount_in,
                    token_in=_token_in,
                    reserves0=_reserves0,
                    reserves1=_reserves1,
                    decimals0=_decimals0,
                    decimals1=_decimals1,
                    fee=_fee,
                    k_func=k_camelot,
                    get_y_func=get_y_camelot,  # ty:ignore[invalid-argument-type]
                )

            return SolidlyStableHop(
                reserve_in=reserve_in,
                reserve_out=reserve_out,
                fee=fee_in,
                decimals_in=decimals_in,
                decimals_out=decimals_out,
                swap_fn=_camelot_stable_swap_fn,
            )

        fee_out = self.fee_token1 if zero_for_one else self.fee_token0
        return ConstantProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee_in,
            fee_out=fee_out,
        )
