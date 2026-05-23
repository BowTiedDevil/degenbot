"""Concrete type definitions (state caches, publish/subscribe)."""
from collections import OrderedDict, defaultdict
from collections.abc import Callable
from typing import Any, Protocol, Self
from weakref import WeakSet

from degenbot.types.abstract import AbstractPoolState


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


class AbstractPublisherMessage:
    """A message sent by a `Publisher` to a `Subscriber`."""


class PoolStateMessage(AbstractPublisherMessage):
    """A message notifying that the publisher (a liquidity pool) has updated its state."""

    state: AbstractPoolState


class TextMessage(AbstractPublisherMessage):
    """A generic text message."""

    def __init__(self, text: str) -> None:
        """Initialize the instance."""
        self.text = text

    def __eq__(self, other: object) -> bool:
        """Check equality with another object.

        Returns:
            The computed value.

        """
        if not isinstance(other, TextMessage):
            return NotImplemented
        return self.text == other.text

    def __hash__(self) -> int:
        """Return the hash value.

        Returns:
            The computed value.

        """
        return hash(self.text)

    def __repr__(self) -> str:
        """Return a string representation.

        Returns:
            The computed value.

        """
        return f"{self.__class__.__name__}(text={self.text})"

    def __str__(self) -> str:
        """Return a string representation.

        Returns:
            The computed value.

        """
        return self.text


class Publisher(Protocol):
    """Can send a `Message` to a `Subscriber`."""

    _subscribers: WeakSet["Subscriber"]

    def subscribe(self, subscriber: "Subscriber") -> None:
        """Subscribe to receive messages from this `Publisher`."""

    def unsubscribe(self, subscriber: "Subscriber") -> None:
        """Stop receiving messages from this `Publisher`."""

    def _notify_subscribers(self, message: AbstractPublisherMessage) -> None:
        """Send a message to all subscribers."""


class PublisherMixin:
    """A set of default methods to accept subscribe & unsubscribe requests, and notify all.

    subscribers of a message. Classes using this mixin meet the `Publisher` protocol requirements.
    """

    def subscribe(self: Publisher, subscriber: "Subscriber") -> None:
        """Perform subscribe."""
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber: "Subscriber") -> None:
        """Perform unsubscribe."""
        self._subscribers.discard(subscriber)

    def _notify_subscribers(self: Publisher, message: AbstractPublisherMessage) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)


class Subscriber(Protocol):
    """Can subscribe to messages from a `Publisher`."""

    def notify(self, publisher: "Publisher", message: AbstractPublisherMessage) -> None:
        """Deliver `message` to `Subscriber`."""


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
