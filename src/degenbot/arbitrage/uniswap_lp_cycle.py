import asyncio
import math
from collections.abc import Iterable, Mapping, Sequence
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor
from dataclasses import dataclass
from fractions import Fraction
from typing import Any
from weakref import WeakSet

import eth_abi.abi
from eth_typing import ChecksumAddress, HexStr
from scipy.optimize import OptimizeResult, minimize_scalar
from web3 import Web3

from degenbot.aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from degenbot.aerodrome.types import AerodromeV2PoolState
from degenbot.arbitrage.types import (
    ArbitrageCalculationResult,
    UniswapPoolSwapVector,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
    UniswapV4PoolSwapAmounts,
)
from degenbot.cache import get_checksum_address
from degenbot.constants import WRAPPED_NATIVE_TOKENS, ZERO_ADDRESS
from degenbot.erc20_token import Erc20Token, EtherPlaceholder
from degenbot.exceptions import (
    ArbitrageError,
    DegenbotValueError,
    EVMRevertError,
    LiquidityPoolError,
    RateOfExchangeBelowMinimum,
)
from degenbot.logging import logger
from degenbot.types import (
    AbstractArbitrage,
    Message,
    PoolStateMessage,
    Publisher,
    PublisherMixin,
    Subscriber,
    TextMessage,
)
from degenbot.uniswap.types import UniswapV2PoolState, UniswapV3PoolState, UniswapV4PoolState
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MIN_SQRT_RATIO,
    get_sqrt_ratio_at_tick,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

type Pool = AerodromeV2Pool | AerodromeV3Pool | UniswapV2Pool | UniswapV3Pool | UniswapV4Pool
type PoolState = AerodromeV2PoolState | UniswapV2PoolState | UniswapV3PoolState | UniswapV4PoolState
type SwapAmount = UniswapV2PoolSwapAmounts | UniswapV3PoolSwapAmounts | UniswapV4PoolSwapAmounts
type PoolId = bytes | HexStr


@dataclass(slots=True, frozen=True)
class V4PoolKey:
    currency0: ChecksumAddress
    currency1: ChecksumAddress
    fee: int
    tick_spacing: int
    hooks: ChecksumAddress


UNISWAP_V2_SWAP_FUNCTION_PROTOTYPE = "swap(uint256,uint256,address,bytes)"
UNISWAP_V2_SWAP_FUNCTION_SELECTOR = Web3.keccak(
    text=UNISWAP_V2_SWAP_FUNCTION_PROTOTYPE,
)[:4]

UNISWAP_V3_SWAP_FUNCTION_PROTOTYPE = "swap(address,bool,int256,uint160,bytes)"
UNISWAP_V3_SWAP_FUNCTION_SELECTOR = Web3.keccak(
    text=UNISWAP_V3_SWAP_FUNCTION_PROTOTYPE,
)[:4]

ERC20_TOKEN_TRANSFER_FUNCTION_PROTOTYPE = "transfer(address,uint256)"
ERC20_TOKEN_TRANSFER_FUNCTION_SELECTOR = Web3.keccak(
    text=ERC20_TOKEN_TRANSFER_FUNCTION_PROTOTYPE,
)[:4]


class UniswapLpCycle(PublisherMixin, AbstractArbitrage):
    def _notify_subscribers(self: Publisher, message: Message) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)

    def __init__(
        self,
        input_token: Erc20Token,
        swap_pools: Iterable[Pool],
        id: str,  # noqa:A002
        max_input: int | None = None,
    ):
        for swap_pool in swap_pools:
            if not isinstance(swap_pool, Pool.__value__):
                raise DegenbotValueError(
                    message=f"Incompatible pool type ({type(swap_pool)}) provided."
                )
        self.swap_pools: tuple[Pool, ...] = tuple(swap_pools)
        if len(set(self.swap_pools)) != len(self.swap_pools):
            raise DegenbotValueError(message="Swap pools must not contain duplicates.")

        self.id = id
        self.input_token = input_token

        if max_input is None:
            logger.warning("No maximum input provided, setting to 100 WETH")
            max_input = 100 * 10**18
        elif max_input <= 0:
            raise DegenbotValueError(message="Maximum input must be positive.")
        self.max_input = max_input

        _swap_vectors: list[UniswapPoolSwapVector] = []
        for i, pool in enumerate(self.swap_pools):
            input_token = self.input_token if i == 0 else _swap_vectors[-1].token_out

            match input_token:
                case pool.token0:
                    _swap_vectors.append(
                        UniswapPoolSwapVector(
                            token_in=pool.token0,
                            token_out=pool.token1,
                            zero_for_one=pool.token0 == input_token,
                        )
                    )
                case pool.token1:
                    _swap_vectors.append(
                        UniswapPoolSwapVector(
                            token_in=pool.token1,
                            token_out=pool.token0,
                            zero_for_one=pool.token0 == input_token,
                        )
                    )
                case EtherPlaceholder() if (
                    wrapped_native_token := WRAPPED_NATIVE_TOKENS[pool.chain_id]
                ) in pool.tokens:
                    # Handle case where input token is Ether and pool holds wrapped native
                    _swap_vectors.append(
                        UniswapPoolSwapVector(
                            token_in=pool.token0
                            if pool.token0 == wrapped_native_token
                            else pool.token1,
                            token_out=pool.token1
                            if pool.token0 == wrapped_native_token
                            else pool.token0,
                            zero_for_one=pool.token0 == wrapped_native_token,
                        )
                    )
                case Erc20Token() if (
                    input_token == WRAPPED_NATIVE_TOKENS[pool.chain_id]
                    and ZERO_ADDRESS in pool.tokens
                ):
                    # Handle case where input token is wrapped native and pool holds Ether
                    _swap_vectors.append(
                        UniswapPoolSwapVector(
                            token_in=pool.token0 if pool.token0 == ZERO_ADDRESS else pool.token1,
                            token_out=pool.token1 if pool.token0 == ZERO_ADDRESS else pool.token0,
                            zero_for_one=pool.token1 == ZERO_ADDRESS,
                        )
                    )
                case _:
                    raise DegenbotValueError(
                        message=f"Token {input_token} could not be matched. Pool holds {pool.token0} & {pool.token1}"  # noqa: E501
                    )

        self._swap_vectors = tuple(_swap_vectors)

        self.name = " → ".join([pool.name for pool in self.swap_pools])

        self._pool_viability: dict[Pool, bool] = {
            pool: self._pool_is_viable(pool=pool, state=pool.state, vector=swap_vector)
            for pool, swap_vector in zip(self.swap_pools, self._swap_vectors, strict=True)
        }
        assert len(self._pool_viability) == len(self.swap_pools), f"{self.id=} {self.swap_pools=}"

        self._subscribers: WeakSet[Subscriber] = WeakSet()
        for pool in self.swap_pools:
            pool.subscribe(self)

    def __getstate__(self) -> dict[str, Any]:
        dropped_attributes = ("_subscribers",)
        copied_attributes = ()

        return {
            k: (v.copy() if k in copied_attributes else v)
            for k, v in self.__dict__.items()
            if k not in dropped_attributes
        }

    def __str__(self) -> str:
        return self.name

    def _build_swap_amounts(
        self,
        token_in_quantity: int,
        state_overrides: Mapping[Pool, PoolState] | None = None,
    ) -> tuple[SwapAmount, ...]:
        """
        Generate inputs for all swaps along the arbitrage path, starting with the specified amount
        of the input token defined in the constructor.
        """

        if state_overrides is None:
            state_overrides = {}

        _token_out_quantity = 0
        swap_amounts: list[SwapAmount] = []
        for i, (pool, swap_vector) in enumerate(
            zip(self.swap_pools, self._swap_vectors, strict=True)
        ):
            _token_in_quantity = token_in_quantity if i == 0 else _token_out_quantity

            if _token_in_quantity == 0:
                raise ArbitrageError(message="Zero amount swap")

            try:
                match pool, state_overrides.get(pool):
                    case AerodromeV2Pool(), (
                        AerodromeV2PoolState() | None
                    ) as aerodrome_v2_pool_state:
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=swap_vector.token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=aerodrome_v2_pool_state,
                        )
                        if _token_out_quantity == 0:  # pragma: no cover
                            raise ArbitrageError(
                                message=f"Zero-output swap through pool {pool} @ {pool.address}"
                            )
                        swap_amounts.append(
                            UniswapV2PoolSwapAmounts(
                                pool=pool.address,
                                amounts_in=(_token_in_quantity, 0)
                                if swap_vector.zero_for_one
                                else (0, _token_in_quantity),
                                amounts_out=(0, _token_out_quantity)
                                if swap_vector.zero_for_one
                                else (_token_out_quantity, 0),
                            )
                        )
                    case UniswapV2Pool(), (UniswapV2PoolState() | None) as uniswap_v2_pool_state:
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=swap_vector.token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=uniswap_v2_pool_state,
                        )
                        if _token_out_quantity == 0:  # pragma: no cover
                            raise ArbitrageError(
                                message=f"Zero-output swap through pool {pool} @ {pool.address}"
                            )
                        swap_amounts.append(
                            UniswapV2PoolSwapAmounts(
                                pool=pool.address,
                                amounts_in=(_token_in_quantity, 0)
                                if swap_vector.zero_for_one
                                else (0, _token_in_quantity),
                                amounts_out=(0, _token_out_quantity)
                                if swap_vector.zero_for_one
                                else (_token_out_quantity, 0),
                            )
                        )
                    case UniswapV3Pool(), (UniswapV3PoolState() | None) as uniswap_v3_pool_state:
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=swap_vector.token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=uniswap_v3_pool_state,
                        )
                        if _token_out_quantity == 0:  # pragma: no cover
                            raise ArbitrageError(
                                message=f"Zero-output swap through pool {pool} @ {pool.address}"
                            )
                        swap_amounts.append(
                            UniswapV3PoolSwapAmounts(
                                pool=pool.address,
                                amount_specified=_token_in_quantity,
                                zero_for_one=swap_vector.zero_for_one,
                                sqrt_price_limit_x96=MIN_SQRT_RATIO + 1
                                if swap_vector.zero_for_one
                                else MAX_SQRT_RATIO - 1,
                            )
                        )
                    case UniswapV4Pool(), (UniswapV4PoolState() | None) as uniswap_v4_pool_state:
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=swap_vector.token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=uniswap_v4_pool_state,
                        )
                        if _token_out_quantity == 0:  # pragma: no cover
                            raise ArbitrageError(
                                message=f"Zero-output swap through pool {pool} @ {pool.address}"
                            )
                        swap_amounts.append(
                            UniswapV4PoolSwapAmounts(
                                address=pool.address,
                                id=pool.pool_id,
                                amount_specified=_token_in_quantity,
                                zero_for_one=swap_vector.zero_for_one,
                                sqrt_price_limit_x96=MIN_SQRT_RATIO + 1
                                if swap_vector.zero_for_one
                                else MAX_SQRT_RATIO - 1,
                            )
                        )
                    case _:  # pragma: no cover
                        raise DegenbotValueError(
                            message="Could not identify pool and override type."
                        )
            except LiquidityPoolError as exc:  # pragma: no cover
                raise ArbitrageError(message=str(exc)) from exc

        return tuple(swap_amounts)

    def _pool_is_viable(
        self,
        pool: Pool,
        state: PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        """
        Check if the pool can perform a swap along the given vector.
        """

        match pool, state:
            case (
                AerodromeV2Pool(),
                AerodromeV2PoolState(),
            ):
                if state.reserves_token0 == 0 or state.reserves_token1 == 0:
                    return False

                return (
                    state.reserves_token1 > 1 if vector.zero_for_one else state.reserves_token0 > 1
                )
            case (
                UniswapV2Pool(),
                UniswapV2PoolState(),
            ):
                if state.reserves_token0 == 0 or state.reserves_token1 == 0:
                    return False

                return (
                    state.reserves_token1 > 1 if vector.zero_for_one else state.reserves_token0 > 1
                )

            case (
                UniswapV3Pool() | UniswapV4Pool(),
                UniswapV3PoolState() | UniswapV4PoolState(),
            ):
                if pool.sparse_liquidity_map:
                    # Liquidity cannot be checked with a sparse mapping, so default to True
                    return True

                if state.tick_data == {}:
                    # The pool has no liquidity
                    return False

                if state.sqrt_price_x96 == 0:
                    # The pool is not initialized
                    assert state.tick_data == {}, (
                        f"Found pool @ {pool.address} with liquidity positions, but price=0!"
                    )
                    return False

                if (vector.zero_for_one is True and state.sqrt_price_x96 <= MIN_SQRT_RATIO + 1) or (
                    vector.zero_for_one is False and state.sqrt_price_x96 >= MAX_SQRT_RATIO - 1
                ):
                    # The price has reached the min/max price, and the swap would drive it beyond
                    # that limit
                    return False

                # ----------------------------------------------------------------------------------
                # After this point, at least one liquidity position is assumed
                # ----------------------------------------------------------------------------------

                if vector.zero_for_one:
                    # A 0->1 swap will lower the price & tick, so pool viability can be
                    # determined by checking for a liquidity position starting below
                    # the current price
                    return get_sqrt_ratio_at_tick(min(state.tick_data)) < state.sqrt_price_x96
                # A 1->0 swap will raise the price & tick. Check for a liquidity position
                # above the current price, similar to the above comment.
                return get_sqrt_ratio_at_tick(max(state.tick_data)) > state.sqrt_price_x96

            case _:  # pragma: no cover
                raise DegenbotValueError(
                    message=f"Could not identify pool {pool} and state {state}."
                )

    def _pre_calculation_check(
        self,
        min_rate_of_exchange: Fraction = Fraction(1, 1),
        state_overrides: Mapping[Pool, PoolState] | None = None,
    ) -> None:
        """
        Perform pool viability and minimum rate of exchange checks. Raises an exception if a
        non-viable pool is found, or if the instantaneous rate of exchange is below the specified
        minimum.
        """

        if state_overrides is None:
            state_overrides = {}

        # Check the pools for viability
        for pool, vector in zip(self.swap_pools, self._swap_vectors, strict=True):
            assert pool in self._pool_viability

            if pool in state_overrides:
                if not self._pool_is_viable(pool=pool, state=state_overrides[pool], vector=vector):
                    raise ArbitrageError(message=f"Pool {pool!r} is not viable")
            elif self._pool_viability[pool] is False:
                raise ArbitrageError(message=f"Pool {pool!r} is not viable")

        # Evaluate the instantaneous rate of exchange for the path by accumulating swap and fee
        # multipliers for each pool
        exchange_rate_multipliers: list[Fraction] = []
        for pool, vector in zip(self.swap_pools, self._swap_vectors, strict=True):
            match pool, state_overrides.get(pool):
                case AerodromeV2Pool(), (AerodromeV2PoolState() | None) as aerodrome_v2_pool_state:
                    # The multiplier for the pool is the rate of exchange for the output token,
                    # reduced by the fee taken on the input amount
                    swap_multiplier = pool.get_absolute_exchange_rate(
                        token=vector.token_out,
                        override_state=aerodrome_v2_pool_state,
                    )
                    fee = pool.fee_token0 if vector.zero_for_one else pool.fee_token1
                    fee_multiplier = Fraction(fee.denominator - fee.numerator, fee.denominator)

                    exchange_rate_multipliers.append(swap_multiplier)
                    exchange_rate_multipliers.append(fee_multiplier)

                case UniswapV2Pool(), (UniswapV2PoolState() | None) as uniswap_v2_pool_state:
                    # The multiplier for the pool is the rate of exchange for the output token,
                    # reduced by the fee taken on the input amount
                    swap_multiplier = pool.get_absolute_exchange_rate(
                        token=vector.token_out,
                        override_state=uniswap_v2_pool_state,
                    )
                    fee = pool.fee_token0 if vector.zero_for_one else pool.fee_token1
                    fee_multiplier = Fraction(fee.denominator - fee.numerator, fee.denominator)

                    exchange_rate_multipliers.append(swap_multiplier)
                    exchange_rate_multipliers.append(fee_multiplier)

                case UniswapV3Pool(), (UniswapV3PoolState() | None) as uniswap_v3_pool_state:
                    swap_multiplier = pool.get_absolute_exchange_rate(
                        token=vector.token_out,
                        override_state=uniswap_v3_pool_state,
                    )

                    # TODO: add support for mixed-fee pools if necessary
                    fee = Fraction(pool.fee, pool.FEE_DENOMINATOR)
                    fee_multiplier = Fraction(fee.denominator - fee.numerator, fee.denominator)

                    exchange_rate_multipliers.append(swap_multiplier)
                    exchange_rate_multipliers.append(fee_multiplier)

                case UniswapV4Pool(), (UniswapV4PoolState() | None) as uniswap_v4_pool_state:
                    swap_multiplier = pool.get_absolute_exchange_rate(
                        token=vector.token_out,
                        override_state=uniswap_v4_pool_state,
                    )

                    # TODO: add support for mixed-fee pools if necessary
                    fee = Fraction(pool.fee, pool.FEE_DENOMINATOR)
                    fee_multiplier = Fraction(fee.denominator - fee.numerator, fee.denominator)

                    exchange_rate_multipliers.append(swap_multiplier)
                    exchange_rate_multipliers.append(fee_multiplier)

                case _:  # pragma: no cover
                    raise DegenbotValueError(message=f"Could not identify pool {pool}.")

        net_rate_of_exchange = Fraction(
            math.prod(multiplier.numerator for multiplier in exchange_rate_multipliers),
            math.prod(multiplier.denominator for multiplier in exchange_rate_multipliers),
        )
        if net_rate_of_exchange < min_rate_of_exchange:
            raise RateOfExchangeBelowMinimum(net_rate_of_exchange)

    def _calculate(
        self,
        state_overrides: Mapping[Pool, PoolState] | None = None,
    ) -> ArbitrageCalculationResult:
        """
        Calculate the optimal arbitrage profit using the maximum input as an upper bound.
        """

        if state_overrides is None:
            state_overrides = {}

        # The bounded Brent optimizer requires bounds for the input amount, and a bracketed guess
        # to initiate the search
        bounds: tuple[float, float] = (
            1.0,
            float(self.max_input),
        )
        bracket: tuple[float, float] = (0.25 * self.max_input, 0.50 * self.max_input)

        def arb_profit(x: float) -> float:
            token_in_quantity = int(x)  # round the input down
            token_out_quantity: int = 0

            try:
                for i, (pool, swap_vector) in enumerate(
                    zip(self.swap_pools, self._swap_vectors, strict=True)
                ):
                    state = state_overrides.get(pool)

                    match pool, state:
                        case AerodromeV2Pool(), (
                            AerodromeV2PoolState() | None
                        ) as aerodrome_v2_pool_state:
                            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                                token_in=swap_vector.token_in,
                                token_in_quantity=token_in_quantity
                                if i == 0
                                else token_out_quantity,
                                override_state=aerodrome_v2_pool_state,
                            )
                        case UniswapV2Pool(), (
                            UniswapV2PoolState() | None
                        ) as uniswap_v2_pool_state:
                            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                                token_in=swap_vector.token_in,
                                token_in_quantity=token_in_quantity
                                if i == 0
                                else token_out_quantity,
                                override_state=uniswap_v2_pool_state,
                            )
                        case UniswapV3Pool(), (
                            UniswapV3PoolState() | None
                        ) as uniswap_v3_pool_state:
                            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                                token_in=swap_vector.token_in,
                                token_in_quantity=token_in_quantity
                                if i == 0
                                else token_out_quantity,
                                override_state=uniswap_v3_pool_state,
                            )
                        case UniswapV4Pool(), (
                            UniswapV4PoolState() | None
                        ) as uniswap_v4_pool_state:
                            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                                token_in=swap_vector.token_in,
                                token_in_quantity=token_in_quantity
                                if i == 0
                                else token_out_quantity,
                                override_state=uniswap_v4_pool_state,
                            )
                        case _:  # pragma: no cover
                            raise DegenbotValueError(
                                message=f"Unrecognized pool {pool} and state {state}."
                            )
            except (EVMRevertError, LiquidityPoolError):  # pragma: no cover
                # The optimizer might send invalid amounts into the swap calculation during
                # iteration. We don't want it to stop, so catch the exception and pretend the
                # swap resulted in zero output
                token_out_quantity = 0

            # minimize_scalar requires the function to have a minimum value
            # for the solver to settle on an optimum input, so return the
            # negated profit
            return float(token_out_quantity - token_in_quantity)

        opt: OptimizeResult = minimize_scalar(
            fun=lambda x: -arb_profit(x),
            method="bounded",
            bounds=bounds,
            bracket=bracket,
            options={"xatol": 1.0},
        )

        # Negate the result to convert to a sensible value (positive profit)
        best_profit = -int(opt.fun)
        swap_amount = int(opt.x)

        best_amounts = self._build_swap_amounts(
            token_in_quantity=swap_amount,
            state_overrides=state_overrides,
        )

        newest_state_block = None
        if not state_overrides:
            pool_state_blocks = tuple(
                block for pool in self.swap_pools if (block := pool.state.block) is not None
            )
            if len(pool_state_blocks) == len(self.swap_pools):
                newest_state_block = max(pool_state_blocks)

        return ArbitrageCalculationResult(
            id=self.id,
            input_token=self.input_token,
            profit_token=self.input_token,
            input_amount=swap_amount,
            profit_amount=best_profit,
            swap_amounts=best_amounts,
            state_block=newest_state_block,
        )

    def calculate(
        self,
        state_overrides: Mapping[Pool, PoolState] | None = None,
        min_rate_of_exchange: Fraction = Fraction(1, 1),
    ) -> ArbitrageCalculationResult:
        """
        Calculate the results of the arbitrage at the current pool states, or at one or more
        overridden pool states if provided.
        """

        self._pre_calculation_check(
            min_rate_of_exchange=min_rate_of_exchange,
            state_overrides=state_overrides,
        )

        return self._calculate(state_overrides=state_overrides)

    async def calculate_with_pool(
        self,
        executor: ProcessPoolExecutor | ThreadPoolExecutor,
        state_overrides: Mapping[Pool, PoolState] | None = None,
        min_rate_of_exchange: Fraction = Fraction(1, 1),
    ) -> asyncio.Future[ArbitrageCalculationResult]:
        """
        Wrap the arbitrage calculation into an asyncio future using the specified executor.

        Arguments
        ---------
        executor : Executor
            An executor (from `concurrent.futures`) to process the calculation work. Both
            `ThreadPoolExecutor` and `ProcessPoolExecutor` are supported, but `ProcessPoolExecutor`
            is recommended for CPU-bound work like this.
        state_overrides : Mapping[ChecksumAddress, UniswapPoolStateOverride], optional
            A dict (or equivalent mapping) of pool states, keyed by the checksummed address of that
            pool.
        min_rate_of_exchange : Fraction, optional
            The minimum net rate of exchange for the arbitrage path. Rates below this minimum will
            raise an exception.

        Returns
        -------
        A future which returns a `ArbitrageCalculationResult` (or exception) when awaited.

        Notes
        -----
        This is an async function that must be called with the `await` keyword.
        """

        if isinstance(executor, ProcessPoolExecutor) and any(
            pool.sparse_liquidity_map for pool in self.swap_pools if isinstance(pool, UniswapV3Pool)
        ):
            raise DegenbotValueError(
                message="Cannot perform calculation with process pool executor. One or more V3 pools has a sparse bitmap."  # noqa:E501
            )

        self._pre_calculation_check(
            min_rate_of_exchange=min_rate_of_exchange,
            state_overrides=state_overrides,
        )

        return asyncio.get_running_loop().run_in_executor(
            executor,
            self._calculate,
            state_overrides,
        )

    def generate_payloads(
        self,
        from_address: ChecksumAddress | str,
        swap_amount: int,
        pool_swap_amounts: Sequence[SwapAmount],
    ) -> list[tuple[ChecksumAddress, bytes, int]]:
        """
        Generate a list of ABI-encoded calldata for each step in the swap path.

        Calldata is built using the eth_abi.encode method and the ABI for the
        ``swap`` function at the Uniswap pool. V2 and V3 pools are supported.

        Arguments
        ---------
        from_address: str
            The address that will execute the calldata. Must be a smart
            contract implementing the required callbacks specific to the pool.

        swap_amount: int
            The initial amount of `token_in` to swap through the first pool.

        pool_swap_amounts: Iterable[UniswapV2PoolSwapAmounts |
        UniswapV3PoolSwapAmounts] | UniswapV4PoolSwapAmounts]
            An iterable of swap amounts to be encoded.

        Returns
        -------
        ``list[(str, bytes, int)]``
            A list of payload tuples. Each payload tuple has form (
            address: ChecksumAddress, calldata: bytes, value: int).
        """

        from_address = get_checksum_address(from_address)

        msg_value = 0  # This arbitrage does not require a `msg.value` payment
        payloads = []
        for i, (swap_pool, _swap_amounts) in enumerate(
            zip(self.swap_pools, pool_swap_amounts, strict=True)
        ):
            # Special case when a Uniswap V2 pool is the next step in the path
            if i < len(self.swap_pools) - 1 and isinstance(
                (next_pool := self.swap_pools[i + 1]), AerodromeV2Pool | UniswapV2Pool
            ):
                swap_destination_address = next_pool.address
            else:
                swap_destination_address = from_address

            match swap_pool, _swap_amounts:
                case AerodromeV2Pool() | UniswapV2Pool(), UniswapV2PoolSwapAmounts():
                    # Special case: If first pool is type V2, input token must be transferred prior
                    # to the swap
                    if i == 0:
                        payloads.append(
                            (
                                # address
                                self.input_token.address,
                                # bytes calldata
                                ERC20_TOKEN_TRANSFER_FUNCTION_SELECTOR
                                + eth_abi.abi.encode(
                                    types=(
                                        "address",
                                        "uint256",
                                    ),
                                    args=(
                                        swap_pool.address,
                                        swap_amount,
                                    ),
                                ),
                                msg_value,
                            )
                        )

                    logger.debug(f"PAYLOAD: building V2 swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: swap amounts {_swap_amounts}")
                    logger.debug(f"PAYLOAD: destination address {swap_destination_address}")

                    payloads.append(
                        (
                            # address
                            swap_pool.address,
                            # bytes calldata
                            UNISWAP_V2_SWAP_FUNCTION_SELECTOR
                            + eth_abi.abi.encode(
                                types=(
                                    "uint256",
                                    "uint256",
                                    "address",
                                    "bytes",
                                ),
                                args=(
                                    *_swap_amounts.amounts_out,
                                    swap_destination_address,
                                    b"",
                                ),
                            ),
                            msg_value,
                        )
                    )
                case UniswapV3Pool(), UniswapV3PoolSwapAmounts():
                    logger.debug(f"PAYLOAD: building V3 swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: swap amounts {_swap_amounts}")
                    logger.debug(f"PAYLOAD: destination address {swap_destination_address}")

                    payloads.append(
                        (
                            # address
                            swap_pool.address,
                            # bytes calldata
                            UNISWAP_V3_SWAP_FUNCTION_SELECTOR
                            + eth_abi.abi.encode(
                                types=(
                                    "address",
                                    "bool",
                                    "int256",
                                    "uint160",
                                    "bytes",
                                ),
                                args=(
                                    swap_destination_address,
                                    _swap_amounts.zero_for_one,
                                    _swap_amounts.amount_specified,
                                    _swap_amounts.sqrt_price_limit_x96,
                                    b"",
                                ),
                            ),
                            msg_value,
                        )
                    )
                case UniswapV4Pool(), UniswapV4PoolSwapAmounts():
                    logger.debug(f"PAYLOAD: building V4 swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: swap amounts {_swap_amounts}")
                    logger.debug(f"PAYLOAD: destination address {swap_destination_address}")

                    payloads.append(
                        V4PoolKey(
                            currency0=swap_pool.token0.address,
                            currency1=swap_pool.token1.address,
                            fee=swap_pool.fee,
                            tick_spacing=swap_pool.tick_spacing,
                            hooks=swap_pool.hook_address,
                        )
                    )

                case _:  # pragma: no cover
                    raise DegenbotValueError(message="Could not identify pool and swap amounts.")

        return payloads

    def notify(self, publisher: Publisher, message: Message) -> None:
        match publisher, message:
            case (
                (
                    AerodromeV2Pool()
                    | AerodromeV3Pool()
                    | UniswapV2Pool()
                    | UniswapV3Pool()
                    | UniswapV4Pool()
                ) as pool,
                PoolStateMessage() as state_message,
            ) if pool in self.swap_pools and isinstance(
                state_message.state,
                (
                    AerodromeV2PoolState
                    | UniswapV2PoolState
                    | UniswapV3PoolState
                    | UniswapV4PoolState
                ),
            ):
                # Check the pool's viability at the new state
                self._pool_viability[pool] = self._pool_is_viable(
                    pool=pool,
                    state=state_message.state,
                    vector=self._swap_vectors[self.swap_pools.index(pool)],
                )

                try:
                    self._pre_calculation_check()
                except ArbitrageError:
                    return
                else:
                    self._notify_subscribers(TextMessage("Profitable state discovered."))

            case _:  # pragma: no cover
                logger.error(
                    f"Message {message} from publisher {publisher} was not handled by {self}"
                )
