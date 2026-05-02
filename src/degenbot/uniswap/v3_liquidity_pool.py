# ruff: noqa: PLR0904

# TODO: add event prototype exporter method and handler for callbacks
import dataclasses
from collections import deque
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING, Any, Self, TypedDict, cast
from weakref import WeakSet

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, scoped_session
from web3.exceptions import ContractLogicError
from web3.types import BlockIdentifier

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.database import db_session
from degenbot.database.models.pools import LiquidityPoolTable, UniswapV3PoolTableBase
from degenbot.erc20 import Erc20Token, Erc20TokenManager
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.evm import EVMRevertError
from degenbot.exceptions.liquidity_pool import (
    AddressMismatch,
    ExternalUpdateError,
    IncompleteSwap,
    LateUpdateError,
    LiquidityPoolError,
)
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.provider import ProviderAdapter
from degenbot.registry import pool_registry
from degenbot.types.abstract import AbstractArbitrage, AbstractConcentratedLiquidityPool
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import AbstractPublisherMessage, Publisher, PublisherMixin, Subscriber
from degenbot.types.hop_types import BoundedProductHop, HopType, V3TickRangeInfo
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot, MissingLiquidityData
from degenbot.uniswap.concentrated.state_manager import (
    ConcentratedLiquidityStateManager,
)
from degenbot.uniswap.concentrated.v3_simulator import calculate_swap as _v3_swap
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS, UniswapV3ExchangeDeployment
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v3_functions import (
    exchange_rate_from_sqrt_price_x96,
    generate_v3_pool_address,
    get_tick_word_and_bit_position,
)
from degenbot.uniswap.v3_libraries.tick_bitmap import flip_tick, gen_ticks
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
)
from degenbot.uniswap.v3_types import (
    InitializedTickMap,
    Liquidity,
    LiquidityGross,
    LiquidityMap,
    LiquidityNet,
    SqrtPriceX96,
    Tick,
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolLiquidityMappingUpdate,
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
    UniswapV3PoolStateUpdated,
)

if TYPE_CHECKING:
    from hexbytes import HexBytes

type Token0Amount = int
type Token1Amount = int


class UniswapV3LiquidityAtTickAsDict(TypedDict):
    block: int
    liquidity_gross: int
    liquidity_net: int


class UniswapV3BitmapAtWordAsDict(TypedDict):
    bitmap: int
    block: int


def get_pool_from_database(
    address: ChecksumAddress,
    chain_id: int,
    session: Session | scoped_session[Session] = db_session,
) -> UniswapV3PoolTableBase | None:
    return session.scalar(
        select(LiquidityPoolTable).where(
            LiquidityPoolTable.address == address,
            LiquidityPoolTable.chain == chain_id,
        )
    )  # type: ignore[return-value]


class UniswapV3Pool(PublisherMixin, AbstractConcentratedLiquidityPool):
    type PoolState = UniswapV3PoolState
    _state: PoolState
    _state_mgr: ConcentratedLiquidityStateManager[UniswapV3PoolState]

    UNISWAP_V3_MAINNET_POOL_INIT_HASH = (
        "0xe34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54"
    )
    TICK_STRUCT_TYPES = (
        "uint128",
        "int128",
        "uint256",
        "uint256",
        "int56",
        "uint160",
        "uint32",
        "bool",
    )
    SLOT0_STRUCT_TYPES = (
        "uint160",
        "int24",
        "uint16",
        "uint16",
        "uint16",
        "uint8",
        "bool",
    )

    FEE_DENOMINATOR = 1_000_000

    @classmethod
    def from_exchange(
        cls,
        address: str,
        exchange: UniswapV3ExchangeDeployment,
        **kwargs: Any,
    ) -> Self:
        """
        Create a new `UniswapV3Pool` with exchange information taken from the provided deployment.
        """

        for key in [
            "deployer_address",
            "init_hash",
        ]:  # pragma: no cover
            if key in kwargs:
                logger.warning(
                    f"Ignoring keyword argument {key}={kwargs[key]} in favor of value in exchange deployment."  # noqa: E501
                )
                kwargs.pop(key)

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
        address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        tick_bitmap: (
            dict[int, UniswapV3BitmapAtWord]
            | dict[str, UniswapV3BitmapAtWord]
            | dict[int, UniswapV3BitmapAtWordAsDict]
            | dict[str, UniswapV3BitmapAtWordAsDict]
            | None
        ) = None,
        tick_data: (
            dict[int, UniswapV3LiquidityAtTick]
            | dict[str, UniswapV3LiquidityAtTick]
            | dict[int, UniswapV3LiquidityAtTickAsDict]
            | dict[str, UniswapV3LiquidityAtTickAsDict]
            | None
        ) = None,
        provider: ProviderAdapter | None = None,
        state_block: BlockNumber | None = None,
        verify_address: bool = True,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> None:
        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        self._provider = (
            provider if provider is not None else connection_manager.get_provider(self.chain_id)
        )
        # Track whether provider was fetched from connection_manager (True) or passed in (False)
        self._provider_from_connection_manager = provider is None
        state_block = state_block if state_block is not None else self._provider.get_block_number()
        self._initial_state_block = state_block

        pool_from_db = get_pool_from_database(address=self.address, chain_id=self.chain_id)

        token0_address: ChecksumAddress | str
        token1_address: ChecksumAddress | str
        factory_address: ChecksumAddress | str

        if pool_from_db is not None:
            token0_address = pool_from_db.token0.address
            token1_address = pool_from_db.token1.address

            factory_address = pool_from_db.exchange.factory
            deployer_address = pool_from_db.exchange.deployer

            assert pool_from_db.fee_token0 == pool_from_db.fee_token1
            self.fee = pool_from_db.fee_token0

            self.tick_spacing = pool_from_db.tick_spacing
        else:
            try:
                factory_address, (token0_address, token1_address), self.fee, self.tick_spacing = (
                    self.get_immutable_pool_values(self._provider)
                )

            except (ContractLogicError, DecodingError) as exc:
                # Contracts differ slightly across Uniswap V3 forks, so decoding may fail. Catch
                # this here and raise as a pool-specific exception
                raise LiquidityPoolError(message="Could not decode contract data") from exc

        try:
            sqrt_price_x96, tick, liquidity = self.get_mutable_pool_values(
                self._provider, state_block=state_block
            )
        except (ContractLogicError, DecodingError) as exc:
            # Contracts differ slightly across Uniswap V3 forks, so decoding may fail. Catch this
            # here and raise as a pool-specific exception
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        self.factory = get_checksum_address(factory_address)
        self.deployer_address = (
            get_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )

        try:
            # Use degenbot deployment values if available
            factory_deployment = FACTORY_DEPLOYMENTS[self.chain_id][self.factory]
            self.init_hash = factory_deployment.pool_init_hash
            if factory_deployment.deployer is not None:
                self.deployer_address = factory_deployment.deployer
        except KeyError:
            # Deployment is unknown. Uses any inputs provided, otherwise use default values from
            # original Uniswap contracts
            self.init_hash = (
                init_hash if init_hash is not None else self.UNISWAP_V3_MAINNET_POOL_INIT_HASH
            )

        token_manager = Erc20TokenManager(chain_id=self.chain_id, provider=self._provider)
        try:
            self.token0, self.token1 = (
                token_manager.get_erc20token(
                    address=token0_address,
                    silent=silent,
                ),
                token_manager.get_erc20token(
                    address=token1_address,
                    silent=silent,
                ),
            )
        except DegenbotValueError as e:
            raise LiquidityPoolError(message="Could not build one or more tokens.") from e

        if verify_address and self.address != self._verified_address():  # pragma: no branch
            raise AddressMismatch

        self.name = f"{self.token0}-{self.token1} ({self.__class__.__name__}, {100 * self.fee / self.FEE_DENOMINATOR:.2f}%)"  # noqa: E501

        if (tick_bitmap is not None) != (tick_data is not None):
            raise DegenbotValueError(message="Provide both tick_bitmap and tick_data.")

        # If liquidity info was not provided, treat the mapping as sparse
        self.sparse_liquidity_map = tick_bitmap is None or tick_data is None

        working_tick_bitmap = (
            {}
            if tick_bitmap is None
            else {
                int(word): (
                    bitmap_at_word
                    if isinstance(bitmap_at_word, UniswapV3BitmapAtWord)
                    else UniswapV3BitmapAtWord(**bitmap_at_word)
                )
                for word, bitmap_at_word in tick_bitmap.items()
            }
        )
        working_tick_data = (
            {}
            if tick_data is None
            else {
                int(tick): (
                    liquidity_at_tick
                    if isinstance(liquidity_at_tick, UniswapV3LiquidityAtTick)
                    else UniswapV3LiquidityAtTick(**liquidity_at_tick)
                )
                for tick, liquidity_at_tick in tick_data.items()
            }
        )

        if tick_bitmap is None and tick_data is None:
            word, _ = get_tick_word_and_bit_position(tick=tick, tick_spacing=self.tick_spacing)
            self._fetch_and_populate_initialized_ticks(
                word_position=word,
                tick_bitmap=working_tick_bitmap,
                tick_data=working_tick_data,
                block_number=state_block,
            )

        initial_state = self.PoolState.__value__(
            address=self.address,
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            tick_bitmap=working_tick_bitmap,
            tick_data=working_tick_data,
            block=state_block,
        )
        self._state_lock = Lock()
        self._state_mgr = ConcentratedLiquidityStateManager(
            initial_state=initial_state,
            state_cache_depth=state_cache_depth,
        )

        pool_registry.add(
            pool=self,
            chain_id=self.chain_id,
            pool_address=self.address,
        )

        self._subscribers: WeakSet[Subscriber] = WeakSet()

        if not silent:  # pragma: no branch
            logger.info(self.name)
            logger.info(f"• Address: {self.address}")
            logger.info(f"• Token 0: {self.token0}")
            logger.info(f"• Token 1: {self.token1}")
            logger.info(f"• Fee: {self.fee}")
            logger.info(f"• Liquidity: {self.liquidity}")
            logger.info(f"• SqrtPrice: {self.sqrt_price_x96}")
            logger.info(f"• Tick: {self.tick}")
            logger.info(f"• State Block (Initial): {self._initial_state_block}")

    def __getstate__(self) -> dict[str, Any]:
        # Remove, copy, or substitute attributes that will be available to the reconstructed object
        # after pickling/unpickling
        copied_attributes: set[str] = set()
        dropped_attributes = {
            "_provider",
            "_provider_from_connection_manager",
            "_state_lock",
            "_subscribers",
        }

        with self._state_lock:
            return {
                k: (v.copy() if k in copied_attributes else v)
                for k, v in self.__dict__.items()
                if k not in dropped_attributes
            }

    def __setstate__(self, state: dict[str, Any]) -> None:
        state["_state_lock"] = Lock()
        # After unpickling, provider must be re-acquired from connection_manager
        state["_provider_from_connection_manager"] = True
        self.__dict__ = state

    def __getnewargs_ex__(self) -> tuple[tuple[()], dict[str, Any]]:
        """
        Return empty args so __init__ is not called during unpickling.
        The object is reconstructed via __setstate__.
        """
        return (), {}

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(address={self.address}, token0={self.token0}, token1={self.token1}, fee={100 * self.fee / self.FEE_DENOMINATOR:.2f}%, tick spacing={self.tick_spacing})"  # noqa:E501

    def __str__(self) -> str:
        return self.name

    def _calculate_swap(
        self,
        *,
        zero_for_one: bool,
        amount_specified: int,
        sqrt_price_limit_x96: int,
        override_state: PoolState | None = None,
    ) -> tuple[Token0Amount, Token1Amount, SqrtPriceX96, Liquidity, Tick]:
        """
        This function is ported and adapted from the UniswapV3Pool.sol contract at
        https://github.com/Uniswap/v3-core/blob/main/contracts/UniswapV3Pool.sol

        Returns a tuple with amounts and final pool state values for a successful swap:
        (amount0, amount1, sqrt_price_x96, liquidity, tick)

        A negative amount indicates the token quantity sent to the swapper, and a positive amount
        indicates the token quantity deposited.

        This method will fetch missing liquidity data as needed, but this data is discarded.
        """

        if override_state is not None:
            snapshot = LiquidityMapSnapshot.from_state(
                override_state,
                tick_spacing=self.tick_spacing,
                sparse=self.sparse_liquidity_map,
            )
            liquidity_start = override_state.liquidity
            sqrt_price_x96_start = override_state.sqrt_price_x96
            tick_start = override_state.tick
        else:
            # The tick bitmap and tick data accessed through the property are copies,
            # so they can be freely modified without corrupting the state
            snapshot = LiquidityMapSnapshot(
                tick_data=self.tick_data,
                tick_bitmap=self.tick_bitmap,
                tick_spacing=self.tick_spacing,
                sparse=self.sparse_liquidity_map,
            )
            liquidity_start = self.liquidity
            sqrt_price_x96_start = self.sqrt_price_x96
            tick_start = self.tick

        if self.sparse_liquidity_map:
            # Sparse map may raise MissingLiquidityData. Fetch missing data and retry.
            while True:
                try:
                    result = _v3_swap(
                        snapshot=snapshot,
                        zero_for_one=zero_for_one,
                        amount_specified=amount_specified,
                        sqrt_price_limit_x96=sqrt_price_limit_x96,
                        fee=self.fee,
                        liquidity_start=liquidity_start,
                        sqrt_price_x96_start=sqrt_price_x96_start,
                        tick_start=tick_start,
                    )
                except MissingLiquidityData as exc:
                    # Fetch missing word into mutable copies, then rebuild snapshot
                    working_bitmap = dict(snapshot.tick_bitmap)
                    working_data = dict(snapshot.tick_data)
                    self._fetch_and_populate_initialized_ticks(
                        word_position=exc.word,
                        tick_bitmap=cast("InitializedTickMap", working_bitmap),
                        tick_data=cast("LiquidityMap", working_data),
                        block_number=self.update_block,
                    )
                    snapshot = LiquidityMapSnapshot(
                        tick_data=working_data,
                        tick_bitmap=working_bitmap,
                        tick_spacing=self.tick_spacing,
                        sparse=True,
                    )
                else:
                    return (
                        result.amount0,
                        result.amount1,
                        result.sqrt_price_x96,
                        result.liquidity,
                        result.tick,
                    )
        else:
            result = _v3_swap(
                snapshot=snapshot,
                zero_for_one=zero_for_one,
                amount_specified=amount_specified,
                sqrt_price_limit_x96=sqrt_price_limit_x96,
                fee=self.fee,
                liquidity_start=liquidity_start,
                sqrt_price_x96_start=sqrt_price_x96_start,
                tick_start=tick_start,
            )
            return (
                result.amount0,
                result.amount1,
                result.sqrt_price_x96,
                result.liquidity,
                result.tick,
            )

    def get_tick_bitmap_at_word(
        self,
        provider: ProviderAdapter,
        word_position: int,
        block_identifier: BlockIdentifier,
    ) -> int:
        (bitmap_at_word,) = raw_call(
            provider,
            address=self.address,
            calldata=encode_function_calldata(
                function_prototype="tickBitmap(int16)",
                function_arguments=[word_position],
            ),
            return_types=["uint256"],
            block_identifier=block_identifier,
        )
        return cast("int", bitmap_at_word)

    def get_populated_ticks_in_word(
        self,
        provider: ProviderAdapter,
        word_position: int,
        block_identifier: BlockIdentifier,
    ) -> list[tuple[int, int, int]]:
        bitmap_at_word = self.get_tick_bitmap_at_word(
            provider,
            word_position=word_position,
            block_identifier=block_identifier,
        )

        active_ticks = [
            ((word_position << 8) + i) * self.tick_spacing
            for i in range(256)
            if bitmap_at_word & (1 << i) > 0
        ]

        results: list[HexBytes] = []
        block = block_identifier if isinstance(block_identifier, int) else None
        for tick in active_ticks:
            result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="ticks(int24)",
                    function_arguments=[tick],
                ),
                block=block,
            )
            results.append(result)

        populated_ticks = []
        for tick, result in zip(active_ticks, results, strict=True):
            liquidity_gross, liquidity_net, *_ = eth_abi.abi.decode(
                types=self.TICK_STRUCT_TYPES,
                data=result,
            )
            populated_ticks.append((tick, liquidity_gross, liquidity_net))

        return populated_ticks

    def _fetch_and_populate_initialized_ticks(
        self,
        *,
        word_position: int,
        tick_bitmap: InitializedTickMap,
        tick_data: LiquidityMap,
        block_number: BlockNumber | None = None,
    ) -> None:
        """
        Update the supplied tick bitmap with initialized tick values within a specified word
        position. A word is divided into 256 ticks, spaced at a fixed interval.
        """

        if block_number is None:
            block_number = self._provider.get_block_number()

        working_tick_data: list[tuple[Tick, LiquidityGross, LiquidityNet]] = []
        working_tick_bitmap = self.get_tick_bitmap_at_word(
            self._provider,
            word_position=word_position,
            block_identifier=block_number,
        )
        if working_tick_bitmap != 0:
            working_tick_data = self.get_populated_ticks_in_word(
                self._provider,
                word_position=word_position,
                block_identifier=block_number,
            )

        tick_bitmap[word_position] = UniswapV3BitmapAtWord(
            bitmap=working_tick_bitmap,
            block=block_number,
        )
        for tick, liquidity_gross, liquidity_net in working_tick_data:
            tick_data[tick] = UniswapV3LiquidityAtTick(
                liquidity_net=liquidity_net,
                liquidity_gross=liquidity_gross,
                block=block_number,
            )

    def _verified_address(self) -> ChecksumAddress:
        return generate_v3_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self.token0.address, self.token1.address),
            fee=self.fee,
            init_hash=self.init_hash,
        )

    def get_immutable_pool_values(
        self,
        provider: ProviderAdapter,
    ) -> tuple[
        str,  # factory
        tuple[str, str],  # tokens
        int,  # fee
        int,  # tick spacing
    ]:
        try:
            factory_result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="factory()",
                    function_arguments=None,
                ),
            )
            token0_result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="token0()",
                    function_arguments=None,
                ),
            )
            token1_result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="token1()",
                    function_arguments=None,
                ),
            )
            fee_result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="fee()",
                    function_arguments=None,
                ),
            )
            tick_spacing_result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="tickSpacing()",
                    function_arguments=None,
                ),
            )

            (factory,) = eth_abi.abi.decode(types=["address"], data=factory_result)
            (token0,) = eth_abi.abi.decode(types=["address"], data=token0_result)
            (token1,) = eth_abi.abi.decode(types=["address"], data=token1_result)
            (fee,) = eth_abi.abi.decode(types=["uint256"], data=fee_result)
            (tick_spacing,) = eth_abi.abi.decode(types=["uint256"], data=tick_spacing_result)

        except (ContractLogicError, DecodingError) as exc:
            # Contracts differ slightly across Uniswap V3 forks, so decoding may fail. Catch this
            # here and raise as a pool-specific exception
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        else:
            return (
                cast("str", factory),
                cast("tuple[str,str]", (token0, token1)),
                cast("int", fee),
                cast("int", tick_spacing),
            )

    def get_mutable_pool_values(
        self,
        provider: ProviderAdapter,
        state_block: BlockNumber,
    ) -> tuple[SqrtPriceX96, Tick, Liquidity]:
        try:
            slot0_result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="slot0()",
                    function_arguments=None,
                ),
                block=state_block,
            )
            liquidity_result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="liquidity()",
                    function_arguments=None,
                ),
                block=state_block,
            )

            price, tick, *_ = eth_abi.abi.decode(types=self.SLOT0_STRUCT_TYPES, data=slot0_result)
            (liquidity,) = eth_abi.abi.decode(types=["uint256"], data=liquidity_result)

        except (ContractLogicError, DecodingError) as exc:
            # Contracts differ slightly across Uniswap V3 forks, so decoding may fail. Catch this
            # here and raise as a pool-specific exception
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        else:
            return (
                cast("int", price),
                cast("int", tick),
                cast("int", liquidity),
            )

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def liquidity(self) -> int:
        return self._state_mgr.liquidity

    @property
    def sqrt_price_x96(self) -> int:
        return self._state_mgr.sqrt_price_x96

    @property
    def _state_cache(self) -> deque[PoolState]:
        return self._state_mgr.state_cache

    @_state_cache.setter
    def _state_cache(self, value: deque[PoolState]) -> None:
        if not hasattr(self, "_state_mgr"):
            self._state_mgr = object.__new__(ConcentratedLiquidityStateManager)
            self._state_mgr.state_cache = value
            return
        self._state_mgr.state_cache = value

    @property
    def state(self) -> PoolState:
        return self._state_mgr.state

    @property
    def tick(self) -> int:
        return self._state_mgr.tick

    @property
    def tick_bitmap(self) -> InitializedTickMap:
        return cast("InitializedTickMap", self._state_mgr.tick_bitmap)

    @property
    def tick_data(self) -> LiquidityMap:
        return cast("LiquidityMap", self._state_mgr.tick_data)

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        return self.token0, self.token1

    @property
    def update_block(self) -> BlockNumber:
        if TYPE_CHECKING:
            assert self.state.block is not None
        return self.state.block

    def swap_is_viable(
        self,
        state: PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        return self._state_mgr.swap_is_viable(
            state=state,
            zero_for_one=vector.zero_for_one,
            sparse_liquidity_map=self.sparse_liquidity_map,
        )

    def _get_provider_for_chain(self) -> ProviderAdapter:
        """Get the provider for this pool's chain.

        If the provider was passed in explicitly during construction, use the cached provider.
        Otherwise, fetch from connection_manager to handle provider updates.
        """
        if self._provider_from_connection_manager:
            try:
                return connection_manager.get_provider(self._chain_id)
            except Exception:  # noqa: BLE001
                # Fall back to cached provider if connection_manager doesn't have one
                # (e.g., in multiprocessing context)
                return self._provider
        return self._provider

    def auto_update(
        self,
        *,
        block_number: BlockNumber | None = None,
        silent: bool = True,
    ) -> None:
        """
        Retrieves and records the current slot0 and liquidity state from the pool at the provided
        block number, or the latest block if not provided.

        @dev this method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.
        """

        with self._state_lock:
            if block_number is not None and block_number < self.update_block:
                raise LateUpdateError

            provider = self._get_provider_for_chain()
            block_number = block_number if block_number is not None else provider.get_block_number()

            slot0_result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="slot0()",
                    function_arguments=None,
                ),
                block=block_number,
            )
            liquidity_result = provider.call(
                to=self.address,
                data=encode_function_calldata(
                    function_prototype="liquidity()",
                    function_arguments=None,
                ),
                block=block_number,
            )

            sqrt_price_x96: int
            tick: int
            liquidity: int

            sqrt_price_x96, tick, *_ = eth_abi.abi.decode(
                types=self.SLOT0_STRUCT_TYPES, data=slot0_result
            )
            (liquidity,) = eth_abi.abi.decode(types=["uint256"], data=liquidity_result)

            if (
                sqrt_price_x96 == self.sqrt_price_x96
                and liquidity == self.liquidity
                and self.tick == tick
            ):
                return

            working_state = dataclasses.replace(
                self.state,
                liquidity=liquidity,
                sqrt_price_x96=sqrt_price_x96,
                tick=tick,
                block=block_number,
            )

            self._state_mgr.push_state(working_state)

            self._notify_subscribers(
                message=UniswapV3PoolStateUpdated(working_state),
            )

            if not silent:  # pragma: no cover
                logger.info(f"Liquidity: {self.liquidity}")
                logger.info(f"SqrtPriceX96: {self.sqrt_price_x96}")
                logger.info(f"Tick: {self.tick}")

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: PoolState | None = None,
    ) -> Token0Amount | Token1Amount:
        """
        This function implements the common degenbot interface `calculate_tokens_out_from_tokens_in`
        to calculate the number of tokens withdrawn (out) for a given number of tokens deposited
        (in).

        It is similar to calling quoteExactInputSingle using the quoter contract with arguments:
        `quoteExactInputSingle(
            tokenIn=token_in,
            tokenOut=[automatically determined by helper],
            fee=[automatically determined by helper],
            amountIn=token_in_quantity,
            sqrt_price_limitX96 = 0
        )` which returns the value `amountOut`

        Note that this wrapper function always assumes that the sqrt_price_limit_x96 argument is
        unset, thus the swap calculation will continue until the target amount is satisfied,
        regardless of price impact.

        Accepts an override of state values.
        """

        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_in not found!")

        zero_for_one = token_in == self.token0

        try:
            amount0_delta, amount1_delta, *_ = self._calculate_swap(
                zero_for_one=zero_for_one,
                amount_specified=token_in_quantity,
                sqrt_price_limit_x96=(MIN_SQRT_RATIO + 1 if zero_for_one else MAX_SQRT_RATIO - 1),
                override_state=override_state,
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e
        else:
            if zero_for_one is True and amount0_delta < token_in_quantity:
                raise IncompleteSwap(amount_in=amount0_delta, amount_out=-amount1_delta)
            if zero_for_one is False and amount1_delta < token_in_quantity:
                raise IncompleteSwap(amount_in=amount1_delta, amount_out=-amount0_delta)

            return -amount1_delta if zero_for_one else -amount0_delta

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: PoolState | None = None,
    ) -> int:
        """
        This function implements the common degenbot interface `calculate_tokens_in_from_tokens_out`
        to calculate the number of tokens deposited (in) for a given number of tokens withdrawn
        (out).

        It is similar to calling quoteExactOutputSingle using the quoter contract with arguments:
        `quoteExactOutputSingle(
            tokenIn=[automatically determined by helper],
            tokenOut=token_out,
            fee=[automatically determined by helper],
            amountOut=token_out_quantity,
            sqrtPriceLimitX96 = 0
        )` which returns the value `amountIn`

        Note that this wrapper function always assumes that the sqrtPriceLimitX96 argument is unset,
        thus the swap calculation will continue until the target amount is satisfied, regardless of
        price impact

        Accepts an override of state values.
        """

        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_out not found!")

        is_zero_for_one = token_out == self.token1

        try:
            amount0_delta, amount1_delta, *_ = self._calculate_swap(
                zero_for_one=is_zero_for_one,
                amount_specified=-token_out_quantity,
                sqrt_price_limit_x96=(
                    MIN_SQRT_RATIO + 1 if is_zero_for_one else MAX_SQRT_RATIO - 1
                ),
                override_state=override_state,
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e
        else:
            if is_zero_for_one is True and -amount1_delta < token_out_quantity:
                raise IncompleteSwap(amount_in=amount0_delta, amount_out=-amount1_delta)
            if is_zero_for_one is False and -amount0_delta < token_out_quantity:
                raise IncompleteSwap(amount_in=amount1_delta, amount_out=-amount0_delta)

            return amount0_delta if is_zero_for_one else amount1_delta

    def external_update(
        self,
        update: UniswapV3PoolExternalUpdate,
    ) -> bool:
        """
        Process a `UniswapV3PoolExternalUpdate` with one or more of the following update types:
            - `block_number`: int
            - `tick`: int
            - `liquidity`: int
            - `sqrt_price_x96`: int

        `block_number` is validated against the most recently recorded block prior to recording any
        changes.

        Returns a bool indicating whether any updated state value was recorded.

        @dev This method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.
        """

        if (
            update.block_number <= self._initial_state_block
            or update.block_number < self.update_block
        ):
            raise ExternalUpdateError(message=f"Rejected update for block {update.block_number}")

        if (
            update.liquidity == self.liquidity
            and update.sqrt_price_x96 == self.sqrt_price_x96
            and update.tick == self.tick
        ):
            return False

        with self._state_lock:
            state_block = update.block_number

            working_state = dataclasses.replace(
                self.state,
                liquidity=update.liquidity,
                sqrt_price_x96=update.sqrt_price_x96,
                tick=update.tick,
                block=state_block,
            )

            self._state_mgr.push_state(working_state)

            self._notify_subscribers(
                message=UniswapV3PoolStateUpdated(working_state),
            )

            return True

    def update_liquidity_map(
        self,
        update: UniswapV3PoolLiquidityMappingUpdate,
    ) -> None:
        """
        Applies an update to the liquidity map.

        @dev This method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.
        """

        with self._state_lock:
            state_block = update.block_number

            # The tick bitmap and tick data dictionaries accessed through the attribute are copies,
            # so they can be freely modified without corrupting the state
            working_tick_bitmap = self.tick_bitmap
            working_tick_data = self.tick_data

            working_liquidity = self.liquidity

            assert working_liquidity >= 0, (
                f"Starting liquidity violates invariant: pool {self.address} {self.tick=} {self.liquidity=}"  # noqa: E501
            )

            # Adjust in-range liquidity if the modified region includes the active tick.
            # NOTE: This compares the update block to `initial_state_block` so that onchain
            # liquidity updates from blocks prior to the creation of this pool helper can be applied
            # without triggering an inconsistent invariant check. Particularly, the values for
            # `self.tick` and `self.liquidity` may not align with the pool state when these
            # liquidity events occured.
            if (
                update.tick_lower <= self.tick < update.tick_upper
                and state_block > self._initial_state_block
            ):
                working_liquidity += update.liquidity
                assert working_liquidity >= 0, (
                    f"In-range liquidity adjustment violated invariant: pool {self.address} {self.tick=} {self.liquidity=} {self.update_block=} {update=}"  # noqa: E501
                )

            for tick in (update.tick_lower, update.tick_upper):
                tick_word, _ = get_tick_word_and_bit_position(tick, self.tick_spacing)

                if self.sparse_liquidity_map and tick_word not in working_tick_bitmap:
                    # The liquidity map at the affected word must be complete prior to changing the
                    # status of any tick
                    self._fetch_and_populate_initialized_ticks(
                        word_position=tick_word,
                        tick_bitmap=working_tick_bitmap,
                        tick_data=working_tick_data,
                        block_number=(
                            # Populate the liquidity data from the previous block
                            state_block - 1
                        ),
                    )

                # Get the liquidity info for this tick. If the mapping is empty at this tick, it is
                # uninitialized and must be flipped in the bitmap and initialized as empty in the
                # mapping
                if tick not in working_tick_data:
                    working_tick_data[tick] = UniswapV3LiquidityAtTick(
                        liquidity_net=0,
                        liquidity_gross=0,
                        block=state_block,
                    )
                    flip_tick(
                        tick_bitmap=working_tick_bitmap,
                        sparse=self.sparse_liquidity_map,
                        tick=tick,
                        tick_spacing=self.tick_spacing,
                        update_block=state_block,
                    )

                current_liquidity_net = working_tick_data[tick].liquidity_net
                current_liquidity_gross = working_tick_data[tick].liquidity_gross

                new_liquidity_gross = current_liquidity_gross + update.liquidity
                assert new_liquidity_gross >= 0, (
                    f"Negative gross liquidity for pool {self.address}!"
                )

                if new_liquidity_gross == 0:
                    # Delete tick from the map if there is no remaining liquidity referencing it,
                    # and flip it in the bitmap
                    del working_tick_data[tick]
                    flip_tick(
                        tick_bitmap=working_tick_bitmap,
                        sparse=self.sparse_liquidity_map,
                        tick=tick,
                        tick_spacing=self.tick_spacing,
                        update_block=state_block,
                    )
                    continue

                # Liquidity positions include the lower tick, but exclude the upper tick.
                if tick == update.tick_lower:
                    new_liquidity_net = current_liquidity_net + update.liquidity
                else:
                    new_liquidity_net = current_liquidity_net - update.liquidity

                working_tick_data[tick] = UniswapV3LiquidityAtTick(
                    liquidity_net=new_liquidity_net,
                    liquidity_gross=new_liquidity_gross,
                    block=state_block,
                )

            working_state = dataclasses.replace(
                self.state,
                liquidity=working_liquidity,
                tick_data=working_tick_data,
                tick_bitmap=working_tick_bitmap,
                block=max(self.update_block, state_block),
            )

            self._state_mgr.push_state(working_state)

            self._notify_subscribers(
                message=UniswapV3PoolStateUpdated(working_state),
            )

    def get_arbitrage_helpers(
        self,
    ) -> tuple[AbstractArbitrage, ...]:
        """
        Get all arbitrage helpers subscribed to this pool.
        """
        return tuple(
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, AbstractArbitrage)
            if self in subscriber.swap_pools
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

        A V3 pool encodes the token1/token0 exchange rate in `sqrt_price_x96`, so it can be directly
        obtained.
        """

        if token not in self.tokens:
            raise DegenbotValueError(message=f"Unknown token {token}")

        state = self.state if override_state is None else override_state

        return (
            exchange_rate_from_sqrt_price_x96(state.sqrt_price_x96)
            if token == self.token1
            else 1 / exchange_rate_from_sqrt_price_x96(state.sqrt_price_x96)
        )

    def get_nominal_price(
        self,
        token: Erc20Token,
        override_state: PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal price for the given token, expressed in units of the other, corrected for
        decimal place values.
        """

        return 1 / self.get_nominal_rate(token, override_state=override_state)

    def get_nominal_rate(
        self,
        token: Erc20Token,
        override_state: PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal rate of exchange for a swap **withdrawing** the given token, corrected for
        both token decimal place values.
        """

        return self.get_absolute_exchange_rate(token=token, override_state=override_state) * (
            Fraction(10**self.token1.decimals, 10**self.token0.decimals)
            if token == self.token0
            else Fraction(10**self.token0.decimals, 10**self.token1.decimals)
        )

    def discard_states_before_block(self, block: BlockNumber) -> None:
        """Discard cached states earlier than the given block."""
        with self._state_lock:
            self._state_mgr.discard_states_before_block(block)

    def restore_state_before_block(self, block: BlockNumber) -> None:
        """Restore the last pool state recorded prior to a target block."""
        with self._state_lock:
            restored: UniswapV3PoolState = self._state_mgr.restore_state_before_block(block)
            self._notify_subscribers(message=UniswapV3PoolStateUpdated(restored))

    def simulate_exact_input_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        sqrt_price_limit_x96: int | None = None,
        override_state: PoolState | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """
        Simulate an exact input swap.
        """

        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message=f"Unknown token {token_in}")

        zero_for_one = token_in == self.token0

        try:
            amount0_delta, amount1_delta, end_sqrt_price_x96, end_liquidity, end_tick = (
                self._calculate_swap(
                    zero_for_one=zero_for_one,
                    amount_specified=token_in_quantity,
                    sqrt_price_limit_x96=(
                        sqrt_price_limit_x96
                        if sqrt_price_limit_x96 is not None
                        else (MIN_SQRT_RATIO + 1 if zero_for_one else MAX_SQRT_RATIO - 1)
                    ),
                    override_state=override_state,
                )
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e
        else:
            return UniswapV3PoolSimulationResult(
                amount0_delta=amount0_delta,
                amount1_delta=amount1_delta,
                initial_state=override_state if override_state is not None else self.state,
                final_state=dataclasses.replace(
                    self.state,
                    liquidity=end_liquidity,
                    sqrt_price_x96=end_sqrt_price_x96,
                    tick=end_tick,
                    block=self.update_block if override_state is None else None,
                ),
            )

    def simulate_exact_output_swap(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        sqrt_price_limit_x96: int | None = None,
        override_state: PoolState | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """
        Simulate an exact output swap.
        """

        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message=f"Unknown token {token_out}")

        zero_for_one = token_out == self.token1

        try:
            amount0_delta, amount1_delta, end_sqrtprice, end_liquidity, end_tick = (
                self._calculate_swap(
                    zero_for_one=zero_for_one,
                    amount_specified=-token_out_quantity,
                    sqrt_price_limit_x96=(
                        sqrt_price_limit_x96
                        if sqrt_price_limit_x96 is not None
                        else (MIN_SQRT_RATIO + 1 if zero_for_one else MAX_SQRT_RATIO - 1)
                    ),
                    override_state=override_state,
                )
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e
        else:
            return UniswapV3PoolSimulationResult(
                amount0_delta=amount0_delta,
                amount1_delta=amount1_delta,
                initial_state=override_state if override_state is not None else self.state,
                final_state=dataclasses.replace(
                    self.state,
                    liquidity=end_liquidity,
                    sqrt_price_x96=end_sqrtprice,
                    tick=end_tick,
                    block=self.update_block if override_state is None else None,
                ),
            )

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,  # noqa: ARG002
        state_override: UniswapV3PoolState | None = None,
    ) -> SimulationResult:
        if token_in == self.token0.address:
            token_in_obj = self.token0
        elif token_in == self.token1.address:
            token_in_obj = self.token1
        else:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        result = self.simulate_exact_input_swap(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=state_override,
        )
        zero_for_one = token_in_obj == self.token0
        amount_out = -result.amount1_delta if zero_for_one else -result.amount0_delta
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=result.initial_state,
            final_state=result.final_state,
        )

    def simulate_swap_for_output(
        self,
        token_in: ChecksumAddress,  # noqa: ARG002
        token_out: ChecksumAddress,
        amount_out: int,
        state_override: UniswapV3PoolState | None = None,
    ) -> SimulationResult:
        if token_out == self.token0.address:
            token_out_obj = self.token0
        elif token_out == self.token1.address:
            token_out_obj = self.token1
        else:
            raise DegenbotValueError(message=f"token_out {token_out} not in pool")

        result = self.simulate_exact_output_swap(
            token_out=token_out_obj,
            token_out_quantity=amount_out,
            override_state=state_override,
        )
        zero_for_one = token_out_obj == self.token1
        amount_in = result.amount0_delta if zero_for_one else result.amount1_delta
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=result.initial_state,
            final_state=result.final_state,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001, ARG002
        return Fraction(self.fee, self.FEE_DENOMINATOR)

    _TICK_RANGE_CACHE: dict[tuple[str, int, bool], tuple[tuple[V3TickRangeInfo, ...], int] | None]
    _MAX_TICK_RANGE_CACHE_SIZE: int = 128

    def _get_tick_ranges(
        self,
        zero_for_one: bool,  # noqa: FBT001
        max_ranges: int = 3,
    ) -> tuple[tuple[V3TickRangeInfo, ...], int] | None:
        if not hasattr(self, "_TICK_RANGE_CACHE"):
            self._TICK_RANGE_CACHE = {}

        cache_key = (str(self.address), self.tick, zero_for_one)

        if cache_key in self._TICK_RANGE_CACHE:
            return self._TICK_RANGE_CACHE[cache_key]

        result = self._compute_tick_ranges(zero_for_one=zero_for_one, max_ranges=max_ranges)

        if len(self._TICK_RANGE_CACHE) >= self._MAX_TICK_RANGE_CACHE_SIZE:
            self._TICK_RANGE_CACHE.clear()

        self._TICK_RANGE_CACHE[cache_key] = result
        return result

    def _compute_tick_ranges(
        self,
        *,
        zero_for_one: bool,
        max_ranges: int = 3,
    ) -> tuple[tuple[V3TickRangeInfo, ...], int] | None:
        if getattr(self, "sparse_liquidity_map", True):
            return None

        tick_data = getattr(self, "tick_data", None)
        tick_bitmap = getattr(self, "tick_bitmap", None)
        tick_spacing = getattr(self, "tick_spacing", 0)

        if tick_data is None or tick_bitmap is None or tick_spacing == 0:
            return None

        current_tick = self.tick
        less_than_or_equal = not zero_for_one

        try:
            ticks_along_path = gen_ticks(
                tick_data=tick_data,
                starting_tick=current_tick,
                tick_spacing=tick_spacing,
                less_than_or_equal=less_than_or_equal,
            )
        except (ValueError, KeyError, IndexError):
            return None

        initialized_ticks: list[int] = []
        try:
            for tick, is_initialized in ticks_along_path:
                clamped_tick = max(MIN_TICK, tick) if less_than_or_equal else min(MAX_TICK, tick)
                if clamped_tick != tick:
                    break
                if len(initialized_ticks) >= max_ranges + 1:
                    break
                if is_initialized or tick == current_tick:
                    initialized_ticks.append(tick)
        except StopIteration:
            pass

        if len(initialized_ticks) < 2:  # noqa: PLR2004
            return None

        ranges: list[V3TickRangeInfo] = []
        current_idx = 0

        for i in range(len(initialized_ticks) - 1):
            if zero_for_one:
                tick_lower = initialized_ticks[i + 1]
                tick_upper = initialized_ticks[i]
            else:
                tick_lower = initialized_ticks[i]
                tick_upper = initialized_ticks[i + 1]

            tick_info = tick_data.get(tick_lower if zero_for_one else tick_upper)
            liquidity = tick_info.liquidity_net if tick_info else self.liquidity

            sqrt_price_lower = int(get_sqrt_ratio_at_tick(tick_lower))
            sqrt_price_upper = int(get_sqrt_ratio_at_tick(tick_upper))

            ranges.append(
                V3TickRangeInfo(
                    tick_lower=tick_lower,
                    tick_upper=tick_upper,
                    liquidity=liquidity,
                    sqrt_price_lower=sqrt_price_lower,
                    sqrt_price_upper=sqrt_price_upper,
                )
            )

            if tick_lower <= current_tick < tick_upper:
                current_idx = i

        if len(ranges) < 1:
            return None

        return (tuple(ranges), current_idx)

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: UniswapV3PoolState | None = None,
    ) -> HopType:
        from degenbot.uniswap.v3_libraries.functions import v3_virtual_reserves  # noqa: PLC0415

        state = state_override or self.state
        fee = self.extract_fee(zero_for_one=zero_for_one)
        reserve_in, reserve_out = v3_virtual_reserves(
            liquidity=state.liquidity,
            sqrt_price_x96=state.sqrt_price_x96,
            zero_for_one=zero_for_one,
        )

        if state_override is None:
            tick_ranges = self._get_tick_ranges(zero_for_one)
            if tick_ranges is not None:
                ranges, current_idx = tick_ranges
                return BoundedProductHop(
                    reserve_in=reserve_in,
                    reserve_out=reserve_out,
                    fee=fee,
                    liquidity=state.liquidity,
                    sqrt_price=state.sqrt_price_x96,
                    tick_lower=state.tick,
                    tick_upper=state.tick,
                    tick_ranges=ranges,
                    current_range_index=current_idx,
                )

        return BoundedProductHop(
            reserve_in=reserve_in,
            reserve_out=reserve_out,
            fee=fee,
            liquidity=state.liquidity,
            sqrt_price=state.sqrt_price_x96,
            tick_lower=state.tick,
            tick_upper=state.tick,
        )
