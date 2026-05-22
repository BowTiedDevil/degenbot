"""LogListener — dispatch registry for eth_subscribe log events.

A pure Python lookup table mapping ``(address, topic0)`` → handler list.
Receives raw log dicts via ``dispatch(log)``, looks up handlers, and
calls them sequentially. Exact match only — no wildcards, no prefix
matching.

Handlers are sync ``Callable[[dict], None]``. Exceptions propagate
(fail loudly). Unmatched logs are silently discarded (~200ns per miss).

Example::

    from degenbot.listener import LogListener

    listener = LogListener()


    def handle_sync(log: dict) -> None:
        pool.external_update(...)


    listener.register("0xABC...", SYNC_TOPIC, handle_sync)

    # In the consume loop:
    for log in subscription.drain():
        listener.dispatch(log)
"""

from __future__ import annotations

from collections.abc import Callable  # noqa: TC003


def _normalize_hex(value: str | bytes) -> str:
    """Normalize a hex value to lowercase with 0x prefix."""
    if isinstance(value, bytes):
        return f"0x{value.hex()}"
    if value.startswith(("0x", "0X")):
        return value.lower()
    return f"0x{value.lower()}"


class LogListener:
    """Dispatch registry mapping ``(address, topic0)`` → handler list.

    Handlers are sync ``Callable[[dict], None]``. Registration and
    unregistration are O(1). Dispatch is O(handlers) for matched logs,
    O(1) for misses.

    Handlers are called in insertion order. A ``list`` preserves order
    while a companion ``set`` provides O(1) duplicate checking.

    Thread-safety: **not thread-safe**. Intended for single-threaded
    async event loop use (the consume loop is ``async`` but handlers
    are ``sync`` and called sequentially).
    """

    def __init__(self) -> None:
        """Initialize the instance."""
        # Ordered list for dispatch, set for O(1) duplicate checks
        self._handlers: dict[tuple[str, str], list[Callable[[dict], None]]] = {}
        self._handler_sets: dict[tuple[str, str], set[Callable[[dict], None]]] = {}

    def register(
        self,
        address: str,
        topic0: str,
        handler: Callable[[dict], None],
    ) -> None:
        """Register a handler for ``(address, topic0)``.

        Args:
            address: Contract address (checksummed or lowercase —
                normalized internally to lowercase for matching).
            topic0: First topic of the event (event signature hash —
                normalized internally to lowercase for matching).
            handler: Sync callable taking a raw log dict.

        Raises:
            TypeError: If the handler is not callable.

        """
        if not callable(handler):
            msg = f"Handler must be callable, got {type(handler).__name__}"
            raise TypeError(msg)
        key = (_normalize_hex(address), _normalize_hex(topic0))
        if key not in self._handlers:
            self._handlers[key] = []
            self._handler_sets[key] = set()
        if handler not in self._handler_sets[key]:
            self._handler_sets[key].add(handler)
            self._handlers[key].append(handler)

    def unregister(
        self,
        address: str,
        topic0: str,
        handler: Callable[[dict], None],
    ) -> None:
        """Remove a handler for ``(address, topic0)``.

        No-op if the handler was not registered.
        """
        key = (_normalize_hex(address), _normalize_hex(topic0))
        handlers = self._handlers.get(key)
        if handlers is None:
            return
        try:
            handlers.remove(handler)
            self._handler_sets[key].discard(handler)
        except ValueError:
            pass
        if not handlers:
            del self._handlers[key]
            del self._handler_sets[key]

    def dispatch(self, log: dict) -> None:
        """Dispatch a raw log dict to all matching handlers.

        Looks up ``(log["address"], log["topics"][0])`` in the registry.
        If found, calls each handler sequentially in insertion order.
        If no match, silently returns (miss ~200ns).

        Exceptions propagate — handler succeeds or the whole dispatch
        fails loudly.
        """
        address = log.get("address")
        topics = log.get("topics")

        if address is None or topics is None:
            return

        if not topics:
            return

        topic0 = topics[0]
        address = _normalize_hex(address)
        topic0 = _normalize_hex(topic0)

        key = (address, topic0)
        handlers = self._handlers.get(key)
        if handlers is None:
            return

        for handler in handlers:
            handler(log)

    def handler_count(self) -> int:
        """Return the total number of registered handlers."""
        return sum(len(hs) for hs in self._handlers.values())

    def key_count(self) -> int:
        """Return the number of registered ``(address, topic0)`` keys."""
        return len(self._handlers)

    def __repr__(self) -> str:
        """Return a string representation."""
        return f"LogListener(keys={self.key_count()}, handlers={self.handler_count()})"
