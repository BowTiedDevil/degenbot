"""Session-scoped skip memo + skip-log deduper for path registration (2CBDPR).

``build_paths`` re-attempts the same candidate pools on every block. Most V4
skip reasons are immutable pool facts — hook/dynamic-fee/encoder-fee admission
rejections, discovery pool-id mismatches, duplicate registrations. Retrying
can never change the verdict, and re-logging floods the GIL-bridged log path
(~20k skip lines/block at mainnet scale; the missed-WS-pong investigation
measured ~1M lines/16 min, driving RSS toward the cgroup ceiling).

``SkipGate`` does two things:
- **fatal memo**: once a (kind, key) skip is recorded as ``fatal=True``,
  ``fatal_tag`` reports the stored tag so the pipeline can short-circuit the
  build entirely on later blocks (no RPC, no log line);
- **log dedupe**: log lines for transient skips are emitted at most once per
  cooldown window per (kind, key).

The memo value holds the LAST fatal tag so the short-circuit keeps the exact
counter/tag semantics of the first rejection.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from dataclasses import dataclass


@dataclass
class _Entry:
    """Per-(kind, key) state."""

    tag: str
    fatal: bool
    last_logged_at: float


class SkipGate:
    """Memoize fatal skips and rate-limit skip log lines per (kind, key)."""

    def __init__(
        self,
        *,
        cooldown_seconds: float = 60.0,
        now: Callable[[], float] = time.monotonic,
    ) -> None:
        self._cooldown = float(cooldown_seconds)
        self._now = now
        self._entries: dict[tuple[str, str], _Entry] = {}

    def note(self, kind: str, key: str, tag: str, *, fatal: bool = False) -> bool:
        """Record an occurrence for ``(kind, key)``.

        ``True`` when the caller should emit a log line: the first note, or the
        first after the cooldown window elapsed. A fatal note memoizes ``tag``
        for :meth:`fatal_tag` lookups (survives cooldowns; a later fatal note
        updates the stored tag).
        """
        k = (kind, key)
        t = self._now()
        entry = self._entries.get(k)
        if entry is not None:
            if fatal:
                entry.fatal = True
                entry.tag = tag
            if t - entry.last_logged_at < self._cooldown:
                return False
            entry.last_logged_at = t
            return True
        self._entries[k] = _Entry(tag=tag, fatal=fatal, last_logged_at=t)
        return True

    def fatal_tag(self, kind: str, key: str) -> str | None:
        """The memoized fatal tag for ``(kind, key)``, or ``None``."""
        entry = self._entries.get((kind, key))
        if entry is None or not entry.fatal:
            return None
        return entry.tag
