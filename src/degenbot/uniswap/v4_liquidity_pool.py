"""UniswapV4Pool: concentrated liquidity AMM with hook support.

See: contract_reference/uniswap/V4/PoolManager.sol (PoolManager, Pool, Hooks)
"""

import dataclasses
from collections.abc import Callable
from enum import Enum
from typing import Any, ClassVar, Final, cast
from weakref import WeakSet

import eth_abi.abi
from eth_typing import ChecksumAddress
from hexbytes import HexBytes
from web3 import Web3

from degenbot.arbitrage.types import UniswapV4PoolSwapAmounts, V4PoolKey
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    EVMRevertError,
    ExternalUpdateError,
    HookedPoolResult,
    IncompleteSwap,
    LiquidityPoolError,
)
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import BoundedProductHop, HopType, V3TickRangeInfo
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult
from degenbot.types.state_cache import StateCache
from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot, MissingLiquidityData
from degenbot.uniswap.concentrated.state_manager import ConcentratedLiquidityStateManager
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.concentrated.v4_simulator import calculate_swap as _v4_swap
from degenbot.uniswap.log_decoders import (
    V4_MODIFY_LIQUIDITY_TOPIC,
    V4_SWAP_TOPIC,
    decode_v4_modify_liquidity,
    decode_v4_swap,
)
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
from degenbot.uniswap.v3_libraries.functions import v3_virtual_reserves
from degenbot.uniswap.v3_libraries.tick_bitmap import gen_ticks
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
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
    UniswapV4PoolExternalUpdate,
    UniswapV4PoolKey,
    UniswapV4PoolLiquidityMappingUpdate,
    UniswapV4PoolState,
    UniswapV4PoolStateUpdated,
)


@dataclasses.dataclass(slots=True)
class SwapResult:
    """SwapResult class."""

    sqrt_price_x96: int
    tick: int
    liquidity: int


@dataclasses.dataclass(slots=True, frozen=True)
class SwapDelta:
    """SwapDelta class."""

    currency0: int
    currency1: int

    @property
    def amount_in(self) -> int:
        """The deposited token amount."""
        return -min(self.currency0, self.currency1)

    @property
    def amount_out(self) -> int:
        """The withdrawn token amount."""
        return max(self.currency0, self.currency1)


@dataclasses.dataclass(slots=True, frozen=True)
class ProtocolFee:
    """ProtocolFee class."""

    zero_for_one: int
    one_for_zero: int


@dataclasses.dataclass(slots=True, frozen=True)
class Slot0:
    """Slot0 class."""

    sqrt_price_x96: int
    tick: int
    protocol_fee: ProtocolFee
    lp_fee: int


PIPS_DENOMINATOR = 1_000_000
NATIVE_CURRENCY_ADDRESS = ZERO_ADDRESS


class Hooks(Enum):
    """Hooks class."""

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
    AbstractLiquidityPool,
):
    """UniswapV4Pool class."""

    _state_mgr: ConcentratedLiquidityStateManager[UniswapV4PoolState]

    LOG_HANDLERS: ClassVar[dict[str, Any]] = {
        V4_SWAP_TOPIC: decode_v4_swap,
        V4_MODIFY_LIQUIDITY_TOPIC: decode_v4_modify_liquidity,
    }

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
        "_subscribers",
        "_tick_data_fetcher",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {
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
        sqrt_price_x96: int,
        tick: int,
        liquidity: int,
        protocol_fee_zero_for_one: int,
        protocol_fee_one_for_zero: int,
        lp_fee: int,
        tick_data: dict[Tick, dict[str, Any] | LiquidityAtTick] | None = None,
        tick_bitmap: dict[BitmapWord, dict[str, Any] | BitmapAtWord] | None = None,
        state_block: BlockNumber | int | None = None,
        state_cache_depth: int = 8,
        tick_data_fetcher: Callable[[int, int], None] | None = None,
    ) -> None:
        """Initialize the instance.

        Raises:
            DegenbotValueError: If tick_bitmap and tick_data are not both provided or both omitted.

        """
        self._pool_manager_address = get_checksum_address(pool_manager_address)
        self._pool_id: Final[HexBytes] = HexBytes(pool_id)

        self._chain_id: Final[int | None] = chain_id if chain_id is not None else token0.chain_id

        # Default state_block to zero if not provided
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
                    BitmapAtWord(**bitmap_at_word)
                    if not isinstance(bitmap_at_word, BitmapAtWord)
                    else bitmap_at_word
                )
                for word, bitmap_at_word in tick_bitmap.items()
            })

        if tick_data is not None:
            working_tick_data.update({
                int(tick): (
                    LiquidityAtTick(**liquidity_at_tick)
                    if not isinstance(liquidity_at_tick, LiquidityAtTick)
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
        self._state_mgr = ConcentratedLiquidityStateManager(
            initial_state=initial_state,
            state_cache_depth=state_cache_depth,
        )
        self._subscribers: WeakSet[Subscriber] = WeakSet()

    def __eq__(self, other: object) -> bool:
        """Check equality with another object.

        Returns:
            True if the other object is the same pool, False otherwise.

        """
        if isinstance(other, type(self)):
            return self.address == other.address and self.pool_id == other.pool_id
        return super().__eq__(other)

    def __hash__(self) -> int:
        """Return the hash value.

        Returns:
            The hash value combining address and pool ID.

        """
        return hash(HexBytes(self.address) + self.pool_id)

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            The string representation of the pool.

        """
        return f"{self.__class__.__name__}(pool_id={self.pool_id.to_0x_hex()},  token0={self._token0}, token1={self._token1}, fee={self.fee}, tick spacing={self.tick_spacing})"  # noqa:E501

    def __str__(self) -> str:
        """Return the canonical string representation.

        Returns:
            The pool name string.

        """
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
        """Port from ``UniswapV4Pool._calculate_swap``.

        Operates on a frozen ``LiquidityMapSnapshot`` and returns ``SwapResult``
        with no side effects.

        Returns:
            Tuple of (swap delta, protocol fee, swap fee, swap result).

        Raises:
            LiquidityPoolError: If tick data fetcher fails to resolve a word.
            MissingLiquidityData: If a sparse liquidity map is missing a required word.

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
        """Calculate tokens in from tokens out.

        Returns:
            The required input token amount.

        Raises:
            DegenbotValueError: If token_out is not held by this pool.
            HookedPoolResult: If the pool has active hooks that affect the swap.
            IncompleteSwap: If the swap cannot fulfill the full output amount.
            LiquidityPoolError: If the simulated execution reverts.

        """
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
            raise HookedPoolResult(
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
        """Calculate tokens out from tokens in.

        Returns:
            The expected output token amount.

        Raises:
            DegenbotValueError: If token_in is not held by this pool.
            HookedPoolResult: If the pool has active hooks that affect the swap.
            IncompleteSwap: If the swap cannot fulfill the full input amount.
            LiquidityPoolError: If the simulated execution reverts.

        """
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
            raise HookedPoolResult(
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
    def address(self) -> ChecksumAddress:
        """Address.

        Returns:
            The pool manager address.

        """
        return self._pool_manager_address

    @property
    def chain_id(self) -> int | None:
        """Return chain id.

        Returns:
            The chain ID, or None if not set.

        """
        return self._chain_id

    @property
    def _state_cache(self) -> StateCache[UniswapV4PoolState]:
        return self._state_mgr.state_cache

    @_state_cache.setter
    def _state_cache(self, value: StateCache[UniswapV4PoolState]) -> None:
        if not hasattr(self, "_state_mgr"):
            self._state_mgr = object.__new__(ConcentratedLiquidityStateManager)
            self._state_mgr.state_cache = value
            return
        self._state_mgr.state_cache = value

    @property
    def liquidity(self) -> int:
        """Return liquidity.

        Returns:
            The current active liquidity.

        """
        return self._state_mgr.liquidity

    @property
    def pool_id(self) -> HexBytes:
        """Pool id.

        Returns:
            The pool ID bytes.

        """
        return self._pool_id

    @property
    def pool_key(self) -> UniswapV4PoolKey:
        """Pool key.

        Returns:
            The V4 pool key struct.

        """
        return self._pool_key

    @property
    def sqrt_price_x96(self) -> int:
        """Return sqrt price x96.

        Returns:
            The current sqrt price as a Q64.96 value.

        """
        return self._state_mgr.sqrt_price_x96

    @property
    def state(self) -> UniswapV4PoolState:
        """State.

        Returns:
            The current pool state.

        """
        return self._state_mgr.state

    @property
    def tick(self) -> int:
        """Return tick.

        Returns:
            The current tick.

        """
        return self._state_mgr.tick

    @property
    def tick_bitmap(self) -> InitializedTickMap:
        """Tick bitmap."""
        return cast("InitializedTickMap", self._state_mgr.tick_bitmap)

    @property
    def tick_data(self) -> LiquidityMap:
        """Tick data."""
        return cast("LiquidityMap", self._state_mgr.tick_data)

    @property
    def tick_spacing(self) -> int:
        """Return tick spacing.

        Returns:
            The tick spacing for the pool.

        """
        return self.pool_key.tick_spacing

    @property
    def fee(self) -> int:
        """Return fee.

        Returns:
            The fee in pips.

        """
        return self.pool_key.fee

    @property
    def update_block(self) -> BlockNumber:
        """Update block.

        Returns:
            The block number of the most recent state update.

        Raises:
            DegenbotValueError: If the state does not have a block number.

        """
        block = self._state_mgr.update_block
        if block is None:
            raise DegenbotValueError(message="State does not have a block number.")
        return block

    @property
    def initial_state_block(self) -> int:
        (
            """Block number at which the pool's initial state was captured.

        Returns:
            The block number from construction (DB snapshot or RPC fetch).

        """
            ""
        )
        return self._initial_state_block

    def swap_is_viable(
        self,
        state: UniswapV4PoolState,
        vector: UniswapPoolSwapVector,
    ) -> bool:
        """Swap is viable.

        Returns:
            True if a swap can proceed with the given state, False otherwise.

        """
        return self._state_mgr.swap_is_viable(
            state=state,
            zero_for_one=vector.zero_for_one,
            sparse_liquidity_map=self._sparse_liquidity_map,
        )

    def update_tick_data(
        self,
        tick_bitmap: dict[int, Any],
        tick_data: dict[int, Any],
        block: int,
    ) -> None:
        """Apply updated tick bitmap and data from the tick data fetcher.

        Replaces the tick_bitmap and tick_data on the current state and
        pushes the new state through the state manager.
        """
        new_state = dataclasses.replace(
            self.state,
            tick_bitmap=tick_bitmap,
            tick_data=tick_data,
            block=max(self.update_block, block),
        )
        self._state_mgr.push_state(new_state)

    def external_update(
        self,
        update: UniswapV4PoolExternalUpdate,
    ) -> bool:
        """Process a `UniswapV4PoolExternalUpdate`.

        Accepts one or more of the following update types:

        - `block_number`: int
        - `tick`: int
        - `liquidity`: int
        - `sqrt_price_x96`: int

        `block_number` is validated against the most recently recorded block prior
        to recording any changes.

        Returns:
            True if any updated state value was recorded, False otherwise.

        Raises:
            ExternalUpdateError: If the update is for a past block.

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

        with self._state_cache.lock():
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
        """Apply an update to the liquidity map.

        Raises:
            MissingLiquidityData: If a sparse map is missing a required word and no fetcher is set.

        @dev This method uses a lock to guard state-modifying methods that might cause race
        conditions when used with threads.

        """
        if update.liquidity == 0:
            return

        with self._state_cache.lock():  # noqa:PLR1702
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
                    working_tick_data[tick] = LiquidityAtTick(
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

                working_tick_data[tick] = LiquidityAtTick(
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

    def discard_states_before_block(self, block: BlockNumber) -> None:
        """Discard cached states earlier than the given block."""
        with self._state_cache.lock():
            self._state_mgr.discard_states_before_block(block)

    def restore_state_before_block(self, block: BlockNumber) -> None:
        """Restore the last pool state recorded prior to a target block."""
        with self._state_cache.lock():
            restored: UniswapV4PoolState = self._state_mgr.restore_state_before_block(block)
            self._notify_subscribers(message=UniswapV4PoolStateUpdated(restored))

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,  # noqa: ARG002
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        """Simulate swap.

        Returns:
            The simulation result with amounts and state transitions.

        Raises:
            DegenbotValueError: If tokens are unknown or state type is mismatched.

        """
        v4_state: UniswapV4PoolState | None = None
        if state_override is not None:
            if not isinstance(state_override, UniswapV4PoolState):
                msg = f"Expected UniswapV4PoolState, got {type(state_override).__name__}"
                raise DegenbotValueError(message=msg)
            v4_state = state_override

        if token_in == self._token0.address:
            token_in_obj = self._token0
        elif token_in == self._token1.address:
            token_in_obj = self._token1
        else:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        initial_state = v4_state or self.state
        amount_out = self.calculate_tokens_out_from_tokens_in(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=v4_state,
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
        *,
        token_in: Erc20Token | None = None,  # noqa: ARG002
        token_out: Erc20Token | None = None,  # noqa: ARG002
    ) -> HopType:
        """Convert to hop state.

        Returns:
            A BoundedProductHop for the solver.

        """
        # token_in/token_out unused — 2-token pools determine pair from zero_for_one.
        # Callers should ensure these match pool.token0/pool.token1 if provided.
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

    def _get_tick_ranges(
        self,
        zero_for_one: bool,  # noqa: FBT001
        max_ranges: int = 3,
    ) -> tuple[tuple[V3TickRangeInfo, ...], int] | None:
        if self._sparse_liquidity_map:
            return None

        tick_data = self.tick_data
        tick_bitmap = self.tick_bitmap
        tick_spacing = self.tick_spacing

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
        try:  # noqa: PLW0717
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

    def build_swap_amount(
        self,
        zero_for_one: bool,  # noqa: FBT001
        amount_in: int,
        amount_out: int,
    ) -> UniswapV4PoolSwapAmounts:
        """Build swap amount.

        Returns:
            The swap amounts object for encoding.

        """
        limit = MIN_SQRT_RATIO + 1 if zero_for_one else MAX_SQRT_RATIO - 1
        return UniswapV4PoolSwapAmounts(
            address=self.address,
            id=self.pool_id,
            pool_key=V4PoolKey(
                currency0=self.token0.address,
                currency1=self.token1.address,
                fee=self.fee,
                tick_spacing=self.tick_spacing,
                hooks=self.hook_address,
            ),
            amount_in=amount_in,
            amount_out=amount_out,
            amount_specified=amount_in,
            zero_for_one=zero_for_one,
            sqrt_price_limit_x96=limit,
        )
