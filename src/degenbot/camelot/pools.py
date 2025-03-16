from fractions import Fraction

from degenbot.cache import get_checksum_address
from degenbot.camelot.functions import get_y_camelot, k_camelot
from degenbot.config import connection_manager
from degenbot.erc20_token import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.uniswap.types import UniswapV2PoolState
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool


class CamelotLiquidityPool(UniswapV2Pool):
    CAMELOT_ARBITRUM_POOL_INIT_HASH = (
        "0xa856464ae65f7619087bc369daaf7e387dae1e5af69cfa7935850ebf754b04c1"
    )

    def __init__(
        self,
        address: str,
        *,
        chain_id: int | None = None,
        silent: bool = False,
    ) -> None:
        address = get_checksum_address(address)

        if chain_id is None:  # pragma: no branch
            chain_id = connection_manager.default_chain_id

        w3 = connection_manager.get_web3(chain_id)
        state_block = w3.eth.get_block_number()

        fee_token0: int
        fee_token1: int
        _, _, fee_token0, fee_token1 = raw_call(
            w3=w3,
            address=address,
            calldata=encode_function_calldata(
                function_prototype="getReserves()",
                function_arguments=None,
            ),
            return_types=["uint256", "uint256", "uint256", "uint256"],
        )

        self.fee_denominator: int
        (self.fee_denominator,) = raw_call(
            w3=w3,
            address=address,
            calldata=encode_function_calldata(
                function_prototype="FEE_DENOMINATOR()",
                function_arguments=None,
            ),
            return_types=["uint256"],
        )

        super().__init__(
            address=address,
            chain_id=chain_id,
            init_hash=self.CAMELOT_ARBITRUM_POOL_INIT_HASH,
            fee=(
                Fraction(fee_token0, self.fee_denominator),
                Fraction(fee_token1, self.fee_denominator),
            ),
            silent=silent,
            state_block=state_block,
        )

        self.stable_swap: bool
        (self.stable_swap,) = raw_call(
            w3=w3,
            address=address,
            calldata=encode_function_calldata(
                function_prototype="stableSwap()",
                function_arguments=None,
            ),
            return_types=["bool"],
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
