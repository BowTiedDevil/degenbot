from degenbot._ffi import Bot

class PoolStateSubscription:
    """Handle to a registered pub/sub subscription.

    Hold the instance for as long as the subscriber should stay active.
    Dropping it (or calling ``.unsubscribe()``) unregisters: the strong
    ``Arc`` drops, ``LogDispatcher``'s ``Weak`` goes dead, and subsequent
    ``notify`` calls silently skip this subscriber.
    """

    def unsubscribe(self) -> None:
        """Release the strong anchor — idempotent."""

def register_subscriber(
    bot: Bot,
    pool_id: int,
    callback: object,
) -> PoolStateSubscription:
    """Register a Python callback as a ``PoolStateSubscriber`` for ``pool_id``.

    The callback is invoked as ``callback(pool_id)`` each time the bot's
    ``LogDispatcher`` applies a decoded event to ``pool_id`` (or after a reorg
    restore). Notifications fire in registration order; a dropped (GC'd)
    callback is silently skipped.

    Raises:
        RuntimeError: If the callback isn't callable.

    """

__all__ = ["PoolStateSubscription", "register_subscriber"]
