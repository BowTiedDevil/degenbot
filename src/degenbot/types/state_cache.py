"""Generic temporal state cache for pool state snapshots.

Owns the deque and the lock. Pool classes compose with
``StateCache[TheirPoolState]`` instead of re-implementing the
deque + lock + temporal navigation pattern.

**The caller is responsible for all locking.** The lock is exposed via
:meth:`lock` so pool classes can hold it across compound operations
(e.g. read state → construct simulation → return).

The caller (pool) is also responsible for:

- Comparing old vs new state (pool-specific fields).
- Constructing the new state object (``dataclasses.replace``).
- Calling ``_notify_subscribers()`` after a successful append.

This separation keeps ``StateCache`` free of pool-specific logic
and pub/sub dependencies.
"""

from __future__ import annotations

from collections import deque
from contextlib import contextmanager
from threading import Lock
from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from collections.abc import Generator, Iterator

    from degenbot.types.aliases import BlockNumber


@runtime_checkable
class CacheableState(Protocol):
    """A pool state that can be stored in a :class:`StateCache`.

    Requires a ``block`` attribute for temporal navigation.
    """

    @property
    def block(self) -> int | None:
        """Return block."""
        ...


class StateCache[T: CacheableState]:
    """Return block."""

    """
    Generic temporal state cache for pool state snapshots.

    Owns the deque, the lock, and the temporal navigation methods.
    Pool classes compose with a ``StateCache[TheirPoolState]`` instead
    of re-implementing the pattern.

    **The caller is responsible for all locking.** Use :meth:`lock`
    to acquire the internal lock for a compound operation. All
    mutation methods (``append``, ``discard_before_block``,
    ``restore_before_block``) are unlocked — call them inside a
    ``with cache.lock():`` block.

    Args:
        max_depth: Maximum number of historical states to retain.

    """

    def __init__(self, max_depth: int = 8) -> None:
        """Initialize the instance."""
        self._cache: deque[T] = deque(maxlen=max(1, max_depth))
        self._lock = Lock()

    @contextmanager
    def lock(self) -> Generator[None, None, None]:
        """Acquire the internal lock for a compound operation.

        Use this when the pool needs to hold the lock across multiple
        reads or read-then-write sequences (e.g. simulation methods
        that read current state and produce a result atomically).
        """
        with self._lock:
            yield

    def __getstate__(self) -> dict[str, object]:
        """Return the pickled state.

        Returns:
            The computed value.

        """
        # Lock is not picklable; drop it and reconstruct on load
        state = self.__dict__.copy()
        state.pop("_lock", None)
        return state

    def __setstate__(self, state: dict[str, object]) -> None:
        """Restore from pickled state."""
        self.__dict__.update(state)
        self._lock = Lock()

    def __len__(self) -> int:
        """Return the length.

        Returns:
            The computed value.

        """
        return len(self._cache)

    def __iter__(self) -> Iterator[T]:
        """Iterate over cached states from oldest to newest.

        Returns:
            The computed value.

        """
        return iter(self._cache)

    @property
    def current(self) -> T:
        """The most recent state."""
        return self._cache[-1]

    @property
    def state_block(self) -> int:
        """The block number of the current state.

        Raises:
            ValueError: If the current state's block is ``None``.

        """
        block = self._cache[-1].block
        if block is None:
            msg = "State block is None"
            raise ValueError(msg)
        return block

    def append(
        self,
        state: T,
        *,
        block: int | None = None,
    ) -> bool:
        """Append a new state to the cache.

        If ``block`` is provided and matches the current state's block,
        replaces the current state (same-block update).

        **Unlocked** — call inside ``with cache.lock():``.

        Args:
            state: The new pool state.
            block: The block number of the update. If it matches the
                current state's block, the current state is popped first.

        Returns:
            ``True`` if the state was appended (new or same-block replacement).
            ``False`` if the state is older than the current state.

        """
        current_block = self._cache[-1].block if self._cache else None
        if current_block is not None and block is not None and block < current_block:
            return False

        if block is not None and current_block is not None and block == current_block:
            self._cache.pop()

        self._cache.append(state)
        return True

    def discard_before_block(self, block: BlockNumber) -> None:
        """Discard cached states earlier than the given block.

        **Unlocked** — call inside ``with cache.lock():``.

        Raises:
            ValueError: If all cached states are before the target block.

        """
        if (earliest_block := self._cache[0].block) and earliest_block >= block:
            return

        if (newest_block := self._cache[-1].block) and newest_block < block:
            msg = f"No pool state known prior to block {block}"
            raise ValueError(msg)

        while (earliest_block := self._cache[0].block) is None or earliest_block < block:
            self._cache.popleft()

    def restore_before_block(self, block: BlockNumber) -> T:
        """Restore the last state recorded prior to a target block.

        Removes states at or after the target block, returning the
        state just before it.

        **Unlocked** — call inside ``with cache.lock():``.

        Returns:
            The state just before the target block.

        Raises:
            ValueError: If no state exists before the target block.

        """
        newest_block = self._cache[-1].block
        if newest_block is not None and newest_block < block:
            return self._cache[-1]

        earliest_block = self._cache[0].block
        if earliest_block is not None and earliest_block >= block:
            msg = f"No pool state known prior to block {block}"
            raise ValueError(msg)

        while self._cache[-1].block is None or self._cache[-1].block >= block:
            self._cache.pop()

        return self._cache[-1]
