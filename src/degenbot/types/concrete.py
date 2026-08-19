"""Concrete type definitions (state caches)."""

from collections import OrderedDict, defaultdict
from collections.abc import Callable
from typing import Any, Self


class KeyedDefaultDict[KT, VT](defaultdict[KT, VT]):
    """A modified defaultdict that passes the key to default_factory at runtime and records it.

    This differs from the defaultdict behavior, which calls default_factory with no arguments.
    """

    def __init__(self, default_factory: Callable[[KT], VT]) -> None:
        """Initialize the instance."""
        self._default_factory = default_factory

    def __missing__(self, key: KT) -> VT:
        """Implement __missing__.

        Returns:
            The computed value.

        """
        value = self._default_factory(key)
        self[key] = value
        return value


class BoundedCache[KT, VT](OrderedDict[KT, VT]):
    """A cache holding key-value pairs, tracked by entry order. The cache automatically removes old.

    items if the number of items would exceed the maximum number of entries set by `max_items`.

    Setting a value at an existing key will overwrite that value without affecting ordering.
    """

    def __init__(self, max_items: int) -> None:
        """Initialize the instance."""
        super().__init__()
        self.max_items = max_items

    def __reduce__(self) -> tuple[Any, ...]:
        """Return pickling information.

        Returns:
            The computed value.

        """
        state = super().__reduce__()
        return (
            state[0],
            (self.max_items,),  # max_items argument must be provided to properly unpickle
            None,
            None,
            state[4],
        )

    def __setitem__(self, key: KT, value: VT) -> None:
        """Set an item by index/key."""
        super().__setitem__(key, value)
        if len(self) > self.max_items:
            self.popitem(last=False)

    def copy(self) -> Self:
        """Copy.

        Returns:
            The computed value.

        """
        new_copy = self.__class__(max_items=self.max_items)
        for k, v in self.items():
            new_copy[k] = v
        return new_copy
