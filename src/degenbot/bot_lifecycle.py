"""Shared Python-handle teardown for ``Bot`` and ``AsyncBot``.

``Bot`` and ``AsyncBot`` are parallel single-chain facades (ADR-006 D5) with
identical attribute shape (``_trackers``, ``pools``, ``tokens``,
``managed_pools``, ``db``, ``_py_bot``, ``_provider``) but no shared base
class. This module holds the teardown body in one place so both delegate to
it rather than duplicating ~15 lines.

Two entry points:

- :func:`release_python_state` — the *mid-lifecycle* handshake: drop
  tracker caches/snapshots + pool/token registries once the Rust engine has
  taken ownership of canonical pool state. The Bot keeps running; only the
  redundant Python caches go.
- :func:`close` — the *end-of-life* teardown: composes
  :func:`release_python_state` and adds the connection teardown
  (``db.remove()``, ``provider.close()``) plus reference drops. Idempotent
  via a per-instance ``_closed`` flag.

The Rust ``PyBot`` is reference-counted; closing a Python wrapper only drops
that wrapper's ref. A running engine that took its own ref (via
``EngineRegistry(bot=bot)`` → ``UniswapArbEngine(py_bot=...)``) is unaffected.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Protocol

if TYPE_CHECKING:
    from degenbot.types.abstract.pool_tracker import AbstractPoolTracker


class _BotLike(Protocol):
    """Structural shape required by the teardown functions.

    The volatile members are typed :data:`~typing.Any` because teardown
    reaches into registry/provider internals (``pools._reset``,
    ``tokens.reset``, ``db.remove``, ``_provider.close``) whose precise types
    live in unrelated modules — pulling them in here would create import
    cycles for a structural protocol that only needs call-site shape.
    """

    _trackers: dict[str, AbstractPoolTracker[Any]]
    pools: Any
    tokens: Any
    db: Any
    _provider: Any
    _py_bot: Any
    _closed: bool


def release_python_state(bot: _BotLike) -> None:
    """Drop Python-side pool/token/tracker caches once Rust owns canonical state.

    Clears every tracker's ``_tracked_pools``/``_untracked_pools``, calls
    ``unload_snapshot()`` where present, then resets the pool and token
    registries. Idempotent and safe to call before :func:`close`.
    """
    # 1. Drop tracker caches and snapshots (prevent them pinning pool objects)
    for tracker in bot._trackers.values():  # noqa: SLF001
        if hasattr(tracker, "_tracked_pools"):
            tracker._tracked_pools.clear()  # noqa: SLF001
        if hasattr(tracker, "_untracked_pools"):
            tracker._untracked_pools.clear()  # noqa: SLF001
        unload_snapshot = getattr(tracker, "unload_snapshot", None)
        if callable(unload_snapshot):
            unload_snapshot()

    # 2. Drop the pool and token registries (Rust owns canonical state)
    bot.pools._reset()  # type: ignore[attr-defined]  # noqa: SLF001
    bot.tokens.reset()  # type: ignore[attr-defined]


def close(bot: _BotLike) -> None:
    """End-of-life teardown: release state, remove DB session, close provider, drop refs.

    Idempotent — safe to call directly and again from a context manager's
    ``__exit__``/``__aexit__``. Composes :func:`release_python_state`, so it
    is also safe after an explicit mid-lifecycle ``release_python_state`` call.
    """
    if getattr(bot, "_closed", False):
        return
    bot._closed = True  # type: ignore[attr-defined]  # noqa: SLF001

    # 1. Drop tracker caches/snapshots + pool/token registries (idempotent)
    release_python_state(bot)

    # 2. Remove the scoped DB session (returns the thread-local session)
    bot.db.remove()  # type: ignore[attr-defined]

    # 3. Close the provider connection if it exposes close()
    if hasattr(bot._provider, "close"):  # noqa: SLF001
        bot._provider.close()  # type: ignore[attr-defined]  # noqa: SLF001

    # 4. Drop our own references (engine keeps its own PyBot ref)
    bot._py_bot = None  # type: ignore[attr-defined]  # noqa: SLF001
    bot._provider = None  # type: ignore[attr-defined]  # noqa: SLF001
