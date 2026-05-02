# ruff: noqa: PLR0904


import dataclasses
from collections import deque
from collections.abc import Sequence
from enum import Enum
from fractions import Fraction
from threading import Lock
from typing import Any, Final, cast
from weakref import WeakSet

import eth_abi.abi
from eth_abi.exceptions import DecodingError
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from sqlalchemy import select
from sqlalchemy.orm import Session, scoped_session
from web3 import Web3
from web3.exceptions import ContractLogicError
from web3.types import BlockIdentifier

from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import connection_manager
from degenbot.constants import ZERO_ADDRESS
from degenbot.database import db_session
from degenbot.database.models.pools import (
    PoolManagerTable,
    UniswapV4PoolTable,
    UniswapV4PoolTableBase,
)
from degenbot.erc20 import Erc20Token, Erc20TokenManager
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.evm import EVMRevertError
from degenbot.exceptions.liquidity_pool import (
    ExternalUpdateError,
    IncompleteSwap,
    LateUpdateError,
    LiquidityPoolError,
    PossibleInaccurateResult,
)
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.provider import ProviderAdapter
from degenbot.registry import managed_pool_registry
from degenbot.types.abstract import AbstractArbitrage, AbstractConcentratedLiquidityPool
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import (
    AbstractPublisherMessage,
    Publisher,
    PublisherMixin,
    Subscriber,
)
from degenbot.types.hop_types import HopType
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot, MissingLiquidityData
from degenbot.uniswap.concentrated.state_manager import ConcentratedLiquidityStateManager
from degenbot.uniswap.concentrated.v4_simulator import calculate_swap as _v4_swap
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v3_functions import (
    exchange_rate_from_sqrt_price_x96,
    get_tick_word_and_bit_position,
)
from degenbot.uniswap.v3_types import BitmapWord, Liquidity, LiquidityGross, LiquidityNet, Pip, Tick
from degenbot.uniswap.v4_libraries.tick_bitmap import flip_tick
from degenbot.uniswap.v4_libraries.tick_math import MAX_SQRT_PRICE, MIN_SQRT_PRICE
from degenbot.uniswap.v4_types import (
    FeeToProtocol,
    InitializedTickMap,
    LiquidityMap,
    SwapFee,
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
    UniswapV4PoolExternalUpdate,
    UniswapV4PoolKey,
    UniswapV4PoolLiquidityMappingUpdate,
    UniswapV4PoolState,
    UniswapV4PoolStateUpdated,
)


@dataclasses.dataclass(slots=True)
class SwapResult:
    sqrt_price_x96: int
    tick: int
    liquidity: int


@dataclasses.dataclass(slots=True, frozen=True)
class SwapDelta:
    currency0: int
    currency1: int

    @property
    def amount_in(self) -> int:
        "The deposited token amount."
        return -min(self.currency0, self.currency1)

    @property
    def amount_out(self) -> int:
        "The withdrawn token amount."
        return max(self.currency0, self.currency1)


@dataclasses.dataclass(slots=True, frozen=True)
class ProtocolFee:
    zero_for_one: int
    one_for_zero: int


@dataclasses.dataclass(slots=True, frozen=True)
class Slot0:
    sqrt_price_x96: int
    tick: int
    protocol_fee: ProtocolFee
    lp_fee: int


PIPS_DENOMINATOR = 1_000_000
NATIVE_CURRENCY_ADDRESS = ZERO_ADDRESS


class Hooks(Enum):
    # ref: https://github.com/Uniswap/v4-core/blob/main/src/libraries/Hooks.sol
    BEFORE_INITIALIZE = 1 << 13
    AFTER_INITIALIZE = 1 << 12
    BEFORE_ADD_LIQUIDITY = 1 << 11
    AFTER_ADD_LIQUIDITY = 1 << 10
    BEFORE_REMOVE_LIQUIDITY = 1 << 9
    AFTER_REMOVE_LIQUIDITY = 1 << 8
    BEFORE_SWAP = 1 << 7
    AFTER_SWAP = 1 << 6
    BEFORE_DONATE = 1 << 5
    AFTER_DONATE = 1 << 4
    BEFORE_SWAP_RETURNS_DELTA = 1 << 3
    AFTER_SWAP_RETURNS_DELTA = 1 << 2
    AFTER_ADD_LIQUIDITY_RETURNS_DELTA = 1 << 1
    AFTER_REMOVE_LIQUIDITY_RETURNS_DELTA = 1 << 0


def get_pool_from_database(
    pool_hash: HexBytes,
    pool_manager_address: ChecksumAddress,
    chain_id: int,
    session: Session | scoped_session[Session] = db_session,
) -> UniswapV4PoolTableBase | None:
    pool_manager_in_db = session.scalar(
        select(PoolManagerTable).where(
            PoolManagerTable.address == pool_manager_address,
            PoolManagerTable.chain == chain_id,
        )
    )
    if pool_manager_in_db is None:
        return None

    return session.scalar(
        select(UniswapV4PoolTable).where(
            UniswapV4PoolTable.pool_hash == pool_hash.to_0x_hex(),
            UniswapV4PoolTable.manager.has(id=pool_manager_in_db.id),
        )
    )


class UniswapV4Pool(PublisherMixin, AbstractConcentratedLiquidityPool):
    _state_mgr: ConcentratedLiquidityStateManager[UniswapV4PoolState]

    SLOT0_STRUCT_TYPES = (
        "uint160",  # sqrtPriceX96
        "int24",  # tick
        "uint24",  # protocolFee
        "uint24",  # lpFee
    )
    TICK_LIQUIDITY_STRUCT_TYPES = (
        "uint128",  # liquidityGross
        "int128",  # liquidityNet
    )

    FEE_DENOMINATOR = 1_000_000

    def __init__(
        self,
        *,
        pool_id: bytes | str,
        pool_manager_address: str,
        state_view_address: str | None = None,
        tokens: Sequence[str] | None = None,
        fee: Pip | None = None,
        tick_spacing: int | None = None,
        hook_address: str | None = None,
        chain_id: ChainId | None = None,
        tick_data: dict[Tick, dict[str, Any] | UniswapV4LiquidityAtTick] | None = None,
        tick_bitmap: dict[BitmapWord, dict[str, Any] | UniswapV4BitmapAtWord] | None = None,
        provider: ProviderAdapter | None = None,
        state_block: BlockNumber | int | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
    ) -> None:
        self._chain_id: Final[int] = (
            chain_id if chain_id is not None else connection_manager.default_chain_id
        )
        self._provider = (
            provider if provider is not None else connection_manager.get_provider(self.chain_id)
        )
        state_block = state_block if state_block is not None else self._provider.get_block_number()
        self._initial_state_block = state_block

        self._pool_manager_address = get_checksum_address(pool_manager_address)

        pool_id = HexBytes(pool_id)

        pool_from_db = get_pool_from_database(
            pool_hash=pool_id,
            pool_manager_address=self._pool_manager_address,
            chain_id=self.chain_id,
        )
        if pool_from_db is not None:
            currency0_address = pool_from_db.currency0.address
            currency1_address = pool_from_db.currency1.address
            self.hook_address = get_checksum_address(pool_from_db.hooks)
            tick_spacing = pool_from_db.tick_spacing
            assert pool_from_db.fee_currency0 == pool_from_db.fee_currency1
            fee = pool_from_db.fee_currency0
            state_view_address = pool_from_db.manager.state_view
        else:
            if state_view_address is None:
                raise DegenbotValueError(
                    message="A state view contract address must be provided for a pool not in the database."  # noqa: E501
                )
            if fee is None:
                raise DegenbotValueError(
                    message="A fee must be provided for a pool not in the database."
                )
            if tick_spacing is None:
                raise DegenbotValueError(
                    message="A tick spacing must be provided for a pool not in the database."
                )
            if tokens is None:
                raise DegenbotValueError(
                    message="Token addresses must be provided for a pool not in the database."
                )

            currency0_address, currency1_address = sorted(
                [get_checksum_address(token) for token in tokens],
                key=lambda token: token.lower(),
            )
            assert currency0_address != currency1_address
            self.hook_address = (
                get_checksum_address(hook_address) if hook_address is not None else ZERO_ADDRESS
            )

        self._state_view_address = get_checksum_address(state_view_address)

        token_manager = Erc20TokenManager(chain_id=self.chain_id, provider=self._provider)
        self.token0: Final[Erc20Token] = token_manager.get_erc20token(
            address=currency0_address,
            silent=silent,
        )
        self.token1: Final[Erc20Token] = token_manager.get_erc20token(
            address=currency1_address,
            silent=silent,
        )

        self.active_hooks: frozenset[Hooks] = frozenset(
            hook for hook in Hooks if int(self.hook_address, 16) & hook.value != 0
        )

        # Construct the PoolKey
        self._pool_key = UniswapV4PoolKey(
            currency0=self.token0.address,
            currency1=self.token1.address,
            fee=fee,
            tick_spacing=tick_spacing,
            hooks=self.hook_address,
        )

        self._pool_id: Final[HexBytes] = pool_id
        self.name = f"{self.token0}-{self.token1} ({self.__class__.__name__}, id={self.pool_id.to_0x_hex()})"  # noqa:E501

        try:
            working_slot0, working_liquidity = self._get_state_values(
                provider=self._provider, state_block=state_block
            )
            working_sqrt_price_x96 = working_slot0.sqrt_price_x96
            working_tick = working_slot0.tick
            self.lp_fee = working_slot0.lp_fee
            self.protocol_fee = working_slot0.protocol_fee
        except (ContractLogicError, DecodingError) as exc:
            # Contracts differ slightly across Uniswap V4 forks, so decoding may fail. Catch this
            # here and raise as a pool-specific exception
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        assert self.pool_id == (
            calculated_id := Web3.keccak(
                eth_abi.abi.encode(
                    types=["address", "address", "uint24", "int24", "address"],
                    args=[
                        self.pool_key.currency0,
                        self.pool_key.currency1,
                        self.pool_key.fee,
                        self.pool_key.tick_spacing,
                        self.pool_key.hooks,
                    ],
                )
            )
        ), (
            f"Supplied pool ID {self.pool_id.to_0x_hex()} does not match calculated ID {calculated_id.to_0x_hex()}, {self.pool_key=}"  # noqa
        )

        # If liquidity info was not provided, treat the mapping as sparse
        self.sparse_liquidity_map = tick_bitmap is None or tick_data is None

        working_tick_bitmap = {}
        working_tick_data = {}

        if tick_bitmap is not None:
            # transform dict to UniswapV4BitmapAtWord
            working_tick_bitmap.update({
                int(word): (
                    UniswapV4BitmapAtWord(**bitmap_at_word)
                    if not isinstance(
                        bitmap_at_word,
                        UniswapV4BitmapAtWord,
                    )
                    else bitmap_at_word
                )
                for word, bitmap_at_word in tick_bitmap.items()
            })

        if tick_data is not None:
            working_tick_data.update({
                int(tick): (
                    # transform dict to UniswapV4LiquidityAtTick
                    UniswapV4LiquidityAtTick(**liquidity_at_tick)
                    if not isinstance(
                        liquidity_at_tick,
                        UniswapV4LiquidityAtTick,
                    )
                    else liquidity_at_tick
                )
                for tick, liquidity_at_tick in tick_data.items()
            })

        if tick_bitmap is None and tick_data is None:
            word, _ = get_tick_word_and_bit_position(
                tick=working_tick, tick_spacing=self.tick_spacing
            )
            self._fetch_and_populate_initialized_ticks(
                word_position=word,
                tick_bitmap=working_tick_bitmap,
                tick_data=working_tick_data,
                block_number=state_block,
            )

        initial_state = UniswapV4PoolState(
            id=self.pool_id,
            address=self._pool_manager_address,
            liquidity=working_liquidity,
            sqrt_price_x96=working_sqrt_price_x96,
            tick=working_tick,
            tick_bitmap=working_tick_bitmap,
            tick_data=working_tick_data,
            block=state_block,
        )
        self._state_lock = Lock()
        self._state_mgr = ConcentratedLiquidityStateManager(
            initial_state=initial_state,
            state_cache_depth=state_cache_depth,
        )

        managed_pool_registry.add(
            pool=self,
            chain_id=self.chain_id,
            pool_manager_address=self.address,
            pool_id=self.pool_id,
        )

        self._subscribers: WeakSet[Subscriber] = WeakSet()

        if not silent:  # pragma: no branch
            logger.info(self.name)
            logger.info(f"• ID: {self.pool_id.to_0x_hex()}")
            logger.info(f"• Token 0: {self.token0}")
            logger.info(f"• Token 1: {self.token1}")
            logger.info(f"• Liquidity: {self.liquidity}")
            logger.info(f"• SqrtPrice: {self.sqrt_price_x96}")
            logger.info(f"• Tick: {self.tick}")

    def __eq__(self, other: object) -> bool:
        if isinstance(other, type(self)):
            return self.address == other.address and self.pool_id == other.pool_id
        return super().__eq__(other)

    def __hash__(self) -> int:
        return hash(HexBytes(self.address) + self.pool_id)

    def __getstate__(self) -> dict[str, Any]:
        # Remove objects that cannot be pickled and are unnecessary to perform
        # the calculation
        copied_attributes: set[str] = set()
        dropped_attributes = (
            "_provider",
            "_state_lock",
            "_subscribers",
        )

        with self._state_lock:
            return {
                k: (v.copy() if k in copied_attributes else v)
                for k, v in self.__dict__.items()
                if k not in dropped_attributes
            }

    def __setstate__(self, state: dict[str, Any]) -> None:
        state["_state_lock"] = Lock()
        self.__dict__ = state

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(pool_id={self.pool_id.to_0x_hex()},  token0={self.token0}, token1={self.token1}, fee={self.fee}, tick spacing={self.tick_spacing})"  # noqa:E501

    def __str__(self) -> str:
        return self.name

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

        if block_number is None:
            block_number = self._provider.get_block_number()

        working_tick_bitmap = 0
        working_tick_data: list[tuple[Tick, LiquidityGross, LiquidityNet]] = []
        working_tick_bitmap = self.get_tick_bitmap_at_word(
            provider=self._provider,
            word_position=word_position,
            block_identifier=block_number,
        )
        if working_tick_bitmap != 0:
            working_tick_data = self.get_populated_ticks_in_word(
                provider=self._provider,
                word_position=word_position,
                block_identifier=block_number,
            )

        tick_bitmap[word_position] = UniswapV4BitmapAtWord(
            bitmap=working_tick_bitmap,
            block=block_number,
        )
        for tick, liquidity_gross, liquidity_net in working_tick_data:
            tick_data[tick] = UniswapV4LiquidityAtTick(
                liquidity_net=liquidity_net,
                liquidity_gross=liquidity_gross,
                block=block_number,
            )

    def _get_state_values(
        self,
        provider: ProviderAdapter,
        state_block: BlockNumber,
    ) -> tuple[Slot0, Liquidity]:
        slot0_calldata = encode_function_calldata(
            function_prototype="getSlot0(bytes32)",
            function_arguments=[self.pool_id],
        )
        liquidity_calldata = encode_function_calldata(
            function_prototype="getLiquidity(bytes32)",
            function_arguments=[self.pool_id],
        )

        slot0_result = provider.call(
            to=self._state_view_address,
            data=slot0_calldata,
            block=state_block,
        )
        liquidity_result = provider.call(
            to=self._state_view_address,
            data=liquidity_calldata,
            block=state_block,
        )

        price, tick, protocol_fee, lp_fee = cast(
            "tuple[int, ...]",
            eth_abi.abi.decode(types=self.SLOT0_STRUCT_TYPES, data=slot0_result),
        )

        (liquidity,) = cast(
            "tuple[int]",
            eth_abi.abi.decode(types=["uint256"], data=liquidity_result),
        )

        # Extract the two fees (uint12) from the close-packed uint24 protocol fee
        # ref: https://github.com/Uniswap/v4-core/blob/main/src/types/Slot0.sol
        protocol_fee_one_to_zero, protocol_fee_zero_to_one = (
            protocol_fee >> 12,  # discard the lower 12 bits by shifting
            protocol_fee & 0xFFF,  # mask to keep only the lower 12 bits
        )

        return (
            Slot0(
                sqrt_price_x96=price,
                tick=tick,
                protocol_fee=ProtocolFee(
                    one_for_zero=protocol_fee_one_to_zero,
                    zero_for_one=protocol_fee_zero_to_one,
                ),
                lp_fee=lp_fee,
            ),
            liquidity,
        )

    @staticmethod
    def _calculate_swap_fee(
        protocol_fee: int,
        lp_fee: int,
    ) -> SwapFee:
        protocol_fee &= 0xFFF
        lp_fee &= 0xFFFFFF
        numerator = protocol_fee * lp_fee
        return (protocol_fee + lp_fee) - (numerator // PIPS_DENOMINATOR)

    def _calculate_swap(
        self,
        *,
        zero_for_one: bool,
        amount_specified: int,
        sqrt_price_x96_limit: int,
        override_state: UniswapV4PoolState | None = None,
    ) -> tuple[SwapDelta, FeeToProtocol, SwapFee, SwapResult]:
        """
        port from ``UniswapV4Pool._calculate_swap``. Operates on a frozen
        ``LiquidityMapSnapshot`` and returns ``SwapResult`` with no side effects.
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
            snapshot = LiquidityMapSnapshot(
                tick_data=self.tick_data,
                tick_bitmap=self.tick_bitmap,
                tick_spacing=self.tick_spacing,
                sparse=self.sparse_liquidity_map,
            )
            liquidity_start = self.liquidity
            sqrt_price_x96_start = self.sqrt_price_x96
            tick_start = self.tick

        protocol_fee = (
            self.protocol_fee.zero_for_one if zero_for_one else self.protocol_fee.one_for_zero
        )
        swap_fee = (
            self.lp_fee
            if protocol_fee == 0
            else self._calculate_swap_fee(protocol_fee, self.lp_fee)
        )

        if amount_specified == 0:
            return (
                SwapDelta(currency0=0, currency1=0),
                0,
                swap_fee,
                SwapResult(
                    sqrt_price_x96=sqrt_price_x96_start,
                    tick=tick_start,
                    liquidity=liquidity_start,
                ),
            )

        if self.sparse_liquidity_map:
            while True:
                try:
                    result = _v4_swap(
                        snapshot=snapshot,
                        zero_for_one=zero_for_one,
                        amount_specified=amount_specified,
                        sqrt_price_x96_limit=sqrt_price_x96_limit,
                        lp_fee=self.lp_fee,
                        protocol_fee=protocol_fee,
                        liquidity_start=liquidity_start,
                        sqrt_price_x96_start=sqrt_price_x96_start,
                        tick_start=tick_start,
                    )
                    break
                except MissingLiquidityData as exc:
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
            result = _v4_swap(
                snapshot=snapshot,
                zero_for_one=zero_for_one,
                amount_specified=amount_specified,
                sqrt_price_x96_limit=sqrt_price_x96_limit,
                lp_fee=self.lp_fee,
                protocol_fee=protocol_fee,
                liquidity_start=liquidity_start,
                sqrt_price_x96_start=sqrt_price_x96_start,
                tick_start=tick_start,
            )

        swap_delta = SwapDelta(currency0=result.amount0, currency1=result.amount1)

        return (
            swap_delta,
            protocol_fee,
            swap_fee,
            SwapResult(
                sqrt_price_x96=result.sqrt_price_x96,
                tick=result.tick,
                liquidity=result.liquidity,
            ),
        )

    def _notify_subscribers(self: Publisher, message: AbstractPublisherMessage) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: UniswapV4PoolState | None = None,
    ) -> int:
        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_out not found!")

        zero_for_one = token_out == self.token1

        try:
            swap_delta, *_ = self._calculate_swap(
                zero_for_one=zero_for_one,
                amount_specified=token_out_quantity,
                sqrt_price_x96_limit=MIN_SQRT_PRICE + 1 if zero_for_one else MAX_SQRT_PRICE - 1,
                override_state=override_state,
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e

        assert swap_delta.amount_out <= token_out_quantity

        if conflicting_hooks := (
            {
                Hooks.AFTER_SWAP,
                Hooks.AFTER_SWAP_RETURNS_DELTA,
                Hooks.BEFORE_SWAP,
                Hooks.BEFORE_SWAP_RETURNS_DELTA,
            }
            & self.active_hooks
        ):
            raise PossibleInaccurateResult(
                amount_in=swap_delta.amount_in,
                amount_out=swap_delta.amount_out,
                hooks=conflicting_hooks,
            )

        if swap_delta.amount_out < token_out_quantity:
            raise IncompleteSwap(
                amount_in=swap_delta.amount_in,
                amount_out=swap_delta.amount_out,
            )

        return swap_delta.amount_in

    def calculate_tokens_out_from_tokens_in(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        override_state: UniswapV4PoolState | None = None,
    ) -> int:
        if token_in not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_in not found!")

        zero_for_one = token_in == self.token0

        try:
            swap_delta, *_ = self._calculate_swap(
                zero_for_one=zero_for_one,
                amount_specified=-token_in_quantity,
                sqrt_price_x96_limit=MIN_SQRT_PRICE + 1 if zero_for_one else MAX_SQRT_PRICE - 1,
                override_state=override_state,
            )
        except EVMRevertError as e:  # pragma: no cover
            raise LiquidityPoolError(message=f"Simulated execution reverted: {e}") from e

        assert swap_delta.amount_in <= token_in_quantity

        if conflicting_hooks := (
            {
                Hooks.AFTER_SWAP,
                Hooks.AFTER_SWAP_RETURNS_DELTA,
                Hooks.BEFORE_SWAP,
                Hooks.BEFORE_SWAP_RETURNS_DELTA,
            }
            & self.active_hooks
        ):
            raise PossibleInaccurateResult(
                amount_in=swap_delta.amount_in,
                amount_out=swap_delta.amount_out,
                hooks=conflicting_hooks,
            )

        if swap_delta.amount_in < token_in_quantity:
            raise IncompleteSwap(
                amount_in=swap_delta.amount_in,
                amount_out=swap_delta.amount_out,
            )

        return swap_delta.amount_out

    def get_tick_bitmap_at_word(
        self, provider: ProviderAdapter, word_position: int, block_identifier: BlockIdentifier
    ) -> int:
        (bitmap_at_word,) = cast(
            "tuple[int]",
            raw_call(
                provider=provider,
                address=self._state_view_address,
                calldata=encode_function_calldata(
                    function_prototype="getTickBitmap(bytes32,int16)",
                    function_arguments=[self.pool_id, word_position],
                ),
                return_types=["uint256"],
                block_identifier=block_identifier,
            ),
        )
        return bitmap_at_word

    def get_populated_ticks_in_word(
        self,
        provider: ProviderAdapter,
        word_position: int,
        block_identifier: BlockIdentifier,
    ) -> list[tuple[Tick, LiquidityGross, LiquidityNet]]:
        bitmap_at_word = self.get_tick_bitmap_at_word(
            provider=provider,
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
                to=self._state_view_address,
                data=encode_function_calldata(
                    function_prototype="getTickLiquidity(bytes32,int24)",
                    function_arguments=[self.pool_id, tick],
                ),
                block=block,
            )
            results.append(result)

        populated_ticks: list[tuple[Tick, LiquidityGross, LiquidityNet]] = []
        for tick, result in zip(active_ticks, results, strict=True):
            liquidity_gross, liquidity_net = eth_abi.abi.decode(
                types=self.TICK_LIQUIDITY_STRUCT_TYPES,
                data=result,
            )
            populated_ticks.append((tick, liquidity_gross, liquidity_net))

        return populated_ticks

    @property
    def address(self) -> ChecksumAddress:  # type: ignore[override]
        return self._pool_manager_address

    @property
    def chain_id(self) -> int:
        return self._chain_id

    @property
    def _state_cache(self) -> deque[UniswapV4PoolState]:
        return self._state_mgr.state_cache

    @_state_cache.setter
    def _state_cache(self, value: deque[UniswapV4PoolState]) -> None:
        if not hasattr(self, "_state_mgr"):
            self._state_mgr = object.__new__(ConcentratedLiquidityStateManager)
            self._state_mgr.state_cache = value
            return
        self._state_mgr.state_cache = value

    @property
    def liquidity(self) -> int:
        return self._state_mgr.liquidity

    @property
    def pool_id(self) -> HexBytes:
        return self._pool_id

    @property
    def pool_key(self) -> UniswapV4PoolKey:
        return self._pool_key

    @property
    def sqrt_price_x96(self) -> int:
        return self._state_mgr.sqrt_price_x96

    @property
    def state(self) -> UniswapV4PoolState:
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
    def tick_spacing(self) -> int:
        return self.pool_key.tick_spacing

    @property
    def fee(self) -> int:
        return self.pool_key.fee

    @property
    def tokens(self) -> tuple[Erc20Token, Erc20Token]:
        return self.token0, self.token1

    @property
    def update_block(self) -> BlockNumber:
        block = self._state_mgr.update_block
        if block is None:
            raise DegenbotValueError(message="State does not have a block number.")
        return block

    def swap_is_viable(
        self,
        state: UniswapV4PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        return self._state_mgr.swap_is_viable(
            state=state,
            zero_for_one=vector.zero_for_one,
            sparse_liquidity_map=self.sparse_liquidity_map,
        )

    def auto_update(
        self,
        block_number: BlockNumber | None = None,
        *,
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

            provider = self._provider
            block_number = block_number if block_number is not None else provider.get_block_number()

            new_slot0, new_liquidity = self._get_state_values(
                provider=provider, state_block=block_number
            )
            new_sqrt_price_x96 = new_slot0.sqrt_price_x96
            new_tick = new_slot0.tick
            self.lp_fee = new_slot0.lp_fee
            self.protocol_fee = new_slot0.protocol_fee

            if (
                new_sqrt_price_x96 == self.sqrt_price_x96
                and new_liquidity == self.liquidity
                and self.tick == new_tick
            ):
                return

            working_state = dataclasses.replace(
                self.state,
                liquidity=new_liquidity,
                sqrt_price_x96=new_sqrt_price_x96,
                tick=new_tick,
                block=block_number,
            )

            self._state_mgr.push_state(working_state)

            self._notify_subscribers(
                message=UniswapV4PoolStateUpdated(working_state),
            )

            if not silent:  # pragma: no cover
                logger.info(f"Liquidity: {self.liquidity}")
                logger.info(f"SqrtPriceX96: {self.sqrt_price_x96}")
                logger.info(f"Tick: {self.tick}")

    def external_update(
        self,
        update: UniswapV4PoolExternalUpdate,
    ) -> bool:
        """
        Process a `UniswapV4PoolExternalUpdate` with one or more of the following update types:
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

        if update.block_number < self.update_block:
            raise ExternalUpdateError(
                message=f"Rejected update for block {update.block_number} in the past, current update block is {self.update_block}"  # noqa:E501
            )

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
                message=UniswapV4PoolStateUpdated(working_state),
            )

            return True

    def update_liquidity_map(
        self,
        update: UniswapV4PoolLiquidityMappingUpdate,
    ) -> None:
        """
        Applies an update to the liquidity map.

        @dev This method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.
        """

        if update.liquidity == 0:
            return

        with self._state_lock:
            state_block = update.block_number

            # The tick bitmap and tick data dictionaries accessed from the property are copies, so
            # they can be freely modified without corrupting states for previous blocks
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
                    working_tick_data[tick] = UniswapV4LiquidityAtTick(
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
                    f"Negative gross liquidity ({new_liquidity_gross})!"
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

                working_tick_data[tick] = UniswapV4LiquidityAtTick(
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
                message=UniswapV4PoolStateUpdated(working_state),
            )

    def get_arbitrage_helpers(self) -> tuple[AbstractArbitrage, ...]:
        return tuple(
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, AbstractArbitrage)
        )

    def get_absolute_price(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """
        Get the absolute price for the given token, expressed in units of the other.
        """

        return 1 / self.get_absolute_exchange_rate(token, override_state=override_state)

    def get_absolute_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
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
        override_state: UniswapV4PoolState | None = None,
    ) -> Fraction:
        """
        Get the nominal price for the given token, expressed in units of the other, corrected for
        decimal place values.
        """

        return 1 / self.get_nominal_exchange_rate(token, override_state=override_state)

    def get_nominal_exchange_rate(
        self,
        token: Erc20Token,
        override_state: UniswapV4PoolState | None = None,
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

    def discard_states_before_block(self, block: BlockNumber) -> None:
        """Discard cached states earlier than the given block."""
        with self._state_lock:
            self._state_mgr.discard_states_before_block(block)

    def restore_state_before_block(self, block: BlockNumber) -> None:
        """Restore the last pool state recorded prior to a target block."""
        with self._state_lock:
            restored: UniswapV4PoolState = self._state_mgr.restore_state_before_block(block)
            self._notify_subscribers(message=UniswapV4PoolStateUpdated(restored))

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,  # noqa: ARG002
        state_override: UniswapV4PoolState | None = None,
    ) -> SimulationResult:
        if token_in == self.token0.address:
            token_in_obj = self.token0
        elif token_in == self.token1.address:
            token_in_obj = self.token1
        else:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        initial_state = state_override or self.state
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=state_override,
        )
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=initial_state,
            final_state=initial_state,
        )

    def extract_fee(self, zero_for_one: bool) -> Fraction:  # noqa: FBT001, ARG002
        return Fraction(self.fee, self.FEE_DENOMINATOR)

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: UniswapV4PoolState | None = None,
    ) -> HopType:
        return super().to_hop_state(zero_for_one=zero_for_one, state_override=state_override)  # type: ignore[misc, no-any-return]
