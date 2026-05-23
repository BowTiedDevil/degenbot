"""Tests verifying PublisherMixin provides complete pub/sub behavior:
subscribe, unsubscribe, and _notify_subscribers.
"""

from weakref import WeakSet

from degenbot.types.concrete import (
    AbstractPublisherMessage,
    PublisherMixin,
)


class _FakeMessage(AbstractPublisherMessage):
    pass


class _FakePublisher(PublisherMixin):
    def __init__(self) -> None:
        self._subscribers: WeakSet[_FakeSubscriber] = WeakSet()


class _FakeSubscriber:
    def __init__(self) -> None:
        self.received: list[tuple[_FakePublisher, _FakeMessage]] = []

    def notify(self, publisher: _FakePublisher, message: _FakeMessage) -> None:
        self.received.append((publisher, message))


class TestPublisherMixinNotification:
    def test_notify_subscribers_delivers_message_to_all_subscribers(self):
        """PublisherMixin._notify_subscribers sends the message to every subscriber."""
        pub = _FakePublisher()
        sub1 = _FakeSubscriber()
        sub2 = _FakeSubscriber()
        pub.subscribe(sub1)
        pub.subscribe(sub2)

        msg = _FakeMessage()
        pub._notify_subscribers(msg)

        assert len(sub1.received) == 1
        assert sub1.received[0] == (pub, msg)
        assert len(sub2.received) == 1
        assert sub2.received[0] == (pub, msg)

    def test_notify_subscribers_delivers_nothing_when_no_subscribers(self):
        """PublisherMixin._notify_subscribers is a no-op when there are no subscribers."""
        pub = _FakePublisher()
        msg = _FakeMessage()
        # Should not raise
        pub._notify_subscribers(msg)

    def test_unsubscribed_subscriber_does_not_receive_messages(self):
        """After unsubscribe, a former subscriber no longer receives messages."""
        pub = _FakePublisher()
        sub = _FakeSubscriber()
        pub.subscribe(sub)
        pub.unsubscribe(sub)

        msg = _FakeMessage()
        pub._notify_subscribers(msg)

        assert len(sub.received) == 0
