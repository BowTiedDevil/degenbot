"""State manager for concentrated-liquidity pools.

Owns the mutable ``_state_cache`` deque and provides thin read/write helpers.
**The caller is responsible for all locking** — this module does not import
``threading``.  RPC fetching and event emission stay on the pool.
"""

from __future__ import annotations

from collections import deque
from collections.abc import Mapping
from typing import TYPE_CHECKING, Generic, Protocol, TypeVar

from degenbot.exceptions.liquidity_pool import NoPoolStateAvailable
from degenbot.uniswap.v3_libraries.tick_math import (
    MAX_SQRT_RATIO,
    MIN_SQRT_RATIO,
    get_sqrt_ratio_at_tick,
)

if TYPE_CHECKING:
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


_StateT = TypeVar("_StateT", bound=_StateLike)


class ConcentratedLiquidityStateManager(Generic[_StateT]):
    """Unlocked data structure for a bounded history of pool states.

    Every public method is **non-blocking**; the caller (pool) must hold the
    lock when calling any method that mutates state or traverses history.
    """

    def __init__(
        self,
        *,
        initial_state: _StateT,
        state_cache_depth: int = 8,
    ) -> None:
        self._state_cache: deque[_StateT] = deque(maxlen=max(1, state_cache_depth))
        self._state_cache.append(initial_state)
        self._initial_state_block: int | None = initial_state.block

    # --- read helpers ---

    @property
    def state(self) -> _StateT:
        return self._state_cache[-1]

    @property
    def liquidity(self) -> int:
        return self.state.liquidity

    @property
    def sqrt_price_x96(self) -> int:
        return self.state.sqrt_price_x96

    @property
    def tick(self) -> int:
        return self.state.tick

    @property
    def tick_bitmap(self) -> dict[int, object]:
        return dict(self.state.tick_bitmap)

    @property
    def tick_data(self) -> dict[int, object]:
        return dict(self.state.tick_data)

    @property
    def update_block(self) -> int | None:
        return self.state.block

    @property
    def state_cache(self) -> deque[_StateT]:
        return self._state_cache

    @state_cache.setter
    def state_cache(self, value: deque[_StateT]) -> None:
        self._state_cache = value

    # --- write helpers ---

    def push_state(self, new_state: _StateT) -> None:
        """Append *new_state*, replacing the entry at the same block if present."""
        if self._state_cache[-1].block == new_state.block:
            self._state_cache.pop()
        self._state_cache.append(new_state)

    def discard_states_before_block(self, block: BlockNumber) -> None:
        """Drop states strictly older than *block*."""
        if (earliest := self._state_cache[0].block) and earliest >= block:
            return

        if (newest := self._state_cache[-1].block) and newest < block:
            raise NoPoolStateAvailable(block=block)

        while (self._state_cache[0].block or 0) < block:
            self._state_cache.popleft()

    def restore_state_before_block(self, block: BlockNumber) -> _StateT:
        """Rewind to the most recent state prior to *block*.

        Returns the restored state so the caller can emit an event or
        subscribe notification.
        """
        if (newest := self._state_cache[-1].block) and newest < block:
            return self._state_cache[-1]

        if (earliest := self._state_cache[0].block) and earliest >= block:
            raise NoPoolStateAvailable(block=block)

        while (self._state_cache[-1].block or 0) >= block:
            self._state_cache.pop()

        return self._state_cache[-1]

    # --- swap viability (pure query, no lock needed) ---

    @staticmethod
    def swap_is_viable(
        *,
        state: _StateLike,
        zero_for_one: bool,
        sparse_liquidity_map: bool,
    ) -> bool:
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
