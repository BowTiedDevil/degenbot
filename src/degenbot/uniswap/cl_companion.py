"""Shared concentrated-liquidity (CL) companion surface (T2 FBJTUM, epic OU4SYZ).

``UniswapV3Pool`` and ``UniswapV4Pool`` carried duplicated copies of the
same CL write-back surface — the tick-map reads, the 3-format
``update_tick_data`` normalisation, ``external_update``,
``update_liquidity_map`` and the discard/restore delegation. This module
collapses that surface into one base so "CL state write-back + the sparse
backfill gate" is written once.

What stays on the subclasses: identity (address/factory/tokens/fee/
pool_key/hook/protocol-fees), the family calc mixins, and ``__eq__``/
``__hash__``. This base does no family math (ADR-005: the driver shell is
not a co-implementation) and adds no cross-family Rust seam (ADR-014);
the companions stay Python (ADR-023).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from degenbot.exceptions import ExternalUpdateError
from degenbot.exceptions.pool import LiquidityMapWordMissing, NoPoolStateAvailable
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.math import (
    get_tick_word_and_bit_position as cl_get_tick_word_and_bit_position,
)

if TYPE_CHECKING:
    from typing import Protocol

    from degenbot._ffi import LiquidityPool
    from degenbot.types import BlockNumber
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.uniswap.types import UniswapPoolSwapVector

    class LiquidityMappingUpdate(Protocol):
        """Structural shape of the family Mint/Burn/ModifyLiquidity updates."""

        block_number: int
        liquidity: int
        tick_lower: int
        tick_upper: int


# A CL state (the family NamedTuple the ``state`` property builds).
type CLState = Any


class ConcentratedLiquidityCompanion(AbstractLiquidityPool):
    """Shared CL companion surface over a Rust-owned ``LiquidityPool`` handle.

    Subclasses: ``UniswapV3Pool`` / ``UniswapV4Pool`` (and anything built
    over the CL handle, e.g. ``AerodromeV3Pool``). The subclass state
    mixins supply ``tick_spacing`` and the identity; this base supplies the
    Rust-owned scalar/tick-map reads + the write-back delegation + the
    unified sparse-word backfill gate.
    """

    _py_pool: LiquidityPool
    name: str
    _initial_state_block: int
    address: ChecksummedAddress

    @property
    def tick_spacing(self) -> int:  # pragma: no cover - overridden by both CL families
        """Tick spacing (overridden: V3 state mixin, V4 pool key)."""
        msg = "tick_spacing must be provided by the family"
        raise NotImplementedError(msg)

    # --- Rust-owned scalar / tick-map reads --------------------------------

    @property
    def liquidity(self) -> int:
        """Liquidity. Returns: The current active liquidity (from Rust via the handle)."""
        return self._py_pool.liquidity

    @property
    def sqrt_price_x96(self) -> int:
        """Sqrt price x96. Returns: The current sqrt price as a Q64.96 value (from Rust)."""
        return self._py_pool.sqrt_price_x96

    @property
    def tick(self) -> int:
        """Tick. Returns: The current tick (from Rust via the handle)."""
        return self._py_pool.tick

    @property
    def tick_bitmap(self) -> dict[int, BitmapAtWord]:
        """Tick bitmap.

        A deep-copy snapshot of the Rust-side tick bitmap: derived from
        ``tick_data`` keys, + for Sparse pools the checked-but-empty words
        from Rust ``known_bitmap_words`` as ``(0, block)`` entries (a word
        checked via ``update_tick_data``/fetch-merge survives as
        present-but-zero — the fetch loop breaks). Tracked pools return the
        pure derivation (absent word = known-empty; the map is complete).
        """
        raw = self._py_pool.tick_bitmap_snapshot()
        return {
            int(word): (
                BitmapAtWord(bitmap=int(row[0]), block=int(row[1]))
                if not isinstance(row, BitmapAtWord)
                else row
            )
            for word, row in raw.items()
        }

    @property
    def tick_data(self) -> dict[int, LiquidityAtTick]:
        """Tick data.

        Returns a deep-copy snapshot of the Rust-side tick data
        (``{tick: (liquidity_gross, liquidity_net, block)}`` lifted into
        immutable ``LiquidityAtTick`` rows).
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
    def update_block(self) -> BlockNumber:  # type: ignore[name-defined]
        """Update block. Returns: The block number of the most recent state update (from Rust)."""
        return self._py_pool.update_block

    @property
    def initial_state_block(self) -> int:
        """Block number at which the pool's initial state was captured.

        Returns:
            The block number from construction (DB snapshot or RPC fetch).

        """
        return self._initial_state_block

    # --- shared CL write-back ----------------------------------------------

    def swap_is_viable(  # ruff:ignore[no-self-use]
        self,
        state: CLState,
        vector: UniswapPoolSwapVector,  # ruff:ignore[unused-method-argument]
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

        Delegates to ``LiquidityPool.update_tick_data`` (replaces the
        Rust-side ``tick_data`` HashMap; scalars unchanged). The
        ``tick_bitmap`` KEYS are the checked words: for Sparse pools the FFI
        records them in Rust ``known_bitmap_words`` (a checked word is never
        re-fetched); the VALUES are NOT stored (the bitmap derives from the
        tick rows). Tracked pools record nothing (their bitmap is complete).
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
                    int(info[2]) if len(info) > 2 else block,  # ruff:ignore[magic-value-comparison]
                )
        self._py_pool.update_tick_data(tick_bitmap, normalized, block)

    def external_update(
        self,
        update: LiquidityMappingUpdate,
    ) -> bool:
        """Process a family-external update (Swap event).

        Delegates the scalar write to ``LiquidityPool.apply_swap`` (journals
        the priors then lands the new ``sqrt_price_x96``/``liquidity``/
        ``tick`` at ``block_number`` in one write guard).

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
        return True

    def update_liquidity_map(
        self,
        update: LiquidityMappingUpdate,
    ) -> None:
        """Apply an update to the liquidity map (Mint/Burn/ModifyLiquidity).

        Delegates the tick mutation to ``LiquidityPool.apply_liquidity_update``
        (Rust does the tick bitmap + tick_data mutation under one write
        guard). The active ``liquidity`` scalar adjustment (when
        ``current_tick`` is in range) is then landed via a separate
        ``apply_swap`` carrying the new scalar.

        Raises:
            LiquidityMapWordMissing: A Sparse boundary word could not be
                backfilled (no stored fetcher or the fetch failed) — the
                event is NOT applied (a silent apply over an unknown word
                would corrupt the reorg journal's tick priors).

        """
        state_block = update.block_number

        # Unified sparse-word backfill gate (T2 FBJTUM): Rust `coverage` is
        # the fact (the twin's double-tracked sparseness flags are retired).
        # For each boundary tick, a word ABSENT from the derived bitmap is
        # non-deterministic (Sparse semantics) → backfill it via the
        # RUST-stored fetcher at state_block - 1; a failed fetch RAISES
        # rather than applying over an unknown word (the reorg journal's
        # priors for those ticks would be wrong). Tracked pools: inert (the
        # bitmap is complete; absent word = known-empty).
        if self._py_pool.coverage == "sparse":
            for tick in (update.tick_lower, update.tick_upper):
                word, _ = cl_get_tick_word_and_bit_position(tick, self.tick_spacing)
                # Short-circuit: ensure_word_known is only called for a word
                # the derived bitmap says is unknown.
                if word not in self.tick_bitmap and not self._py_pool.ensure_word_known(
                    word, state_block - 1
                ):
                    raise LiquidityMapWordMissing(word)

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

    # --- reorg journal delegation ------------------------------------------

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

        Delegates to ``LiquidityPool.restore_v3_before_block``
        (V3/V4-generic).

        Raises:
            NoPoolStateAvailable: If no state exists prior to the target block.

        """
        try:
            self._py_pool.restore_v3_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e


__all__ = ["ConcentratedLiquidityCompanion"]
