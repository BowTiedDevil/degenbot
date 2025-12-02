import dataclasses
from bisect import bisect_left
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING, Any, cast
from weakref import WeakSet

import eth_abi.abi
from eth_typing import ChecksumAddress
from web3 import Web3
from web3.types import BlockIdentifier, TxParams

from degenbot.aerodrome.functions import (
    calc_exact_in_stable,
    generate_aerodrome_v2_pool_address,
    generate_aerodrome_v3_pool_address,
)
from degenbot.aerodrome.types import (
    AerodromeV2PoolExternalUpdate,
    AerodromeV2PoolState,
    AerodromeV2PoolStateUpdated,
    AerodromeV3PoolState,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.erc20 import Erc20Token, Erc20TokenManager
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.liquidity_pool import (
    AddressMismatch,
    ExternalUpdateError,
    InvalidSwapInputAmount,
    LateUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
)
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.registry import pool_registry
from degenbot.solidly.solidly_functions import general_calc_exact_in_volatile
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import (
    AbstractPublisherMessage,
    BoundedCache,
    Publisher,
    PublisherMixin,
    Subscriber,
)
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v2_functions import constant_product_calc_exact_out
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

if TYPE_CHECKING:
    from hexbytes import HexBytes


class AerodromeV2Pool(PublisherMixin, AbstractLiquidityPool):
    type PoolState = AerodromeV2PoolState
    _state_cache: BoundedCache[BlockNumber, PoolState]
    _state: PoolState

    FEE_DENOMINATOR = 10_000

    def __init__(
        self,
        address: ChecksumAddress | str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        state_block: BlockNumber | None = None,
        verify_address: bool = True,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> None:
        self.address = get_checksum_address(address)

        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        w3 = connection_manager.get_web3(self.chain_id)
        state_block = state_block if state_block is not None else w3.eth.block_number

        self.factory, (token0, token1), self.stable, fee, (reserves0, reserves1) = (
            self.get_factory_tokens_stable_reserves_batched(w3=w3, state_block=state_block)
        )
        self.deployer_address = (
            get_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )

        self._state_lock = Lock()
        self._state = self.PoolState.__value__(
            address=self.address,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            block=state_block,
        )

        self.fee = self.fee_token0 = self.fee_token1 = Fraction(fee, type(self).FEE_DENOMINATOR)

        token_manager = Erc20TokenManager(chain_id=self.chain_id)
        self.token0, self.token1 = (
            token_manager.get_erc20token(
                address=token0,
                silent=silent,
            ),
            token_manager.get_erc20token(
                address=token1,
                silent=silent,
            ),
        )

        if verify_address and self.address != self._verified_address():  # pragma: no cover
            raise AddressMismatch

        self.name = f"{self.token0}-{self.token1} ({self.__class__.__name__}, {100 * self.fee.numerator / self.fee.denominator:.2f}%)"  # noqa:E501

        self._state_cache = BoundedCache(max_items=state_cache_depth)
        self._state_cache[self.update_block] = self.state

        pool_registry.add(pool_address=self.address, chain_id=self.chain_id, pool=self)

        self._subscribers: WeakSet[Subscriber] = WeakSet()

        if not silent:  # pragma: no cover
            logger.info(self.name)
            logger.info(f"• Token 0: {self.token0} - Reserves: {self.reserves_token0}")
            logger.info(f"• Token 1: {self.token1} - Reserves: {self.reserves_token1}")

    def __getstate__(self) -> dict[str, Any]:
        # Remove objects that either cannot be pickled or are unnecessary to perform the calculation
        copied_attributes = ()
        dropped_attributes = (
            "_state_lock",
            "_state_cache",
            "_subscribers",
        )

        with self._state_lock:
            return {
                k: (v.copy() if k in copied_attributes else v)
                for k, v in self.__dict__.items()
                if k not in dropped_attributes
            }

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(address={self.address}, token0={self.token0}, token1={self.token1}, stable={self.stable})"  # noqa:E501

    def _notify_subscribers(self: Publisher, message: AbstractPublisherMessage) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)

    def _verified_address(self) -> ChecksumAddress:
        # The implementation address is hard-coded into the contract
        implementation_address = get_checksum_address(
            connection_manager.get_web3(self.chain_id).eth.get_code(self.address)[10:30]
        )

        return generate_aerodrome_v2_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self.token0.address, self.token1.address),
            implementation_address=get_checksum_address(implementation_address),
            stable=self.stable,
        )

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def reserves_token0(self) -> int:
        return self.state.reserves_token0

    @property
    def reserves_token1(self) -> int:
        return self.state.reserves_token1

    @property
    def state(self) -> PoolState:
        return self._state

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        return self.token0, self.token1

    @property
    def update_block(self) -> BlockNumber:
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

    @property
    def w3(self) -> Web3:
        return connection_manager.get_web3(self.chain_id)

    def swap_is_viable(
        self,
        state: PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        if state.reserves_token0 == 0 or state.reserves_token1 == 0:
            return False
        return state.reserves_token1 > 1 if vector.zero_for_one else state.reserves_token0 > 1

    def auto_update(
        self,
        *,
        block_number: BlockNumber | None = None,
        silent: bool = True,
    ) -> None:
        """
        Retrieves and records the current state from the pool at the provided block number, or the
        latest block if not provided.

        @dev this method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.
        """
        with self._state_lock:
            if block_number is not None and block_number < self.update_block:
                raise LateUpdateError

            state_updated = False
            w3 = self.w3
            block_number = block_number if block_number is not None else w3.eth.get_block_number()

            reserves0, reserves1 = self.get_reserves(w3=w3, block_identifier=block_number)

            if (self.reserves_token0, self.reserves_token1) != (reserves0, reserves1):
                state_updated = True
                self._state = dataclasses.replace(
                    self.state,
                    reserves_token0=reserves0,
                    reserves_token1=reserves1,
                    block=block_number,
                )

            if state_updated:
                self._state_cache[block_number] = self.state
                self._notify_subscribers(
                    message=AerodromeV2PoolStateUpdated(self.state),
                )

                if not silent:  # pragma: no cover
                    logger.info(f"[{self.name}]")
                    logger.info(f"{self.token0}: {self.reserves_token0}")
                    logger.info(f"{self.token1}: {self.reserves_token1}")

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out_quantity: int,
        token_out: Erc20Token,
        override_state: PoolState | None = None,
    ) -> int:
        """
        Calculates the required token INPUT of token_in for a target OUTPUT at current pool
        reserves.

        Accepts a `PoolState` state override for calculation against an arbitrary state
        in lieu of the recorded state.
        """

        if token_out_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        if override_state:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        if token_out == self.token1:
            reserves_in = (
                override_state.reserves_token0
                if override_state is not None
                else self.reserves_token0
            )
            reserves_out = (
                override_state.reserves_token1
                if override_state is not None
                else self.reserves_token1
            )

        elif token_out == self.token0:
            reserves_in = (
                override_state.reserves_token1
                if override_state is not None
                else self.reserves_token1
            )
            reserves_out = (
                override_state.reserves_token0
                if override_state is not None
                else self.reserves_token0
            )

        else:  # pragma: no cover
            raise DegenbotValueError(
                message=f"Could not identify token_out: {token_out}! This pool holds: {self.token0} {self.token1}"  # noqa:E501
            )

        # last token becomes infinitely expensive, so largest possible swap out is reserves - 1
        if token_out_quantity > reserves_out - 1:
            raise LiquidityPoolError(
                message=f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})"  # noqa:E501
            )

        if self.stable:
            raise NotImplementedError

        return constant_product_calc_exact_out(
            amount_out=token_out_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=self.fee,
        )

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        """
        Calculates the expected token OUTPUT for a target INPUT at current pool reserves.
        """

        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_in not recognized.")

        if token_in_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        if override_state:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        reserves_0 = (
            override_state.reserves_token0 if override_state is not None else self.reserves_token0
        )
        reserves_1 = (
            override_state.reserves_token1 if override_state is not None else self.reserves_token1
        )

        if self.stable:
            return calc_exact_in_stable(
                amount_in=token_in_quantity,
                token_in=0 if token_in == self.token0 else 1,
                reserves0=reserves_0,
                reserves1=reserves_1,
                decimals0=10**self.token0.decimals,
                decimals1=10**self.token1.decimals,
                fee=self.fee,
            )
        return general_calc_exact_in_volatile(
            amount_in=token_in_quantity,
            token_in=0 if token_in == self.token0 else 1,
            reserves0=reserves_0,
            reserves1=reserves_1,
            fee=self.fee,
        )

    def external_update(
        self,
        update: AerodromeV2PoolExternalUpdate,
    ) -> None:
        if update.block_number < self.update_block:
            raise ExternalUpdateError(
                message=f"Rejected update for block {update.block_number} in the past, current update block is {self.update_block}"  # noqa:E501
            )

        with self._state_lock:
            self._state = dataclasses.replace(
                self.state,
                reserves_token0=update.reserves_token0,
                reserves_token1=update.reserves_token1,
                block=update.block_number,
            )
            self._state_cache[update.block_number] = self.state
            self._notify_subscribers(
                message=AerodromeV2PoolStateUpdated(self.state),
            )

    def get_absolute_price(
        self, token: Erc20Token, override_state: PoolState | None = None
    ) -> Fraction:
        """
        Get the absolute price for the given token, expressed in units of the other.
        """

        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: PoolState | None = None,
    ) -> Fraction:
        """
        Get the absolute exchange rate for the given token, expressed in terms of a unit amount of
        its paired token.

        e.g. taking the USDC-WETH pool in https://blog.uniswap.org/uniswap-v3-math-primer — the
        WETH/USDC exchange rate is 649004842.70137. Rounding down, this signifies that the smallest
        swap (1 USDC) results in a 649004842 WETH output.

        The exchange rate for a V2 pool is a simple ratio of the output token reserves to the input
        token reserves.
        """

        if token not in self.tokens:
            raise DegenbotValueError(message=f"Unknown token {token}")

        state = self.state if override_state is None else override_state

        return (
            Fraction(state.reserves_token1, state.reserves_token0)
            if token == self.token1
            else Fraction(state.reserves_token0, state.reserves_token1)
        )

    def get_nominal_price(
        self,
        token: Erc20Token,
        override_state: PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal price for the given token, expressed per nominal unit of its paired token.
        The price is corrected for the decimal place values of both tokens.
        """

        return 1 / self.get_nominal_exchange_rate(token=token, override_state=override_state)

    def get_nominal_exchange_rate(
        self,
        token: Erc20Token,
        override_state: PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal rate for the given token, expressed in units of the other, corrected for
        decimal place values.
        """

        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            Fraction(10**self.token1.decimals, 10**self.token0.decimals)
            if token == self.token0
            else Fraction(10**self.token0.decimals, 10**self.token1.decimals)
        )

    def get_factory_tokens_stable_reserves_batched(
        self,
        w3: Web3,
        state_block: BlockNumber,
    ) -> tuple[
        ChecksumAddress,  # factory
        tuple[ChecksumAddress, ChecksumAddress],  # tokens
        bool,  # stable
        int,  # fee
        tuple[int, int],  # reserves
    ]:
        with w3.batch_requests() as batch:
            batch.add_mapping(
                {
                    # These calls default to use 'latest' for block number, which is OK since the
                    # values are immutable
                    w3.eth.call: [
                        TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="factory()",
                                function_arguments=None,
                            ),
                        ),
                        TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="token0()",
                                function_arguments=None,
                            ),
                        ),
                        TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="token1()",
                                function_arguments=None,
                            ),
                        ),
                        TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="stable()",
                                function_arguments=None,
                            ),
                        ),
                    ],
                }
            )
            batch.add(
                # This call uses a specific block so the reserve values are consistent
                w3.eth.call(
                    transaction=TxParams(
                        to=self.address,
                        data=encode_function_calldata(
                            function_prototype="getReserves()",
                            function_arguments=None,
                        ),
                    ),
                    block_identifier=state_block,
                )
            )

            factory, token0, token1, stable, reserves = batch.execute()

        (factory,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", factory))
        (token0,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", token0))
        (token1,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", token1))
        (stable,) = eth_abi.abi.decode(types=["bool"], data=cast("HexBytes", stable))
        reserves0, reserves1, _ = eth_abi.abi.decode(
            types=["uint256", "uint256", "uint256"], data=cast("HexBytes", reserves)
        )

        (fee,) = eth_abi.abi.decode(
            types=["uint256"],
            data=w3.eth.call(
                transaction=TxParams(
                    to=get_checksum_address(cast("str", factory)),
                    data=encode_function_calldata(
                        function_prototype="getFee(address,bool)",
                        function_arguments=[self.address, stable],
                    ),
                )
            ),
        )

        return (
            get_checksum_address(cast("str", factory)),
            (get_checksum_address(cast("str", token0)), get_checksum_address(cast("str", token1))),
            cast("bool", stable),
            cast("int", fee),
            (cast("int", reserves0), cast("int", reserves1)),
        )

    def get_reserves(
        self, w3: Web3, block_identifier: BlockIdentifier | None = None
    ) -> tuple[int, int]:
        reserves_token0, reserves_token1 = raw_call(
            w3=w3,
            address=self.address,
            block_identifier=block_identifier,
            calldata=encode_function_calldata(
                function_prototype="getReserves()",
                function_arguments=None,
            ),
            return_types=["uint256", "uint256"],
        )

        return cast("int", reserves_token0), cast("int", reserves_token1)

    def discard_states_before_block(self, block: BlockNumber) -> None:
        """
        Discard states recorded prior to a target block.
        """

        with self._state_lock:
            known_blocks = sorted(self._state_cache.keys())

            # Finds the index prior to the requested block number
            block_index = bisect_left(known_blocks, block)

            # The earliest known state already meets the criterion, so return early
            if block_index == 0:
                return

            if block_index == len(known_blocks):
                raise NoPoolStateAvailable(block=block)

            for known_block in known_blocks[:block_index]:
                del self._state_cache[known_block]

    def restore_state_before_block(
        self,
        block: BlockNumber,
    ) -> None:
        """
        Restore the last pool state recorded prior to a target block.

        Use this method to maintain consistent state data following a chain re-organization.
        """

        with self._state_lock:
            known_blocks = sorted(self._state_cache.keys())

            # Finds the index prior to the requested block number
            block_index = bisect_left(known_blocks, block)

            if block_index == 0:
                raise NoPoolStateAvailable(block=block)

            # The last known state already meets the criterion, so return early
            if block_index == len(known_blocks):
                return

            # Remove states at and after the specified block
            for known_block in known_blocks[block_index:]:
                del self._state_cache[known_block]

            # Restore previous state and block
            self._state = list(self._state_cache.values())[-1]
            self._notify_subscribers(message=AerodromeV2PoolStateUpdated(self.state))


class AerodromeV3Pool(UniswapV3Pool):
    type PoolState = AerodromeV3PoolState

    TICK_STRUCT_TYPES = (
        "uint128",
        "int128",
        "int128",
        "uint256",
        "uint256",
        "uint256",
        "int56",
        "uint160",
        "uint32",
        "bool",
    )  # type:ignore[assignment]

    SLOT0_STRUCT_TYPES = (
        "uint160",
        "int24",
        "uint16",
        "uint16",
        "uint16",
        "bool",
    )  # type:ignore[assignment]

    def _verified_address(self) -> ChecksumAddress:
        # The implementation address is hard-coded into the contract
        implementation_address = get_checksum_address(
            connection_manager.get_web3(self.chain_id).eth.get_code(self.address)[10:30]
        )

        return generate_aerodrome_v3_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self.token0.address, self.token1.address),
            implementation_address=get_checksum_address(implementation_address),
            tick_spacing=self.tick_spacing,
        )
