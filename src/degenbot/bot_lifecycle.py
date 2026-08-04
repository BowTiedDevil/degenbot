"""Shared Python-handle teardown for ``Bot``.

``Bot`` is the single-chain facade (ADR-006 D5) with attribute shape
(``_trackers``, ``pools``, ``tokens``, ``managed_pools``, ``db``,
``_py_bot``, ``_provider``, ``_io``). This module holds the teardown body
in one place so the facade delegates to it rather than inlining ~15 lines.

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
``EngineRegistry(bot=bot)`` → ``ArbitrageEngine(py_bot=...)``) is unaffected.
"""

from __future__ import annotations

from typing import Any, Protocol


class _BotLike(Protocol):
    """Structural shape required by the teardown functions.

    The volatile members are typed :data:`~typing.Any` because teardown
    reaches into registry/provider internals (``pools._reset``,
    ``tokens.reset``, ``db.remove``, ``_provider.close``) whose precise types
    live in unrelated modules — pulling them in here would create import
    cycles for a structural protocol that only needs call-site shape.
    """

    _trackers: Any
    pools: Any
    tokens: Any
    managed_pools: Any
    db: Any
    _provider: Any
    _py_bot: Any
    _io: Any
    _async_adapter: Any
    _erc20_builder: Any
    _aerodrome_v2_builder: Any
    _curve_builder: Any
    _balancer_builder: Any
    _builders: Any
    _closed: bool


def release_python_state(bot: _BotLike) -> None:
    """Drop Python-side pool/token/tracker caches once Rust owns canonical state.

    Clears every tracker's ``_tracked_pools``/``_untracked_pools``, calls
    ``unload_snapshot()`` where present, then resets the pool and token
    registries. Idempotent and safe to call before :func:`close`.
    """
    # 1. Drop tracker caches and snapshots (prevent them pinning pool objects)
    for tracker in bot._trackers.values():  # ruff:ignore[private-member-access]
        if hasattr(tracker, "_tracked_pools"):
            tracker._tracked_pools.clear()  # ruff:ignore[private-member-access]
        if hasattr(tracker, "_untracked_pools"):
            tracker._untracked_pools.clear()  # ruff:ignore[private-member-access]
        unload_snapshot = getattr(tracker, "unload_snapshot", None)
        if callable(unload_snapshot):
            unload_snapshot()

    # 2. Drop the pool and token registries (Rust owns canonical state). The
    # mid-lifecycle handoff keeps the Rust ``BotState`` pools — the live pump
    # keeps writing V3 Mint/Burn/Swap through the shared core, so we must NOT
    # unregister (propagate_to_rust=False). Unregistering is end-of-life only
    # (`close`), where the whole Rust state is torn down alongside the bot.
    bot.pools._reset(propagate_to_rust=False)  # type: ignore[attr-defined]  # ruff:ignore[private-member-access]
    bot.tokens.reset()  # type: ignore[attr-defined]


def close(bot: _BotLike) -> None:
    """End-of-life teardown: release state, remove DB session, close provider, drop refs.

    Idempotent — safe to call directly and again from a context manager's
    ``__exit__``/``__aexit__``. Composes :func:`release_python_state`, so it
    is also safe after an explicit mid-lifecycle ``release_python_state`` call.
    """
    if getattr(bot, "_closed", False):
        return
    bot._closed = True  # type: ignore[attr-defined]  # ruff:ignore[private-member-access]

    # 1. Drop tracker caches/snapshots + pool/token registries (idempotent)
    release_python_state(bot)

    # 2. Remove the scoped DB session (returns the thread-local session)
    bot.db.remove()  # type: ignore[attr-defined]

    # 3. Dispose the SQLAlchemy Engine so its connection pool (and the
    # underlying sqlite3.Connection) is actually closed. ``remove()`` alone
    # only returns the thread-local Session; the Engine survives and would
    # surface as ``ResourceWarning: unclosed database`` under GC. Defensive:
    # test stand-ins and MagicMock subs may not expose dispose().
    dispose = getattr(bot.db, "dispose", None)
    if callable(dispose):
        dispose()  # type: ignore[call-arg]

    # 4. Close the provider connection if it exposes close()
    if hasattr(bot._provider, "close"):  # ruff:ignore[private-member-access]
        bot._provider.close()  # type: ignore[attr-defined]  # ruff:ignore[private-member-access]

    # 5. Drop our own references (engine keeps its own PyBot ref)
    bot._py_bot = None  # type: ignore[attr-defined]  # ruff:ignore[private-member-access]
    bot._provider = None  # type: ignore[attr-defined]  # ruff:ignore[private-member-access]

    # 6. Drop the I/O seam (`PyBotIo`) and the on-demand async adapter. Both
    #    hold a strong reference to the shared provider's `Arc<dyn Provider>`;
    #    leaving them live pins that Arc — and if the provider's backing
    #    endpoint is a locally-owned anvil fork that is torn down next, alloy's
    #    pubsub service reconnects into the now-dead socket, logging
    #    `Reconnection attempt N/10 …` for up to 10 backoff attempts. Dropping
    #    them here lets the provider's pubsub shut down cleanly
    #    ("request channel closed") before any consumer of the endpoint dies.
    bot._io = None  # ruff:ignore[private-member-access]
    bot._async_adapter = None  # ruff:ignore[private-member-access]

    # 7. Drop the registries + builders + ctx that each hold a strong ref to
    #    `_py_bot` (PoolRegistry → PyBot, Tokens/Trackers, BuilderContext →
    #    PyBot). Rust `PyBot` is refcounted; until EVERY Python holder is
    #    dropped the rust core stays alive and keeps its `ConstructionIo`'s
    #    `Arc<dyn Provider>` — the same reconnect-into-a-dead-socket hazard as
    #    `_io`. Dropping all of them lets the rust `PyBot`/`BotState` release
    #    the provider when the last one goes.
    bot.pools = None
    bot.tokens = None
    bot.managed_pools = None
    bot._trackers = None  # ruff:ignore[private-member-access]
    bot._erc20_builder = None  # ruff:ignore[private-member-access]
    bot._aerodrome_v2_builder = None  # ruff:ignore[private-member-access]
    bot._curve_builder = None  # ruff:ignore[private-member-access]
    bot._balancer_builder = None  # ruff:ignore[private-member-access]
    bot._builders = None  # ruff:ignore[private-member-access]
