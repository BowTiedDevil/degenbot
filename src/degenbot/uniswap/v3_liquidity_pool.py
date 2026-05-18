import contextlib
import dataclasses
from collections import deque
from collections.abc import Callable
from threading import Lock
from typing import TYPE_CHECKING, Any, ClassVar, TypedDict, cast
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot.arbitrage.types import UniswapV3PoolSwapAmounts
from degenbot.checksum_cache import get_checksum_address
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import EVMRevertError, ExternalUpdateError, LiquidityPoolError
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import BoundedProductHop, HopType, V3TickRangeInfo
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot, MissingLiquidityData
from degenbot.uniswap.concentrated.state_manager import ConcentratedLiquidityStateManager
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.concentrated.v3_simulator import calculate_swap as _v3_swap
from degenbot.uniswap.deployments import FACTORY_DEPLOYMENTS as _FACTORY_DEPLOYMENTS
from degenbot.uniswap.log_decoders import (
    V3_BURN_TOPIC,
    V3_MINT_TOPIC,
    V3_SWAP_TOPIC,
    decode_v3_burn,
    decode_v3_mint,
    decode_v3_swap,
)
from degenbot.uniswap.types import UniswapPoolSwapVector
from degenbot.uniswap.v3_functions import generate_v3_pool_address, get_tick_word_and_bit_position
from degenbot.uniswap.v3_libraries.functions import v3_virtual_reserves
from degenbot.uniswap.v3_libraries.tick_bitmap import flip_tick, gen_ticks
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MAX_TICK,
    MIN_SQRT_RATIO,
    MIN_TICK,
    get_sqrt_ratio_at_tick,
)
from degenbot.uniswap.v3_pool_calc import UniswapV3PoolCalc
from degenbot.uniswap.v3_pool_state import V3PoolState
from degenbot.uniswap.v3_types import (
    InitializedTickMap,
    Liquidity,
    LiquidityMap,
    SqrtPriceX96,
    Tick,
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolLiquidityMappingUpdate,
    UniswapV3PoolSimulationResult,
    UniswapV3PoolState,
    UniswapV3PoolStateUpdated,
)

type Token0Amount = int
type Token1Amount = int


class LiquidityAtTickAsDict(TypedDict):
    block: int
    liquidity_gross: int
    liquidity_net: int


class BitmapAtWordAsDict(TypedDict):
    bitmap: int
    block: int


class UniswapV3Pool(
    PublisherMixin,
    PoolPickleMixin,
    V3PoolState,
    UniswapV3PoolCalc,
    AbstractLiquidityPool,
):
    variant: ClassVar[str | None] = None

    LOG_HANDLERS: ClassVar[dict[str, Any]] = {
        V3_SWAP_TOPIC: decode_v3_swap,
        V3_MINT_TOPIC: decode_v3_mint,
        V3_BURN_TOPIC: decode_v3_burn,
    }

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
        address: str,
        *,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        tick_bitmap: (
            dict[int, BitmapAtWord]
            | dict[str, BitmapAtWord]
            | dict[int, BitmapAtWordAsDict]
            | dict[str, BitmapAtWordAsDict]
            | None
        ) = None,
        tick_data: (
            dict[int, LiquidityAtTick]
            | dict[str, LiquidityAtTick]
            | dict[int, LiquidityAtTickAsDict]
            | dict[str, LiquidityAtTickAsDict]
            | None
        ) = None,
        state_block: BlockNumber | None = None,
        state_cache_depth: int = 8,
        token0: Erc20Token,
        token1: Erc20Token,
        factory: str,
        fee: int,
        tick_spacing: int,
        sqrt_price_x96: int,
        tick: int,
        liquidity: int,
        tick_data_fetcher: Callable[[int, int], None] | None = None,
    ) -> None:
        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else token0.chain_id
        self._token0 = token0
        self._token1 = token1
        self.factory = get_checksum_address(factory)
        self._fee = fee
        self._tick_spacing = tick_spacing

        state_block_ = state_block if state_block is not None else 0
        self._initial_state_block = state_block_

        # Derive deployer/init_hash from factory deployments or fallback
        self.deployer_address = (
            get_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )
        self.init_hash = (
            init_hash if init_hash is not None else self.UNISWAP_V3_MAINNET_POOL_INIT_HASH
        )

        with contextlib.suppress(KeyError):
            if self._chain_id is not None:
                factory_deployment = _FACTORY_DEPLOYMENTS[self._chain_id][self.factory]
                self.init_hash = factory_deployment.pool_init_hash
                if factory_deployment.deployer is not None:
                    self.deployer_address = factory_deployment.deployer

        self.name = (
            f"{self._token0}-{self._token1} ({self.__class__.__name__}, "
            f"{100 * self._fee / self.FEE_DENOMINATOR:.2f}%)"
        )

        if (tick_bitmap is not None) != (tick_data is not None):
            raise DegenbotValueError(message="Provide both tick_bitmap and tick_data.")

        self._sparse_liquidity_map = tick_bitmap is None or tick_data is None

        working_tick_bitmap = (
            {}
            if tick_bitmap is None
            else {
                int(word): (
                    bitmap_at_word
                    if isinstance(bitmap_at_word, BitmapAtWord)
                    else BitmapAtWord(**bitmap_at_word)
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
                    if isinstance(liquidity_at_tick, LiquidityAtTick)
                    else LiquidityAtTick(**liquidity_at_tick)
                )
                for tick, liquidity_at_tick in tick_data.items()
            }
        )

        # Tick data fetcher for sparse liquidity maps
        # Set by Bot builder to delegate on-chain tick fetching
        self._tick_data_fetcher = tick_data_fetcher

        initial_state = self.PoolState.__value__(
            address=self.address,
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            tick_bitmap=working_tick_bitmap,
            tick_data=working_tick_data,
            block=state_block_,
        )
        self._state_lock = Lock()
        self._state_mgr = ConcentratedLiquidityStateManager(
            initial_state=initial_state,
            state_cache_depth=state_cache_depth,
        )
        self._subscribers: WeakSet[Subscriber] = WeakSet()

    def __getnewargs_ex__(self) -> tuple[tuple[()], dict[str, Any]]:
        """
        Return empty args so __init__ is not called during unpickling.
        The object is reconstructed via __setstate__.
        """
        return (), {}

    def __repr__(self) -> str:  # pragma: no cover
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1}, fee={100 * self._fee / self.FEE_DENOMINATOR:.2f}%, tick spacing={self._tick_spacing})"  # noqa:E501

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
                tick_spacing=self._tick_spacing,
                sparse=self._sparse_liquidity_map,
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
                tick_spacing=self._tick_spacing,
                sparse=self._sparse_liquidity_map,
            )
            liquidity_start = self.liquidity
            sqrt_price_x96_start = self.sqrt_price_x96
            tick_start = self.tick

        if self._sparse_liquidity_map:
            # Sparse map may raise MissingLiquidityData. Fetch missing data and retry.
            fetched_words: set[int] = set()
            while True:
                try:
                    result = _v3_swap(
                        snapshot=snapshot,
                        zero_for_one=zero_for_one,
                        amount_specified=amount_specified,
                        sqrt_price_limit_x96=sqrt_price_limit_x96,
                        fee=self._fee,
                        liquidity_start=liquidity_start,
                        sqrt_price_x96_start=sqrt_price_x96_start,
                        tick_start=tick_start,
                    )
                except MissingLiquidityData as exc:
                    if exc.word in fetched_words:
                        raise LiquidityPoolError(
                            message=f"Tick data fetcher did not resolve word {exc.word} "
                            f"on a previous attempt. "
                            f"pool={self.address} zfo={zero_for_one} "
                            f"amount_specified={amount_specified}"
                        ) from exc
                    if self._tick_data_fetcher is not None:
                        fetched_words.add(exc.word)
                        # Fetch missing word via the injected fetcher (typically from Bot)
                        self._tick_data_fetcher(exc.word, self.update_block)
                        # Rebuild snapshot from updated tick data
                        snapshot = LiquidityMapSnapshot.from_state(
                            self.state,
                            tick_spacing=self._tick_spacing,
                            sparse=True,
                        )
                    else:
                        raise
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
                fee=self._fee,
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

    def _verified_address(self) -> ChecksumAddress:
        return generate_v3_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self._token0.address, self._token1.address),
            fee=self._fee,
            init_hash=self.init_hash,
        )

    @property
    def chain_id(self) -> int | None:
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

        with self._state_lock:  # noqa:PLR1702
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
                tick_word, _ = get_tick_word_and_bit_position(tick, self._tick_spacing)

                if tick_word not in working_tick_bitmap:
                    # The liquidity map at the affected word must be complete prior to changing the
                    # status of any tick
                    if self._tick_data_fetcher is not None:
                        self._tick_data_fetcher(tick_word, state_block - 1)
                        # Refresh working bitmap from updated pool state ONLY if the fetcher
                        # successfully added the word
                        if tick_word in self.tick_bitmap:
                            working_tick_bitmap = self.tick_bitmap
                            # Merge newly fetched ticks into working_tick_data without overwriting
                            # existing entries (which may have been modified in earlier iterations)
                            for fetched_tick, fetched_liquidity in self.tick_data.items():
                                if fetched_tick not in working_tick_data:
                                    working_tick_data[fetched_tick] = fetched_liquidity
                        else:
                            # Fetcher failed to add the word (e.g., historical block unavailable)
                            # Create an empty entry so flip_tick can work
                            working_tick_bitmap[tick_word] = BitmapAtWord(
                                bitmap=0, block=state_block
                            )
                    elif self._sparse_liquidity_map:
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
                        tick_spacing=self._tick_spacing,
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
                        sparse=self._sparse_liquidity_map,
                        tick=tick,
                        tick_spacing=self._tick_spacing,
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
                message=UniswapV3PoolStateUpdated(working_state),
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

        # Capture initial state before any potential modifications (e.g., tick data fetching)
        initial_state = override_state if override_state is not None else self.state

        zero_for_one = token_in == self._token0

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
                initial_state=initial_state,
                final_state=dataclasses.replace(
                    initial_state,
                    liquidity=end_liquidity,
                    sqrt_price_x96=end_sqrt_price_x96,
                    tick=end_tick,
                    block=self.update_block if override_state is None else initial_state.block,
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

        # Capture initial state before any potential modifications (e.g., tick data fetching)
        initial_state = override_state if override_state is not None else self.state

        zero_for_one = token_out == self._token1

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
                initial_state=initial_state,
                final_state=dataclasses.replace(
                    initial_state,
                    liquidity=end_liquidity,
                    sqrt_price_x96=end_sqrtprice,
                    tick=end_tick,
                    block=self.update_block if override_state is None else initial_state.block,
                ),
            )

    def simulate_swap(
        self,
        token_in: ChecksumAddress,
        amount_in: int,
        token_out: ChecksumAddress,  # noqa: ARG002
        state_override: AbstractPoolState | None = None,
    ) -> SimulationResult:
        v3_state: UniswapV3PoolState | None = None
        if state_override is not None:
            if not isinstance(state_override, UniswapV3PoolState):
                msg = f"Expected UniswapV3PoolState, got {type(state_override).__name__}"
                raise DegenbotValueError(message=msg)
            v3_state = state_override
        if token_in == self._token0.address:
            token_in_obj = self._token0
        elif token_in == self._token1.address:
            token_in_obj = self._token1
        else:
            raise DegenbotValueError(message=f"token_in {token_in} not in pool")

        result = self.simulate_exact_input_swap(
            token_in=token_in_obj,
            token_in_quantity=amount_in,
            override_state=v3_state,
        )
        zero_for_one = token_in_obj == self._token0
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
        if token_out == self._token0.address:
            token_out_obj = self._token0
        elif token_out == self._token1.address:
            token_out_obj = self._token1
        else:
            raise DegenbotValueError(message=f"token_out {token_out} not in pool")

        result = self.simulate_exact_output_swap(
            token_out=token_out_obj,
            token_out_quantity=amount_out,
            override_state=state_override,
        )
        zero_for_one = token_out_obj == self._token1
        amount_in = result.amount0_delta if zero_for_one else result.amount1_delta
        return SimulationResult(
            amount_in=amount_in,
            amount_out=amount_out,
            initial_state=result.initial_state,
            final_state=result.final_state,
        )

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

    def build_swap_amount(
        self,
        zero_for_one: bool,  # noqa: FBT001
        amount_in: int,
        amount_out: int,
    ) -> UniswapV3PoolSwapAmounts:
        limit = MIN_SQRT_RATIO + 1 if zero_for_one else MAX_SQRT_RATIO - 1
        return UniswapV3PoolSwapAmounts(
            pool=self.address,
            amount_in=amount_in,
            amount_out=amount_out,
            amount_specified=amount_in,
            zero_for_one=zero_for_one,
            sqrt_price_limit_x96=limit,
        )
