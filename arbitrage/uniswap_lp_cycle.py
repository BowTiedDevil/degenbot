import asyncio
import dataclasses
from typing import (
    TYPE_CHECKING,
    Dict,
    List,
    Optional,
    Sequence,
    Tuple,
    TypeAlias,
    Union,
)
from warnings import warn

import eth_abi
from eth_typing import ChecksumAddress
from scipy.optimize import minimize_scalar  # type: ignore
from web3 import Web3

from degenbot.exceptions import (
    ArbitrageError,
    EVMRevertError,
    LiquidityPoolError,
    ZeroLiquidityError,
)
from degenbot.logging import logger
from degenbot.token import Erc20Token
from degenbot.types import ArbitrageHelper
from degenbot.uniswap.v2.liquidity_pool import (
    CamelotLiquidityPool,
    LiquidityPool,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
)
from degenbot.uniswap.v3 import V3LiquidityPool
from degenbot.uniswap.v3.libraries import TickMath
from degenbot.uniswap.v3.v3_liquidity_pool import (
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
)

StateOverrideTypes: TypeAlias = Sequence[
    Union[
        Tuple[LiquidityPool, UniswapV2PoolState],
        Tuple[LiquidityPool, UniswapV2PoolSimulationResult],
        Tuple[V3LiquidityPool, UniswapV3PoolState],
        Tuple[V3LiquidityPool, UniswapV3PoolSimulationResult],
    ]
]


@dataclasses.dataclass(slots=True, frozen=True)
class ArbitrageCalculationResult:
    id: str
    input_token: Erc20Token
    profit_token: Erc20Token
    input_amount: int
    profit_amount: int
    swap_amounts: List


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapPoolSwapVector:
    token_in: Erc20Token
    token_out: Erc20Token
    zero_for_one: bool


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV2PoolSwapAmounts:
    amounts: Tuple[int, int]


@dataclasses.dataclass(slots=True, frozen=True)
class UniswapV3PoolSwapAmounts:
    amount_specified: int
    zero_for_one: bool
    sqrt_price_limit_x96: int


class UniswapLpCycle(ArbitrageHelper):
    def __init__(
        self,
        input_token: Erc20Token,
        swap_pools: List[Union[LiquidityPool, V3LiquidityPool]],
        id: str,
        max_input: Optional[int] = None,
    ):
        self.id = id
        self.input_token = input_token

        if max_input is None:
            warn("No maximum input provided, setting to 100 WETH")
            max_input = 100 * 10**18
        self.max_input = max_input

        self.gas_estimate = 0

        for pool in swap_pools:
            if not isinstance(pool, (LiquidityPool, V3LiquidityPool)):
                raise ArbitrageError(
                    f"Could not identify Uniswap version for pool {pool}!"
                )
        self.swap_pools = swap_pools
        self.swap_pool_addresses = [pool.address for pool in self.swap_pools]
        self.swap_pool_tokens = [
            [pool.token0, pool.token1] for pool in self.swap_pools
        ]

        # set up a pre-determined list of "swap vectors", which allows the helper
        # to identify the tokens and direction of each swap along the path
        self._swap_vectors: List[UniswapPoolSwapVector] = []
        for i, pool in enumerate(self.swap_pools):
            if i == 0:
                if self.input_token == pool.token0:
                    token_in = pool.token0
                    token_out = pool.token1
                    zero_for_one = True
                elif self.input_token == pool.token1:
                    token_in = pool.token1
                    token_out = pool.token0
                    zero_for_one = False
                else:
                    raise ValueError("Input token could not be identified!")
            else:
                # token_out references the output from the previous pool
                if token_out == pool.token0:
                    token_in = pool.token0
                    token_out = pool.token1
                    zero_for_one = True
                elif token_out == pool.token1:
                    token_in = pool.token1
                    token_out = pool.token0
                    zero_for_one = False
                else:
                    raise ValueError("Input token could not be identified!")
            self._swap_vectors.append(
                UniswapPoolSwapVector(
                    token_in=token_in,
                    token_out=token_out,
                    zero_for_one=zero_for_one,
                )
            )

        self.name = " -> ".join([pool.name for pool in self.swap_pools])

        self.pool_states: Dict[
            ChecksumAddress,
            Optional[
                Union[
                    UniswapV2PoolState,
                    UniswapV3PoolState,
                ]
            ],
        ] = {pool.address: None for pool in self.swap_pools}

        self.best: dict = {
            "input_token": self.input_token,
            "last_swap_amount": 0,
            "profit_amount": 0,
            "profit_token": self.input_token,
            "strategy": "cycle",
            "swap_amount": 0,
            "swap_pools": self.swap_pools,
            "swap_pool_addresses": self.swap_pool_addresses,
            "swap_pool_amounts": [],
            "swap_pool_tokens": self.swap_pool_tokens,
        }

    def __str__(self) -> str:
        return self.name

    def _sort_overrides(
        self,
        overrides: Optional[StateOverrideTypes],
    ) -> Dict[ChecksumAddress, Union[UniswapV2PoolState, UniswapV3PoolState]]:
        """
        Validate the overrides, extract and insert the resulting pool states
        into a dictionary.
        """

        if overrides is None:
            return {}

        sorted_overrides: Dict[
            ChecksumAddress, Union[UniswapV2PoolState, UniswapV3PoolState]
        ] = {}

        for pool, override in overrides:
            if isinstance(
                override,
                (
                    UniswapV2PoolState,
                    UniswapV3PoolState,
                ),
            ):
                logger.debug(f"Applying override {override} to {pool}")
                sorted_overrides[pool.address] = override
            elif isinstance(
                override,
                (
                    UniswapV2PoolSimulationResult,
                    UniswapV3PoolSimulationResult,
                ),
            ):
                logger.debug(
                    f"Applying override {override.future_state} to {pool}"
                )
                sorted_overrides[pool.address] = override.future_state
            else:
                raise ValueError(
                    f"Override for {pool} has unsupported type {type(override)}"
                )

        return sorted_overrides

    def _build_amounts_out(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        pool_state_overrides: Optional[
            Dict[
                ChecksumAddress, Union[UniswapV2PoolState, UniswapV3PoolState]
            ]
        ] = None,
    ) -> List[Union[UniswapV2PoolSwapAmounts, UniswapV3PoolSwapAmounts]]:
        """
        Generate human-readable inputs for a complete swap along the arbitrage
        path, starting with `token_in_quantity` amount of `token_in`.
        """

        if pool_state_overrides is None:
            pool_state_overrides = {}

        pools_amounts_out: List[
            Union[UniswapV2PoolSwapAmounts, UniswapV3PoolSwapAmounts]
        ] = []

        for i, (pool, pool_vector) in enumerate(
            zip(self.swap_pools, self._swap_vectors)
        ):
            token_in = pool_vector.token_in
            token_out = pool_vector.token_out
            zero_for_one = pool_vector.zero_for_one

            try:
                token_out_quantity: int
                token_in_remainder: int
                # calculate the swap output through the pool
                if isinstance(pool, LiquidityPool):
                    pool_state_override = pool_state_overrides.get(
                        pool.address
                    )
                    if TYPE_CHECKING:
                        assert pool_state_override is None or isinstance(
                            pool_state_override,
                            UniswapV2PoolState,
                        )
                    token_out_quantity = (
                        pool.calculate_tokens_out_from_tokens_in(
                            token_in=token_in,
                            token_in_quantity=token_in_quantity
                            if i == 0
                            else token_out_quantity,
                            override_state=pool_state_override,
                        )
                    )
                elif isinstance(pool, V3LiquidityPool):
                    pool_state_override = pool_state_overrides.get(
                        pool.address
                    )
                    if TYPE_CHECKING:
                        assert pool_state_override is None or isinstance(
                            pool_state_override,
                            UniswapV3PoolState,
                        )
                    (
                        token_out_quantity,
                        token_in_remainder,
                    ) = pool.calculate_tokens_out_from_tokens_in(
                        token_in=token_in,
                        token_in_quantity=token_in_quantity
                        if i == 0
                        else token_out_quantity,
                        override_state=pool_state_override,
                        with_remainder=True,
                    )
                else:
                    raise ValueError(
                        f"Could not determine Uniswap version for pool {pool}"
                    )
            except LiquidityPoolError as e:
                raise ArbitrageError(
                    f"(calculate_tokens_out_from_tokens_in): {e}"
                )
            else:
                if token_out_quantity == 0:
                    raise ArbitrageError(
                        f"Zero-output swap through pool {pool} @ {pool.address}"
                    )
                # if token_in_remainder:
                #     print(
                #         f"swap through {pool} resulted in remainder: {token_in_quantity=}, {token_in_remainder=}, {token_out_quantity=}"
                #     )

            # determine the uniswap version for the pool and format the output appropriately
            if isinstance(pool, LiquidityPool):
                pools_amounts_out.append(
                    UniswapV2PoolSwapAmounts(
                        amounts=(0, token_out_quantity)
                        if zero_for_one
                        else (token_out_quantity, 0),
                    )
                )
            elif isinstance(pool, V3LiquidityPool):
                pools_amounts_out.append(
                    UniswapV3PoolSwapAmounts(
                        amount_specified=token_in_quantity
                        if i == 0
                        else -token_out_quantity,
                        zero_for_one=zero_for_one,
                        sqrt_price_limit_x96=TickMath.MIN_SQRT_RATIO + 1
                        if zero_for_one
                        else TickMath.MAX_SQRT_RATIO - 1,
                    )
                )
            else:
                raise ValueError(
                    f"Could not identify Uniswap version for pool: {self.swap_pools[i]}"
                )

        return pools_amounts_out

    def _update_pool_states(self) -> None:
        """
        Update `self.pool_states` by retrieving `pool.state` from all helpers in `self.swap_pools`
        """
        self.pool_states = {
            pool.address: pool.state for pool in self.swap_pools
        }

    def auto_update(
        self,
        silent: bool = True,
        block_number: Optional[int] = None,
        override_update_method: Optional[str] = None,
    ) -> bool:
        """
        TBD
        """

        found_updates = False

        if None in self.pool_states.values():
            found_updates = True
            self._update_pool_states()
            self.clear_best()

            return found_updates

        if override_update_method:
            logger.debug(f"OVERRIDDEN UPDATE METHOD: {override_update_method}")

        for pool in self.swap_pools:
            pool_updated = False
            if isinstance(pool, LiquidityPool):
                if (
                    pool._update_method == "polling"
                    or override_update_method == "polling"
                ):
                    pool_updated = pool.update_reserves(
                        silent=silent,
                        override_update_method=override_update_method,
                        update_block=block_number,
                    )
                elif pool._update_method == "external":
                    if pool.state != self.pool_states[pool.address]:
                        logger.debug(
                            f"(UniswapLpCycle) found update for pool {pool}"
                        )
                        pool_updated = True

                if pool_updated:
                    logger.debug(
                        f"(UniswapLpCycle) found update for pool {pool}"
                    )
                    found_updates = True
                    break

            elif isinstance(pool, V3LiquidityPool):
                pool_updated, _ = pool.auto_update(
                    silent=silent,
                    block_number=block_number,
                )

                if pool_updated:
                    logger.debug(
                        f"(UniswapLpCycle) found update for pool {pool}"
                    )
                    found_updates = True
                    break
            else:
                raise ValueError(f"Could not identify pool {pool}!")

        if found_updates:
            self._update_pool_states()
            self.clear_best()

        return found_updates

    def _calculate(
        self,
        override_state: Optional[StateOverrideTypes] = None,
    ) -> ArbitrageCalculationResult:
        _overrides = self._sort_overrides(override_state)

        # check the pools for zero liquidity in the direction of the trade
        for i, pool in enumerate(self.swap_pools):
            if isinstance(pool, LiquidityPool):
                if (
                    pool.reserves_token1 <= 1
                    and self._swap_vectors[i].zero_for_one
                ):
                    raise ZeroLiquidityError(
                        f"V2 pool {pool.address} has no liquidity for a 0 -> 1 swap"
                    )
                elif (
                    pool.reserves_token0 <= 1
                    and not self._swap_vectors[i].zero_for_one
                ):
                    raise ZeroLiquidityError(
                        f"V2 pool {pool.address} has no liquidity for a 1 -> 0 swap"
                    )

            if isinstance(pool, V3LiquidityPool) and pool.state.liquidity == 0:
                # check if the swap is 0 -> 1 and cannot swap any more token0 for token1
                if (
                    pool.state.sqrt_price_x96 == TickMath.MIN_SQRT_RATIO + 1
                    and self._swap_vectors[i].zero_for_one
                ):
                    raise ZeroLiquidityError(
                        f"V3 pool {pool.address} has no liquidity for a 0 -> 1 swap"
                    )
                # check if the swap is 1 -> 0 (zeroForOne=False) and cannot swap any more token1 for token0
                elif (
                    pool.state.sqrt_price_x96 == TickMath.MAX_SQRT_RATIO - 1
                    and not self._swap_vectors[i].zero_for_one
                ):
                    raise ZeroLiquidityError(
                        f"V3 pool {pool.address} has no liquidity for a 1 -> 0 swap"
                    )

        # bound the amount to be swapped
        bounds: Tuple[float, float] = (
            1.0,
            float(self.max_input),
        )

        # bracket the initial guess for the algo
        bracket_amount: int = self.max_input
        bracket: Tuple[float, float, float] = (
            0.45 * bracket_amount,
            0.50 * bracket_amount,
            0.55 * bracket_amount,
        )

        def arb_profit(x):
            token_in_quantity = int(x)  # round the input down

            if TYPE_CHECKING:
                token_out_quantity: int

            for i, pool in enumerate(self.swap_pools):
                pool_override = _overrides.get(pool.address)

                if TYPE_CHECKING:
                    assert isinstance(pool, LiquidityPool) and (
                        pool_override is None
                        or isinstance(pool_override, UniswapV2PoolState)
                    )
                    assert isinstance(pool, V3LiquidityPool) and (
                        pool_override is None
                        or isinstance(pool_override, UniswapV3PoolState)
                    )

                try:
                    token_in = self._swap_vectors[i].token_in
                    token_out_quantity = (
                        pool.calculate_tokens_out_from_tokens_in(
                            token_in=token_in,
                            token_in_quantity=token_in_quantity
                            if i == 0
                            else token_out_quantity,
                            override_state=pool_override,
                        )
                    )
                except (EVMRevertError, LiquidityPoolError) as e:
                    # The optimizer might send invalid amounts into the swap calculation during
                    # iteration. We don't want it to stop, so catch the exception and pretend
                    # the swap results in token_out_quantity = 0.
                    token_out_quantity = 0
                    break

            return -float(token_out_quantity - token_in_quantity)

        opt = minimize_scalar(
            fun=arb_profit,
            method="bounded",
            bounds=bounds,
            bracket=bracket,
            options={"xatol": 1.0},
        )

        # The arb_profit function converts the value to a negative number so the minimize_scalar
        # correctly finds the optimum input. However we need a practical positive profit,
        # so we negate the result afterwards
        swap_amount = int(opt.x)
        best_profit = -int(opt.fun)

        try:
            best_amounts = self._build_amounts_out(
                token_in=self.input_token,
                token_in_quantity=swap_amount,
                pool_state_overrides=_overrides,
            )
        # except (EVMRevertError, LiquidityPoolError) as e:
        except ArbitrageError as e:
            # Simulated EVM reverts inside the ported `swap` function were ignored to execute the optimizer
            # through to completion, but now we want to raise a real error to avoid generating bad payloads
            # that will revert
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
        override_state: Optional[StateOverrideTypes] = None,
    ) -> ArbitrageCalculationResult:
        """
        Stateless calculation that does not use `self.best`
        """

        result = self._calculate(override_state=override_state)

        return result

    async def calculate_with_pool(
        self,
        executor,
        override_state: Optional[StateOverrideTypes] = None,
    ) -> asyncio.Future:
        """
        Wrap the arbitrage calculation into an asyncio task using the specified executor.

        Arguments
        ---------
        executor : Executor
            An executor (from `concurrent.futures`) to process the calculation
            work. Both `ThreadPoolExecutor` and `ProcessPoolExecutor` are
            supported, but `ProcessPoolExecutor` is recommended.
        override_state : List[Tuple[Union[LiquidityPool, V3LiquidityPool], dict]], optional
            An ordered list of tuples, representing an ordered pair of helper
            objects for Uniswap V2 / V3 pools and their overridden states.

        Returns
        -------
        A future which returns a `ArbitrageCalculationResult` (or exception)
        when awaited.

        Notes
        -----
        This is an async function that must be called with the `await` keyword.
        """

        for pool in self.swap_pools:
            if isinstance(pool, V3LiquidityPool) and pool.sparse_bitmap:
                raise ValueError(
                    f"Cannot process arbitrage {self} with executor: {pool.address} has sparse bitmap"
                )

        return asyncio.get_running_loop().run_in_executor(
            executor,
            self.calculate,
            override_state,
        )

    def calculate_arbitrage_return_best(
        self,
        override_state: Optional[StateOverrideTypes] = None,
    ):
        """
        A wrapper over `calculate_arbitrage`, useful for sending the
        calculation into a process pool and retrieving the results after
        pickling/unpickling the object and losing connection to the original.
        """

        self.calculate_arbitrage(override_state)
        return self.id, self.best

    def calculate_arbitrage(
        self,
        override_state: Optional[StateOverrideTypes] = None,
    ) -> Tuple[bool, Tuple[int, int]]:
        """
        TBD
        """

        result = self._calculate(override_state=override_state)

        if override_state is None:
            self.best.update(
                {
                    "last_swap_amount": result.input_amount,
                    "profit_amount": result.profit_amount,
                    "swap_amount": result.input_amount,
                    "swap_pool_amounts": result.swap_amounts,
                }
            )

        profitable = result.profit_amount > 0
        return profitable, (result.input_amount, result.profit_amount)

    def clear_best(self):
        self.best.update(
            {
                "profit_amount": 0,
                "swap_amount": 0,
                "swap_pool_amounts": [],
            }
        )

    @classmethod
    def from_addresses(
        cls,
        input_token_address: str,
        swap_pool_addresses: List[Tuple[str, str]],
        id: str,
        max_input: Optional[int] = None,
    ) -> "UniswapLpCycle":
        """
        Create a new `UniswapLpCycle` object from token and pool addresses.

        Arguments
        ---------
        input_token_address : str
            A address for the input_token
        swap_pool_addresses : List[str]
            An ordered list of tuples, representing the address for each pool in the swap path, and a string specifying the Uniswap version for that pool (either "V2" or "V3")

            e.g. swap_pool_addresses = [
                ("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD","V3"),
                ("0xbb2b8038a1640196fbe3e38816f3e67cba72d940","V2")
            ]
        max_input: int, optional
            The maximum input amount for the input token (limited by the balance of the deployed contract or operating EOA)
        id: str, optional
            A unique identifier for bookkeeping purposes, not validated
        """

        # create the token object
        token = Erc20Token(
            input_token_address,
            min_abi=True,
        )

        # create the pool objects
        pool_objects: List[
            Union[
                LiquidityPool,
                V3LiquidityPool,
                CamelotLiquidityPool,
            ]
        ] = []
        for pool_address, pool_type in swap_pool_addresses:
            if pool_type == "V2":
                pool_objects.append(LiquidityPool(address=pool_address))
            elif pool_type == "V3":
                pool_objects.append(V3LiquidityPool(address=pool_address))
            elif pool_type == "CamelotV2":
                pool_objects.append(CamelotLiquidityPool(address=pool_address))
            else:
                raise ArbitrageError(f"Pool type {pool_type} unknown!")

        return cls(
            input_token=token,
            swap_pools=pool_objects,
            max_input=max_input,
            id=id,
        )

    def generate_payloads(
        self,
        from_address: Union[str, ChecksumAddress],
        swap_amount: Optional[int] = None,
        pool_swap_amounts: Optional[
            List[Union[UniswapV2PoolSwapAmounts, UniswapV3PoolSwapAmounts]]
        ] = None,
    ) -> List[Tuple[ChecksumAddress, bytes, int]]:
        """
        Generate a list of ABI-encoded calldata for each step in the swap path.

        Calldata is built using the eth_abi.encode method and the ABI for the
        ``swap`` function at the Uniswap pool. V2 and V3 pools are supported.

        Arguments
        ---------
        from_address: str
            The address that will execute the calldata. Must be a smart
            contract implementing the required callbacks specific to the pool.

        swap_amount: int, optional
            The initial amount of `token_in` to swap through the first pool.
            If this argument is `None`, amount will be retrieved from
            `self.best`.

        pool_swap_amounts: list, optional
            An iterable of swap amounts to be encoded. If this argument is
            `None`, amounts will be retrieved from `self.best`.

        Returns
        -------
        ``list[(str, bytes, int)]``
            A list of payload tuples. Each payload tuple has form
            (address: ChecksumAddress, calldata: bytes, value: int).

        Raises
        ------
        ArbitrageError
            if the generated payloads would revert on-chain, or if the inputs were invalid
        """

        from_address = Web3.toChecksumAddress(from_address)

        if swap_amount is None:
            swap_amount = self.best["swap_amount"]

        if pool_swap_amounts is None:
            pool_swap_amounts = self.best["swap_pool_amounts"]

        # Abandon empty inputs.
        # @dev this looks like a useful place for a ValueError, but threaded
        # clients may execute a pool update for a swap pool before the call
        # to generate payloads is processed. Abandon the call in this case
        # and raise a generic non-fatal exception.
        if not pool_swap_amounts:
            raise ArbitrageError(
                "Pool amounts empty, abandoning payload generation."
            )

        payloads = []
        msg_value: int = (
            0  # This arbitrage does not require a `msg.value` payment
        )

        last_pool = self.swap_pools[-1]

        try:
            for i, swap_pool in enumerate(self.swap_pools):
                if i == 0 and isinstance(swap_pool, LiquidityPool):
                    # If first hop is a V2 pool, generate a payload to
                    # transfer the initial swap amount to it
                    transfer_payload = (
                        # address
                        self.input_token.address,
                        # bytes calldata
                        Web3.keccak(text="transfer(address,uint256)")[0:4]
                        + eth_abi.encode(
                            [
                                "address",
                                "uint256",
                            ],
                            [self.swap_pools[0].address, swap_amount],
                        ),
                        msg_value,
                    )

                    payloads.append(transfer_payload)

                _swap_amounts: Union[
                    UniswapV2PoolSwapAmounts, UniswapV3PoolSwapAmounts
                ] = pool_swap_amounts[i]

                if swap_pool is last_pool:
                    next_pool = None
                else:
                    next_pool = self.swap_pools[i + 1]

                if next_pool is not None:
                    # V2 pools can accept a pre-swap transfer, so the contract
                    # does not have to perform intermediate custody
                    if isinstance(next_pool, LiquidityPool):
                        swap_destination_address = next_pool.address
                    # V3 pools cannot accept a pre-swap transfer, so the contract
                    # must maintain custody prior to a swap
                    elif isinstance(next_pool, V3LiquidityPool):
                        swap_destination_address = from_address
                else:
                    # Set the destination address for the last swap to the sending address.
                    swap_destination_address = from_address

                if isinstance(swap_pool, LiquidityPool):
                    if TYPE_CHECKING:
                        assert isinstance(
                            _swap_amounts, UniswapV2PoolSwapAmounts
                        )
                    logger.debug(f"PAYLOAD: building V2 swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: swap amounts {_swap_amounts}")
                    logger.debug(
                        f"PAYLOAD: destination address {swap_destination_address}"
                    )
                    payloads.append(
                        (
                            # address
                            swap_pool.address,
                            # bytes calldata
                            Web3.keccak(
                                text="swap(uint256,uint256,address,bytes)"
                            )[0:4]
                            + eth_abi.encode(
                                [
                                    "uint256",
                                    "uint256",
                                    "address",
                                    "bytes",
                                ],
                                [
                                    *_swap_amounts.amounts,
                                    swap_destination_address,
                                    b"",
                                ],
                            ),
                            msg_value,
                        )
                    )
                elif isinstance(swap_pool, V3LiquidityPool):
                    if TYPE_CHECKING:
                        assert isinstance(
                            _swap_amounts, UniswapV3PoolSwapAmounts
                        )
                    logger.debug(f"PAYLOAD: building V3 swap at pool {i}")
                    logger.debug(f"PAYLOAD: pool address {swap_pool.address}")
                    logger.debug(f"PAYLOAD: swap amounts {_swap_amounts}")
                    logger.debug(
                        f"PAYLOAD: destination address {swap_destination_address}"
                    )
                    payloads.append(
                        (
                            # address
                            swap_pool.address,
                            # bytes calldata
                            Web3.keccak(
                                text="swap(address,bool,int256,uint160,bytes)"
                            )[0:4]
                            + eth_abi.encode(
                                [
                                    "address",
                                    "bool",
                                    "int256",
                                    "uint160",
                                    "bytes",
                                ],
                                [
                                    swap_destination_address,
                                    _swap_amounts.zero_for_one,
                                    _swap_amounts.amount_specified,
                                    _swap_amounts.sqrt_price_limit_x96,
                                    b"",
                                ],
                            ),
                            msg_value,
                        )
                    )
                else:
                    raise ValueError(
                        f"Could not identify pool: {swap_pool}, type={type(swap_pool)}"
                    )
        except Exception as e:
            logger.exception("generate_payloads catch-all")
            raise ArbitrageError(f"generate_payloads (catch-all)): {e}") from e

        return payloads
