"""UniswapV3Pool: concentrated liquidity AMM companion over a PyLiquidityPool handle.

ADR-005 slice 8b — the V3 companion rewritten over the same `PyLiquidityPool`
handle topology as the V2 `LiquidityPool`. Rust `BotState` is the single
source of truth for V3 mutable state (scalars, tick data, reorg journal);
this companion reads it through `self._py_pool` (the atomic `snapshot_v3()`
for scalars + `tick_data_snapshot()`/`tick_bitmap_snapshot()` for the tick maps)
and delegates `external_update` (Swap) / `update_liquidity_map` (Mint/Burn) /
`update_tick_data` (sparse-map backfill) / discard / restore to the handle.
Immutable identity (tokens, factory, fee, tick_spacing) stays Python-side —
matches V2 (calc lives in the `UniswapV3PoolCalc` mixin).

`_state_mgr` / `_state_cache` / `state_cache_depth` are dropped — the
`StateCache` temporal-navigation layer lives in Rust now (journal +
discard/restore). V2 already has none; V3 follows.

Sparse-map bitmap note: Rust's tick bitmap is DERIVED from `tick_data` keys
(no separate bitmap store), so a "checked-empty word" (a tick-data fetcher
probed `tickBitmap(word)` and the on-chain bitmap was zero) would vanish from
the derived bitmap, causing the simulator to re-fetch the same word forever.
The companion tracks `_bitmap_override` client-side: words the fetcher has
checked (via `update_tick_data`) are remembered so the simulator sees them as
present-but-zero (not missing), breaking the fetch loop. Mirrors how V2 has no
sparse-map concept at all.
"""

import dataclasses
from collections.abc import Callable
from typing import Any, ClassVar, TypedDict
from weakref import WeakSet

from eth_typing import ChecksumAddress

from degenbot.arbitrage.types import UniswapV3PoolSwapAmounts
from degenbot.checksum_cache import get_checksum_address
from degenbot.degenbot_rs import PyLiquidityPool
from degenbot.erc20 import Erc20Token
from degenbot.exceptions import DegenbotValueError
from degenbot.exceptions.pool import (
    EVMRevertError,
    ExternalUpdateError,
    LiquidityPoolError,
    NoPoolStateAvailable,
)
from degenbot.types.abstract import AbstractLiquidityPool, AbstractPoolState
from degenbot.types.aliases import BlockNumber, ChainId
from degenbot.types.concrete import PublisherMixin, Subscriber
from degenbot.types.hop_types import BoundedProductHop, HopType, V3TickRangeInfo
from degenbot.types.pool_protocols import SimulationResult
from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot, MissingLiquidityData
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.concentrated.v3_simulator import calculate_swap as _v3_swap
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
from degenbot.uniswap.v3_libraries.tick_bitmap import gen_ticks
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
    """Serialized form of ``LiquidityAtTick`` for tick data interchange."""

    liquidity_net: int
    liquidity_gross: int
    block: BlockNumber


class BitmapAtWordAsDict(TypedDict):
    """Serialized form of ``BitmapAtWord`` for tick bitmap interchange."""

    bitmap: int
    block: BlockNumber


class UniswapV3Pool(
    PublisherMixin,
    V3PoolState,
    UniswapV3PoolCalc,
    AbstractLiquidityPool,
):
    """A Uniswap V3 concentrated-liquidity pool companion over a ``PyLiquidityPool`` handle.

    Rust owns the mutable state (scalars + tick data + reorg journal) as
    ``V3PoolState``; this companion reads it through ``self._py_pool`` (one
    atomic ``snapshot_v3()`` for scalars + ``tick_data_snapshot()`` /
    ``tick_bitmap_snapshot()`` for the tick maps) and delegates
    ``external_update`` (Swap) / ``update_liquidity_map`` (Mint/Burn) /
    ``update_tick_data`` (sparse-map backfill) / discard / restore to the
    handle. Immutable identity (tokens, factory, fee, tick_spacing) stays
    Python-side — matches V2.

    Construct via ``Bot.build_pool()`` (which registers in Rust and hands the
    handle here); tests use ``make_v3_pool``.
    """

    variant: ClassVar[str | None] = None

    LOG_HANDLERS: ClassVar[dict[str, Any]] = {
        V3_SWAP_TOPIC: decode_v3_swap,
        V3_MINT_TOPIC: decode_v3_mint,
        V3_BURN_TOPIC: decode_v3_burn,
    }

    type PoolState = UniswapV3PoolState

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

    def __init__(
        self,
        py_pool: PyLiquidityPool,
        *,
        address: ChecksumAddress | str,
        token0: Erc20Token,
        token1: Erc20Token,
        factory: str,
        fee: int,
        tick_spacing: int,
        chain_id: ChainId | None = None,
        deployer_address: str | None = None,
        init_hash: str | None = None,
        tick_data_fetcher: Callable[[int, int], None] | None = None,
        state_block: BlockNumber | None = None,
        sparse_liquidity_map: bool | None = None,
        tick_bitmap_override: dict[int, Any] | None = None,
    ) -> None:
        """Initialize the instance over a ``PyLiquidityPool`` handle.

        Construction is purely in-memory (no I/O, no failure modes).
        """
        self._py_pool = py_pool
        self.address = get_checksum_address(address)
        self._chain_id = chain_id if chain_id is not None else token0.chain_id
        self._token0 = token0
        self._token1 = token1
        self.factory = get_checksum_address(factory)
        self._fee = fee
        self._tick_spacing = tick_spacing

        # The block of the registration snapshot (the genesis journal delta).
        # Used to gate early-block liquidity updates (matches the pre-companion
        # ``_initial_state_block`` rule: liquidity events prior to the
        # registration snapshot bypass the in-range active-liquidity
        # adjustment so historical replay doesn't trip the invariant).
        self._initial_state_block = (
            state_block if state_block is not None else self._py_pool.update_block
        )

        # Derive deployer/init_hash from constructor args or class defaults.
        self.deployer_address = (
            get_checksum_address(deployer_address) if deployer_address is not None else self.factory
        )
        self.init_hash = (
            init_hash if init_hash is not None else self.UNISWAP_V3_MAINNET_POOL_INIT_HASH
        )

        self.name = (
            f"{self._token0}-{self._token1} ({self.__class__.__name__}, "
            f"{100 * self._fee / self.FEE_DENOMINATOR:.2f}%)"
        )

        # Sparse-map detection: a pool is "sparse" when no tick data has been
        # seeded yet (the fetcher backfills on demand). The companion infers
        # sparseness from the Rust-side tick map unless the builder explicitly
        # overrides it.
        self._sparse_liquidity_map = (
            sparse_liquidity_map
            if sparse_liquidity_map is not None
            else len(self._py_pool.tick_data_snapshot()) == 0
        )

        # Track tick-data-fetcher-checked words client-side so a "checked but
        # empty" word (on-chain ``tickBitmap(word) == 0``) appears in the
        # bitmap the simulator sees as present-but-zero rather than missing —
        # otherwise the sparse-map fetch loop re-fetches the same word forever
        # (Rust derives the bitmap from ``tick_data`` KEYS only, so a word with
        # no initialized ticks vanishes from the derived bitmap). Mirrors the
        # pre-companion StateManager which stored the bitmap dict explicitly.
        self._bitmap_override: dict[int, BitmapAtWord] = {}
        if tick_bitmap_override is not None:
            for word, bitmap_at_word in tick_bitmap_override.items():
                if isinstance(bitmap_at_word, BitmapAtWord):
                    self._bitmap_override[int(word)] = bitmap_at_word
                elif isinstance(bitmap_at_word, dict):
                    self._bitmap_override[int(word)] = BitmapAtWord(
                        bitmap=int(bitmap_at_word.get("bitmap", 0)),
                        block=int(bitmap_at_word.get("block", 0)),
                    )
                else:
                    self._bitmap_override[int(word)] = BitmapAtWord(
                        bitmap=int(bitmap_at_word[0]),
                        block=int(bitmap_at_word[1]) if len(bitmap_at_word) > 1 else 0,
                    )

        # Tick data fetcher for sparse liquidity maps (rare simulation
        # backfill path; the engine owns tick data in the production path).
        self._tick_data_fetcher = tick_data_fetcher

        self._subscribers: WeakSet[Subscriber] = WeakSet()

    def __repr__(self) -> str:  # pragma: no cover
        """Return the canonical string representation.

        Returns:
            The string representation of the pool.

        """
        return f"{self.__class__.__name__}(address={self.address}, token0={self._token0}, token1={self._token1}, fee={100 * self._fee / self.FEE_DENOMINATOR:.2f}%, tick spacing={self._tick_spacing})"  # noqa:E501

    def __str__(self) -> str:
        """Return the canonical string representation.

        Returns:
            The pool name string.

        """
        return self.name

    def _calculate_swap(
        self,
        *,
        zero_for_one: bool,
        amount_specified: int,
        sqrt_price_limit_x96: int,
        override_state: PoolState | None = None,
    ) -> tuple[Token0Amount, Token1Amount, SqrtPriceX96, Liquidity, Tick]:
        """Ported and adapted from the UniswapV3Pool.sol contract.

        https://github.com/Uniswap/v3-core/blob/main/contracts/UniswapV3Pool.sol

        Returns a tuple with amounts and final pool state values for a successful swap:
        (amount0, amount1, sqrt_price_x96, liquidity, tick)

        A negative amount indicates the token quantity sent to the swapper, and a positive amount
        indicates the token quantity deposited.

        Returns:
            Tuple of (amount0, amount1, sqrt_price_x96, liquidity, tick).

        Raises:
            LiquidityPoolError: If tick data fetcher fails to resolve a word.
            MissingLiquidityData: If a sparse liquidity map is missing a required word.

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
            # The tick bitmap and tick data accessed through the property are
            # copies, so they can be freely modified without corrupting state.
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
            # Sparse map may raise MissingLiquidityData. Fetch + retry.
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
                        # Fetch missing word via the injected fetcher
                        # (typically from Bot). The fetcher calls the
                        # companion's ``update_tick_data`` which records the
                        # word in ``_bitmap_override`` so the next
                        # ``tick_bitmap`` read sees it as present-but-zero.
                        self._tick_data_fetcher(exc.word, self.update_block)
                        # Rebuild snapshot from updated tick data
                        snapshot = LiquidityMapSnapshot(
                            tick_data=self.tick_data,
                            tick_bitmap=self.tick_bitmap,
                            tick_spacing=self._tick_spacing,
                            sparse=self._sparse_liquidity_map,
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
        """Compute the deterministic V3 pool address via CREATE2.

        Returns:
            The checksummed address that should match this pool's address.

        """
        return generate_v3_pool_address(
            deployer_address=self.deployer_address,
            token_addresses=(self._token0.address, self._token1.address),
            fee=self._fee,
            init_hash=self.init_hash,
        )

    @property
    def chain_id(self) -> int | None:
        """Return chain id.

        Returns:
            The chain ID, or None if not set.

        """
        return self._chain_id

    @property
    def liquidity(self) -> int:
        """Return liquidity.

        Returns:
            The current active liquidity (from Rust via the handle).

        """
        return self._py_pool.liquidity

    @property
    def sqrt_price_x96(self) -> int:
        """Return sqrt price x96.

        Returns:
            The current sqrt price as a Q64.96 value (from Rust).

        """
        return self._py_pool.sqrt_price_x96

    @property
    def state(self) -> PoolState:
        """State.

        Returns:
            The current pool state, built from one atomic Rust scalar snapshot
            (``_py_pool.snapshot_v3()``) + the tick-map snapshots. The scalars
            (sqrt_price/liquidity/tick/block) cannot tear; the tick maps are
            deep-copied snapshots the simulation path can mutate freely.

        Raises:
            DegenbotValueError: If the pool is not registered in Rust.

        """
        snap = self._py_pool.snapshot_v3()
        if snap is None:
            msg = "No V3 pool state available (pool not registered in Rust)"
            raise DegenbotValueError(message=msg)
        sqrt_price_x96, liquidity, tick, block = snap
        return self.PoolState.__value__(
            address=self.address,
            liquidity=liquidity,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            tick_bitmap=self.tick_bitmap,
            tick_data=self.tick_data,
            block=block,
        )

    @property
    def tick(self) -> int:
        """Return tick.

        Returns:
            The current tick (from Rust via the handle).

        """
        return self._py_pool.tick

    @property
    def tick_bitmap(self) -> InitializedTickMap:
        """Tick bitmap.

        Returns a deep-copy snapshot of the Rust-side tick bitmap (built from
        ``tick_data`` keys — Rust derives the bitmap, no separate store) MERGED
        with the companion's verbatim ``_bitmap_override`` (words the
        builder/snapshot/fetcher passed via ``update_tick_data`` — these win
        over the derived bitmap for any word present in the override, so a
        snapshot's on-chain bitmap is preserved verbatim AND a fetcher-checked
        empty word is seen as present-but-zero). The result is a fresh dict
        each read so the simulation path can mutate it freely without
        corrupting state.
        """
        raw = self._py_pool.tick_bitmap_snapshot()
        result: dict[int, BitmapAtWord] = {
            int(word): (
                BitmapAtWord(bitmap=int(row[0]), block=int(row[1]))
                if not isinstance(row, BitmapAtWord)
                else row
            )
            for word, row in raw.items()
        }
        # Apply the verbatim override on top: any word present in the override
        # replaces the derived entry (so snapshot bitmaps are preserved
        # verbatim and checked-empty words appear as present-but-zero).
        for word, bitmap_at_word in self._bitmap_override.items():
            result[word] = bitmap_at_word  # noqa: PERF403
        return result

    @property
    def tick_data(self) -> LiquidityMap:
        """Tick data.

        Returns a deep-copy snapshot of the Rust-side tick data, built from
        ``tick_data_snapshot()`` (``{tick: (liquidity_gross, liquidity_net,
        block)}``), lifting each row into an immutable ``LiquidityAtTick``.
        Mirrors how V2's ``state`` returns a fresh state each read.
        """
        raw = self._py_pool.tick_data_snapshot()
        return {
            int(tick): (
                LiquidityAtTick(
                    liquidity_net=int(row[1]),
                    liquidity_gross=int(row[0]),
                    block=int(row[2]),
                )
                if not isinstance(row, LiquidityAtTick)
                else row
            )
            for tick, row in raw.items()
        }

    @property
    def update_block(self) -> BlockNumber:
        """Update block.

        Returns:
            The block number of the most recent state update (from Rust).

        """
        return self._py_pool.update_block

    def swap_is_viable(  # noqa: PLR6301
        self,
        state: PoolState,
        vector: UniswapPoolSwapVector,  # noqa: ARG002
    ) -> bool:
        """Swap is viable.

        Returns:
            True if a swap can proceed with the given state, False otherwise.

        """
        if state.liquidity == 0:
            return False
        return state.sqrt_price_x96 > 1

    def update_tick_data(
        self,
        tick_bitmap: dict[int, Any],
        tick_data: dict[int, Any],
        block: int,
    ) -> None:
        """Apply updated tick bitmap and data from the tick data fetcher.

        Delegates to ``PyLiquidityPool.update_tick_data`` (replaces the
        Rust-side ``tick_data`` HashMap; scalars unchanged; ``update_block``
        advances when newer). Records every word in ``tick_bitmap`` into
        ``_bitmap_override`` so the verbatim on-chain bitmap is preserved
        (snapshot round-trip) AND checked-empty words are seen as
        present-but-zero (sparse-map fetch loop break) — Rust's derived bitmap
        can't reproduce either, so the companion overlays the verbatim words.
        """
        # Normalize LiquidityAtTick/BitmapAtWord inputs into the tuple shape
        # the Rust write path expects: {tick: (gross, net, block)}.
        normalized: dict[int, tuple[int, int, int]] = {}
        for tick, info in tick_data.items():
            if isinstance(info, LiquidityAtTick):
                normalized[int(tick)] = (
                    int(info.liquidity_gross),
                    int(info.liquidity_net),
                    int(info.block),
                )
            elif isinstance(info, dict):
                normalized[int(tick)] = (
                    int(info["liquidity_gross"]),
                    int(info["liquidity_net"]),
                    int(info.get("block", 0)),
                )
            else:
                normalized[int(tick)] = (
                    int(info[0]),
                    int(info[1]),
                    int(info[2]) if len(info) > 2 else block,  # noqa: PLR2004
                )
        self._py_pool.update_tick_data(tick_bitmap, normalized, block)
        # Record every word the caller passed (verbatim bitmap override) so
        # the ``tick_bitmap`` property overlays them on the derived bitmap.
        for word, bitmap_at_word in tick_bitmap.items():
            if isinstance(bitmap_at_word, BitmapAtWord):
                self._bitmap_override[int(word)] = bitmap_at_word
            elif isinstance(bitmap_at_word, dict):
                self._bitmap_override[int(word)] = BitmapAtWord(
                    bitmap=int(bitmap_at_word.get("bitmap", 0)),
                    block=int(bitmap_at_word.get("block", block)),
                )
            else:
                self._bitmap_override[int(word)] = BitmapAtWord(
                    bitmap=int(bitmap_at_word[0]),
                    block=int(bitmap_at_word[1]) if len(bitmap_at_word) > 1 else block,
                )
        self._notify_subscribers(message=UniswapV3PoolStateUpdated(self.state))
        # NOTE: ``_sparse_liquidity_map`` is NOT flipped here — the fetcher's
        # incremental word backfill (the common caller of this method) must
        # keep the pool sparse so subsequent swaps re-enter the fetch retry
        # loop. A full-snapshot replace (builder path) sets sparseness at
        # construction via the ``sparse_liquidity_map`` param, NOT via this
        # method (the builder seeds tick data on the handle via the Rust FFI
        # ``update_tick_data``, not this companion method).

    def external_update(
        self,
        update: UniswapV3PoolExternalUpdate,
    ) -> bool:
        """Process a `UniswapV3PoolExternalUpdate` (Swap event).

        Delegates the scalar write to ``PyLiquidityPool.apply_swap`` (journals
        the priors then lands the new ``sqrt_price_x96``/``liquidity``/``tick``
        at ``block_number`` in one write guard).

        Returns:
            True if any updated state value was recorded, False otherwise.

        Raises:
            ExternalUpdateError: If the update is for an invalid block.

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

        self._py_pool.apply_swap(
            sqrt_price_x96=update.sqrt_price_x96,
            liquidity=update.liquidity,
            tick=update.tick,
            block_number=update.block_number,
        )
        self._notify_subscribers(message=UniswapV3PoolStateUpdated(self.state))
        return True

    def update_liquidity_map(
        self,
        update: UniswapV3PoolLiquidityMappingUpdate,
    ) -> None:
        """Apply an update to the liquidity map (Mint/Burn).

        Delegates the tick mutation to ``PyLiquidityPool.apply_liquidity_update``
        (Rust does the tick bitmap + tick_data mutation under one write guard,
        matching ``BotState::apply_v3_liquidity_update``). The active
        ``liquidity`` scalar adjustment (when ``current_tick`` is in range)
        is then landed via a separate ``apply_swap`` carrying the new scalar —
        mirroring the pre-companion ``push_state(working_state)`` semantics.
        """
        state_block = update.block_number

        # Pre-flight: if either boundary tick lives in a sparse word we don't
        # yet have, backfill via the fetcher (mirrors the pre-companion path).
        if self._sparse_liquidity_map and self._tick_data_fetcher is not None:
            for tick in (update.tick_lower, update.tick_upper):
                word, _ = get_tick_word_and_bit_position(tick, self._tick_spacing)
                if word not in self.tick_bitmap:
                    self._tick_data_fetcher(word, state_block - 1)

        applied = self._py_pool.apply_liquidity_update(
            tick_lower=update.tick_lower,
            tick_upper=update.tick_upper,
            liquidity_delta=update.liquidity,
            block_number=state_block,
        )

        # Active-liquidity scalar adjust when the modified region crosses the
        # active tick. Skipped for historical replay (state_block <= the
        # registration block) to mirror the pre-companion invariant rule.
        if (
            applied
            and update.tick_lower <= self.tick < update.tick_upper
            and state_block > self._initial_state_block
        ):
            new_active = self.liquidity + update.liquidity
            assert new_active >= 0, (
                f"In-range liquidity adjustment violated invariant: pool {self.address} "
                f"{self.tick=} {self.liquidity=} {self.update_block=} {update=}"
            )
            # Land the adjusted active scalar via a scalar write (separate
            # from the tick-only ``apply_liquidity_update`` write above).
            self._py_pool.apply_swap(
                sqrt_price_x96=self.sqrt_price_x96,
                liquidity=new_active,
                tick=self.tick,
                block_number=state_block,
            )

        self._notify_subscribers(message=UniswapV3PoolStateUpdated(self.state))

    def discard_states_before_block(self, block: BlockNumber) -> None:
        """Discard cached states earlier than the given block.

        Raises:
            NoPoolStateAvailable: If the target is past the newest delta.

        """
        try:
            self._py_pool.discard_v3_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e

    def restore_state_before_block(self, block: BlockNumber) -> None:
        """Restore the last pool state recorded prior to a target block.

        Delegates to ``PyLiquidityPool.restore_v3_before_block`` (Rust pops
        journal deltas at/after the target + reverse-applies tick priors +
        writes back pre-target scalars in one write guard). The journal's
        ``update_block`` lands at the oldest popped delta's block (the target
        convention); the restored scalars are the pre-target state. Subscribers
        are notified with the restored state.

        Raises:
            NoPoolStateAvailable: If no state exists prior to the target block.

        """
        try:
            restored = self._py_pool.restore_v3_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e
        if restored is not None:
            self._notify_subscribers(message=UniswapV3PoolStateUpdated(self.state))

    def simulate_exact_input_swap(
        self,
        token_in: Erc20Token,
        token_in_quantity: int,
        sqrt_price_limit_x96: int | None = None,
        override_state: PoolState | None = None,
    ) -> UniswapV3PoolSimulationResult:
        """Simulate an exact input swap.

        Returns:
            The simulation result with delta amounts and state transitions.

        Raises:
            DegenbotValueError: If token_in is unknown.
            LiquidityPoolError: If the simulated execution reverts.

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
        """Simulate an exact output swap.

        Returns:
            The simulation result with delta amounts and state transitions.

        Raises:
            DegenbotValueError: If token_out is unknown.
            LiquidityPoolError: If the simulated execution reverts.

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
        """Simulate swap.

        Returns:
            The simulation result with amounts and state transitions.

        Raises:
            DegenbotValueError: If tokens are unknown or mismatched.

        """
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
        """Simulate swap for output.

        Returns:
            The simulation result with amounts and state transitions.

        Raises:
            DegenbotValueError: If token_out is unknown.

        """
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

    def to_hop_state(
        self,
        zero_for_one: bool,  # noqa: FBT001
        state_override: UniswapV3PoolState | None = None,
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

    def build_swap_amount(
        self,
        zero_for_one: bool,  # noqa: FBT001
        amount_in: int,
        amount_out: int,
    ) -> UniswapV3PoolSwapAmounts:
        """Build swap amount.

        Returns:
            The swap amounts object for encoding.

        """
        limit = MIN_SQRT_RATIO + 1 if zero_for_one else MAX_SQRT_RATIO - 1
        return UniswapV3PoolSwapAmounts(
            pool=self.address,
            amount_in=amount_in,
            amount_out=amount_out,
            amount_specified=amount_in,
            zero_for_one=zero_for_one,
            sqrt_price_limit_x96=limit,
        )
