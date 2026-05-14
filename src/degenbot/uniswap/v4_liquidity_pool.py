# ruff: noqa: PLR0904


import dataclasses
from collections import deque
from collections.abc import Callable
from enum import Enum
from threading import Lock
from typing import Any, ClassVar, Final, cast
from weakref import WeakSet

import eth_abi.abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.evm import EVMRevertError
from degenbot.exceptions.liquidity_pool import (
    ExternalUpdateError,
    IncompleteSwap,
    LiquidityPoolError,
    PossibleInaccurateResult,
)
from degenbot.types.abstract import AbstractArbitrage, AbstractConcentratedLiquidityPool
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import (
    PublisherMixin,
    Subscriber,
)
from degenbot.types.hop_types import HopType
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot, MissingLiquidityData
from degenbot.uniswap.concentrated.state_manager import ConcentratedLiquidityStateManager
from degenbot.uniswap.concentrated.v4_simulator import calculate_swap as _v4_swap
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v3_functions import (
    get_tick_word_and_bit_position,
)
from degenbot.uniswap.v3_types import BitmapWord, Pip, Tick
from degenbot.uniswap.v4_libraries.tick_bitmap import flip_tick
from degenbot.uniswap.v4_libraries.tick_math import MAX_SQRT_PRICE, MIN_SQRT_PRICE
from degenbot.uniswap.v4_pool_calc import UniswapV4PoolCalc
from degenbot.uniswap.v4_pool_state import V4PoolState
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


class UniswapV4Pool(
    PublisherMixin,
    PoolPickleMixin,
    V4PoolState,
    UniswapV4PoolCalc,
    AbstractConcentratedLiquidityPool,
):
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

    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_state_lock",
        "_subscribers",
        "_tick_data_fetcher",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {
        "_state_lock": Lock,
        "_subscribers": WeakSet,
    }

    def __init__(
        self,
        *,
        pool_id: bytes | str,
        pool_manager_address: str,
        token0: Erc20Token,
        token1: Erc20Token,
        fee: Pip,
        tick_spacing: int,
        state_view_address: str | None = None,
        hook_address: str | None = None,
        chain_id: ChainId | None = None,
        sqrt_price_x96: int = ...,
        tick: int = ...,
        liquidity: int = ...,
        protocol_fee_zero_for_one: int = ...,
        protocol_fee_one_for_zero: int = ...,
        lp_fee: int = ...,
        tick_data: dict[Tick, dict[str, Any] | UniswapV4LiquidityAtTick] | None = None,
        tick_bitmap: dict[BitmapWord, dict[str, Any] | UniswapV4BitmapAtWord] | None = None,
        state_block: BlockNumber | int | None = None,
        state_cache_depth: int = 8,
        tick_data_fetcher: Callable[[int, int], None] | None = None,
    ) -> None:
        self._pool_manager_address = get_checksum_address(pool_manager_address)
        self._pool_id: Final[HexBytes] = HexBytes(pool_id)

        self._chain_id: Final[int] = chain_id if chain_id is not None else token0.chain_id

        # TODO: check - should this be zero?
        state_block = state_block if state_block is not None else 0
        self._initial_state_block = state_block

        self._token0: Final[Erc20Token] = token0
        self._token1: Final[Erc20Token] = token1
        self.hook_address = (
            get_checksum_address(hook_address) if hook_address is not None else ZERO_ADDRESS
        )
        self._state_view_address = (
            get_checksum_address(state_view_address)
            if state_view_address is not None
            else ZERO_ADDRESS
        )
        self.active_hooks: frozenset[Hooks] = frozenset(
            hook for hook in Hooks if int(self.hook_address, 16) & hook.value != 0
        )

        # Construct the PoolKey
        self._pool_key = UniswapV4PoolKey(
            currency0=self._token0.address,
            currency1=self._token1.address,
            fee=fee,
            tick_spacing=tick_spacing,
            hooks=self.hook_address,
        )

        # Verify pool ID
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
                ),
            )
        ), (
            f"Supplied pool ID {self.pool_id.to_0x_hex()} does not match calculated ID {calculated_id.to_0x_hex()}, {self.pool_key=}"  # noqa
        )

        self.name = f"{self._token0}-{self._token1} ({self.__class__.__name__}, id={self.pool_id.to_0x_hex()})"  # noqa:E501

        self.protocol_fee = ProtocolFee(
            zero_for_one=protocol_fee_zero_for_one,
            one_for_zero=protocol_fee_one_for_zero,
        )
        self.lp_fee = lp_fee

        if (tick_bitmap is not None) != (tick_data is not None):
            raise DegenbotValueError(message="Provide both tick_bitmap and tick_data.")

        self._sparse_liquidity_map = tick_bitmap is None or tick_data is None

        working_tick_bitmap = {}
        working_tick_data = {}

        if tick_bitmap is not None:
            working_tick_bitmap.update({
                int(word): (
                    UniswapV4BitmapAtWord(**bitmap_at_word)
                    if not isinstance(bitmap_at_word, UniswapV4BitmapAtWord)
                    else bitmap_at_word
                )
                for word, bitmap_at_word in tick_bitmap.items()
            })

        if tick_data is not None:
            working_tick_data.update({
                int(tick): (
                    UniswapV4LiquidityAtTick(**liquidity_at_tick)
                    if not isinstance(liquidity_at_tick, UniswapV4LiquidityAtTick)
                    else liquidity_at_tick
                )
                for tick, liquidity_at_tick in tick_data.items()
            })

        self._tick_data_fetcher = tick_data_fetcher

        initial_state = UniswapV4PoolState(
            id=self.pool_id,
            address=self._pool_manager_address,
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
        self._subscribers: WeakSet[Subscriber] = WeakSet()

    def __eq__(self, other: object) -> bool:
        if isinstance(other, type(self)):
            return self.address == other.address and self.pool_id == other.pool_id
        return super().__eq__(other)

    def __hash__(self) -> int:
        return hash(HexBytes(self.address) + self.pool_id)

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(pool_id={self.pool_id.to_0x_hex()},  token0={self._token0}, token1={self._token1}, fee={self.fee}, tick spacing={self.tick_spacing})"  # noqa:E501

    def __str__(self) -> str:
        return self.name

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
                sparse=True,
            )
            liquidity_start = override_state.liquidity
            sqrt_price_x96_start = override_state.sqrt_price_x96
            tick_start = override_state.tick
        else:
            snapshot = LiquidityMapSnapshot(
                tick_data=self.tick_data,
                tick_bitmap=self.tick_bitmap,
                tick_spacing=self.tick_spacing,
                sparse=True,
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

        # Always use the sparse swap loop so that MissingLiquidityData can be
        # handled by fetching additional tick data when the fetcher is available.
        fetched_words: set[int] = set()
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
                if exc.word in fetched_words:
                    raise LiquidityPoolError(
                        message=f"Tick data fetcher did not resolve word {exc.word} "
                        f"on a previous attempt. "
                        f"pool_id={self.pool_id.to_0x_hex()} zfo={zero_for_one} "
                        f"amount_specified={amount_specified}"
                    ) from exc
                if self._tick_data_fetcher is not None:
                    fetched_words.add(exc.word)
                    self._tick_data_fetcher(exc.word, self.update_block)
                    snapshot = LiquidityMapSnapshot.from_state(
                        self.state,
                        tick_spacing=self.tick_spacing,
                        sparse=True,
                    )
                else:
                    raise

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

    def calculate_tokens_in_from_tokens_out(
        self,
        token_out: Erc20Token,
        token_out_quantity: int,
        override_state: UniswapV4PoolState | None = None,
    ) -> int:
        if token_out not in self.tokens:  # pragma: no cover
            raise DegenbotValueError(message="token_out not found!")

        zero_for_one = token_out == self._token1

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

        zero_for_one = token_in == self._token0

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
            sparse_liquidity_map=self._sparse_liquidity_map,
        )

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

        with self._state_lock:  # noqa:PLR1702
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

                if self._sparse_liquidity_map and tick_word not in working_tick_bitmap:
                    # The liquidity map at the affected word must be complete prior to changing the
                    # status of any tick
                    if self._tick_data_fetcher is not None:
                        self._tick_data_fetcher(tick_word, state_block - 1)
                        # Refresh working bitmap from updated pool state
                        working_tick_bitmap = self.tick_bitmap
                        # Merge newly fetched ticks into working_tick_data without overwriting
                        # existing entries (which may have been modified in earlier iterations)
                        for fetched_tick, fetched_liquidity in self.tick_data.items():
                            if fetched_tick not in working_tick_data:
                                working_tick_data[fetched_tick] = fetched_liquidity
                    else:
                        raise MissingLiquidityData(tick_word)

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
                        sparse=self._sparse_liquidity_map,
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
                        sparse=self._sparse_liquidity_map,
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
        if token_in == self._token0.address:
            token_in_obj = self._token0
        elif token_in == self._token1.address:
            token_in_obj = self._token1
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

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: UniswapV4PoolState | None = None,
    ) -> HopType:
        return super().to_hop_state(zero_for_one=zero_for_one, state_override=state_override)  # type: ignore[misc, no-any-return]
