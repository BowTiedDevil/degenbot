"""Pickle helpers for pool serialization and deserialization."""

from collections.abc import Generator
from contextlib import AbstractContextManager, contextmanager, nullcontext
from typing import Any, ClassVar

from degenbot.types.state_cache import StateCache


class PoolPickleMixin:
    """Mixin providing pickle serialization for pool objects.

    Subclasses define:
        _pickle_drops: frozenset of attribute names to remove before pickling
        _pickle_reconstructs: dict of attribute name → callable factory for values to add
            when unpickling. Each factory is called to produce a fresh value.
    """

    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_subscribers",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {}

    def _pickle_lock(self) -> AbstractContextManager[None]:
        """Return a context manager that guards pickle serialization.

        Pools using StateCache delegate the lock there.
        Pools with their own _state_lock can override this method.

        Returns:
            The computed value.

        """
        state_cache = getattr(self, "_state_cache", None)
        if isinstance(state_cache, StateCache):
            return state_cache.lock()
        # Fallback for pools that still have _state_lock (e.g. Curve, Balancer)
        state_lock = getattr(self, "_state_lock", None)
        if state_lock is not None:

            @contextmanager
            def _lock() -> Generator[None, None, None]:
                with state_lock:
                    yield

            return _lock()
        # No lock at all — just a null context
        return nullcontext()

    def __getstate__(self) -> dict[str, Any]:
        """Return the pickled state.

        Returns:
            The computed value.

        """
        with self._pickle_lock():
            return {k: v for k, v in self.__dict__.items() if k not in self._pickle_drops}

    def __setstate__(self, state: dict[str, Any]) -> None:
        """Restore from pickled state."""
        for key, factory in self._pickle_reconstructs.items():
            state[key] = factory()
        self.__dict__ = state
