import contextlib
import dataclasses
from bisect import bisect_left
from collections.abc import Iterable
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING, Any, Self, cast
from weakref import WeakSet

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from eth_typing import BlockIdentifier, ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, scoped_session
from web3 import Web3
from web3.exceptions import ContractLogicError
from web3.types import TxParams

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.database import db_session
from degenbot.database.models.pools import (
    AbstractUniswapV2Pool,
    LiquidityPoolTable,
    UniswapV2PoolTable,
)
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
from degenbot.types.abstract import AbstractArbitrage, AbstractLiquidityPool
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import (
    AbstractPublisherMessage,
    BoundedCache,
    Publisher,
    PublisherMixin,
    Subscriber,
)
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS, UniswapV2ExchangeDeployment
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v2_functions import (
    constant_product_calc_exact_in,
    constant_product_calc_exact_out,
    generate_v2_pool_address,
)
from degenbot.uniswap.v2_types import (
    UniswapV2PoolExternalUpdate,
    UniswapV2PoolSimulationResult,
    UniswapV2PoolState,
    UniswapV2PoolStateUpdated,
)

if TYPE_CHECKING:
    from hexbytes import HexBytes


def get_pool_from_database(
    address: ChecksumAddress,
    chain_id: int,
    session: Session | scoped_session[Session] = db_session,
) -> AbstractUniswapV2Pool | None:
    return session.scalar(
        select(LiquidityPoolTable).where(
            LiquidityPoolTable.address == address,
            LiquidityPoolTable.chain == chain_id,
        )
    )  # type: ignore[return-value]


class UniswapV2Pool(PublisherMixin, AbstractLiquidityPool):
    """
    A Uniswap V2-based liquidity pool implementing the x*y=k constant function invariant.
    """

    type PoolState = UniswapV2PoolState
    type DatabasePoolType = UniswapV2PoolTable

    _state: PoolState
    _state_cache: BoundedCache[BlockNumber, PoolState]

    FEE = Fraction(3, 1000)
    RESERVES_STRUCT_TYPES = ("uint112", "uint112")
    UNISWAP_V2_MAINNET_POOL_INIT_HASH = (
        "0x96e8ac4277198ff8b6f785478aa9a39f403cb768dd02cbee326c3e7da348845f"
    )

    @classmethod
    def from_exchange(
        cls,
        address: str,
        exchange: UniswapV2ExchangeDeployment,
        **kwargs: Any,
    ) -> Self:
        """
        Create a new `UniswapV2Pool` with exchange information taken from the provided deployment.
        """

        return cls(
            address=address,
            deployer_address=exchange.factory.deployer,
            init_hash=exchange.factory.pool_init_hash,
            **kwargs,
        )

    def _notify_subscribers(self: Publisher, message: AbstractPublisherMessage) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)

    def __init__(
        self,
        address: ChecksumAddress | str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        fee: Fraction | Iterable[Fraction] | None = None,
        state_block: BlockNumber | None = None,
        verify_address: bool = True,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> None:
        """
        An abstract representation of an x*y=k invariant automatic matchmaker, based on Uniswap V2.

        Arguments
        ---------
        address:
            The address for the deployed pool contract.
        chain_id:
            The chain ID where the pool contract is deployed.
        deployer_address:
            The address for the deployment contract (optional).
        init_hash:
            The init hash for the factory contract. If one is not provided, the preset deployments
            will be searched first. If no matching deployment is found, the default Uniswap V2 hash
            will be used.
        fee:
            The swap fee as a `Fraction`. If not provided, the default will be used. A 0.3% fee
            can be specified by passing `fee=Fraction(3,1000)`. For split-fee pools of unequal
            value, provide an iterable of fees ordered by token position, e.g.
            `fee=[Fraction(3,1000), Fraction(2,1000)]`
        state_block:
            Fetch initial state values from the chain at a particular block height. Defaults to the
            latest block if omitted.
        verify_address:
            Control if the pool address is verified against the deterministic address.
        silent:
            Suppress status output.
        state_cache_depth:
            How many unique block-state pairs to hold in the state cache.
        """

        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        w3 = connection_manager.get_web3(self.chain_id)
        state_block = state_block if state_block is not None else w3.eth.block_number

        self.init_hash = (
            init_hash if init_hash is not None else self.UNISWAP_V2_MAINNET_POOL_INIT_HASH
        )

        pool_from_db: AbstractUniswapV2Pool = db_session.scalar(
            select(LiquidityPoolTable).where(
                LiquidityPoolTable.address == self.address,
                LiquidityPoolTable.chain == self._chain_id,
            )
        )

        # Get the tokens held by the pool
        if pool_from_db is not None:
            token0_address = pool_from_db.token0.address
            token1_address = pool_from_db.token1.address
        else:
            try:
                _, (token0_address, token1_address) = self.get_immutable_pool_values(w3=w3)
            except (ContractLogicError, DecodingError) as exc:  # pragma: no cover
                # Contracts differ slightly across Uniswap V2 forks, so decoding may fail.
                # Catch this here and raise as a pool-specific exception
                raise LiquidityPoolError(message="Could not decode contract data") from exc

        token_manager = Erc20TokenManager(chain_id=self.chain_id)
        try:
            self.token0 = token_manager.get_erc20token(
                address=token0_address,
                silent=silent,
            )
            self.token1 = token_manager.get_erc20token(
                address=token1_address,
                silent=silent,
            )
        except DegenbotValueError as e:
            raise LiquidityPoolError(message="Could not build one or more tokens.") from e

        # Get the factory & deployer info
        if pool_from_db is not None:
            self.factory = get_checksum_address(pool_from_db.exchange.factory)
            self.deployer = (
                get_checksum_address(pool_from_db.exchange.deployer)
                if pool_from_db.exchange.deployer is not None
                else self.factory
            )
        else:
            try:
                factory, _ = self.get_immutable_pool_values(w3=w3)
                self.factory = get_checksum_address(factory)
            except (ContractLogicError, DecodingError) as exc:  # pragma: no cover
                # Contracts differ slightly across Uniswap V2 forks, so decoding may fail.
                # Catch this here and raise as a pool-specific exception
                raise LiquidityPoolError(message="Could not decode contract data") from exc

            # The deployer address is not typically available via getter, so assume the factory
            # deployed the pool unless an address was explicitly provided
            self.deployer = get_checksum_address(deployer_address or self.factory)

        # Use registered deployment values if available
        with contextlib.suppress(KeyError):
            factory_deployment = FACTORY_DEPLOYMENTS[self.chain_id][self.factory]
            self.init_hash = factory_deployment.pool_init_hash
            if factory_deployment.deployer is not None:  # pragma: no cover
                self.deployer = factory_deployment.deployer

        # Set the fees taken on swaps for both tokens
        if fee is not None:
            match fee:
                case Iterable():
                    self.fee_token0, self.fee_token1 = fee
                case Fraction():
                    self.fee_token0 = self.fee_token1 = fee
                case _:
                    raise DegenbotValueError(message="Fees not passed correctly.")
        elif pool_from_db is not None:
            self.fee_token0 = Fraction(pool_from_db.fee_token0, pool_from_db.fee_denominator)
            self.fee_token1 = Fraction(pool_from_db.fee_token1, pool_from_db.fee_denominator)
        else:
            self.fee_token0 = self.fee_token1 = self.FEE

        if verify_address and self.address != self._verified_address():  # pragma: no branch
            raise AddressMismatch

        fee_string = (
            f"{100 * self.fee_token0.numerator / self.fee_token0.denominator:.2f}"
            if self.fee_token0 == self.fee_token1
            else (
                f"{100 * self.fee_token0.numerator / self.fee_token0.denominator:.2f}"
                f"/"
                f"{100 * self.fee_token1.numerator / self.fee_token1.denominator:.2f}"
            )
        )
        self.name = f"{self.token0}-{self.token1} ({self.__class__.__name__}, {fee_string}%)"

        reserves0, reserves1 = self.get_reserves(w3=w3, block_identifier=state_block)

        self._state_lock = Lock()
        self._state = self.PoolState.__value__(
            address=self.address,
            reserves_token0=reserves0,
            reserves_token1=reserves1,
            block=state_block,
        )
        self._state_cache = BoundedCache(max_items=state_cache_depth)
        self._state_cache[self.update_block] = self.state
        self._subscribers: WeakSet[Subscriber] = WeakSet()

        if not silent:  # pragma: no cover
            logger.info(self.name)
            logger.info(f"• Token 0: {self.token0} - Reserves: {self.reserves_token0}")
            logger.info(f"• Token 1: {self.token1} - Reserves: {self.reserves_token1}")

        pool_registry.add(pool_address=self.address, chain_id=self.chain_id, pool=self)

    @property
    def chain_id(self) -> int:
        return self._chain_id

    def __getstate__(self) -> dict[str, Any]:
        # Remove objects that either cannot be pickled or are unnecessary to perform the calculation
        copied_attributes = ()
        dropped_attributes = (
            "_contract",
            "_state_cache",
            "_state_lock",
            "_subscribers",
        )

        with self._state_lock:
            return {
                k: (v.copy() if k in copied_attributes else v)
                for k, v in self.__dict__.items()
                if k not in dropped_attributes
            }

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(address={self.address}, token0={self.token0}, token1={self.token1})"  # noqa:E501

    def _verified_address(self) -> ChecksumAddress:
        return generate_v2_pool_address(
            deployer_address=self.deployer,
            token_addresses=(self.token0.address, self.token1.address),
            init_hash=self.init_hash,
        )

    def get_immutable_pool_values(
        self,
        w3: Web3,
    ) -> tuple[
        str,  # factory
        tuple[str, str],  # tokens
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
                    ],
                }
            )

            factory, token0, token1 = batch.execute()

        (factory,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", factory))
        (token0,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", token0))
        (token1,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", token1))

        return (
            cast("str", factory),
            cast("tuple[str,str]", (token0, token1)),
        )

    @property
    def update_block(self) -> BlockNumber:
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

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
            self._state_cache[block_number] = self.state

            if state_updated:
                if not silent:  # pragma: no cover
                    logger.info(f"[{self.name}]")
                    logger.info(f"{self.token0}: {self.reserves_token0}")
                    logger.info(f"{self.token1}: {self.reserves_token1}")
                self._notify_subscribers(
                    message=UniswapV2PoolStateUpdated(self.state),
                )

    def calculate_tokens_in_from_ratio_out(
        self,
        token_in: Erc20Token,
        ratio_absolute: Fraction,
    ) -> int:
        """
        Calculates the maximum token input for the target output ratio after
        fees, defined as (quantity out / quantity in), at current pool
        reserves. The ratio must be passed as an absolute value reflecting the
        decimal amounts specified by the ERC-20 token contract
        (e.g. 10 * 10 ** (18-8) ETH/BTC).
        """

        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message=f"Token in {token_in} not held by this pool.")

        if token_in == self.token0:
            # formula: dx = y0/C - x0/(1-FEE), where C = token1/token0
            return max(
                0,
                int(
                    self.reserves_token1 / ratio_absolute
                    - self.reserves_token0 / (1 - self.fee_token0)
                ),
            )

        # formula: dy = x0/C - y0/(1-FEE), where C = token0/token1
        return max(
            0,
            int(
                self.reserves_token0 / ratio_absolute - self.reserves_token1 / (1 - self.fee_token1)
            ),
        )

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
            fee = self.fee_token0
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
            fee = self.fee_token1
        else:  # pragma: no cover
            raise DegenbotValueError(
                message=f"Could not identify token_out: {token_out}! This pool holds: {self.token0} {self.token1}"  # noqa:E501
            )

        # last token becomes infinitely expensive, so largest possible swap out is reserves - 1
        if token_out_quantity > reserves_out - 1:
            raise LiquidityPoolError(
                message=f"Requested amount out ({token_out_quantity}) >= pool reserves ({reserves_out})"  # noqa:E501
            )

        return constant_product_calc_exact_out(
            amount_out=token_out_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=fee,
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

        if token_in_quantity <= 0:  # pragma: no cover
            raise InvalidSwapInputAmount

        if override_state:  # pragma: no cover
            logger.debug(f"State overrides applied: {override_state}")

        if token_in == self.token0:
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
            fee = self.fee_token0
        elif token_in == self.token1:
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
            fee = self.fee_token1
        else:  # pragma: no cover
            raise DegenbotValueError(
                message=f"Could not identify token_in: {token_in}! Pool holds: {self.token0} {self.token1}"  # noqa:E501
            )

        return constant_product_calc_exact_in(
            amount_in=token_in_quantity,
            reserves_in=reserves_in,
            reserves_out=reserves_out,
            fee=fee,
        )

    def external_update(
        self,
        update: UniswapV2PoolExternalUpdate,
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
                message=UniswapV2PoolStateUpdated(self.state),
            )

    def get_absolute_price(
        self,
        token: Erc20Token,
        override_state: PoolState | None = None,
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

    def get_reserves(self, w3: Web3, block_identifier: BlockIdentifier) -> tuple[int, int]:
        reserves_token0, reserves_token1 = raw_call(
            w3=w3,
            address=self.address,
            calldata=encode_function_calldata(
                function_prototype="getReserves()",
                function_arguments=None,
            ),
            return_types=["uint256", "uint256"],
            block_identifier=block_identifier,
        )
        return reserves_token0, reserves_token1

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
            self._notify_subscribers(message=UniswapV2PoolStateUpdated(self.state))

    def simulate_add_liquidity(
        self,
        added_reserves_token0: int,
        added_reserves_token1: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """
        Simulate adding liquidity.
        """
        with self._state_lock:
            reserves_token0 = (
                override_state.reserves_token0 if override_state else self.reserves_token0
            )
            reserves_token1 = (
                override_state.reserves_token1 if override_state else self.reserves_token1
            )

            return UniswapV2PoolSimulationResult(
                amount0_delta=added_reserves_token0,
                amount1_delta=added_reserves_token1,
                initial_state=override_state or self.state,
                final_state=dataclasses.replace(
                    self.state,
                    reserves_token0=reserves_token0 + added_reserves_token0,
                    reserves_token1=reserves_token1 + added_reserves_token1,
                    block=self.update_block if override_state is not None else None,
                ),
            )

    def simulate_remove_liquidity(
        self,
        removed_reserves_token0: int,
        removed_reserves_token1: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """
        Simulate removing liquidity.
        """
        with self._state_lock:
            reserves_token0 = (
                override_state.reserves_token0 if override_state else self.reserves_token0
            )
            reserves_token1 = (
                override_state.reserves_token1 if override_state else self.reserves_token1
            )

            return UniswapV2PoolSimulationResult(
                amount0_delta=-removed_reserves_token0,
                amount1_delta=-removed_reserves_token1,
                initial_state=override_state or self.state,
                final_state=dataclasses.replace(
                    self.state,
                    reserves_token0=reserves_token0 - removed_reserves_token0,
                    reserves_token1=reserves_token1 - removed_reserves_token1,
                    block=self.update_block if override_state is not None else None,
                ),
            )

    def simulate_exact_input_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        """
        Simulate an exact input swap.
        """
        if token_in not in self.tokens:
            raise DegenbotValueError(message="token_in is unknown.")

        zero_for_one = token_in == self.token0
        token_out_quantity = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_in_quantity=token_in_quantity,
            override_state=override_state,
        )
        token0_delta = -token_out_quantity if zero_for_one is False else token_in_quantity
        token1_delta = -token_out_quantity if zero_for_one is True else token_in_quantity

        return UniswapV2PoolSimulationResult(
            amount0_delta=token0_delta,
            amount1_delta=token1_delta,
            initial_state=override_state or self.state,
            final_state=dataclasses.replace(
                self.state,
                reserves_token0=self.reserves_token0 + token0_delta,
                reserves_token1=self.reserves_token1 + token1_delta,
                block=self.update_block if override_state is not None else None,
            ),
        )

    def simulate_exact_output_swap(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: PoolState | None = None,
    ) -> UniswapV2PoolSimulationResult:
        if token_out not in self.tokens:
            raise DegenbotValueError(message="token_out is unknown.")

        zero_for_one = token_out == self.token1

        token_in_quantity = self.calculate_tokens_in_from_tokens_out(
            token_out=token_out,
            token_out_quantity=token_out_quantity,
            override_state=override_state,
        )
        token0_delta = token_in_quantity if zero_for_one is True else -token_out_quantity
        token1_delta = token_in_quantity if zero_for_one is False else -token_out_quantity

        return UniswapV2PoolSimulationResult(
            amount0_delta=token0_delta,
            amount1_delta=token1_delta,
            initial_state=override_state or self.state,
            final_state=dataclasses.replace(
                self.state,
                reserves_token0=self.reserves_token0 + token0_delta,
                reserves_token1=self.reserves_token1 + token1_delta,
                block=self.update_block if override_state is not None else None,
            ),
        )

    def get_arbitrage_helpers(self) -> tuple[AbstractArbitrage, ...]:
        return tuple(
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, AbstractArbitrage)
        )


class UnregisteredLiquidityPool(UniswapV2Pool):
    """
    A disconnected version of `UniswapV2Pool` for use where a pool helper is expected, but no
    chain data available to read the necessary values.

    The pool helper is not added to the pool registry and no reserve values are set.
    """

    def __init__(
        self,
        address: ChecksumAddress | str,
        tokens: list[Erc20Token],
        fee: Fraction | Iterable[Fraction] = Fraction(3, 1000),
    ) -> None:
        self.address = get_checksum_address(address)
        self._state_lock = Lock()
        self._state = UniswapV2PoolState(
            address=self.address,
            reserves_token0=0,
            reserves_token1=0,
            block=None,
        )
        self.token0 = min(tokens)
        self.token1 = max(tokens)

        if isinstance(fee, Iterable):
            self.fee_token0, self.fee_token1 = fee
        else:
            self.fee_token0 = self.fee_token1 = fee

        self._subscribers = WeakSet()
