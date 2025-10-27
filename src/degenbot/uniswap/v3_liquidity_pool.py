# TODO: add event prototype exporter method and handler for callbacks
import dataclasses
from bisect import bisect_left
from fractions import Fraction
from threading import Lock
from typing import TYPE_CHECKING, Any, Self, TypedDict, cast
from weakref import WeakSet

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from eth_typing import ChecksumAddress
from sqlalchemy import select
from sqlalchemy.orm import Session, scoped_session
from web3 import Web3
from web3.exceptions import ContractLogicError
from web3.types import BlockIdentifier, TxParams

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.database import db_session
from degenbot.database.models.pools import AbstractUniswapV3Pool, LiquidityPoolTable
from degenbot.erc20 import Erc20Token, Erc20TokenManager
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.evm import EVMRevertError
from degenbot.exceptions.liquidity_pool import (
    AddressMismatch,
    ExternalUpdateError,
    IncompleteSwap,
    LateUpdateError,
    LiquidityMapWordMissing,
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
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS, UniswapV3ExchangeDeployment
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v3_functions import (
    exchange_rate_from_sqrt_price_x96,
    generate_v3_pool_address,
    get_tick_word_and_bit_position,
)
from degenbot.uniswap.v3_libraries.swap_math import compute_swap_step
from degenbot.uniswap.v3_libraries.tick_bitmap import (
    flip_tick,
    gen_ticks,
    next_initialized_tick_within_one_word,
)
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
    get_tick_at_sqrt_ratio,
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


@dataclasses.dataclass(slots=True, frozen=True)
class LiquidityRangeCacheKey:
    """
    Cache key for liquidity range swap calculations.
    """

    exact_input: bool
    tick_lower: int
    tick_upper: int
    liquidity: int
    zero_for_one: bool
    price_start: SqrtPriceX96
    price_end: SqrtPriceX96


@dataclasses.dataclass(slots=True, frozen=True)
class LiquidityRangeCacheValue:
    """
    Cached result of a complete liquidity range consumption.
    """

    amount_in: int
    amount_out: int
    fee_amount: int
    price_end: SqrtPriceX96
    amount_required: int  # Total amount needed to consume this range


def get_pool_from_database(
    address: ChecksumAddress,
    chain_id: int,
    session: Session | scoped_session[Session] = db_session,
) -> AbstractUniswapV3Pool | None:
    return session.scalar(
        select(LiquidityPoolTable).where(
            LiquidityPoolTable.address == address,
            LiquidityPoolTable.chain == chain_id,
        )
    )  # type: ignore[return-value]


class UniswapV3Pool(PublisherMixin, AbstractLiquidityPool):
    type PoolState = UniswapV3PoolState
    _state: PoolState
    _state_cache: BoundedCache[BlockNumber, PoolState]

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

    @dataclasses.dataclass(slots=True, eq=False)
    class SwapState:
        amount_specified_remaining: int
        amount_calculated: int
        sqrt_price_x96: int
        tick: int
        liquidity: int

        def __post_init__(self) -> None:
            assert self.liquidity >= 0

    @dataclasses.dataclass(slots=True, eq=False)
    class StepComputations:
        sqrt_price_start_x96: int = 0
        sqrt_price_next_x96: int = 0
        tick_next: int = 0
        initialized: bool = False
        amount_in: int = 0
        amount_out: int = 0
        fee_amount: int = 0

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
        state_block: BlockNumber | None = None,
        verify_address: bool = True,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> None:
        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else connection_manager.default_chain_id
        w3 = connection_manager.get_web3(self.chain_id)
        state_block = state_block if state_block is not None else w3.eth.block_number
        self._initial_state_block = state_block

        pool_from_db = get_pool_from_database(address=self.address, chain_id=self.chain_id)

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
                    self.get_immutable_pool_values(w3=w3)
                )

            except (ContractLogicError, DecodingError) as exc:
                # Contracts differ slightly across Uniswap V3 forks, so decoding may fail. Catch
                # this here and raise as a pool-specific exception
                raise LiquidityPoolError(message="Could not decode contract data") from exc

        try:
            sqrt_price_x96, tick, liquidity = self.get_mutable_pool_values(
                w3=w3, state_block=state_block
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

        token_manager = Erc20TokenManager(chain_id=self.chain_id)
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

        _tick_bitmap = (
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

        _tick_data = (
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
                tick_bitmap=_tick_bitmap,
                tick_data=_tick_data,
                block_number=state_block,
            )

        self._state = self.PoolState.__value__(
            address=self.address,
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            tick_bitmap=_tick_bitmap,
            tick_data=_tick_data,
            block=state_block,
        )
        self._state_cache = BoundedCache(max_items=state_cache_depth)
        self._state_cache[self.update_block] = self.state
        self._state_lock = Lock()

        # Liquidity range consumption cache
        self._swap_step_cache: dict[LiquidityRangeCacheKey, LiquidityRangeCacheValue] = {}
        self._swap_step_cache_lock = Lock()

        pool_registry.add(
            pool_address=self.address,
            chain_id=self.chain_id,
            pool=self,
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
            "_state_cache",
            "_state_lock",
            "_subscribers",
            "_swap_step_cache_lock",
        }

        with self._state_lock:
            return {
                k: (v.copy() if k in copied_attributes else v)
                for k, v in self.__dict__.items()
                if k not in dropped_attributes
            }

    def __setstate__(self, state: dict[str, Any]) -> None:
        state["_state_lock"] = Lock()
        state["_swap_step_cache_lock"] = Lock()
        self.__dict__ = state

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

        if amount_specified == 0:  # pragma: no branch
            raise EVMRevertError(error="AS")

        exact_input = amount_specified > 0

        if override_state is not None:
            liquidity_start = override_state.liquidity
            sqrt_price_x96_start = override_state.sqrt_price_x96
            tick_start = override_state.tick
            tick_bitmap = override_state.tick_bitmap
            tick_data = override_state.tick_data
        else:
            liquidity_start = self.liquidity
            sqrt_price_x96_start = self.sqrt_price_x96
            tick_start = self.tick
            # The tick bitmap and tick data dictionaries accessed through the attribute are copies,
            # so they can be freely modified without corrupting the state
            tick_bitmap = self.tick_bitmap
            tick_data = self.tick_data

        assert liquidity_start >= 0

        if zero_for_one and not (
            MIN_SQRT_RATIO < sqrt_price_limit_x96 < sqrt_price_x96_start
        ):  # pragma: no cover
            raise EVMRevertError(error="SPL")

        if not zero_for_one and not (
            sqrt_price_x96_start < sqrt_price_limit_x96 < MAX_SQRT_RATIO
        ):  # pragma: no cover
            raise EVMRevertError(error="SPL")

        swap_state = self.SwapState(
            amount_specified_remaining=amount_specified,
            amount_calculated=0,
            sqrt_price_x96=sqrt_price_x96_start,
            tick=tick_start,
            liquidity=liquidity_start,
        )

        if not self.sparse_liquidity_map:
            # The liquidity mapping is complete. Optimize loop by building a generator that yields
            # ticks and initialization status along the swap path
            ticks_along_swap_path = gen_ticks(
                tick_data=tick_data,
                starting_tick=tick_start,
                tick_spacing=self.tick_spacing,
                less_than_or_equal=zero_for_one,
            )

        step = self.StepComputations()

        while (
            swap_state.amount_specified_remaining != 0
            and swap_state.sqrt_price_x96 != sqrt_price_limit_x96
        ):
            step.sqrt_price_start_x96 = swap_state.sqrt_price_x96

            if not self.sparse_liquidity_map:
                step.tick_next, step.initialized = next(ticks_along_swap_path)
            else:
                try:
                    step.tick_next, step.initialized = next_initialized_tick_within_one_word(
                        tick_bitmap=tick_bitmap,
                        tick_data=tick_data,
                        tick=swap_state.tick,
                        tick_spacing=self.tick_spacing,
                        less_than_or_equal=zero_for_one,
                    )
                except LiquidityMapWordMissing as exc:
                    missing_word = exc.word
                    self._fetch_and_populate_initialized_ticks(
                        word_position=missing_word,
                        tick_bitmap=tick_bitmap,
                        tick_data=tick_data,
                        block_number=self.update_block,
                    )
                    continue

            # Ensure that we do not overshoot the min/max tick, as the tick bitmap is not aware of
            # these bounds
            step.tick_next = (
                max(MIN_TICK, step.tick_next)  # descending ticks
                if zero_for_one
                else min(MAX_TICK, step.tick_next)  # ascending ticks
            )

            step.sqrt_price_next_x96 = get_sqrt_ratio_at_tick(step.tick_next)

            # Determine the current liquidity range boundaries
            if zero_for_one:
                tick_lower, tick_upper = (
                    step.tick_next,
                    step.tick_next + self.tick_spacing,
                )
            else:
                tick_lower, tick_upper = (
                    step.tick_next - self.tick_spacing,
                    step.tick_next,
                )
            assert tick_lower < tick_upper, f"{tick_lower} should be < {tick_upper}"

            cached_result = self._swap_step_cache.get(
                LiquidityRangeCacheKey(
                    exact_input=exact_input,
                    tick_lower=tick_lower,
                    tick_upper=tick_upper,
                    liquidity=swap_state.liquidity,
                    zero_for_one=zero_for_one,
                    price_start=step.sqrt_price_start_x96,
                    price_end=step.sqrt_price_next_x96,
                )
            )

            if (
                cached_result
                and abs(swap_state.amount_specified_remaining) >= cached_result.amount_required
            ):
                swap_state.sqrt_price_x96 = cached_result.price_end
                step.amount_in = cached_result.amount_in
                step.amount_out = cached_result.amount_out
                step.fee_amount = cached_result.fee_amount
            else:
                # compute values to swap to the target tick, price limit, or point where
                # the input/output amount is exhausted
                swap_state.sqrt_price_x96, step.amount_in, step.amount_out, step.fee_amount = (
                    compute_swap_step(
                        sqrt_ratio_x96_current=swap_state.sqrt_price_x96,
                        sqrt_ratio_x96_target=(
                            sqrt_price_limit_x96
                            if (
                                (
                                    zero_for_one is True
                                    and step.sqrt_price_next_x96 < sqrt_price_limit_x96
                                )
                                or (
                                    zero_for_one is False
                                    and step.sqrt_price_next_x96 > sqrt_price_limit_x96
                                )
                            )
                            else step.sqrt_price_next_x96
                        ),
                        liquidity=swap_state.liquidity,
                        amount_remaining=swap_state.amount_specified_remaining,
                        fee_pips=self.fee,
                    )
                )

            if exact_input:
                swap_state.amount_specified_remaining -= step.amount_in + step.fee_amount
                swap_state.amount_calculated -= step.amount_out
            else:
                swap_state.amount_specified_remaining += step.amount_out
                swap_state.amount_calculated += step.amount_in + step.fee_amount

            if swap_state.sqrt_price_x96 == step.sqrt_price_next_x96:  # pragma: no branch
                # Cache the step if it consumed the liquidity range
                if not cached_result:
                    self._cache_swap_step(
                        tick_lower=tick_lower,
                        tick_upper=tick_upper,
                        liquidity=swap_state.liquidity,
                        zero_for_one=zero_for_one,
                        exact_input=exact_input,
                        amount_in=step.amount_in,
                        amount_out=step.amount_out,
                        fee_amount=step.fee_amount,
                        price_start=step.sqrt_price_start_x96,
                        price_end=swap_state.sqrt_price_x96,
                    )
                # If the next tick is initialized, adjust the in-range liquidity
                if step.initialized:
                    liquidity_net_at_next_tick = tick_data[step.tick_next].liquidity_net
                    swap_state = dataclasses.replace(
                        swap_state,
                        liquidity=swap_state.liquidity
                        + (
                            -liquidity_net_at_next_tick
                            if zero_for_one
                            else liquidity_net_at_next_tick
                        ),
                    )
                swap_state.tick = step.tick_next - 1 if zero_for_one else step.tick_next

            elif swap_state.sqrt_price_x96 != step.sqrt_price_start_x96:  # pragma: no branch
                # Recompute unless we're on a lower tick boundary (i.e. already transitioned ticks),
                # and haven't moved
                swap_state.tick = get_tick_at_sqrt_ratio(swap_state.sqrt_price_x96)

        amount0, amount1 = (
            (
                amount_specified - swap_state.amount_specified_remaining,
                swap_state.amount_calculated,
            )
            if zero_for_one == exact_input
            else (
                swap_state.amount_calculated,
                amount_specified - swap_state.amount_specified_remaining,
            )
        )

        return amount0, amount1, swap_state.sqrt_price_x96, swap_state.liquidity, swap_state.tick

    def get_tick_bitmap_at_word(
        self,
        w3: Web3,
        word_position: int,
        block_identifier: BlockIdentifier,
    ) -> int:
        (bitmap_at_word,) = raw_call(
            w3=w3,
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
        w3: Web3,
        word_position: int,
        block_identifier: BlockIdentifier,
    ) -> list[tuple[int, int, int]]:
        bitmap_at_word = self.get_tick_bitmap_at_word(
            w3=w3,
            word_position=word_position,
            block_identifier=block_identifier,
        )

        active_ticks = [
            ((word_position << 8) + i) * self.tick_spacing
            for i in range(256)
            if bitmap_at_word & (1 << i) > 0
        ]

        with w3.batch_requests() as batch:
            for tick in active_ticks:
                batch.add(
                    w3.eth.call(
                        transaction=TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="ticks(int24)",
                                function_arguments=[tick],
                            ),
                        ),
                        block_identifier=block_identifier,
                    )
                )
            results = batch.execute()

        populated_ticks = []
        for tick, result in zip(active_ticks, results, strict=True):
            liquidity_gross, liquidity_net, *_ = eth_abi.abi.decode(
                types=self.TICK_STRUCT_TYPES,
                data=cast("HexBytes", result),
            )
            populated_ticks.append((tick, liquidity_gross, liquidity_net))

        return populated_ticks

    def _fetch_and_populate_initialized_ticks(
        self,
        word_position: int,
        tick_bitmap: InitializedTickMap,
        tick_data: LiquidityMap,
        block_number: BlockNumber | None = None,
    ) -> None:
        """
        Update the supplied tick bitmap with initialized tick values within a specified word
        position. A word is divided into 256 ticks, spaced at a fixed interval.
        """

        w3 = connection_manager.get_web3(self.chain_id)

        if block_number is None:
            block_number = w3.eth.get_block_number()

        _tick_bitmap: int = 0
        _tick_data: list[tuple[Tick, LiquidityGross, LiquidityNet]] = []
        _tick_bitmap = self.get_tick_bitmap_at_word(
            w3=w3,
            word_position=word_position,
            block_identifier=block_number,
        )
        if _tick_bitmap != 0:
            _tick_data = self.get_populated_ticks_in_word(
                w3=w3,
                word_position=word_position,
                block_identifier=block_number,
            )

        tick_bitmap[word_position] = UniswapV3BitmapAtWord(
            bitmap=_tick_bitmap,
            block=block_number,
        )
        for tick, liquidity_gross, liquidity_net in _tick_data:
            tick_data[tick] = UniswapV3LiquidityAtTick(
                liquidity_net=liquidity_net,
                liquidity_gross=liquidity_gross,
                block=block_number,
            )

    def _cache_swap_step(
        self,
        *,
        tick_lower: int,
        tick_upper: int,
        liquidity: int,
        zero_for_one: bool,
        exact_input: bool,
        amount_in: int,
        amount_out: int,
        fee_amount: int,
        price_start: SqrtPriceX96,
        price_end: SqrtPriceX96,
    ) -> None:
        """
        Cache the input and output of a swap step that reached a lower or upper limit of a liquidity
        range.
        """
        cache_key = LiquidityRangeCacheKey(
            exact_input=exact_input,
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity=liquidity,
            zero_for_one=zero_for_one,
            price_start=price_start,
            price_end=price_end,
        )

        # Calculate the total amount required to consume this range
        amount_required = (amount_in + fee_amount) if exact_input else amount_out

        cache_value = LiquidityRangeCacheValue(
            amount_in=amount_in,
            amount_out=amount_out,
            fee_amount=fee_amount,
            price_end=price_end,
            amount_required=amount_required,
        )

        with self._swap_step_cache_lock:
            self._swap_step_cache[cache_key] = cache_value

    def _invalidate_range_cache_for_ticks(self, tick_lower: int, tick_upper: int) -> None:
        """
        Invalidate cache entries that overlap with the specified tick range.

        This should be called when liquidity is updated in a range to ensure
        cached calculations remain valid.
        """
        with self._swap_step_cache_lock:
            self._swap_step_cache = {
                key: value
                for key, value in self._swap_step_cache.items()
                # Preserve only the cached values outside of the updated range
                # TODO: determine if one or more inequalities can be <= or >=
                if (tick_lower > key.tick_upper or tick_upper < key.tick_lower)
            }

    def _verified_address(self) -> ChecksumAddress:
        return generate_v3_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self.token0.address, self.token1.address),
            fee=self.fee,
            init_hash=self.init_hash,
        )

    def get_immutable_pool_values(
        self,
        w3: Web3,
    ) -> tuple[
        str,  # factory
        tuple[str, str],  # tokens
        int,  # fee
        int,  # tick spacing
    ]:
        try:
            with w3.batch_requests() as batch:
                batch.add_mapping(
                    {
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
                                    function_prototype="fee()",
                                    function_arguments=None,
                                ),
                            ),
                            TxParams(
                                to=self.address,
                                data=encode_function_calldata(
                                    function_prototype="tickSpacing()",
                                    function_arguments=None,
                                ),
                            ),
                        ],
                    }
                )

                factory, token0, token1, fee, tick_spacing = batch.execute()

            (factory,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", factory))
            (token0,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", token0))
            (token1,) = eth_abi.abi.decode(types=["address"], data=cast("HexBytes", token1))
            (fee,) = eth_abi.abi.decode(types=["uint256"], data=cast("HexBytes", fee))
            (tick_spacing,) = eth_abi.abi.decode(
                types=["uint256"], data=cast("HexBytes", tick_spacing)
            )

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
        w3: Web3,
        state_block: BlockNumber,
    ) -> tuple[SqrtPriceX96, Tick, Liquidity]:
        try:
            with w3.batch_requests() as batch:
                # This calls use a specific block so the mutable state values are consistent
                batch.add(
                    w3.eth.call(
                        transaction=TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="slot0()",
                                function_arguments=None,
                            ),
                        ),
                        block_identifier=state_block,
                    )
                )
                batch.add(
                    w3.eth.call(
                        transaction=TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="liquidity()",
                                function_arguments=None,
                            ),
                        ),
                        block_identifier=state_block,
                    )
                )

                slot0, liquidity = batch.execute()

            price, tick, *_ = eth_abi.abi.decode(
                types=self.SLOT0_STRUCT_TYPES, data=cast("HexBytes", slot0)
            )
            (liquidity,) = eth_abi.abi.decode(types=["uint256"], data=cast("HexBytes", liquidity))

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
        return self.state.liquidity

    @property
    def sqrt_price_x96(self) -> int:
        return self.state.sqrt_price_x96

    @property
    def state(self) -> PoolState:
        return self._state

    @property
    def tick(self) -> int:
        return self.state.tick

    @property
    def tick_bitmap(self) -> InitializedTickMap:
        return self.state.tick_bitmap.copy()

    @property
    def tick_data(self) -> LiquidityMap:
        return self.state.tick_data.copy()

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
        if self.sparse_liquidity_map:
            # Liquidity cannot be checked with a sparse mapping, so default to True
            return True

        if state.tick_data == {}:
            # The pool has no liquidity
            return False

        if state.sqrt_price_x96 == 0:
            # The pool is not initialized
            assert state.tick_data == {}, (
                f"Found pool @ {self.address} with liquidity positions, but price=0!"
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

            state_updated = False

            w3 = connection_manager.get_web3(self.chain_id)
            block_number = block_number if block_number is not None else w3.eth.get_block_number()

            with w3.batch_requests() as batch:
                batch.add(
                    # This call uses a specific block so the mutable state values are consistent
                    w3.eth.call(
                        transaction=TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="slot0()",
                                function_arguments=None,
                            ),
                        ),
                        block_identifier=block_number,
                    )
                )
                batch.add(
                    # This call uses a specific block so the mutable state values are consistent
                    w3.eth.call(
                        transaction=TxParams(
                            to=self.address,
                            data=encode_function_calldata(
                                function_prototype="liquidity()",
                                function_arguments=None,
                            ),
                        ),
                        block_identifier=block_number,
                    )
                )

                slot0, liquidity = batch.execute()

            _sqrt_price_x96: int
            _tick: int
            _sqrt_price_x96, _tick, *_ = eth_abi.abi.decode(
                types=self.SLOT0_STRUCT_TYPES, data=cast("HexBytes", slot0)
            )

            _liquidity: int
            (_liquidity,) = eth_abi.abi.decode(types=["uint256"], data=cast("HexBytes", liquidity))

            state = dataclasses.replace(
                self.state,
                liquidity=_liquidity,
                sqrt_price_x96=_sqrt_price_x96,
                tick=_tick,
                block=block_number,
            )

            state_updated = state != self.state
            self._state = state
            self._state_cache[block_number] = state

            if state_updated:
                self._notify_subscribers(
                    message=UniswapV3PoolStateUpdated(state),
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

        _is_zero_for_one = token_out == self.token1

        try:
            amount0_delta, amount1_delta, *_ = self._calculate_swap(
                zero_for_one=_is_zero_for_one,
                amount_specified=-token_out_quantity,
                sqrt_price_limit_x96=(
                    MIN_SQRT_RATIO + 1 if _is_zero_for_one else MAX_SQRT_RATIO - 1
                ),
                override_state=override_state,
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e
        else:
            if _is_zero_for_one is True and -amount1_delta < token_out_quantity:
                raise IncompleteSwap(amount_in=amount0_delta, amount_out=-amount1_delta)
            if _is_zero_for_one is False and -amount0_delta < token_out_quantity:
                raise IncompleteSwap(amount_in=amount1_delta, amount_out=-amount0_delta)

            return amount0_delta if _is_zero_for_one else amount1_delta

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

        with self._state_lock:
            state_block = update.block_number

            state = dataclasses.replace(
                self.state,
                liquidity=update.liquidity,
                sqrt_price_x96=update.sqrt_price_x96,
                tick=update.tick,
                block=state_block,
            )

            updated_state = state != self.state

            self._state = state
            self._state_cache[state_block] = state

            if updated_state:
                self._notify_subscribers(
                    message=UniswapV3PoolStateUpdated(state),
                )

            return updated_state

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
            # Invalidate cache entries for the affected range before applying the update
            self._invalidate_range_cache_for_ticks(update.tick_lower, update.tick_upper)

            state_block = update.block_number

            # The tick bitmap and tick data dictionaries accessed through the attribute are copies,
            # so they can be freely modified without corrupting the state
            _tick_bitmap = self.tick_bitmap
            _tick_data = self.tick_data

            _liquidity = self.liquidity

            assert _liquidity >= 0, (
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
                _liquidity += update.liquidity
                assert _liquidity >= 0, (
                    f"In-range liquidity adjustment violated invariant: pool {self.address} {self.tick=} {self.liquidity=} {self.update_block=} {update=}"  # noqa: E501
                )

            for tick in (update.tick_lower, update.tick_upper):
                tick_word, _ = get_tick_word_and_bit_position(tick, self.tick_spacing)

                if self.sparse_liquidity_map and tick_word not in _tick_bitmap:
                    # The liquidity map at the affected word must be complete prior to changing the
                    # status of any tick
                    self._fetch_and_populate_initialized_ticks(
                        word_position=tick_word,
                        tick_bitmap=_tick_bitmap,
                        tick_data=_tick_data,
                        block_number=(
                            # Populate the liquidity data from the previous block
                            state_block - 1
                        ),
                    )

                # Get the liquidity info for this tick. If the mapping is empty at this tick, it is
                # uninitialized and must be flipped in the bitmap and initialized as empty in the
                # mapping
                if tick not in _tick_data:
                    _tick_data[tick] = UniswapV3LiquidityAtTick(
                        liquidity_net=0,
                        liquidity_gross=0,
                        block=state_block,
                    )
                    flip_tick(
                        tick_bitmap=_tick_bitmap,
                        sparse=self.sparse_liquidity_map,
                        tick=tick,
                        tick_spacing=self.tick_spacing,
                        update_block=state_block,
                    )

                current_liquidity_net = _tick_data[tick].liquidity_net
                current_liquidity_gross = _tick_data[tick].liquidity_gross

                new_liquidity_gross = current_liquidity_gross + update.liquidity
                assert new_liquidity_gross >= 0, (
                    f"Negative gross liquidity for pool {self.address}!"
                )

                if new_liquidity_gross == 0:
                    # Delete tick from the map if there is no remaining liquidity referencing it,
                    # and flip it in the bitmap
                    del _tick_data[tick]
                    flip_tick(
                        tick_bitmap=_tick_bitmap,
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

                _tick_data[tick] = UniswapV3LiquidityAtTick(
                    liquidity_net=new_liquidity_net,
                    liquidity_gross=new_liquidity_gross,
                    block=state_block,
                )

            state = dataclasses.replace(
                self.state,
                liquidity=_liquidity,
                tick_data=_tick_data,
                tick_bitmap=_tick_bitmap,
                block=max(self.update_block, state_block),
            )
            self._state = state
            self._state_cache[state_block] = state
            self._notify_subscribers(
                message=UniswapV3PoolStateUpdated(state),
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

        A V4 pool encodes the token1/token0 exchange rate in `sqrt_price_x96`, so it can be directly
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

    def discard_states_before_block(
        self,
        block: BlockNumber,
    ) -> None:
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
                raise NoPoolStateAvailable(block)

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

        # Find the index for the most recent pool state PRIOR to the requested
        # block number.
        #
        # e.g. Calling restore_state_before_block(block=104) for a pool with
        # states at blocks 100, 101, 102, 103, 104. `bisect_left()` returns
        # block_index=3, since block 104 is at index=4. The state held at
        # index=3 is for block 103.

        with self._state_lock:
            known_blocks = sorted(self._state_cache.keys())
            block_index = bisect_left(known_blocks, block)

            if block_index == 0:
                raise NoPoolStateAvailable(block)

            # The last known state already meets the criterion, so return early
            if block_index == len(known_blocks):
                return

            # Remove states at and after the specified block
            for known_block in known_blocks[block_index:]:
                del self._state_cache[known_block]

            self._update_block, self._state = list(self._state_cache.items())[-1]

        self._notify_subscribers(
            message=UniswapV3PoolStateUpdated(self.state),
        )

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

    def clear_swap_step_cache(self) -> None:
        """
        Clear all cached liquidity range calculations.

        This can be useful for memory management or when you want to ensure
        fresh calculations.
        """
        with self._swap_step_cache_lock:
            self._swap_step_cache.clear()

    def get_range_cache_stats(self) -> dict[str, int]:
        """
        Get statistics about the liquidity range cache.

        Returns:
            Dictionary containing cache size and other metrics.
        """
        with self._swap_step_cache_lock:
            return {
                "cache_size": len(self._swap_step_cache),
            }
