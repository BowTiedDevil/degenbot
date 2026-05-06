from typing import Any, ClassVar


class PoolPickleMixin:
    """
    Mixin providing pickle serialization for pool objects.

    Subclasses define:
        _pickle_drops: frozenset of attribute names to remove before pickling
        _pickle_reconstructs: dict of attribute name → callable factory for values to add
            when unpickling. Each factory is called to produce a fresh value.
    """

    _pickle_drops: ClassVar[frozenset[str]] = frozenset({
        "_state_lock",
        "_subscribers",
    })
    _pickle_reconstructs: ClassVar[dict[str, Any]] = {}

    def __getstate__(self) -> dict[str, Any]:
        with self._state_lock:
            return {
                k: v
                for k, v in self.__dict__.items()
                if k not in self._pickle_drops
            }

    def __setstate__(self, state: dict[str, Any]) -> None:
        for key, factory in self._pickle_reconstructs.items():
            state[key] = factory()
        self.__dict__ = state
