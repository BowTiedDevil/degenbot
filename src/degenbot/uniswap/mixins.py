from typing import Protocol, Set


class Subscriber(Protocol):
    """
    Can be notified
    """

    def notify(self, subscriber) -> None:
        ...


class Publisher(Protocol):
    """
    Can accept subscriptions
    """

    _subscribers: Set[Subscriber]


class SubscriptionMixin:
    def subscribe(self: Publisher, subscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self: Publisher, subscriber) -> None:
        self._subscribers.discard(subscriber)

    def _notify_subscribers(self: Publisher):
        for subscriber in self._subscribers:
            subscriber.notify(self)
