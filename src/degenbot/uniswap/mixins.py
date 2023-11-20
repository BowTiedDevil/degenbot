from typing import Protocol, Set, Iterator
from ..baseclasses import ArbitrageHelper


class Subscriber(Protocol):
    """
    Can be notified
    """

    def notify(self, subscriber) -> None:  # pragma: no cover
        ...


class Publisher(Protocol):
    """
    Can publish updates and accept subscriptions
    """

    _subscribers: Set[Subscriber]


class SubscriptionMixin:
    def get_arbitrage_helpers(self: Publisher) -> Iterator[ArbitrageHelper]:
        return (
            subscriber
            for subscriber in self._subscribers
            if isinstance(subscriber, (ArbitrageHelper))
        )

    def subscribe(self: Publisher, subscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber) -> None:
        self._subscribers.discard(subscriber)

    def _notify_subscribers(self: Publisher):
        for subscriber in self._subscribers:
            subscriber.notify(self)
