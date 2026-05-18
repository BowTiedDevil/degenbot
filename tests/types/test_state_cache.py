"""Unit tests for StateCache — generic temporal state cache."""

from __future__ import annotations

import pickle
import threading
from dataclasses import dataclass

import pytest

from degenbot.types.state_cache import CacheableState, StateCache

# --- Test fixtures ---


@dataclass(frozen=True)
class FakeState:
    """Minimal state satisfying CacheableState protocol."""

    block: int | None
    value: int


# --- Construction ---


class TestStateCacheConstruction:
    def test_default_depth(self) -> None:
        cache: StateCache[FakeState] = StateCache()
        assert len(cache) == 0

    def test_custom_depth(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=4)
        assert len(cache) == 0

    def test_depth_clamped_to_one(self) -> None:
        """max_depth=0 should clamp to 1."""
        cache: StateCache[FakeState] = StateCache(max_depth=0)
        cache.append(FakeState(block=1, value=10), block=1)
        assert len(cache) == 1


# --- append() ---


class TestStateCacheAppend:
    def test_append_increases_depth(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        assert len(cache) == 1
        cache.append(FakeState(block=2, value=20), block=2)
        assert len(cache) == 2

    def test_same_block_replaces_current(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=5, value=100), block=5)
        assert len(cache) == 1
        cache.append(FakeState(block=5, value=200), block=5)
        assert len(cache) == 1
        assert cache.current.value == 200

    def test_past_update_rejected(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=10, value=100), block=10)
        result = cache.append(FakeState(block=5, value=50), block=5)
        assert result is False
        assert cache.current.value == 100

    def test_append_returns_true_on_success(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        assert cache.append(FakeState(block=1, value=10), block=1) is True

    def test_append_respects_max_depth(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=2)
        cache.append(FakeState(block=1, value=10), block=1)
        cache.append(FakeState(block=2, value=20), block=2)
        cache.append(FakeState(block=3, value=30), block=3)
        assert len(cache) == 2
        assert cache.current.value == 30

    def test_append_without_block_param(self) -> None:
        """append() without block= still works (no same-block replacement logic)."""
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        # No block param — just appends
        cache.append(FakeState(block=2, value=20))
        assert len(cache) == 2


# --- current property ---


class TestStateCacheCurrent:
    def test_current_returns_latest(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        cache.append(FakeState(block=2, value=20), block=2)
        assert cache.current == FakeState(block=2, value=20)

    def test_current_on_empty_cache_raises(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        with pytest.raises(IndexError):
            _ = cache.current


# --- state_block property ---


class TestStateCacheStateBlock:
    def test_state_block_returns_current_block(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=42, value=10), block=42)
        assert cache.state_block == 42

    def test_state_block_raises_on_none(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=None, value=10))
        with pytest.raises(ValueError, match="State block is None"):
            _ = cache.state_block


# --- discard_before_block() ---


class TestStateCacheDiscard:
    def test_discard_removes_old_states(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        cache.append(FakeState(block=2, value=20), block=2)
        cache.append(FakeState(block=3, value=30), block=3)
        cache.discard_before_block(2)
        assert len(cache) == 2
        assert cache.current.value == 30

    def test_discard_when_earliest_already_meets_target(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=5, value=10), block=5)
        cache.discard_before_block(3)
        assert len(cache) == 1

    def test_discard_when_all_before_target_raises(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        with pytest.raises(ValueError, match="No pool state known prior to block 10"):
            cache.discard_before_block(10)

    def test_discard_with_none_block_states(self) -> None:
        """States with block=None are treated as before any target."""
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=None, value=10))
        cache.append(FakeState(block=3, value=30), block=3)
        cache.discard_before_block(3)
        assert len(cache) == 1
        assert cache.current.value == 30


# --- restore_before_block() ---


class TestStateCacheRestore:
    def test_restore_pops_to_previous_state(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        cache.append(FakeState(block=2, value=20), block=2)
        cache.append(FakeState(block=3, value=30), block=3)
        restored = cache.restore_before_block(3)
        assert restored.value == 20
        assert cache.current.value == 20

    def test_restore_when_current_already_before_target(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        restored = cache.restore_before_block(5)
        assert restored.value == 10

    def test_restore_when_earliest_at_target_raises(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=5, value=10), block=5)
        with pytest.raises(ValueError, match="No pool state known prior to block 3"):
            cache.restore_before_block(3)

    def test_restore_removes_states_at_and_after_block(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        cache.append(FakeState(block=3, value=30), block=3)
        cache.append(FakeState(block=5, value=50), block=5)
        restored = cache.restore_before_block(3)
        assert restored.value == 10
        assert cache.current.value == 10


# --- Pickle support ---


class TestStateCachePickle:
    def test_pickle_round_trip(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        cache.append(FakeState(block=2, value=20), block=2)
        data = pickle.dumps(cache)
        cache2: StateCache[FakeState] = pickle.loads(data)
        assert len(cache2) == 2
        assert cache2.current.value == 20

    def test_pickle_reconstructs_lock(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        data = pickle.dumps(cache)
        cache2: StateCache[FakeState] = pickle.loads(data)
        # Lock should be functional (no exception on append)
        cache2.append(FakeState(block=2, value=20), block=2)
        assert cache2.current.value == 20


# --- Thread safety ---


class TestStateCacheThreadSafety:
    def test_concurrent_appends_under_lock(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=1000)
        errors: list[RuntimeError] = []
        n_threads = 10
        n_appends = 100

        def worker(thread_id: int) -> None:
            try:
                for i in range(n_appends):
                    block = thread_id * n_appends + i + 1
                    with cache.lock():
                        cache.append(FakeState(block=block, value=block), block=block)
            except RuntimeError as e:
                errors.append(e)

        threads = [threading.Thread(target=worker, args=(t,)) for t in range(n_threads)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert not errors
        # All appends should have succeeded
        assert len(cache) > 0
        assert cache.current.block is not None


# --- CacheableState protocol ---


class TestCacheableStateProtocol:
    def test_fake_state_satisfies_protocol(self) -> None:
        """Verify our test fixture conforms to the protocol."""
        state = FakeState(block=1, value=10)
        assert isinstance(state, CacheableState)


# --- lock() context manager ---


class TestStateCacheLock:
    def test_lock_context_manager(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        with cache.lock():
            # Can read current state under lock
            assert cache.current.value == 10
        # Lock is released after context

    def test_lock_prevents_concurrent_append(self) -> None:
        cache: StateCache[FakeState] = StateCache(max_depth=8)
        cache.append(FakeState(block=1, value=10), block=1)
        acquired: list[bool] = []

        def try_lock() -> None:
            # If the main thread holds the lock, this thread will block
            # until it's released. We use a short timeout.

            lock = cache._lock
            acquired.append(lock.acquire(timeout=0.1))
            if acquired[-1]:
                lock.release()

        with cache.lock():
            t = threading.Thread(target=try_lock)
            t.start()
            t.join(timeout=0.5)

        # The other thread should not have acquired the lock while we held it
        assert not acquired or acquired[0] is False
