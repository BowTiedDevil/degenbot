import asyncio
from collections.abc import Iterable, Mapping, Sequence
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor
from fractions import Fraction
from typing import TYPE_CHECKING, Any, TypeAlias

import eth_abi.abi
from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from scipy.optimize import OptimizeResult, minimize_scalar
from web3 import Web3

from ..aerodrome.pools import AerodromeV2Pool, AerodromeV3Pool
from ..erc20_token import Erc20Token
from ..exceptions import (
    ArbitrageError,
    DegenbotValueError,
    EVMRevertError,
    LiquidityPoolError,
    ZeroLiquidityError,
)
from ..logging import logger
from ..types import AbstractArbitrage, PlaintextMessage, Publisher, Subscriber
from ..uniswap.types import (
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
    UniswapV3PoolState,
    UniswapV3PoolStateUpdated,
)
from ..uniswap.v2_liquidity_pool import UniswapV2Pool
from ..uniswap.v3_libraries import TickMath
from ..uniswap.v3_liquidity_pool import UniswapV3Pool
from .types import (
    ArbitrageCalculationResult,
    UniswapPoolSwapVector,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
)

Pool: TypeAlias = UniswapV2Pool | UniswapV3Pool | AerodromeV2Pool | AerodromeV3Pool
PoolState: TypeAlias = UniswapV2PoolState | UniswapV3PoolState
SwapAmount: TypeAlias = UniswapV2PoolSwapAmounts | UniswapV3PoolSwapAmounts


class UniswapLpCycle(AbstractArbitrage):
    def __init__(
        self,
        input_token: Erc20Token,
        swap_pools: Iterable[Pool],
        id: str,
        max_input: int | None = None,
    ):
        for swap_pool in swap_pools:
            if not isinstance(swap_pool, Pool):
                raise DegenbotValueError(f"Incompatible pool type ({type(swap_pool)}) provided.")

        self.swap_pools = tuple(swap_pools)
        self.name = " → ".join([pool.name for pool in self.swap_pools])
        self.id = id
        self.input_token = input_token

        if max_input is None:
            logger.warning("No maximum input provided, setting to 100 WETH")
            max_input = 100 * 10**18
        elif max_input <= 0:
            raise DegenbotValueError("Maximum input must be positive.")
        self.max_input = max_input

        _swap_vectors: list[UniswapPoolSwapVector] = []
        for i, pool in enumerate(self.swap_pools):
            if i == 0:
                match self.input_token:
                    case pool.token0:
                        _swap_vectors.append(
                            UniswapPoolSwapVector(
                                token_in=pool.token0,
                                token_out=pool.token1,
                                zero_for_one=True,
                            )
                        )
                    case pool.token1:
                        _swap_vectors.append(
                            UniswapPoolSwapVector(
                                token_in=pool.token1,
                                token_out=pool.token0,
                                zero_for_one=False,
                            )
                        )
                    case _:  # pragma: no cover
                        raise DegenbotValueError("Input token could not be identified!")
            else:
                match _swap_vectors[-1].token_out:
                    case pool.token0:
                        _swap_vectors.append(
                            UniswapPoolSwapVector(
                                token_in=pool.token0,
                                token_out=pool.token1,
                                zero_for_one=True,
                            )
                        )
                    case pool.token1:
                        _swap_vectors.append(
                            UniswapPoolSwapVector(
                                token_in=pool.token1,
                                token_out=pool.token0,
                                zero_for_one=False,
                            )
                        )
                    case _:  # pragma: no cover
                        raise DegenbotValueError("Input token could not be identified!")
        self._swap_vectors = tuple(_swap_vectors)

        self._subscribers: set[Subscriber] = set()
        for pool in swap_pools:
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
        pool_state_overrides: Mapping[ChecksumAddress, PoolState],
    ) -> list[SwapAmount]:
        """
        Generate inputs for all swaps along the arbitrage path, starting with the specified amount
        of the input token defined in the constructor.
        """

        _token_out_quantity = 0
        swap_amounts: list[SwapAmount] = []
        for i, (pool, swap_vector) in enumerate(
            zip(self.swap_pools, self._swap_vectors, strict=True)
        ):
            pool_state_override = pool_state_overrides.get(pool.address)
            _token_in_quantity = token_in_quantity if i == 0 else _token_out_quantity

            try:
                match pool, pool_state_override:
                    case UniswapV2Pool(), UniswapV2PoolState() | None:
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=swap_vector.token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=pool_state_override,
                        )
                        if _token_out_quantity == 0:  # pragma: no cover
                            raise ArbitrageError(
                                f"Zero-output swap through pool {pool} @ {pool.address}"
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
                    case UniswapV3Pool(), UniswapV3PoolState() | None:
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=swap_vector.token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=pool_state_override,
                        )
                        if _token_out_quantity == 0:  # pragma: no cover
                            raise ArbitrageError(
                                f"Zero-output swap through pool {pool} @ {pool.address}"
                            )
                        swap_amounts.append(
                            UniswapV3PoolSwapAmounts(
                                pool=pool.address,
                                amount_specified=_token_in_quantity,
                                zero_for_one=swap_vector.zero_for_one,
                                sqrt_price_limit_x96=TickMath.MIN_SQRT_RATIO + 1
                                if swap_vector.zero_for_one
                                else TickMath.MAX_SQRT_RATIO - 1,
                            )
                        )
                    case _:  # pragma: no cover
                        raise DegenbotValueError("Could not identify pool and override type.")
            except LiquidityPoolError as e:  # pragma: no cover
                raise ArbitrageError(f"(calculate_tokens_out_from_tokens_in): {e}") from None

        return swap_amounts

    @staticmethod
    def _check_v2_pool_liquidity(
        pool_state: UniswapV2PoolState,
        vector: UniswapPoolSwapVector,
    ) -> None:
        if pool_state.reserves_token0 > 1 and pool_state.reserves_token1 > 1:
            return  # No liquidity issues
        elif pool_state.reserves_token0 == 0 or pool_state.reserves_token1 == 0:  # pragma: no cover
            raise ZeroLiquidityError("Pool has no liquidity")
        elif pool_state.reserves_token1 == 1 and vector.zero_for_one is True:  # pragma: no cover
            raise ZeroLiquidityError("Pool has no liquidity for a 0 -> 1 swap")
        elif pool_state.reserves_token0 == 1 and vector.zero_for_one is False:  # pragma: no cover
            raise ZeroLiquidityError("Pool has no liquidity for a 1 -> 0 swap")

    @staticmethod
    def _check_v3_pool_liquidity(
        pool_state: UniswapV3PoolState,
        vector: UniswapPoolSwapVector,
    ) -> None:
        if (
            pool_state.sqrt_price_x96 == 0
            or pool_state.tick_bitmap == {}
            or pool_state.tick_data == {}
        ):  # pragma: no cover
            raise ZeroLiquidityError("Pool is not uninitialized.")

        if pool_state.liquidity == 0:
            if (
                pool_state.sqrt_price_x96 == TickMath.MIN_SQRT_RATIO + 1
                and vector.zero_for_one is True
            ):  # pragma: no cover
                # Swap is 0 -> 1 and cannot swap any more token0 for token1
                raise ZeroLiquidityError("Pool has no liquidity for a 0 -> 1 swap")
            elif (
                pool_state.sqrt_price_x96 == TickMath.MAX_SQRT_RATIO - 1
                and vector.zero_for_one is False
            ):  # pragma: no cover
                # Swap is 1 -> 0  and cannot swap any more token1 for token0
                raise ZeroLiquidityError("Pool has no liquidity for a 1 -> 0 swap")

    def _pre_calculation_check(
        self,
        state_overrides: Mapping[ChecksumAddress, PoolState],
        min_rate_of_exchange: Fraction | None = None,
    ) -> None:
        """
        Perform liquidity and minimum rate of exchange checks and raise an exception if further
        optimization should be avoided.
        """

        if min_rate_of_exchange is None:  # pragma: no branch
            min_rate_of_exchange = Fraction(1, 1)

        # A scalar value representing the net amount of 1 input token across the complete path
        # including fees. A net rate > 1.0 indicates a profitable swap.
        net_rate_of_exchange = Fraction(1, 1)

        # Check the pool state liquidity in the direction of the trade
        for pool, vector in zip(self.swap_pools, self._swap_vectors, strict=True):
            pool_state = state_overrides.get(pool.address) or pool.state

            match pool, pool_state:
                case UniswapV2Pool(), UniswapV2PoolState():
                    self._check_v2_pool_liquidity(pool_state, vector)
                    exchange_rate = Fraction(pool_state.reserves_token1, pool_state.reserves_token0)
                    fee = pool.fee_token0 if vector.zero_for_one else pool.fee_token1
                case UniswapV3Pool(), UniswapV3PoolState():
                    self._check_v3_pool_liquidity(pool_state, vector)
                    exchange_rate = Fraction(pool_state.sqrt_price_x96**2, 2**192)
                    fee = Fraction(
                        pool.fee, 1000000
                    )  # V3 fees are in hundredths of a bip (0.0001), e.g. 3000 == 0.3%
                case _:  # pragma: no cover
                    raise DegenbotValueError(
                        f"Could not identify pool {pool} and state {pool_state}."
                    )

            net_rate_of_exchange *= (
                exchange_rate if vector.zero_for_one is True else 1 / exchange_rate
            ) * Fraction(fee.denominator - fee.numerator, fee.denominator)

        if net_rate_of_exchange < min_rate_of_exchange:
            raise ArbitrageError(
                f"No acceptable arbitrage at current rate of exchange ({float(net_rate_of_exchange)}), minimum {float(min_rate_of_exchange)}."  # noqa:E501
            )

    def _calculate(
        self,
        state_overrides: Mapping[ChecksumAddress, PoolState],
    ) -> ArbitrageCalculationResult:
        """
        Calculate the optimal arbitrage profit using the maximum input as an upper bound.
        """

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

            for i, (pool, swap_vector) in enumerate(
                zip(self.swap_pools, self._swap_vectors, strict=True)
            ):
                pool_override = state_overrides.get(pool.address)

                try:
                    match pool, pool_override:
                        case UniswapV2Pool(), UniswapV2PoolState() | None:
                            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                                token_in=swap_vector.token_in,
                                token_in_quantity=token_in_quantity
                                if i == 0
                                else token_out_quantity,
                                override_state=pool_override,
                            )
                        case UniswapV3Pool(), UniswapV3PoolState() | None:
                            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                                token_in=swap_vector.token_in,
                                token_in_quantity=token_in_quantity
                                if i == 0
                                else token_out_quantity,
                                override_state=pool_override,
                            )
                        case _:  # pragma: no cover
                            raise DegenbotValueError(
                                f"Override {pool_override} is not valid for pool {pool}."
                            )
                except (EVMRevertError, LiquidityPoolError):
                    # The optimizer might send invalid amounts into the swap
                    # calculation during iteration. We don't want it to stop,
                    # so catch the exception and pretend the swap results in
                    # token_out_quantity = 0.
                    token_out_quantity = 0
                    break

            # minimize_scalar requires the function to have a minimum value
            # for the solver to settle on an optimum input, so return the
            # negated profit
            return -float(token_out_quantity - token_in_quantity)

        opt = minimize_scalar(
            fun=arb_profit,
            method="bounded",
            bounds=bounds,
            bracket=bracket,
            options={"xatol": 1.0},
        )
        if TYPE_CHECKING:
            assert isinstance(opt, OptimizeResult)

        # Negate the result to convert to a sensible value (positive profit)
        best_profit = -int(opt.fun)
        swap_amount = int(opt.x)

        best_amounts = self._build_swap_amounts(
            token_in_quantity=swap_amount,
            pool_state_overrides=state_overrides,
        )

        return ArbitrageCalculationResult(
            id=self.id,
            input_token=self.input_token,
            profit_token=self.input_token,
            input_amount=swap_amount,
            profit_amount=best_profit,
            swap_amounts=best_amounts,
        )

    def calculate(
        self,
        state_overrides: Mapping[ChecksumAddress, PoolState] | None = None,
        min_rate_of_exchange: Fraction | None = None,
    ) -> ArbitrageCalculationResult:
        """
        Calculate the results of the arbitrage at the current pool states, or at one or more
        overridden pool states if provided.
        """

        if state_overrides is None:
            state_overrides = {}

        self._pre_calculation_check(
            state_overrides=state_overrides,
            min_rate_of_exchange=min_rate_of_exchange,
        )

        return self._calculate(state_overrides=state_overrides)

    async def calculate_with_pool(
        self,
        executor: ProcessPoolExecutor | ThreadPoolExecutor,
        state_overrides: Mapping[ChecksumAddress, PoolState] | None = None,
        min_rate_of_exchange: Fraction | None = None,
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
            The minimum net rate of exchange for the arbitrage path. Rates
            below this minimum will trigger an exception.

        Returns
        -------
        A future which returns a `ArbitrageCalculationResult` (or exception)
        when awaited.

        Notes
        -----
        This is an async function that must be called with the `await` keyword.
        """

        if any(
            [
                pool.sparse_liquidity_map
                for pool in self.swap_pools
                if isinstance(pool, UniswapV3Pool)
            ]
        ):
            raise DegenbotValueError(
                f"Cannot calculate {self} with executor. One or more V3 pools has a sparse bitmap."
            )

        if state_overrides is None:
            state_overrides = {}

        self._pre_calculation_check(
            state_overrides=state_overrides,
            min_rate_of_exchange=min_rate_of_exchange,
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
        UniswapV3PoolSwapAmounts]
            An iterable of swap amounts to be encoded.

        Returns
        -------
        ``list[(str, bytes, int)]``
            A list of payload tuples. Each payload tuple has form (
            address: ChecksumAddress, calldata: bytes, value: int).
        """

        from_address = to_checksum_address(from_address)

        MSG_VALUE = 0  # This arbitrage does not require a `msg.value` payment
        payloads = []
        for i, (swap_pool, _swap_amounts) in enumerate(
            zip(self.swap_pools, pool_swap_amounts, strict=True)
        ):
            # Special case when a Uniswap V2 pool is the next step in the path
            if i < len(self.swap_pools) - 1 and isinstance(
                (next_pool := self.swap_pools[i + 1]), UniswapV2Pool
            ):
                swap_destination_address = next_pool.address
            else:
                swap_destination_address = from_address

            match i, swap_pool, _swap_amounts:
                case 0, UniswapV2Pool() as first_pool, _:
                    # Special case: If first pool is type V2, input token must be transferred prior
                    # to the swap
                    payloads.append(
                        (
                            # address
                            self.input_token.address,
                            # bytes calldata
                            Web3.keccak(text="transfer(address,uint256)")[:4]
                            + eth_abi.abi.encode(
                                types=(
                                    "address",
                                    "uint256",
                                ),
                                args=(
                                    first_pool.address,
                                    swap_amount,
                                ),
                            ),
                            MSG_VALUE,
                        )
                    )
                case _, UniswapV2Pool(), UniswapV2PoolSwapAmounts():
                    logger.debug(f"PAYLOAD: building V2 swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: swap amounts {_swap_amounts}")
                    logger.debug(f"PAYLOAD: destination address {swap_destination_address}")

                    payloads.append(
                        (
                            # address
                            swap_pool.address,
                            # bytes calldata
                            Web3.keccak(text="swap(uint256,uint256,address,bytes)")[:4]
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
                            MSG_VALUE,
                        )
                    )
                case _, UniswapV3Pool(), UniswapV3PoolSwapAmounts():
                    logger.debug(f"PAYLOAD: building V3 swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: swap amounts {_swap_amounts}")
                    logger.debug(f"PAYLOAD: destination address {swap_destination_address}")

                    payloads.append(
                        (
                            # address
                            swap_pool.address,
                            # bytes calldata
                            Web3.keccak(text="swap(address,bool,int256,uint160,bytes)")[:4]
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
                            MSG_VALUE,
                        )
                    )
                case _:  # pragma: no cover
                    raise DegenbotValueError("Could not identify pool and swap amounts.")

        return payloads

    def notify(self, publisher: Publisher, message: Any) -> None:
        match publisher, message:
            case (
                UniswapV2Pool()
                | UniswapV3Pool(),
                UniswapV2PoolStateUpdated()
                | UniswapV3PoolStateUpdated(),
            ):
                if message.state.pool in self.swap_pools:
                    self._notify_subscribers(
                        PlaintextMessage(f"Received update from pool {message.state.pool}")
                    )
            case _:  # pragma: no cover
                logger.info(f"Unhandled message {message} from publisher {publisher}")
