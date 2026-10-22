"""Shared concentrated-liquidity companion surface.

``UniswapV3Pool`` and ``UniswapV4Pool`` carried duplicated copies of the
same concentrated liquidity write-back surface — the tick-map reads, the 3-format
``update_tick_data`` normalisation, ``external_update``,
``update_liquidity_map`` and the discard/restore delegation. This module
collapses that surface into one base so the concentrated liquidity state
write-back is written once.

What stays on the subclasses: identity (address/factory/tokens/fee/
pool_key/hook/protocol-fees), the family calc mixins, and ``__eq__``/
``__hash__``. This base does no family math (ADR-005: the driver shell is
not a co-implementation) and adds no cross-family Rust seam (ADR-014);
the companions stay Python (ADR-023).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, cast

from degenbot.exceptions import ExternalUpdateError
from degenbot.exceptions.pool import NoPoolStateAvailable
from degenbot.types.abstract import AbstractLiquidityPool
from degenbot.uniswap.concentrated.types import LiquidityAtTick

if TYPE_CHECKING:
    from degenbot._ffi import Pool
    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import ChecksummedAddress
    from degenbot.uniswap.types import UniswapPoolSwapVector
    from degenbot.uniswap.v3_types import (
        UniswapV3PoolExternalUpdate,
        UniswapV3PoolLiquidityMappingUpdate,
    )
    from degenbot.uniswap.v4_types import (
        UniswapV4PoolExternalUpdate,
        UniswapV4PoolLiquidityMappingUpdate,
    )


# A concentrated liquidity state (the family NamedTuple the ``state`` property builds).
type CLState = Any


class ConcentratedLiquidityCompanion(AbstractLiquidityPool):
    """Shared concentrated liquidity companion surface.

    Wraps a Rust-owned ``Pool`` handle.

    Subclasses: ``UniswapV3Pool`` / ``UniswapV4Pool`` (and anything built
    over the concentrated liquidity handle, e.g. ``AerodromeV3Pool``).
    The subclass state mixins supply ``tick_spacing`` and the identity;
    this base supplies the Rust-owned scalar/tick-map reads + the
    write-back delegation + the unified sparse-word backfill gate.
    """

    _py_pool: Pool
    name: str
    _initial_state_block: int
    address: ChecksummedAddress

    @property
    # Both concentrated liquidity families override this.
    def tick_spacing(self) -> int:  # pragma: no cover
        """Tick spacing (overridden: V3 state mixin, V4 pool key)."""
        msg = "tick_spacing must be provided by the family"
        raise NotImplementedError(msg)

    # --- Rust-owned scalar / tick-map reads --------------------------------

    @property
    def liquidity(self) -> int:
        """Liquidity. Returns: The current active liquidity (from Rust via the handle)."""
        return self._py_pool.concentrated_liquidity().liquidity

    @property
    def sqrt_price_x96(self) -> int:
        """Sqrt price x96. Returns: The current sqrt price as a Q64.96 value (from Rust)."""
        return cast("int", self._py_pool.concentrated_liquidity().sqrt_price_x96)

    @property
    def tick(self) -> int:
        """Tick. Returns: The current tick (from Rust via the handle)."""
        return self._py_pool.concentrated_liquidity().tick

    @property
    def tick_bitmap(self) -> set[int]:
        """Tick bitmap WORD positions known for the pool.

        The load-bearing key set only — the per-word bitmap VALUE is retired
        (it derives from the ``tick_data`` rows). Rust surfaces the words the
        pool knows: every word its ``tick_data`` keys land in, plus (Sparse
        only) the Rust ``known_bitmap_words`` recorded by
        ``update_tick_data``/fetch-merge, so a checked-but-empty word survives
        as a known word (the fetch loop breaks). Tracked pools know exactly
        their tick rows' words.
        """
        return {int(word) for word in self._py_pool.concentrated_liquidity().tick_bitmap}

    @property
    def tick_data(self) -> dict[int, LiquidityAtTick]:
        """Tick data.

        Returns a deep-copy snapshot of the Rust-side tick data
        (``{tick: (liquidity_gross, liquidity_net, block)}`` lifted into
        immutable ``LiquidityAtTick`` rows).
        """
        raw = self._py_pool.concentrated_liquidity().tick_data
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
        return self._py_pool.concentrated_liquidity().update_block

    @property
    def initial_state_block(self) -> int:
        """Block number at which the pool's initial state was captured.

        Returns:
            The block number from construction (DB snapshot or RPC fetch).

        """
        return self._initial_state_block

    # --- shared concentrated liquidity write-back --------------------------

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
        return cast("bool", state.sqrt_price_x96 > 1)

    def update_tick_data(
        self,
        tick_bitmap: set[int] | dict[int, Any],
        tick_data: dict[int, Any],
        block: int,
    ) -> None:
        """Apply updated tick bitmap and data from the tick data fetcher.

        Delegates to ``Pool.update_tick_data`` (replaces the
        Rust-side ``tick_data`` HashMap; scalars unchanged). The
        ``tick_bitmap`` is the set of checked words (any iterable of word
        ints — a ``dict`` iterates its keys): for Sparse pools the FFI records
        them in Rust ``known_bitmap_words`` (a checked word is never
        re-fetched); no bitmap VALUE crosses the boundary (the bitmap derives
        from the tick rows). Tracked pools record nothing (their bitmap is
        complete).
        """
        # Normalize LiquidityAtTick inputs into the tuple shape the Rust write
        # path expects: {tick: (gross, net, block)}.
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
        update: UniswapV3PoolExternalUpdate | UniswapV4PoolExternalUpdate,
    ) -> bool:
        """Process a family-external update (Swap event).

        Delegates the scalar write to ``Pool.apply_swap`` (journals
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
        update: UniswapV3PoolLiquidityMappingUpdate | UniswapV4PoolLiquidityMappingUpdate,
    ) -> None:
        """Apply an update to the liquidity map (Mint/Burn/ModifyLiquidity).

        Delegates the whole Mint/Burn application to
        ``Pool.apply_liquidity_update`` (Rust does the tick bitmap +
        tick_data mutation AND — for in-range events, since the 2026-08-02
        core fix (7347dcdc3) — the active ``liquidity`` scalar adjustment
        under one write guard). The companion performs no scalar write of
        its own: a separate ``apply_swap`` here would double-apply the
        in-range delta (the golden tracker test caught exactly that).

        """
        state_block = update.block_number

        # The active-liquidity scalar adjust for IN-RANGE events lives in the
        # Rust core now (7347dcdc3: "adjust in-range active liquidity on
        # liquidity events") — it is applied inside
        # ``apply_liquidity_update`` under the same write guard, journaled as
        # a scalar event for reorg rollback. The companion must NOT re-apply
        # it: the retired scalar block here double-counted every in-range
        # Mint/Burn since the companion collapse (caught by the golden
        # tracker test against chain truth).
        self._py_pool.apply_liquidity_update(
            tick_lower=update.tick_lower,
            tick_upper=update.tick_upper,
            liquidity_delta=update.liquidity,
            block_number=state_block,
        )

    # --- reorg journal delegation ------------------------------------------

    def discard_states_before_block(self, block: BlockNumber) -> None:
        """Discard cached states earlier than the given block.

        Raises:
            NoPoolStateAvailable: If the target is past the newest delta.

        """
        try:
            self._py_pool.discard_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e

    def restore_state_before_block(self, block: BlockNumber) -> None:
        """Restore the last pool state recorded prior to a target block.

        Delegates to ``Pool.restore_v3_before_block``
        (V3/V4-generic).

        Raises:
            NoPoolStateAvailable: If no state exists prior to the target block.

        """
        try:
            self._py_pool.restore_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e


__all__ = ["ConcentratedLiquidityCompanion"]
