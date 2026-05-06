"""Consolidated fake subscriber implementation.

Replaces the previous ad hoc subscriber fakes:
- FakeSubscriber (from tests/conftest.py)
- FakeSubscriber (from tests/arbitrage/test_path/conftest.py)

The canonical version combines the ``inbox`` + ``subscribe``/``unsubscribe``
API from the root conftest with the ``notifications`` list from the
test_path variant.
"""

from typing import Any

from degenbot.types.concrete import AbstractPublisherMessage, Publisher


class FakeSubscriber:
    """Subscriber that records received messages for test assertions.

    Supports both ``inbox`` (dict records) and ``notifications`` (tuple records)
    so it can serve as a drop-in replacement for either prior variant.
    """

    def __init__(self) -> None:
        self.inbox: list[dict[str, Any]] = []
        self.notifications: list[tuple[object, object]] = []

    def notify(self, publisher: Publisher, message: AbstractPublisherMessage) -> None:
        self.inbox.append({
            "from": publisher,
            "message": message,
        })
        self.notifications.append((publisher, message))

    def subscribe(self, publisher: Publisher) -> None:
        publisher.subscribe(self)

    def unsubscribe(self, publisher: Publisher) -> None:
        publisher.unsubscribe(self)
