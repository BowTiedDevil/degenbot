from typing import Iterator, Protocol, Set

from .baseclasses import BaseArbitrage


class Subscriber(Protocol):
    """
    Can be notified via the `notify()` method
    """

    def notify(self, subscriber: "Publisher") -> None:  # pragma: no cover
        ...


class Publisher(Protocol):
    """
    Can publish updates and accept subscriptions
    """

    _subscribers: Set[Subscriber]


class SubscriptionMixin:
    def get_arbitrage_helpers(self: Publisher) -> Iterator[BaseArbitrage]:
        return (
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, (BaseArbitrage))
        )

    def subscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber: Subscriber) -> None:
        self._subscribers.discard(subscriber)

    def _notify_subscribers(self: Publisher) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(self)
