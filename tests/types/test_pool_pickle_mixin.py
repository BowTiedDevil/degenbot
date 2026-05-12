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


class _FakePool(PublisherMixin, PoolPickleMixin, AddressComparable):
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


class TestPoolPickleMixin:
    def test_getstate_drops_configured_attributes(self):
        """__getstate__ removes all attributes listed in _pickle_drops."""
        pool = _FakePool("0x01", "TestPool")
        state = pool.__getstate__()
        assert "_state_lock" not in state
        assert "_subscribers" not in state
        assert state["address"] == "0x01"
        assert state["name"] == "TestPool"

    def test_setstate_reconstructs_configured_attributes(self):
        """__setstate__ reconstructs attributes from _pickle_reconstructs factories."""
        pool = _FakePool("0x01", "TestPool")
        state = pool.__getstate__()
        pool2 = _FakePool.__new__(_FakePool)
        pool2.__setstate__(state)
        assert hasattr(pool2, "_state_lock")
        # Lock() returns a _thread.lock, verify it's a fresh unlocked lock
        assert pool2._state_lock.locked() is False

    def test_pickle_round_trip(self):
        """A pickled and unpickled pool preserves its data and reconstructs transient attrs."""
        pool = _FakePool("0x01", "TestPool")
        data = pickle.dumps(pool)
        restored = pickle.loads(data)
        assert restored.address == "0x01"
        assert restored.name == "TestPool"
        assert hasattr(restored, "_state_lock")
        assert restored._state_lock.locked() is False

    def test_reconstructed_lock_is_fresh_instance(self):
        """Each unpickled object gets its own Lock, not a shared one."""
        pool = _FakePool("0x01", "TestPool")
        data = pickle.dumps(pool)
        r1 = pickle.loads(data)
        r2 = pickle.loads(data)
        assert r1._state_lock is not r2._state_lock
