# Plan 050: Generic StateCache for Pool State Temporal Navigation

**Status: COMPLETE**

## Implementation Notes

- Used PEP 695 syntax: `class StateCache[T: CacheableState]:`
- **Caller holds the lock.** All mutation methods (`append`, `discard_before_block`, `restore_before_block`) are unlocked. The pool acquires `cache.lock()` for compound operations.
- `ConcentratedLiquidityStateManager` composes with `StateCache` internally (delegates deque + temporal navigation, keeps CL-specific convenience properties).
- V3/V4 `external_update` acquires `self._state_cache.lock()` externally, then calls unlocked `StateCache` methods internally — no deadlock.
- `PoolPickleMixin._pickle_lock()` dispatches to `StateCache.lock()` via `isinstance` check, with fallback to `_state_lock` for Curve/Balancer.
- Pickle: `StateCache.__getstate__` drops `_lock`; `__setstate__` reconstructs it.
- Curve/Balancer pools unchanged — different state model.

## Overview

Extract the duplicated state cache management pattern (deque + lock + temporal navigation)
from every pool family into a single generic `StateCache[T]` class. V2, V3, V4, and
Aerodrome pools currently re-implement identical `external_update`, `discard_states_before_block`,
and `restore_state_before_block` logic. A single `StateCache[PoolState]` replaces N copies.

## Files Involved

**Primary:**
- `src/degenbot/uniswap/v2_liquidity_pool.py` — replace deque/lock/notification with `StateCache`
- `src/degenbot/uniswap/v3_liquidity_pool.py` — same
- `src/degenbot/uniswap/v4_liquidity_pool.py` — same
- `src/degenbot/aerodrome/pools.py` — same

**Secondary:**
- `src/degenbot/types/pool_pickle.py` — simplify pickle drops/reconstructs to one `StateCache` reference
- `src/degenbot/types/concrete.py` — potentially integrate `StateCache` with `PublisherMixin` notifications
- `src/degenbot/camelot/pools.py` — inherits V2's cache; may need no changes
- `src/degenbot/curve/curve_stableswap_liquidity_pool.py` — Curve has a different state model; evaluate separately

**Tests:**
- `tests/types/test_state_cache.py` (new) — unit tests for `StateCache` in isolation
- `tests/uniswap/` — verify pool `external_update()` still works
- `tests/aerodrome/` — same

## Problem

Every pool class independently implements the same pattern:

```python
class UniswapV2Pool:
    _state_cache: deque[UniswapV2PoolState]
    _state_lock: Lock
    _subscribers: WeakSet[Subscriber]
    _pickle_drops: ClassVar[frozenset[str]] = frozenset({"_state_lock", "_subscribers"})
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {"_state_lock": Lock, "_subscribers": WeakSet}

    def external_update(self, update):
        if update.block_number < self.update_block:
            raise ExternalUpdateError(...)
        with self._state_lock:
            if (update.reserves_token0, update.reserves_token1) == (self.reserves_token0, self.reserves_token1):
                return
            working_state = dataclasses.replace(self.state, ...)
            if self.state.block == update.block_number:
                self._state_cache.pop()
            self._state_cache.append(working_state)
            self._notify_subscribers(UniswapV2PoolStateUpdated(self.state))

    def discard_states_before_block(self, block):
        with self._state_lock:
            # ~10 lines of index manipulation

    def restore_state_before_block(self, block):
        with self._state_lock:
            # ~10 lines of index manipulation
```

This is repeated nearly verbatim in:

| Pool | state_cache type | external_update | discard | restore |
|------|-----------------|-----------------|---------|---------|
| `UniswapV2Pool` | `deque[UniswapV2PoolState]` | ✅ | ✅ | ✅ |
| `UniswapV3Pool` | `deque[UniswapV3PoolState]` | ✅ | ✅ | ✅ |
| `UniswapV4Pool` | `deque[UniswapV4PoolState]` | ✅ | ✅ | ✅ |
| `AerodromeV2Pool` | `deque[AerodromeV2PoolState]` | ✅ | ✅ | ✅ |
| `CamelotLiquidityPool` | Inherited from V2 | — | — | — |

The variations are:
- **State type** (each pool has its own frozen dataclass)
- **Comparison logic** (which fields indicate "no change")
- **Construction of new state** from update (pool-specific `dataclasses.replace`)
- **Notification message type** (each pool has its own `*StateUpdated` class)

The deletion test: deleting `external_update` from `UniswapV2Pool` would force the same
logic into `AerodromeV2Pool`, `CamelotLiquidityPool`, and every V2 variant. The pool
isn't adding unique value to the cache management — it's scaffolding.

## Solution

### Step 1: Define `StateCache[T]` with a protocol-based constraint

```python
# src/degenbot/types/state_cache.py

from __future__ import annotations

import dataclasses
from collections import deque
from threading import Lock
from typing import TYPE_CHECKING, Generic, Protocol, TypeVar, runtime_checkable

if TYPE_CHECKING:
    from weakref import WeakSet

    from degenbot.types.abstract.pool_state import AbstractPoolState
    from degenbot.types.concrete import AbstractPublisherMessage, Publisher, Subscriber


@runtime_checkable
class CacheableState(Protocol):
    """A pool state that can be stored in a StateCache.

    Requires a `block` attribute for temporal navigation.
    """

    block: int | None


T = TypeVar("T", bound=CacheableState)


class StateCache(Generic[T]):
    """Generic temporal state cache for pool state snapshots.

    Owns the deque, the lock, and the temporal navigation methods.
    Pool classes compose with a `StateCache[TheirPoolState]` instead
    of re-implementing the pattern.

    Thread-safe: all mutations acquire `_lock`.

    Args:
        max_depth: Maximum number of historical states to retain.
    """

    def __init__(self, max_depth: int = 8) -> None:
        self._cache: deque[T] = deque(maxlen=max(1, max_depth))
        self._lock = Lock()

    @property
    def current(self) -> T:
        """The most recent state."""
        return self._cache[-1]

    @property
    def state_block(self) -> int:
        """The block number of the current state."""
        block = self._cache[-1].block
        if block is None:
            msg = "State block is None"
            raise ValueError(msg)
        return block

    @property
    def depth(self) -> int:
        """Number of cached states."""
        return len(self._cache)

    def append(
        self,
        state: T,
        *,
        block: int | None = None,
    ) -> bool:
        """Append a new state to the cache.

        If `block` is provided and matches the current state's block,
        replaces the current state (same-block update).

        Args:
            state: The new pool state.
            block: The block number of the update. If it matches the
                current state's block, the current state is popped first.

        Returns:
            True if the state was appended (new or same-block replacement).
            False if the state is older than the current state.
        """
        with self._lock:
            current_block = self._cache[-1].block if self._cache else None
            if current_block is not None and block is not None and block < current_block:
                return False  # Reject past update

            if block is not None and current_block is not None and block == current_block:
                self._cache.pop()

            self._cache.append(state)
            return True

    def discard_before_block(self, block: int) -> None:
        """Discard cached states earlier than the given block."""
        with self._lock:
            if (earliest_block := self._cache[0].block) and earliest_block >= block:
                return

            if (newest_block := self._cache[-1].block) and newest_block < block:
                msg = f"No state available at or before block {block}"
                raise ValueError(msg)

            while (earliest_block := self._cache[0].block) is None or earliest_block < block:
                self._cache.popleft()

    def restore_before_block(self, block: int) -> T:
        """Restore the last state recorded prior to a target block.

        Removes states at or after the target block, returning the
        state just before it.

        Returns:
            The state just before the target block.

        Raises:
            ValueError if no state exists before the target block.
        """
        with self._lock:
            newest_block = self._cache[-1].block
            if newest_block is not None and newest_block < block:
                return self._cache[-1]

            earliest_block = self._cache[0].block
            if earliest_block is not None and earliest_block >= block:
                msg = f"No state available before block {block}"
                raise ValueError(msg)

            while self._cache[-1].block is None or self._cache[-1].block >= block:
                self._cache.pop()

            return self._cache[-1]
```

### Step 2: Define `PoolStateCache` with notification integration

The `StateCache` handles the deque + lock + temporal navigation. But pools also need
to notify subscribers on state changes. Rather than putting notification logic in
`StateCache` (which shouldn't depend on the pub/sub layer), define a thin wrapper:

```python
class PoolStateCache(Generic[T]):
    """StateCache + publisher notification.

    Pool classes compose with a `PoolStateCache[TheirPoolState]`.
    On `append()`, it notifies the pool's subscribers with a
    pool-specific message.
    """

    def __init__(
        self,
        max_depth: int = 8,
    ) -> None:
        self._inner = StateCache[T](max_depth=max_depth)

    @property
    def current(self) -> T:
        return self._inner.current

    @property
    def state_block(self) -> int:
        return self._inner.state_block
```

Actually, this introduces complexity for marginal benefit. The simpler approach: pools
call `self._state_cache.append(state, block=block_number)` and then call
`self._notify_subscribers(...)` themselves. The cache is pure data management; notification
is the pool's responsibility. This matches the current pattern where `external_update`
does both steps sequentially.

### Step 3: Update pool classes to use `StateCache`

```python
# Before:
class UniswapV2Pool(PublisherMixin, PoolPickleMixin, V2PoolState, UniswapV2PoolCalc, AbstractLiquidityPool):
    _state_cache: deque[UniswapV2PoolState]
    _state_lock: Lock
    _subscribers: WeakSet[Subscriber]
    _pickle_drops: ClassVar[frozenset[str]] = frozenset({"_state_lock", "_subscribers"})
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {"_state_lock": Lock, "_subscribers": WeakSet}

    def __init__(self, ...):
        self._state_cache = deque(maxlen=state_cache_depth)
        self._state_cache.append(initial_state)
        self._state_lock = Lock()
        self._subscribers = WeakSet()

    def external_update(self, update):
        if update.block_number < self.update_block:
            raise ExternalUpdateError(...)
        with self._state_lock:
            if (update.reserves_token0, update.reserves_token1) == (self.reserves_token0, self.reserves_token1):
                return
            working_state = dataclasses.replace(self.state, reserves_token0=update.reserves_token0, ...)
            if self.state.block == update.block_number:
                self._state_cache.pop()
            self._state_cache.append(working_state)
            self._notify_subscribers(UniswapV2PoolStateUpdated(self.state))

# After:
class UniswapV2Pool(PublisherMixin, PoolPickleMixin, V2PoolState, UniswapV2PoolCalc, AbstractLiquidityPool):
    _pickle_drops: ClassVar[frozenset[str]] = frozenset({"_subscribers"})
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {"_subscribers": WeakSet}

    def __init__(self, ..., state_cache_depth: int = 8):
        self._state_cache = StateCache[UniswapV2PoolState](max_depth=state_cache_depth)
        self._state_cache.append(initial_state)

    @property
    def state(self) -> UniswapV2PoolState:
        return self._state_cache.current

    def external_update(self, update):
        if update.block_number < self._state_cache.state_block:
            raise ExternalUpdateError(...)
        if (update.reserves_token0, update.reserves_token1) == (self.reserves_token0, self.reserves_token1):
            return
        working_state = dataclasses.replace(self.state, reserves_token0=update.reserves_token0, ...)
        self._state_cache.append(working_state, block=update.block_number)
        self._notify_subscribers(UniswapV2PoolStateUpdated(self.state))
```

Key observations:
- `_state_lock` moves into `StateCache` — the pool no longer owns a lock directly
- `discard_states_before_block` and `restore_state_before_block` move to `StateCache`
- The `_pickle_drops` / `_pickle_reconstructs` simplify — `_state_lock` is gone
- `PublisherMixin._subscribers` remains on the pool (not inside `StateCache`)

### Step 4: Handle `CacheableState` protocol conformance

Each pool state type already has a `block` attribute (set to `int | None` — `None` for
pending states). The `CacheableState` protocol requires `block: int | None`, which all
existing state types satisfy. No changes needed to state classes.

However, `CacheableState` is a `@runtime_checkable` protocol. Using `isinstance(state, CacheableState)`
with a `@property` would raise `TypeError` (same limitation as other protocols with
properties in this codebase). Since the constraint is only at the type level (WithTypeVar
bound), this is fine — `mypy` ensures conformance, and runtime checks use `hasattr()`
per the project's existing convention.

### Step 5: Simplify `PoolPickleMixin`

Currently, every pool class declares:

```python
_pickle_drops: ClassVar[frozenset[str]] = frozenset({"_state_lock", "_subscribers"})
_pickle_reconstructs: ClassVar[dict[str, Any]] = {"_state_lock": Lock, "_subscribers": WeakSet}
```

With `StateCache`, `_state_lock` disappears from the pool. The pickle drops/reconstructs
become:

```python
_pickle_drops: ClassVar[frozenset[str]] = frozenset({"_subscribers"})
_pickle_reconstructs: ClassVar[dict[str, Any]] = {"_subscribers": WeakSet}
```

`StateCache` itself is picklable (its `deque` and `Lock` serialize naturally). If
needed, `StateCache` can define its own `__getstate__`/`__setstate__` for the lock.

### Step 6: Evaluate Curve pools separately

Curve pools have a qualitatively different state model: the "state" is spread across
many attributes (`_cached_contract_D`, `_cached_gamma`, `_block_timestamps`, etc.)
rather than captured in a single frozen dataclass. The state cache pattern doesn't apply
to Curve in the same way. Leave Curve unchanged for this plan.

## Implementation Order

### Phase 1: `StateCache` class (additive, no behavior change)

1. Create `src/degenbot/types/state_cache.py` with `CacheableState` protocol and
   `StateCache[T]` class
2. Implement `append()`, `discard_before_block()`, `restore_before_block()`, `current`,
   `state_block`, `depth`
3. Write `tests/types/test_state_cache.py` — unit tests for cache in isolation:
   - Append increases depth
   - Same-block update replaces current
   - Past update rejected
   - Discard removes old states
   - Restore pops to previous state
   - Thread-safety (concurrent append + read)
4. All new tests pass, zero regression to existing tests

### Phase 2: Migrate V2 family (one pool at a time)

5. Update `UniswapV2Pool` to use `StateCache[UniswapV2PoolState]`
6. Remove `_state_lock` from pool, delegate lock to `StateCache`
7. Simplify `_pickle_drops` / `_pickle_reconstructs`
8. Run V2 tests — verify `external_update`, temporal navigation, pickle
9. `AerodromeV2Pool` — same migration
10. `CamelotLiquidityPool` — inherits V2; may need no changes if V2's `StateCache` is
    on the shared mixin

### Phase 3: Migrate V3 family

11. Update `UniswapV3Pool` to use `StateCache[UniswapV3PoolState]`
12. V3 has additional cache-invalidation logic for tick bitmap/data — verify this
    interacts correctly with the temporal navigation
13. Run V3 tests

### Phase 4: Migrate V4 family

14. Update `UniswapV4Pool` to use `StateCache[UniswapV4PoolState]`
15. Run V4 tests

### Phase 5: Clean up

16. Verify `PoolPickleMixin` works with `StateCache` across all pool families
17. Run `just test-all` — zero regression
18. Run `ruff`, `mypy`
19. Update `src/degenbot/types/CONTEXT.md` with `StateCache` term
20. Remove `_state_lock` from all pool pickle configs

## Benefits

- **4 near-identical implementations → 1.** `external_update` logic, temporal navigation,
  and lock management concentrate in `StateCache`.
- **Future pool types get caching for free.** Adding Balancer, etc. — construct
  `StateCache[BalancerPoolState]` and you're done.
- **Bug fixes affect all pools at once.** A fix to same-block update logic is one edit in
  `StateCache.append()`, not 4 edits across pool classes.
- **Testable in isolation.** `StateCache` is a data structure with no pool-specific logic.
  Tests verify temporal navigation independently; pool tests verify only the pool-specific
  state comparison and notification.
- **Simpler pickle.** `_state_lock` disappears from pickle drops/reconstructs. `StateCache`
  handles its own serialization.
- **Lock management centralized.** The threading lock is inside `StateCache`, not scattered
  across pool classes. Future migration to `asyncio.Lock` is one change.

## Risks

- **Lock semantics.** The current pools hold the lock across compare + append + notify.
  With `StateCache`, the lock is held only during append. Notification happens after
  release. This is slightly different — subscribers see the new state after the lock is
  released, not during. This is actually better (no risk of deadlock from subscriber
  callbacks), but it's a behavior change that must be verified.
- **V3 tick cache interaction.** `UniswapV3Pool` uses the state cache alongside a tick
  bitmap cache and tick data cache. The temporal navigation (`discard_before_block`,
  `restore_state_before_block`) also affects these caches. Verify that `StateCache`
  change notifications trigger the right tick cache updates.
- **Generic type complexity.** `StateCache[T]` adds a type parameter. `mypy` may need
  help inferring `T` in some contexts. The `CacheableState` protocol bound helps.
- **Curve exclusion.** Curve's state model is too different for `StateCache`. This means
  the pool landscape has two state management patterns. This is acceptable — the domains
  are genuinely different (StableSwap vs constant-product/concentrated-liquidity).

## Relationship to Other Plans

- **Plan 028** (Builder Registry & Pool Class Restructuring): Extracted state and calc
  mixins. This plan deepens the state mixin by factoring out the cache management that
  every state mixin duplicates.
- **Plan 041** (Elevate Curve State Mixin): Elevated Curve's 25 attributes into a
  `StableswapPoolState` mixin. This plan addresses the V2/V3/V4 side of the same concern.
- **Plan 047** (Event-Driven Log Listener): The current `external_update` notifies
  subscribers. With `StateCache`, notification happens after the cache append. The
  subscription dispatch is unaffected — it receives the same message, just after the
  lock release instead of during.
- **ADR-001** (I/O-Free Pools): `StateCache` is pure data management — no I/O. It
  strengthens the I/O-free guarantee by making the cache layer independently testable
  without a provider.
