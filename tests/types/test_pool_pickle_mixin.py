"""
Tests verifying PoolPickleMixin provides pickle serialization for pool objects.
"""

import pickle
from threading import Lock
from typing import Any, ClassVar
from weakref import WeakSet

from degenbot.types.address_comparable import AddressComparable
from degenbot.types.concrete import PublisherMixin
from degenbot.types.pool_pickle import PoolPickleMixin
from degenbot.types.state_cache import StateCache


class _FakePoolWithLock(PublisherMixin, PoolPickleMixin, AddressComparable):
    """A pool that uses its own _state_lock (e.g. Curve, Balancer)."""

    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_state_lock",
        "_subscribers",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {
        "_state_lock": Lock,
        "_subscribers": WeakSet,
    }

    def __init__(self, address: str, name: str) -> None:
        self.address = address
        self.name = name
        self._state_lock = Lock()
        self._subscribers: WeakSet = WeakSet()


class _FakePoolWithStateCache(PublisherMixin, PoolPickleMixin, AddressComparable):
    """A pool that delegates lock to StateCache (e.g. V2, V3, V4, Aerodrome)."""

    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_subscribers",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {
        "_subscribers": WeakSet,
    }

    def __init__(self, address: str, name: str) -> None:
        self.address = address
        self.name = name
        self._state_cache = StateCache(max_depth=8)
        self._subscribers: WeakSet = WeakSet()


class TestPoolPickleMixinWithLock:
    def test_getstate_drops_configured_attributes(self):
        """__getstate__ removes all attributes listed in _pickle_drops."""
        pool = _FakePoolWithLock("0x01", "TestPool")
        state = pool.__getstate__()
        assert "_state_lock" not in state
        assert "_subscribers" not in state
        assert state["address"] == "0x01"
        assert state["name"] == "TestPool"

    def test_setstate_reconstructs_configured_attributes(self):
        """__setstate__ reconstructs attributes from _pickle_reconstructs factories."""
        pool = _FakePoolWithLock("0x01", "TestPool")
        state = pool.__getstate__()
        pool2 = _FakePoolWithLock.__new__(_FakePoolWithLock)
        pool2.__setstate__(state)
        assert hasattr(pool2, "_state_lock")
        assert pool2._state_lock.locked() is False

    def test_pickle_round_trip(self):
        """A pickled and unpickled pool preserves its data and reconstructs transient attrs."""
        pool = _FakePoolWithLock("0x01", "TestPool")
        data = pickle.dumps(pool)
        restored = pickle.loads(data)
        assert restored.address == "0x01"
        assert restored.name == "TestPool"
        assert hasattr(restored, "_state_lock")
        assert restored._state_lock.locked() is False

    def test_reconstructed_lock_is_fresh_instance(self):
        """Each unpickled object gets its own Lock, not a shared one."""
        pool = _FakePoolWithLock("0x01", "TestPool")
        data = pickle.dumps(pool)
        r1 = pickle.loads(data)
        r2 = pickle.loads(data)
        assert r1._state_lock is not r2._state_lock


class TestPoolPickleMixinWithStateCache:
    def test_getstate_drops_subscribers(self):
        """__getstate__ removes _subscribers."""
        pool = _FakePoolWithStateCache("0x01", "TestPool")
        state = pool.__getstate__()
        assert "_subscribers" not in state
        assert "_state_cache" in state
        assert state["address"] == "0x01"

    def test_pickle_round_trip(self):
        """StateCache pools pickle and unpickle correctly."""
        pool = _FakePoolWithStateCache("0x01", "TestPool")
        data = pickle.dumps(pool)
        restored = pickle.loads(data)
        assert restored.address == "0x01"
        assert restored.name == "TestPool"
        assert hasattr(restored, "_state_cache")
        assert hasattr(restored, "_subscribers")

    def test_state_cache_lock_functional_after_unpickle(self):
        """StateCache lock is functional after unpickling."""
        pool = _FakePoolWithStateCache("0x01", "TestPool")
        data = pickle.dumps(pool)
        restored = pickle.loads(data)
        # Lock should work (no exception)
        with restored._state_cache.lock():
            pass
