from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING, ClassVar

from degenbot.camelot.functions import get_y_camelot, k_camelot
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models import CamelotV2PoolTable
from degenbot.exceptions import DegenbotValueError
from degenbot.logging import logger
from degenbot.solidly.solidly_functions import general_calc_exact_in_stable
from degenbot.types.hop_types import ConstantProductHop, HopType, SolidlyStableHop
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token
    from degenbot.types.aliases import ChainId
    from degenbot.uniswap.v2_types import UniswapV2PoolState


class CamelotLiquidityPool(UniswapV2Pool):
    variant: ClassVar[str | None] = "camelot"

    type DatabasePoolType = CamelotV2PoolTable

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
        address = get_checksum_address(address)
        self.fee_denominator = fee_denominator
        self.stable_swap = stable_swap

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

    def _calculate_tokens_out_from_tokens_in_stable_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        Uses the self.token0 and self.token1 pointers to determine which token is being swapped in
        """

        if override_state is not None:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        if token_in_quantity <= 0:  # pragma: no cover
            raise DegenbotValueError(message="token_in_quantity must be positive")

        precision_multiplier_token0: int = 10**self.token0.decimals
        precision_multiplier_token1: int = 10**self.token1.decimals

        fee_percent = self.fee_denominator * (
            self.fee_token0 if token_in == self.token0 else self.fee_token1
        )

        reserves_token0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_token1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        # Remove fee from amount received
        token_in_quantity -= token_in_quantity * fee_percent // self.fee_denominator
        xy = k_camelot(
            balance_0=reserves_token0,
            balance_1=reserves_token1,
            decimals_0=precision_multiplier_token0,
            decimals_1=precision_multiplier_token1,
        )
        reserves_token0 = reserves_token0 * 10**18 // precision_multiplier_token0
        reserves_token1 = reserves_token1 * 10**18 // precision_multiplier_token1
        reserve_a, reserve_b = (
            (reserves_token0, reserves_token1)
            if token_in == self.token0
            else (reserves_token1, reserves_token0)
        )
        token_in_quantity = (
            token_in_quantity * 10**18 // precision_multiplier_token0
            if token_in == self.token0
            else token_in_quantity * 10**18 // precision_multiplier_token1
        )
        y = reserve_b - get_y_camelot(token_in_quantity + reserve_a, xy, reserve_b)

        return (
            y
            * (
                precision_multiplier_token1
                if token_in == self.token0
                else precision_multiplier_token0
            )
            // 10**18
        )

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: UniswapV2PoolState | None = None,
    ) -> HopType:
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
            reserves0 = state.reserves_token0
            reserves1 = state.reserves_token1
            decimals0 = 10**self.token0.decimals
            decimals1 = 10**self.token1.decimals
            token_in = 0 if zero_for_one else 1

            def _camelot_stable_swap_fn(
                amount_in: int,
                __reserves0: int = reserves0,
                __reserves1: int = reserves1,
                __decimals0: int = decimals0,
                __decimals1: int = decimals1,
                __fee: Fraction = fee_in,
                __token_in: int = token_in,
            ) -> int:
                return general_calc_exact_in_stable(
                    amount_in=amount_in,
                    token_in=__token_in,  # type: ignore[arg-type]
                    reserves0=__reserves0,
                    reserves1=__reserves1,
                    decimals0=__decimals0,
                    decimals1=__decimals1,
                    fee=__fee,
                    k_func=k_camelot,
                    get_y_func=get_y_camelot,  # type: ignore[arg-type]
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

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV2PoolState | None = None,
    ) -> int:
        if self.stable_swap:
            return self._calculate_tokens_out_from_tokens_in_stable_swap(
                token_in=token_in,
                token_in_quantity=token_in_quantity,
                override_state=override_state,
            )
        return super().calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_in_quantity=token_in_quantity,
            override_state=override_state,
        )
