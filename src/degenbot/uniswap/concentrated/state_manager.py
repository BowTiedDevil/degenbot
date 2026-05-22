"""
State manager for concentrated-liquidity pools.

Delegates temporal state navigation (deque, lock, discard/restore) to
a :class:`~degenbot.types.state_cache.StateCache` and adds CL-specific
convenience properties (``liquidity``, ``sqrt_price_x96``, ``tick``,
``tick_bitmap``, ``tick_data``, ``swap_is_viable``).

**The caller is responsible for all locking** — the pool must hold
``_state_cache.lock()`` when calling any method that mutates state or
traverses history.  The only exception is ``swap_is_viable``, which is
a pure query.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

from degenbot.exceptions.pool import NoPoolStateAvailable
from degenbot.types.state_cache import StateCache
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MIN_SQRT_RATIO,
    get_sqrt_ratio_at_tick,
)

if TYPE_CHECKING:
    from collections.abc import Mapping

    from degenbot.types.aliases import BlockNumber


class _StateLike(Protocol):
    """Minimum shape required by :class:`ConcentratedLiquidityStateManager`."""

    @property
    def block(self) -> int | None: ...

    @property
    def liquidity(self) -> int: ...

    @property
    def sqrt_price_x96(self) -> int: ...

    @property
    def tick(self) -> int: ...

    @property
    def tick_bitmap(self) -> Mapping[int, object]: ...

    @property
    def tick_data(self) -> Mapping[int, object]: ...


class ConcentratedLiquidityStateManager[StateT: _StateLike]:
    """
    Unlocked data structure for a bounded history of pool states.

    Delegates the deque and temporal navigation to a
    :class:`~degenbot.types.state_cache.StateCache`. Adds CL-specific
    read helpers on top.

    Every public method is **non-blocking**; the caller (pool) must hold
    the lock when calling any method that mutates state or traverses history.
    """

    def __init__(
        self,
        *,
        initial_state: StateT,
        state_cache_depth: int = 8,
    ) -> None:
        """Initialize the instance."""
        self._state_cache = StateCache(max_depth=state_cache_depth)
        block = initial_state.block
        self._state_cache.append(initial_state, block=block)
        self._initial_state_block: int | None = initial_state.block

    # --- read helpers ---

    @property
    def state(self) -> StateT:
        """State."""
        return self._state_cache.current

    @property
    def liquidity(self) -> int:
        """Return liquidity."""
        return self.state.liquidity

    @property
    def sqrt_price_x96(self) -> int:
        """Return sqrt price x96."""
        return self.state.sqrt_price_x96

    @property
    def tick(self) -> int:
        """Return tick."""
        return self.state.tick

    @property
    def tick_bitmap(self) -> dict[int, object]:
        """Tick bitmap."""
        return dict(self.state.tick_bitmap)

    @property
    def tick_data(self) -> dict[int, object]:
        """Tick data."""
        return dict(self.state.tick_data)

    @property
    def update_block(self) -> int | None:
        """Update block."""
        return self.state.block

    @property
    def state_cache(self) -> StateCache[StateT]:
        """State cache."""
        return self._state_cache

    @state_cache.setter
    def state_cache(self, value: StateCache[StateT]) -> None:
        self._state_cache = value

    # --- write helpers ---

    def push_state(self, new_state: StateT) -> None:
        """Append *new_state*, replacing the entry at the same block if present."""
        self._state_cache.append(new_state, block=new_state.block)

    def discard_states_before_block(self, block: BlockNumber) -> None:
        """Drop states strictly older than *block*."""
        try:
            self._state_cache.discard_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e

    def restore_state_before_block(self, block: BlockNumber) -> StateT:
        """
        Rewind to the most recent state prior to *block*.

        Returns the restored state so the caller can emit an event or
        subscribe notification.
        """
        try:
            return self._state_cache.restore_before_block(block)
        except ValueError as e:
            raise NoPoolStateAvailable(block=block) from e

    # --- swap viability (pure query, no lock needed) ---

    @staticmethod
    def swap_is_viable(
        *,
        state: _StateLike,
        zero_for_one: bool,
        sparse_liquidity_map: bool,
    ) -> bool:
        """Swap is viable."""
        if sparse_liquidity_map:
            return True

        if not state.tick_data:
            return False

        if state.sqrt_price_x96 == 0:
            return False

        if (zero_for_one and state.sqrt_price_x96 <= MIN_SQRT_RATIO + 1) or (
            not zero_for_one and state.sqrt_price_x96 >= MAX_SQRT_RATIO - 1
        ):
            return False

        if zero_for_one:
            return get_sqrt_ratio_at_tick(min(state.tick_data)) < state.sqrt_price_x96
        return get_sqrt_ratio_at_tick(max(state.tick_data)) > state.sqrt_price_x96
