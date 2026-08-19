"""Consolidated fake subscriber implementation.

Replaces the previous ad hoc subscriber fakes:
- FakeSubscriber (from tests/conftest.py)
- FakeSubscriber (from tests/arbitrage/test_path/conftest.py)

The canonical version combines the ``inbox`` + ``subscribe``/``unsubscribe``
API from the root conftest with the ``notifications`` list from the
test_path variant.

## The subscribe path (task 73UJL6 / VI53G5 — route consumers through the Rust seam)

A ``FakeSubscriber`` subscribes to a **Rust-owned** pool companion (a
``UniswapV2Pool`` / ``UniswapV3Pool`` / ``UniswapV4Pool`` etc. built via a
builder, whose owning ``Bot`` + ``pool_id`` the caller has) through the
explicit ``subscribe_rust(bot, pool_id)`` method — no pool-side auto-detect
(the earlier dual-mode auto-detect stashed ``_py_bot`` on every pool class,
which was a Python mirror of Rust-owned state — rejected per AGENTS.md's
“do not introduce a Python mirror of Rust-owned state”).

``subscribe_rust(bot, pool_id)`` routes through the Rust pub/sub seam
(``degenbot._ffi.register_subscriber``), so the ``FakeSubscriber`` receives
state-change notifications through the SAME ``LogDispatcher``
``Weak``-fan-out path the engine ``EngineSubscriber`` uses (the single Rust
notification path for Rust-owned ``BotState`` mutations, per ADR-005 / the
ZBD4MS seam, committed ``e7b721c6``). The caller already has BOTH the
``bot`` and the ``pool_id`` (which ``pool._py_pool.pool_id`` exposes), so
no pool-side bot stash is needed. ``register_subscriber`` invokes the
callback as ``callback(pool_id)`` — so ``FakeSubscriber.__call__(pool_id)``
is the notify entry point on this path, recording ``pool_id``. The returned
``PoolStateSubscription`` handle is stored (anchoring the strong ``Arc`` the
dispatcher's ``Weak`` depends on — dropping it unregisters).

``inbox`` records ``{"pool_id": pool_id}`` dicts and ``notifications``
likewise records ``(None, pool_id)`` tuples (no Python publisher object is
surfaced by the seam's ``pool_id``-only payload).

## Architectural note on the pool-side bot stash (why this is explicit API)

The earlier draft stashed the owning ``Bot`` on every pool companion via
``_py_bot`` (set in ``PoolRegistry.add`` / ``ManagedPoolRegistry.add`` / the
7 ``make_*`` factory helpers) so ``subscribe`` could auto-detect the Rust path
from the publisher object + extract the bot from the pool. That was a Python
mirror of a Rust-owned reference across 7 pool classes + 2 registries —
exactly the anti-pattern AGENTS.md prohibits (“do not introduce a Python
mirror of Rust-owned state”). The ``Bot`` is Rust-owned state; a Python
side-channel duplicating it on every pool is the wrong layering. The clean
fix is the explicit ``subscribe_rust(bot, pool_id)`` API here: the consumer /
test context that wants to subscribe to a Rust-owned pool ALREADY has the
``Bot`` (it built the pool through it) — pass it explicitly rather than
extracting it from the pool.

If a FUTURE real (non-test) consumer callsite is found that genuinely can't
reach the bot, THAT is the architectural signal that ``LiquidityPool``
needs a ``bot()`` getter in Rust (a small, separate task in worker_3's Rust
lane) — not a Python stash. Until then, the explicit API is correct:
``FakeSubscriber`` is a test helper where the test always has the bot.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from degenbot._ffi import Bot
    from degenbot._ffi.subscriber import PoolStateSubscription


class FakeSubscriber:
    """Subscriber that records received messages for test assertions.

    Supports both ``inbox`` (dict records) and ``notifications`` (tuple records)
    so it can serve as a drop-in replacement for either prior variant.

    Subscribes to Rust-owned pools via ``subscribe_rust(bot, pool_id)``
    (routes through the Rust ``register_subscriber`` seam — see the module
    docstring).
    """

    def __init__(self) -> None:
        self.inbox: list[dict[str, Any]] = []
        self.notifications: list[tuple[object, object]] = []
        # PoolStateSubscription handles anchoring the strong Arc for each Rust-routed
        # subscription. Held for the FakeSubscriber's lifetime so the
        # dispatcher's Weak stays live; dropping a handle (or GC of this
        # FakeSubscriber) unregisters. Mirrors the engine-adapter Weak
        # lifecycle.
        self._subscriptions: list[PoolStateSubscription] = []

    def __call__(self, pool_id: int) -> None:
        """Rust-seam notify path (task 73UJL6 / ZBD4MS).

        ``register_subscriber`` invokes the callback as ``callback(pool_id)``;
        the seam's payload is the mutated ``pool_id``. Records ``pool_id`` —
        no Python publisher object is surfaced on this path, so the tuple's
        first slot is ``None``.
        """
        self.inbox.append({"pool_id": pool_id})
        self.notifications.append((None, pool_id))

    def subscribe_rust(self, bot: Bot, pool_id: int) -> None:
        """Subscribe to a Rust-owned pool through the Rust pub/sub seam.

        Routes the subscription through ``degenbot._ffi.register_subscriber``
        (the ZBD4MS seam, committed ``e7b721c6``), so this ``FakeSubscriber``
        receives state-change notifications through the SAME ``LogDispatcher``
        ``Weak``-fan-out path the engine ``EngineSubscriber`` uses — the single
        Rust notification path for Rust-owned ``BotState`` mutations (ADR-005).

        The caller passes BOTH the ``bot`` and the ``pool_id`` it already has
        (the builder/test context that built the pool through the bot exposes
        ``pool._py_pool.pool_id``). No pool-side bot stash is needed — the
        ``Bot`` is Rust-owned state and is NOT mirrored onto the pool object
        (AGENTS.md: “do not introduce a Python mirror of Rust-owned state”).

        ``register_subscriber`` invokes the callback as ``callback(pool_id)``
        on each dispatch — see ``__call__``. The returned ``PoolStateSubscription``
        handle is stored (anchoring the strong ``Arc`` the dispatcher's ``Weak``
        depends on — see ``unsubscribe_rust`` / GC).

        Args:
            bot: The owning ``Bot`` (Rust ``BotState`` handle).
            pool_id: The Rust pool id (from ``pool._py_pool.pool_id``).

        """
        from degenbot._ffi.subscriber import register_subscriber

        subscription = register_subscriber(bot, pool_id, self)
        self._subscriptions.append(subscription)

    def unsubscribe_rust(self) -> None:
        """Release every stored ``PoolStateSubscription`` handle (Rust path).

        Releases the strong-``Arc`` anchor for each Rust-routed subscription so
        the dispatcher's ``Weak`` goes dead + subsequent notify calls silently
        skip (mirrors the engine-adapter Weak lifecycle). Idempotent.

        NOTE: the ``PoolStateSubscription`` handle doesn't expose its ``pool_id``, so
        this releases handles for ALL Rust publishers this subscriber is
        registered against, not a specific one. Restricting would require the
        handle to surface its ``pool_id`` (a Rust seam widening — deferred). The
        test-suite pattern is one-publisher-per-FakeSubscriber, so this is
        safe in practice.
        """
        for sub in list(self._subscriptions):
            sub.unsubscribe()
        self._subscriptions.clear()
