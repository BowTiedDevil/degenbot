"""Type stubs for the degenbot Rust backrun seam.

Python module: `degenbot._ffi.backrun`
Rust: `crates/degenbot-python/src/rpc/backrun_py.rs`

The live `MEVBlocker` searcher feed handle (RSUB-2 / NYVL2F): a PyO3
wrapper owning the `degenbot-rpc` feed pump. `drain()` returns plain
dicts with lossless fields (hex-serialized call data + canonical
address/value strings), `status()` the counters snapshot, `stop()`
the cooperative pump shutdown.
"""

from typing import Any

class BackrunFeed:
    """Live `MEVBlocker` searcher feed handle (RSUB-2).

    Spawn a feed pump against the mainnet relay (or an explicit `url`).
    `watchdog_secs = 0` selects the production default watchdog; a
    non-zero value overrides it. `ring_capacity = 0` selects the default
    bounded event ring; events are dropped from the ring (counted in the
    status counters) when the consumer drains slower than the feed.
    """

    def __init__(
        self, url: str | None = None, watchdog_secs: int = 0, ring_capacity: int = 0
    ) -> None: ...
    def drain(self) -> list[dict[str, Any]]:
        """Drain all buffered events as plain dicts (lossless fields)."""
    def status(self) -> dict[str, Any]:
        """Counters snapshot as a plain dict."""
    def stop(self) -> None:
        """Stop the feed pump (idempotent cooperative shutdown)."""

__all__ = ["BackrunFeed"]
