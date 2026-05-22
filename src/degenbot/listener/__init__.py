"""
Event listener for eth_subscribe log dispatch.

Provides the `LogListener` dispatch registry — a pure Python mapping of
`(address, topic0)` → handler set that receives raw log dicts and calls
registered handlers sequentially.

Usage::

    listener = LogListener()
    listener.register("0xPool", "0xSyncTopic", my_handler)
    ...
    for log in subscription.drain():
        listener.dispatch(log)
"""

from degenbot.listener.log_listener import LogListener

__all__ = ["LogListener"]
