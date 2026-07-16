"""Stub for the dynamically-created ``degenbot._ffi.subscriber`` submodule.

Created at runtime by ``add_subscriber_module`` in the PyO3 wrapper crate
(``degenbot-python/src/bot/subscriber.rs``). Holds the pub/sub subscriber
registration function + the ``PySubscription`` handle.
"""

from degenbot._ffi import PyBot

class PySubscription:
    """Handle to a registered pub/sub subscription.

    Hold the instance for as long as the subscriber should stay active.
    Dropping it (or calling ``.unsubscribe()``) unregisters: the strong
    ``Arc`` drops, ``LogDispatcher``'s ``Weak`` goes dead, and subsequent
    ``notify`` calls silently skip this subscriber.
    """

    def unsubscribe(self) -> None:
        """Release the strong anchor — idempotent."""

def register_subscriber(
    bot: PyBot,
    pool_id: int,
    callback: object,
) -> PySubscription:
    """Register a Python callback as a ``PoolStateSubscriber`` for ``pool_id``.

    The callback is invoked as ``callback(pool_id)`` each time the bot's
    ``LogDispatcher`` applies a decoded event to ``pool_id`` (or after a reorg
    restore). Notifications fire in registration order; a dropped (GC'd)
    callback is silently skipped.

    Raises:
        RuntimeError: If the callback isn't callable.

    """

__all__ = ["PySubscription", "register_subscriber"]
