import asyncio
from collections.abc import Awaitable, Iterable, Mapping, Sequence
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor
from fractions import Fraction
from typing import TYPE_CHECKING, Any
from weakref import WeakSet

import eth_abi.abi
from eth_typing import ChecksumAddress
from scipy.optimize import OptimizeResult, minimize_scalar
from web3 import Web3

from degenbot.arbitrage.types import (
    ArbitrageCalculationResult,
    CurveStableSwapPoolSwapAmounts,
    CurveStableSwapPoolVector,
    UniswapPoolSwapVector,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
)
from degenbot.cache import get_checksum_address
from degenbot.config import connection_manager
from degenbot.constants import MAX_UINT256
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.curve.types import CurveStableswapPoolState
from degenbot.erc20_token import Erc20Token
from degenbot.exceptions import (
    ArbitrageError,
    DegenbotValueError,
    EVMRevertError,
    LiquidityPoolError,
    NoLiquidity,
)
from degenbot.logging import logger
from degenbot.types import (
    AbstractArbitrage,
    AbstractLiquidityPool,
    Message,
    PoolStateMessage,
    Publisher,
    PublisherMixin,
    Subscriber,
    TextMessage,
)
from degenbot.uniswap.types import UniswapV2PoolState, UniswapV3PoolState
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_libraries.tick_math import MAX_SQRT_RATIO, MIN_SQRT_RATIO
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

type CurveOrUniswapPoolState = UniswapV2PoolState | UniswapV3PoolState | CurveStableswapPoolState
type CurveOrUniswapPool = UniswapV2Pool | UniswapV3Pool | CurveStableswapPool
type CurveOrUniswapSwapAmount = (
    CurveStableSwapPoolSwapAmounts | UniswapV2PoolSwapAmounts | UniswapV3PoolSwapAmounts
)


# Default discount applied to amount received.
# This masks small differences in get_dy() vs exchange().
CURVE_V1_DEFAULT_DISCOUNT_FACTOR = 0.9999


class UniswapCurveCycle(PublisherMixin, AbstractArbitrage):
    def _notify_subscribers(self: Publisher, message: Message) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)

    def __init__(
        self,
        input_token: Erc20Token,
        swap_pools: Iterable[CurveOrUniswapPool],
        id: str,  # noqa:A002
        max_input: int | None = None,
    ):
        for swap_pool in swap_pools:
            if not isinstance(swap_pool, CurveOrUniswapPool.__value__):
                raise DegenbotValueError(
                    message=f"Incompatible pool type ({type(swap_pool)}) provided."
                )

        self.swap_pools: tuple[CurveOrUniswapPool, ...] = tuple(swap_pools)
        self.name = " → ".join([pool.name for pool in self.swap_pools])

        self.id = id
        self.input_token = input_token
        self.curve_discount_factor = CURVE_V1_DEFAULT_DISCOUNT_FACTOR

        if max_input == 0:
            raise DegenbotValueError(message="Maximum input must be positive.")

        if max_input is None:
            logger.warning("No maximum input provided, setting to 100 WETH")
            max_input = 100 * 10**18
        self.max_input = max_input

        # Set up pre-determined "swap vectors", which allows the helper
        # to identify the tokens and direction of each swap along the path
        _swap_vectors: list[CurveStableSwapPoolVector | UniswapPoolSwapVector] = []
        for i, pool in enumerate(self.swap_pools):
            match pool:
                case UniswapV2Pool() | UniswapV3Pool():
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
                                raise DegenbotValueError(
                                    message="Input token could not be identified!"
                                )
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
                                raise DegenbotValueError(
                                    message="Input token could not be identified!"
                                )
                case CurveStableswapPool():
                    # A Curve pool may have 3 or more tokens, so instead of a binary
                    # token0/token1 choice, determine the forward token by comparing
                    # current and next pool
                    if i != 1:
                        raise DegenbotValueError(
                            message="Not implemented for Curve pools at position != 1."
                        )

                    token_in = _swap_vectors[-1].token_out
                    next_pool = self.swap_pools[i + 1]
                    shared_tokens = list(
                        set(pool.tokens).intersection(next_pool.tokens),
                    )
                    assert len(shared_tokens) > 0, f"this: {pool.tokens}, next: {next_pool.tokens}"

                    # @dev this assumes the first shared token is the correct one to continue
                    token_out = shared_tokens[0]

                    _swap_vectors.append(
                        CurveStableSwapPoolVector(token_in=token_in, token_out=token_out)
                    )
                case _:  # pragma: no cover
                    raise DegenbotValueError(message="Pool type could not be identified")
        self._swap_vectors = tuple(_swap_vectors)

        self._subscribers: WeakSet[Subscriber] = WeakSet()
        for pool in self.swap_pools:
            pool.subscribe(self)

    def __getstate__(self) -> dict[str, Any]:
        # Remove objects that cannot be pickled and are unnecessary to perform
        # the calculation
        dropped_attributes = ("_subscribers",)

        return {key: value for key, value in self.__dict__.items() if key not in dropped_attributes}

    def __str__(self) -> str:
        return self.name

    def _build_swap_amounts(
        self,
        token_in_quantity: int,
        state_overrides: Mapping[ChecksumAddress, CurveOrUniswapPoolState],
        block_number: int | None = None,
    ) -> list[CurveOrUniswapSwapAmount]:
        """
        Generate inputs for all swaps along the arbitrage path, starting with the specified amount
        of the input token defined in the constructor.
        """

        _token_out_quantity = 0
        swap_amounts: list[CurveOrUniswapSwapAmount] = []
        for i, (pool, swap_vector) in enumerate(
            zip(self.swap_pools, self._swap_vectors, strict=True)
        ):
            pool_state_override = state_overrides.get(pool.address)
            _token_in_quantity = token_in_quantity if i == 0 else _token_out_quantity

            try:
                match pool, pool_state_override, swap_vector:
                    case UniswapV2Pool(), UniswapV2PoolState() | None, UniswapPoolSwapVector():
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=swap_vector.token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=pool_state_override,
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
                    case UniswapV3Pool(), UniswapV3PoolState() | None, UniswapPoolSwapVector():
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=swap_vector.token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=pool_state_override,
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
                    case (
                        CurveStableswapPool(),
                        CurveStableswapPoolState() | None,
                        CurveStableSwapPoolVector(),
                    ):
                        _token_out_quantity = int(
                            self.curve_discount_factor
                            * pool.calculate_tokens_out_from_tokens_in(
                                token_in=swap_vector.token_in,
                                token_out=swap_vector.token_out,
                                token_in_quantity=_token_in_quantity,
                                override_state=pool_state_override,
                                block_identifier=block_number,
                            )
                        )
                        if _token_out_quantity == 0:  # pragma: no cover
                            raise ArbitrageError(
                                message=f"Zero-output swap through pool {pool} @ {pool.address}"
                            )
                        swap_amounts.append(
                            CurveStableSwapPoolSwapAmounts(
                                token_in=swap_vector.token_in,
                                token_in_index=pool.tokens.index(swap_vector.token_in),
                                token_out=swap_vector.token_out,
                                token_out_index=pool.tokens.index(swap_vector.token_out),
                                amount_in=_token_in_quantity,
                                min_amount_out=_token_out_quantity,
                                underlying=(
                                    pool.base_pool is not None
                                    and (
                                        swap_vector.token_in in pool.tokens_underlying
                                        or swap_vector.token_out in pool.tokens_underlying
                                    )
                                ),
                            ),
                        )
                    case _:  # pragma: no cover
                        raise DegenbotValueError(
                            message=f"Could not process pool {pool} and override {pool_state_override}"  # noqa:E501
                        )
            except LiquidityPoolError as exc:  # pragma: no cover
                raise ArbitrageError(message=str(exc)) from exc

        return swap_amounts

    def _pre_calculation_check(
        self,
        state_overrides: Mapping[ChecksumAddress, CurveOrUniswapPoolState],
    ) -> None:
        """
        Perform liquidity and minimum rate of exchange checks and raise an exception if further
        optimization should be avoided.
        """

        # A scalar value representing the net amount of 1 input token across
        # the complete path (including fees).
        # profit_factor > 1.0 indicates a profitable trade.
        profit_factor: float = 1.0

        # Check each pool for liquidity in the direction of the trade and account for its current
        # price and fee. The prices are absolute (not decimal-corrected) since the decimals for
        # intermediate tokens cancel out.
        # e.g. for a WETH -> USDC -> USDT -> WETH arbitrage,
        # profit factor:
        #   [input: WETH] -> [pool0: USDC/WETH]
        #   * [pool1: USDT/USDC]
        #   * [pool2: WETH/USDT] == [output: WETH]
        for pool, vector in zip(self.swap_pools, self._swap_vectors, strict=True):
            pool_state = state_overrides.get(pool.address) or pool.state

            match pool, pool_state, vector:
                case UniswapV2Pool(), UniswapV2PoolState(), UniswapPoolSwapVector():
                    if pool_state.reserves_token0 == 0 or pool_state.reserves_token1 == 0:
                        raise NoLiquidity(message=f"V2 pool {pool.address} has no liquidity")
                    if pool_state.reserves_token1 == 1 and vector.zero_for_one:
                        raise NoLiquidity(
                            message=f"V2 pool {pool.address} has no liquidity for a 0 -> 1 swap"
                        )
                    if pool_state.reserves_token0 == 1 and not vector.zero_for_one:
                        raise NoLiquidity(
                            message=f"V2 pool {pool.address} has no liquidity for a 1 -> 0 swap"
                        )

                    price = pool_state.reserves_token1 / pool_state.reserves_token0
                    fee = pool.fee_token0 if vector.zero_for_one else pool.fee_token1
                    profit_factor *= (price if vector.zero_for_one else 1 / price) * (
                        (fee.denominator - fee.numerator) / fee.denominator
                    )
                case UniswapV3Pool(), UniswapV3PoolState(), UniswapPoolSwapVector():
                    if pool_state.sqrt_price_x96 == 0:
                        raise NoLiquidity(
                            message=f"V3 pool {pool.address} has no liquidity (not initialized)"
                        )
                    if pool_state.tick_bitmap == {}:
                        raise NoLiquidity(
                            message=f"V3 pool {pool.address} has no liquidity (empty bitmap)"
                        )
                    if pool_state.liquidity == 0:  # pragma: no cover
                        # Check if the swap is 0 -> 1 and has reached the lower limit of the price
                        # range
                        if pool_state.sqrt_price_x96 == MIN_SQRT_RATIO + 1 and vector.zero_for_one:
                            raise NoLiquidity(
                                message=f"V3 pool {pool.address} has no liquidity for a 0 -> 1 swap"
                            )
                        # Check if the swap is 1 -> 0 and has reached the upper limit of the price
                        # range
                        if (
                            pool_state.sqrt_price_x96 == MAX_SQRT_RATIO - 1
                            and not vector.zero_for_one
                        ):
                            raise NoLiquidity(
                                message=f"V3 pool {pool.address} has no liquidity for a 1 -> 0 swap"
                            )

                    price = (pool_state.sqrt_price_x96 * pool_state.sqrt_price_x96) / (
                        6277101735386680763835789423207666416102355444464034512896  # 2**192
                    )
                    # V3 fees are integer values representing hundredths of a bip (0.0001)
                    # e.g. fee=3000 represents 0.3%
                    fee = Fraction(pool.fee, 1000000)
                    profit_factor *= (price if vector.zero_for_one else 1 / price) * (
                        (fee.denominator - fee.numerator) / fee.denominator
                    )
                case CurveStableswapPool(), _, _:
                    price = 1.0 * (10**vector.token_out.decimals) / (10**vector.token_in.decimals)
                    fee = Fraction(pool.fee, pool.FEE_DENOMINATOR)
                    profit_factor *= price * ((fee.denominator - fee.numerator) / fee.denominator)
                case _:  # pragma: no cover
                    raise DegenbotValueError(
                        message=f"Could not process pool {pool}, state {pool_state}, and vector {vector}"  # noqa:E501
                    )

        if profit_factor < 1.0:
            raise ArbitrageError(
                message=f"No profitable arbitrage at current prices. Profit factor: {profit_factor}"
            )

    def _calculate(
        self,
        state_overrides: Mapping[ChecksumAddress, CurveOrUniswapPoolState],
        block_number: int | None = None,
    ) -> ArbitrageCalculationResult:
        """
        Calculate the optimal arbitrage profit using the maximum input as an upper bound.
        """

        # bound the amount to be swapped
        bounds = (
            1.0,
            max(2.0, float(self.max_input)),
        )

        # bracket the initial guess for the algo
        bracket_amount = self.max_input
        bracket = (
            0.45 * bracket_amount,
            0.50 * bracket_amount,
            0.55 * bracket_amount,
        )

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

                        case CurveStableswapPool(), CurveStableswapPoolState() | None:
                            token_out_quantity = int(
                                self.curve_discount_factor
                                * pool.calculate_tokens_out_from_tokens_in(
                                    token_in=swap_vector.token_in,
                                    token_in_quantity=(
                                        token_in_quantity if i == 0 else token_out_quantity
                                    ),
                                    token_out=swap_vector.token_out,
                                    override_state=pool_override,
                                    block_identifier=block_number,
                                )
                            )

                except (EVMRevertError, LiquidityPoolError):
                    # The optimizer might send invalid amounts into the swap calculation during
                    # iteration. We don't want it to stop, so catch the exception and pretend the
                    # swap resulted in zero output
                    token_out_quantity = 0
                    break

            # minimize_scalar requires the function to have a minimum value
            # for the solver to settle on an optimum input, so return the
            # negated profit
            return -float(token_out_quantity - token_in_quantity)

        opt: OptimizeResult = minimize_scalar(
            fun=arb_profit,
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
            block_number=block_number,
        )

        newest_state_block = (
            None
            if state_overrides
            else max([block for pool in self.swap_pools if (block := pool.state.block) is not None])
        )

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
        state_overrides: Mapping[ChecksumAddress, CurveOrUniswapPoolState] | None = None,
    ) -> ArbitrageCalculationResult:
        """
        Calculate the results of the arbitrage at the current pool states, or at one or more
        overridden pool states if provided.
        """

        if state_overrides is None:
            state_overrides = {}

        self._pre_calculation_check(state_overrides=state_overrides)

        return self._calculate(state_overrides=state_overrides)

    async def calculate_with_pool(
        self,
        executor: ProcessPoolExecutor | ThreadPoolExecutor,
        state_overrides: Mapping[ChecksumAddress, CurveOrUniswapPoolState] | None = None,
    ) -> Awaitable[Any]:
        """
        Wrap the arbitrage calculation into an asyncio future using the
        specified executor.

        Arguments
        ---------
        executor : Executor
            An executor (from `concurrent.futures`) to process the calculation
            work. Both `ThreadPoolExecutor` and `ProcessPoolExecutor` are
            supported, but `ProcessPoolExecutor` is recommended.
        state_overrides : Mapping[ChecksumAddress, StateOverrideTypes], optional
            A mapping (dict or dict-like) of pool states, keyed by the pool address.

        Returns
        -------
        A future which returns a `ArbitrageCalculationResult` (or exception)
        when awaited.

        Notes
        -----
        This is an async function that must be called with the `await` keyword.
        """

        if state_overrides is None:
            state_overrides = {}

        self._pre_calculation_check(state_overrides=state_overrides)

        if isinstance(executor, ProcessPoolExecutor) and any(
            pool.sparse_liquidity_map for pool in self.swap_pools if isinstance(pool, UniswapV3Pool)
        ):  # pragma: no cover
            raise DegenbotValueError(
                message=f"Cannot calculate {self} with executor. One or more V3 pools has a sparse liquidity map."  # noqa: E501
            )

        curve_pool = self.swap_pools[1]
        curve_swap_vector = self._swap_vectors[1]

        if TYPE_CHECKING:
            assert isinstance(curve_pool, CurveStableswapPool)
            assert isinstance(curve_swap_vector, CurveStableSwapPoolVector)

        block_number = connection_manager.get_web3(curve_pool.chain_id).eth.get_block_number()

        # Some Curve pools utilize on-chain lookups in their calc, so do a simple pre-calc to
        # cache those values for a given block since the pool will be disconnected once sent
        # into the process pool, e.g. it will have no web3 object for communication with the chain
        curve_pool.calculate_tokens_out_from_tokens_in(
            token_in=curve_swap_vector.token_in,
            token_in_quantity=1,
            token_out=curve_swap_vector.token_out,
            block_identifier=block_number,
        )

        return asyncio.get_running_loop().run_in_executor(
            executor,
            self._calculate,
            state_overrides,
            block_number,
        )

    def generate_payloads(
        self,
        from_address: ChecksumAddress | str,
        swap_amount: int,
        pool_swap_amounts: Sequence[CurveOrUniswapSwapAmount],
        infinite_approval: bool = False,
    ) -> list[tuple[ChecksumAddress, bytes, int]]:
        """
        Generate a list of tuple-formatted payloads for each step in the swap path.

        Calldata is built using the eth_abi.encode method and the ABI for the
        swap functions at each pool. Curve V1 and Uniswap V2/V3 pools are supported.

        Arguments
        ---------
        from_address: str
            The address that will execute the calldata. Must be a smart contract implementing the
            required callbacks specific to the pool.

        swap_amount: int
            The initial amount of `token_in` to swap through the first pool.

        pool_swap_amounts: Sequence[CurveOrUniswapSwapAmount]
            An ordered sequence of swap amounts.

        infinite_approval: bool
            Whether the infinite approval should be requested for the Curve pool. If False, only
            the minimum required amount will be approved.

        Returns
        -------
        ``list[(str, bytes, int)]``
            A list of payload tuples. Each payload tuple has form
            (
                address: ChecksumAddress,
                calldata: bytes,
                value: int
            ).
        """

        from_address = get_checksum_address(from_address)

        msg_value = 0  # This arbitrage does not require a `msg.value` payment
        payloads = []

        for i, (swap_pool, _swap_amounts) in enumerate(
            zip(self.swap_pools, pool_swap_amounts, strict=True)
        ):
            try:
                next_pool = self.swap_pools[i + 1]
            except IndexError:
                next_pool = None

            match next_pool:
                case UniswapV2Pool():
                    # V2 pools require a pre-swap transfer, so the contract does not have to
                    # perform intermediate custody and the swap can send the tokens directly to the
                    # next pool
                    swap_destination_address = next_pool.address
                case UniswapV3Pool() | CurveStableswapPool():
                    # V3 and Curve pools do not accept a pre-swap transfer, so the contract must
                    # maintain custody prior to a swap
                    swap_destination_address = from_address
                case None:
                    # Set the destination address for the last swap to the sending address
                    swap_destination_address = from_address
                case _:
                    raise DegenbotValueError(message=f"Unknown pool type {next_pool}")

            match i, swap_pool, _swap_amounts:
                case 0, UniswapV2Pool() as first_pool, _:
                    # Special case: If first pool is type V2, input token must be transferred prior
                    # to the swap
                    logger.debug(
                        f"PAYLOAD: transferring {swap_amount} WETH to V2 pool {first_pool}"
                    )
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
                            msg_value,
                        )
                    )
                case _, UniswapV2Pool(), UniswapV2PoolSwapAmounts():
                    if _swap_amounts.amounts_out[0] == 0:
                        _token_in = swap_pool.token0
                        _token_out = swap_pool.token1
                        _amount_out = _swap_amounts.amounts_out[1]
                    else:
                        _token_in = swap_pool.token1
                        _token_out = swap_pool.token0
                        _amount_out = _swap_amounts.amounts_out[0]
                    logger.debug(f"PAYLOAD: building V2 swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: swap {_token_in} -> {_amount_out} {_token_out} ")
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
                            msg_value,
                        )
                    )
                case _, UniswapV3Pool(), UniswapV3PoolSwapAmounts():
                    if _swap_amounts.zero_for_one:
                        _token_in = swap_pool.token0
                        _token_out = swap_pool.token1
                    else:
                        _token_in = swap_pool.token1
                        _token_out = swap_pool.token0
                    logger.debug(f"PAYLOAD: building V3 swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: token in  = {_token_in}")
                    logger.debug(f"PAYLOAD: token out = {_token_out}")
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
                            msg_value,
                        )
                    )
                case _, CurveStableswapPool(), CurveStableSwapPoolSwapAmounts():
                    logger.debug(f"PAYLOAD: building Curve swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: swap amounts {_swap_amounts}")
                    logger.debug(f"PAYLOAD: token in  = {_swap_amounts.token_in}")
                    logger.debug(f"PAYLOAD: token out = {_swap_amounts.token_out}")
                    logger.debug(f"PAYLOAD: destination address {swap_destination_address}")

                    current_approval = _swap_amounts.token_in.get_approval(
                        from_address, swap_pool.address
                    )
                    amount_to_approve: int | None = None
                    if infinite_approval is True and current_approval != MAX_UINT256:
                        amount_to_approve = MAX_UINT256
                    elif infinite_approval is False and current_approval < _swap_amounts.amount_in:
                        amount_to_approve = _swap_amounts.amount_in

                    if amount_to_approve is not None:
                        logger.debug(
                            f"PAYLOAD: approve {amount_to_approve} {_swap_amounts.token_in} by "
                            f"{swap_pool} {swap_destination_address}"
                        )
                        payloads.append(
                            (
                                # address
                                _swap_amounts.token_in.address,
                                # bytes calldata
                                Web3.keccak(text="approve(address,uint256)")[:4]
                                + eth_abi.abi.encode(
                                    types=["address", "uint256"],
                                    args=[swap_pool.address, amount_to_approve],
                                ),
                                msg_value,
                            )
                        )

                    logger.debug(
                        f"PAYLOAD: exchange {_swap_amounts.amount_in} {_swap_amounts.token_in_index}->{_swap_amounts.token_out_index}, min out = {_swap_amounts.min_amount_out}"  # noqa: E501
                    )
                    if _swap_amounts.underlying:
                        payloads.append(
                            (
                                # address
                                swap_pool.address,
                                # bytes calldata
                                Web3.keccak(
                                    text="exchange_underlying(int128,int128,uint256,uint256)"
                                )[:4]
                                + eth_abi.abi.encode(
                                    types=["int128", "int128", "uint256", "uint256"],
                                    args=[
                                        _swap_amounts.token_in_index,
                                        _swap_amounts.token_out_index,
                                        _swap_amounts.amount_in,
                                        _swap_amounts.min_amount_out,
                                    ],
                                ),
                                msg_value,
                            )
                        )
                    else:
                        payloads.append(
                            (
                                # address
                                swap_pool.address,
                                # bytes calldata
                                Web3.keccak(text="exchange(int128,int128,uint256,uint256)")[:4]
                                + eth_abi.abi.encode(
                                    types=["int128", "int128", "uint256", "uint256"],
                                    args=[
                                        _swap_amounts.token_in_index,
                                        _swap_amounts.token_out_index,
                                        _swap_amounts.amount_in,
                                        _swap_amounts.min_amount_out,
                                    ],
                                ),
                                msg_value,
                            )
                        )
                    if isinstance(next_pool, UniswapV2Pool):
                        logger.debug(
                            f"PAYLOAD: transferring {_swap_amounts.min_amount_out} {_swap_amounts.token_out} to V2 pool {next_pool}"  # noqa: E501
                        )
                        payloads.append(
                            (
                                # address
                                _swap_amounts.token_out.address,
                                # bytes calldata
                                Web3.keccak(text="transfer(address,uint256)")[:4]
                                + eth_abi.abi.encode(
                                    types=(
                                        "address",
                                        "uint256",
                                    ),
                                    args=(
                                        next_pool.address,
                                        _swap_amounts.min_amount_out,
                                    ),
                                ),
                                msg_value,
                            )
                        )
                case _:  # pragma: no cover
                    raise DegenbotValueError(
                        message=f"Could not identify pool {swap_pool} and amounts {_swap_amounts}"
                    )

        return payloads

    def notify(self, publisher: Publisher, message: Any) -> None:
        match publisher, message:
            case (
                AbstractLiquidityPool(),
                PoolStateMessage(),
            ) if publisher in self.swap_pools:
                self._notify_subscribers(TextMessage(f"Received pool update from {publisher}"))
            case _:  # pragma: no cover
                logger.info(f"Unhandled message {message} from publisher {publisher}")
