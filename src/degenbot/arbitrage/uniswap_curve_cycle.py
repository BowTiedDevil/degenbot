import asyncio
from collections.abc import Awaitable, Sequence
from concurrent.futures import ProcessPoolExecutor, ThreadPoolExecutor
from fractions import Fraction
from typing import TYPE_CHECKING, Any, TypeAlias

import eth_abi.abi
from eth_typing import ChecksumAddress
from eth_utils.address import to_checksum_address
from scipy.optimize import minimize_scalar
from web3 import Web3

from ..config import get_web3
from ..constants import MAX_UINT256
from ..curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from ..curve.types import CurveStableswapPoolState, CurveStableSwapPoolStateUpdated
from ..erc20_token import Erc20Token
from ..exceptions import ArbitrageError, EVMRevertError, LiquidityPoolError, ZeroLiquidityError
from ..logging import logger
from ..types import (
    AbstractArbitrage,
    PlaintextMessage,
    Publisher,
    Subscriber,
    UniswapSimulationResult,
)
from ..uniswap.v2_liquidity_pool import LiquidityPool
from ..uniswap.v2_types import (
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)
from ..uniswap.v3_libraries import TickMath
from ..uniswap.v3_liquidity_pool import V3LiquidityPool
from ..uniswap.v3_types import (
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
    UniswapV3PoolStateUpdated,
)
from .types import (
    ArbitrageCalculationResult,
    CurveStableSwapPoolSwapAmounts,
    CurveStableSwapPoolVector,
    UniswapPoolSwapVector,
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
)

PoolStates: TypeAlias = UniswapV2PoolState | UniswapV3PoolState | CurveStableswapPoolState
PoolTypes: TypeAlias = LiquidityPool | V3LiquidityPool | CurveStableswapPool
SwapAmounts: TypeAlias = (
    CurveStableSwapPoolSwapAmounts | UniswapV2PoolSwapAmounts | UniswapV3PoolSwapAmounts
)


# Default discount applied to amount received.
# This masks small differences in get_dy() vs exchange().
CURVE_V1_DEFAULT_DISCOUNT_FACTOR = 0.9999


class UniswapCurveCycle(Subscriber, AbstractArbitrage):
    def __init__(
        self,
        input_token: Erc20Token,
        swap_pools: Sequence[PoolTypes],
        id: str,
        max_input: int | None = None,
    ):
        if any([not isinstance(pool, PoolTypes) for pool in swap_pools]):
            raise ValueError("Must provide only Curve StableSwap or Uniswap liquidity pools.")

        self.swap_pools: tuple[PoolTypes, ...] = tuple(swap_pools)
        self.name = " → ".join([pool.name for pool in self.swap_pools])

        self.curve_discount_factor = CURVE_V1_DEFAULT_DISCOUNT_FACTOR

        for pool in swap_pools:
            pool.subscribe(self)

        self.id = id
        self.input_token = input_token

        if max_input == 0:
            raise ValueError("Maximum input must be positive.")

        if max_input is None:
            logger.warning("No maximum input provided, setting to 100 WETH")
            max_input = 100 * 10**18
        self.max_input = max_input

        self.gas_estimate: int

        # Set up pre-determined "swap vectors", which allows the helper
        # to identify the tokens and direction of each swap along the path
        _swap_vectors: list[CurveStableSwapPoolVector | UniswapPoolSwapVector] = []
        for i, pool in enumerate(self.swap_pools):
            match pool:
                case LiquidityPool() | V3LiquidityPool():
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
                                raise ValueError("Input token could not be identified!")
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
                                raise ValueError("Input token could not be identified!")
                case CurveStableswapPool():
                    # A Curve pool may have 3 or more tokens, so instead of a binary
                    # token0/token1 choice, determine the forward token by comparing
                    # current and next pool
                    if i != 1:
                        raise ValueError("Not implemented for Curve pools at position != 1.")

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
                    raise ValueError("Pool type could not be identified")
        self._swap_vectors = tuple(_swap_vectors)

        self._subscribers: set[Subscriber] = set()
        for pool in swap_pools:
            pool.subscribe(self)

    def __getstate__(self) -> dict[str, Any]:
        # Remove objects that cannot be pickled and are unnecessary to perform
        # the calculation
        dropped_attributes = (
            "_subscribers",
            "gas_estimate",
        )

        return {key: value for key, value in self.__dict__.items() if key not in dropped_attributes}

    def __str__(self) -> str:
        return self.name

    @staticmethod
    def _sort_overrides(
        overrides: Sequence[
            tuple[
                PoolTypes,
                PoolStates | UniswapSimulationResult,
            ]
        ]
        | None,
    ) -> dict[ChecksumAddress, PoolStates]:
        """
        Validate the overrides, extract and insert the resulting pool states into a dictionary
        keyed by the pool address.
        """

        if overrides is None:
            return {}

        sorted_overrides: dict[ChecksumAddress, PoolStates] = {}
        for pool, override in overrides:
            match override:
                case CurveStableswapPoolState() | UniswapV2PoolState() | UniswapV3PoolState():
                    logger.debug(f"Applying override {override} to {pool}")
                    sorted_overrides[pool.address] = override
                case (
                    UniswapV2PoolSimulationResult()
                    | UniswapV3PoolSimulationResult()
                    # | CurveStableswapPoolSimulationResult, <----- todo
                ):
                    logger.debug(f"Applying override {override.final_state} to {pool}")
                    sorted_overrides[pool.address] = override.final_state
                case _:  # pragma: no cover
                    raise ValueError(f"Override for {pool} has unsupported type {type(override)}")

        return sorted_overrides

    def _build_amounts_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        pool_state_overrides: dict[ChecksumAddress, PoolStates] | None = None,
        block_number: int | None = None,
    ) -> list[SwapAmounts]:
        """
        Generate human-readable inputs for a complete swap along the arbitrage path, starting with
        `token_in_quantity` amount of `token_in`.
        """

        if pool_state_overrides is None:  # pragma: no branch
            pool_state_overrides = {}

        pools_amounts_out: list[SwapAmounts] = []

        _token_in_quantity: int = 0
        _token_out_quantity: int = 0

        for i, (pool, swap_vector) in enumerate(
            zip(self.swap_pools, self._swap_vectors, strict=True)
        ):
            match pool, swap_vector:
                case LiquidityPool() | V3LiquidityPool(), UniswapPoolSwapVector():
                    token_in = swap_vector.token_in
                    token_out = swap_vector.token_out
                    zero_for_one = swap_vector.zero_for_one
                case CurveStableswapPool(), CurveStableSwapPoolVector():
                    token_in = swap_vector.token_in
                    token_out = swap_vector.token_out
                case _:  # pragma: no cover
                    raise ValueError(f"Could not process pool {pool} and vector {swap_vector}")

            _token_in_quantity = token_in_quantity if i == 0 else _token_out_quantity

            try:
                pool_state_override = pool_state_overrides.get(pool.address)
                match pool, pool_state_override:
                    case LiquidityPool(), UniswapV2PoolState() | None:
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=pool_state_override,
                        )
                    case V3LiquidityPool(), UniswapV3PoolState() | None:
                        _token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                            token_in=token_in,
                            token_in_quantity=_token_in_quantity,
                            override_state=pool_state_override,
                        )
                    case CurveStableswapPool(), CurveStableswapPoolState() | None:
                        _token_out_quantity = int(
                            self.curve_discount_factor
                            * pool.calculate_tokens_out_from_tokens_in(
                                token_in=token_in,
                                token_out=token_out,
                                token_in_quantity=_token_in_quantity,
                                override_state=pool_state_override,
                                block_identifier=block_number,
                            )
                        )
                    case _:  # pragma: no cover
                        raise ValueError(
                            f"Could not process pool {pool} and override {pool_state_override}"
                        )
            except LiquidityPoolError as e:
                raise ArbitrageError(f"(calculate_tokens_out_from_tokens_in): {e}") from None
            else:
                if _token_out_quantity == 0:
                    raise ArbitrageError(f"Zero-output swap through pool {pool} @ {pool.address}")

            match pool:
                case LiquidityPool():
                    pools_amounts_out.append(
                        UniswapV2PoolSwapAmounts(
                            pool=pool.address,
                            amounts_in=(_token_in_quantity, 0)
                            if zero_for_one
                            else (0, _token_in_quantity),
                            amounts_out=(0, _token_out_quantity)
                            if zero_for_one
                            else (_token_out_quantity, 0),
                        )
                    )

                case V3LiquidityPool():
                    pools_amounts_out.append(
                        UniswapV3PoolSwapAmounts(
                            pool=pool.address,
                            amount_specified=_token_in_quantity,
                            zero_for_one=zero_for_one,
                            sqrt_price_limit_x96=TickMath.MIN_SQRT_RATIO + 1
                            if zero_for_one
                            else TickMath.MAX_SQRT_RATIO - 1,
                        )
                    )

                case CurveStableswapPool():
                    pools_amounts_out.append(
                        CurveStableSwapPoolSwapAmounts(
                            token_in=token_in,
                            token_in_index=pool.tokens.index(token_in),
                            token_out=token_out,
                            token_out_index=pool.tokens.index(token_out),
                            amount_in=_token_in_quantity,
                            min_amount_out=_token_out_quantity,
                            underlying=(
                                pool.is_metapool
                                and (
                                    token_in in pool.tokens_underlying
                                    or token_out in pool.tokens_underlying
                                )
                            ),
                        ),
                    )

        return pools_amounts_out

    def _pre_calculation_check(
        self,
        override_state: Sequence[
            tuple[
                PoolTypes,
                PoolStates | UniswapSimulationResult,
            ]
        ]
        | None = None,
    ) -> None:
        state_overrides = self._sort_overrides(override_state)

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
                case LiquidityPool(), UniswapV2PoolState(), UniswapPoolSwapVector():
                    if pool_state.reserves_token0 == 0 or pool_state.reserves_token1 == 0:
                        raise ZeroLiquidityError(f"V2 pool {pool.address} has no liquidity")
                    if pool_state.reserves_token1 == 1 and vector.zero_for_one:
                        raise ZeroLiquidityError(
                            f"V2 pool {pool.address} has no liquidity for a 0 -> 1 swap"
                        )
                    if pool_state.reserves_token0 == 1 and not vector.zero_for_one:
                        raise ZeroLiquidityError(
                            f"V2 pool {pool.address} has no liquidity for a 1 -> 0 swap"
                        )

                    price = pool_state.reserves_token1 / pool_state.reserves_token0
                    fee = pool.fee_token0 if vector.zero_for_one else pool.fee_token1
                    profit_factor *= (price if vector.zero_for_one else 1 / price) * (
                        (fee.denominator - fee.numerator) / fee.denominator
                    )
                case V3LiquidityPool(), UniswapV3PoolState(), UniswapPoolSwapVector():
                    if pool_state.sqrt_price_x96 == 0:
                        raise ZeroLiquidityError(
                            f"V3 pool {pool.address} has no liquidity (not initialized)"
                        )
                    if pool_state.tick_bitmap == {}:
                        raise ZeroLiquidityError(
                            f"V3 pool {pool.address} has no liquidity (empty bitmap)"
                        )
                    if pool_state.liquidity == 0:
                        # Check if the swap is 0 -> 1 and has reached the lower limit of the price
                        # range
                        if (
                            pool_state.sqrt_price_x96 == TickMath.MIN_SQRT_RATIO + 1
                            and vector.zero_for_one
                        ):
                            raise ZeroLiquidityError(
                                f"V3 pool {pool.address} has no liquidity for a 0 -> 1 swap"
                            )
                        # Check if the swap is 1 -> 0 and has reached the upper limit of the price
                        # range
                        if (
                            pool_state.sqrt_price_x96 == TickMath.MAX_SQRT_RATIO - 1
                            and not vector.zero_for_one
                        ):
                            raise ZeroLiquidityError(
                                f"V3 pool {pool.address} has no liquidity for a 1 -> 0 swap"
                            )

                    price = pool_state.sqrt_price_x96**2 / (2**192)
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
                    raise ValueError(
                        f"Could not process pool {pool}, state {pool_state}, and vector {vector}"
                    )

            # print(f"{profit_factor=}")

        if profit_factor < 1.0:
            raise ArbitrageError(
                f"No profitable arbitrage at current prices. Profit factor: {profit_factor}"
            )

    def _calculate(
        self,
        override_state: Sequence[
            tuple[
                PoolTypes,
                PoolStates | UniswapSimulationResult,
            ]
        ]
        | None = None,
        block_number: int | None = None,
    ) -> ArbitrageCalculationResult:
        state_overrides = self._sort_overrides(override_state)

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
                        case LiquidityPool(), UniswapV2PoolState() | None:
                            token_out_quantity = pool.calculate_tokens_out_from_tokens_in(
                                token_in=swap_vector.token_in,
                                token_in_quantity=token_in_quantity
                                if i == 0
                                else token_out_quantity,
                                override_state=pool_override,
                            )
                        case V3LiquidityPool(), UniswapV3PoolState() | None:
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

        # Negate the result to convert to a sensible value (positive profit)
        best_profit = -int(opt.fun)
        swap_amount = int(opt.x)

        try:
            best_amounts = self._build_amounts_out(
                token_in=self.input_token,
                token_in_quantity=swap_amount,
                pool_state_overrides=state_overrides,
                block_number=block_number,
            )
        # except (EVMRevertError, LiquidityPoolError) as e:
        except ArbitrageError as e:
            # Simulated EVM reverts inside the ported `swap` function were
            # ignored to execute the optimizer to completion. Now the optimal
            # value should be tested and raise an exception if it would
            # generate a bad payload that will revert
            raise ArbitrageError(f"No possible arbitrage: {e}") from None
        except Exception as e:
            raise ArbitrageError(f"No possible arbitrage: {e}") from e

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
        override_state: Sequence[
            tuple[
                PoolTypes,
                PoolStates | UniswapSimulationResult,
            ]
        ]
        | None = None,
    ) -> ArbitrageCalculationResult:
        """
        Calculate the optimum arbitrage input and intermediate swap values for the current pool
        states.
        """

        self._pre_calculation_check(override_state)

        return self._calculate(override_state=override_state)

    async def calculate_with_pool(
        self,
        executor: ProcessPoolExecutor | ThreadPoolExecutor,
        override_state: Sequence[
            tuple[
                PoolTypes,
                PoolStates | UniswapSimulationResult,
            ]
        ]
        | None = None,
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
        override_state : StateOverrideTypes, optional
            An sequence of tuples, representing an ordered pair of helper
            objects for Uniswap V2 / V3 pools and their overridden states.

        Returns
        -------
        A future which returns a `ArbitrageCalculationResult` (or exception)
        when awaited.

        Notes
        -----
        This is an async function that must be called with the `await` keyword.
        """

        self._pre_calculation_check(override_state)

        if isinstance(executor, ProcessPoolExecutor) and any(
            [
                pool.sparse_liquidity_map
                for pool in self.swap_pools
                if isinstance(pool, V3LiquidityPool)
            ]
        ):  # pragma: no cover
            raise ValueError(
                f"Cannot calculate {self} with executor. One or more V3 pools has a sparse liquidity map."  # noqa: E501
            )

        curve_pool = self.swap_pools[1]
        curve_swap_vector = self._swap_vectors[1]

        if TYPE_CHECKING:
            assert isinstance(curve_pool, CurveStableswapPool)
            assert isinstance(curve_swap_vector, CurveStableSwapPoolVector)

        block_number = get_web3().eth.get_block_number()

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
            override_state,
            block_number,
        )

    def generate_payloads(
        self,
        from_address: ChecksumAddress | str,
        swap_amount: int,
        pool_swap_amounts: Sequence[SwapAmounts],
        infinite_approval: bool = False,
    ) -> list[tuple[ChecksumAddress, bytes, int]]:
        """
        TBD
        """

        from_address = to_checksum_address(from_address)

        payloads = []
        msg_value: int = 0  # This arbitrage does not require a `msg.value` payment

        first_pool = self.swap_pools[0]
        last_pool = self.swap_pools[-1]

        try:
            if isinstance(first_pool, LiquidityPool):
                # Special case: If first pool is type V2, input token must be
                # transferred prior to the swap
                logger.debug(f"PAYLOAD: transferring {swap_amount} WETH to V2 pool {first_pool}")
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

            for i, (swap_pool, _swap_amounts) in enumerate(
                zip(self.swap_pools, pool_swap_amounts, strict=True)
            ):
                next_pool = None if swap_pool is last_pool else self.swap_pools[i + 1]

                if next_pool is not None:
                    # V2 pools require a pre-swap transfer, so the contract
                    # does not have to perform intermediate custody and the
                    # swap can send the tokens directly to the next pool
                    if isinstance(next_pool, LiquidityPool):
                        swap_destination_address = next_pool.address
                    # V3 pools cannot accept a pre-swap transfer, so the contract
                    # must maintain custody prior to a swap
                    elif isinstance(next_pool, V3LiquidityPool | CurveStableswapPool):
                        swap_destination_address = from_address
                else:
                    # Set the destination address for the last swap to the
                    # sending address
                    swap_destination_address = from_address

                match swap_pool, _swap_amounts:
                    case LiquidityPool(), UniswapV2PoolSwapAmounts():
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
                    case V3LiquidityPool(), UniswapV3PoolSwapAmounts():
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
                    case CurveStableswapPool(), CurveStableSwapPoolSwapAmounts():
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
                        elif (
                            infinite_approval is False
                            and current_approval < _swap_amounts.amount_in
                        ):
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
                            f"PAYLOAD: exchange {_swap_amounts.amount_in} "
                            f"{_swap_amounts.token_in_index}->{_swap_amounts.token_out_index}, "
                            f"min out = {_swap_amounts.min_amount_out}"
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
                        if next_pool is not None and isinstance(next_pool, LiquidityPool):
                            # V2 pools require a pre-swap transfer, so the contract
                            # does not have to perform intermediate custody and the
                            # swap can send the tokens directly to the next pool
                            logger.debug(
                                f"PAYLOAD: transferring {_swap_amounts.min_amount_out} "
                                f"{_swap_amounts.token_out} to V2 pool {next_pool}"
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
                        raise ValueError(
                            f"Could not identify pool {swap_pool} and amounts {_swap_amounts}"
                        )

        except Exception as e:
            logger.exception("generate_payloads catch-all")
            raise ArbitrageError(f"generate_payloads (catch-all)): {e}") from e

        return payloads

    def notify(self, publisher: Publisher, message: Any) -> None:
        match publisher, message:
            case (
                LiquidityPool()
                | V3LiquidityPool()
                | CurveStableswapPool(),
                UniswapV2PoolStateUpdated()
                | UniswapV3PoolStateUpdated()
                | CurveStableSwapPoolStateUpdated(),
            ):
                if message.state.pool in self.swap_pools:
                    self._notify_subscribers(
                        PlaintextMessage(f"Received update from pool {message.state.pool}")
                    )
            case _:  # pragma: no cover
                logger.info(f"Unhandled message {message} from publisher {publisher}")
