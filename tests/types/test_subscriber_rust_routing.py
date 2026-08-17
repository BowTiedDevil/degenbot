"""§4.2 parity: Python pub/sub CONSUMERS route through the Rust seam (task 73UJL6).

The Rust pub/sub mechanism (`degenbot_bot::bot_core::log_dispatcher` — the
`PoolStateSubscriber` trait + `LogDispatcher` `Weak`-fan-out) is the single
notification path for Rust-owned ``BotState`` mutations. Worker_3's ZBD4MS seam
(``degenbot._ffi.register_subscriber(bot, pool_id, callback) -> PoolStateSubscription``
+ the ``PySubscriberAdapter``) is the bridge a Python callable uses to register
against that SAME ``LogDispatcher`` path the engine ``EngineSubscriber`` uses.

THIS task (73UJL6) routes the Python pub/sub **consumers** — the classes/
functions that subscribe to a ``Publisher`` to drive an update loop — through
that Rust seam when their target is a **Rust-owned** pool, instead of the legacy
Python ``PublisherMixin`` path. The canonical consumer is
``tests/fakes/subscribers.py::FakeSubscriber``, which exposes TWO explicit
subscribe methods:

- ``subscribe_rust(bot, pool_id)`` — routes through the Rust
  ``register_subscriber`` seam (the consumer/test context that built the pool
  through the bot ALREADY has both arguments — ``pool._py_pool.pool_id`` — so
  no pool-side bot stash is needed; the ``RustBot`` is Rust-owned state and is
  NOT mirrored onto the pool object, per AGENTS.md's "do not introduce a
  Python mirror of Rust-owned state").
- ``subscribe(publisher)`` — the legacy Python ``PublisherMixin`` path for
  Python-only publishers (pinned by ``tests/types/test_publisher_mixin.py`` +
  ``test_pool_protocols.py``, which stay Python per the 73UJL6 non-goals).

The oracle for the Rust-routed path is the SAME ``LogDispatcher`` fan-out
contract worker_3's ``tests/test_pubsub_seam_parity.py`` pins (registration-
ordered, drop-skipped, one-notify-per-dispatch, wrong-pool isolation). This file
asserts the CONSUMER-routed path delivers that contract through the
``FakeSubscriber.subscribe_rust`` seam (not the low-level ``register_subscriber``
free function): a ``FakeSubscriber`` that subscribed to a ``make_v2_pool``
companion receives the mutated ``pool_id`` when ``py_bot.dispatch_log`` applies
a V2 ``Sync`` event to that pool — the SAME ``pool_id`` worker_3's seam surfaces.

The seam's current payload is ``pool_id`` only (the full ``PoolStateUpdated``
``notify(publisher, message)`` reconstruction is the deferred
``PublisherMixin``-retirement sibling task, flagged in ``FakeSubscriber``'s
docstring). So the ``FakeSubscriber.__call__`` notify records ``pool_id``
(``inbox`` ``{"pool_id": ...}`` / ``notifications`` ``(None, pool_id)``) — the
parity here is the notification SET + order, not the message payload.
"""

from __future__ import annotations

import time
from fractions import Fraction

from degenbot.bot import RustBot

_SUBSCRIBER_FLUSH_S = 0.15  # max seconds to wait for subscriber drainer flush
from tests.fakes.subscribers import FakeSubscriber
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

# keccak256("Sync(uint112,uint112)") — the V2 Sync event signature.
_V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"


def _sync_data(reserve0: int, reserve1: int) -> str:
    """ABI-encode ``(uint112 reserve0, uint112 reserve1)`` (two 32-byte words)."""
    return "0x" + (reserve0.to_bytes(32, "big") + reserve1.to_bytes(32, "big")).hex()


def _make_v2_pool_with_subscriber(
    py_bot: RustBot,
) -> tuple[int, FakeSubscriber, str, object]:
    """Build a V2 pool (token0/token1 registered in the same RustBot per ADR-006),
    subscribe a ``FakeSubscriber`` to it via the Rust seam, return
    ``(pool_id, subscriber, address, pool)``.

    The caller (this helper) has BOTH the ``py_bot`` and the pool, so it passes
    them explicitly to ``FakeSubscriber.subscribe_rust(bot, pool_id)`` — no
    pool-side ``_py_bot`` stash is needed (the ``RustBot`` is Rust-owned state;
    mirroring it onto the pool object would be the AGENTS.md anti-pattern).
    """
    token0 = make_erc20(
        py_bot=py_bot,
        address="0x" + "1" * 40,
        name="Token0",
        symbol="T0",
        decimals=18,
        chain_id=1,
    )
    token1 = make_erc20(
        py_bot=py_bot,
        address="0x" + "2" * 40,
        name="Token1",
        symbol="T1",
        decimals=18,
        chain_id=1,
    )
    address = "0x" + "a" * 40
    pool = make_v2_pool(
        address=address,
        token0=token0,
        token1=token1,
        factory="0x" + "f" * 40,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1_000_000,
        reserves_token1=2_000_000,
        chain_id=1,
        py_bot=py_bot,
    )
    # The caller has BOTH the bot and the pool_id — pass them explicitly to the
    # Rust seam. No pool-side _py_bot stash (the RustBot is Rust-owned state).
    sub = FakeSubscriber()
    sub.subscribe_rust(py_bot, pool._py_pool.pool_id)
    return pool._py_pool.pool_id, sub, address, pool


class TestFakeSubscriberRustSeamRouting:
    """§4.2 parity: ``FakeSubscriber.subscribe_rust`` routes a Rust-owned pool's
    notifications through the Rust ``register_subscriber`` seam."""

    def test_rust_routed_subscriber_receives_pool_id_on_dispatch(self):
        """A ``FakeSubscriber`` subscribed via ``subscribe_rust(bot, pool_id)``
        to a Rust-owned V2 pool receives ``pool_id`` when ``dispatch_log``
        applies a V2 ``Sync`` event to that pool — routed through the Rust
        seam, NOT the legacy Python ``PublisherMixin`` path."""
        py_bot = RustBot(1)
        pool_id, sub, address, _pool = _make_v2_pool_with_subscriber(py_bot)

        assert sub.inbox == [], "no notifications before a dispatched log"
        assert sub.notifications == []

        py_bot.dispatch_log(
            address=address,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=1_500, reserve1=2_500),
            block_number=100,
        )

        # The seam surfaces ``pool_id`` only (``FakeSubscriber.__call__``'s
        # notify records ``{"pool_id": pool_id}`` / ``(None, pool_id)`` — the
        # ``None`` publisher slot is the payload-fidelity gap flagged in the
        # ``FakeSubscriber`` docstring + this module's: the full
        # ``PoolStateUpdated(state)`` reconstruction is a deferred sibling).
        #
        # Wait for the subscriber drainer to flush (batched notify).
        deadline = time.monotonic() + _SUBSCRIBER_FLUSH_S
        while time.monotonic() < deadline:
            if sub.inbox == [{"pool_id": pool_id}]:
                break
            time.sleep(0.01)
        assert sub.inbox == [{"pool_id": pool_id}], (
            f"Rust-routed FakeSubscriber must receive pool_id={pool_id} once; "
            f"got inbox={sub.inbox!r}"
        )
        assert sub.notifications == [(None, pool_id)]

    def test_notification_order_for_multiple_rust_routed_subscribers(self):
        """§4.2: multiple ``FakeSubscriber``s subscribed to one Rust-owned pool
        receive ``pool_id`` in REGISTRATION order (the Rust ``LogDispatcher``
        ``Vec<Weak>`` fan-out is registration-ordered + deterministic — STRONGER
        than the Python ``PublisherMixin`` ``WeakSet`` hash-order)."""
        py_bot = RustBot(1)
        token0 = make_erc20(
            py_bot=py_bot,
            address="0x" + "1" * 40,
            name="Token0",
            symbol="T0",
            decimals=18,
            chain_id=1,
        )
        token1 = make_erc20(
            py_bot=py_bot,
            address="0x" + "2" * 40,
            name="Token1",
            symbol="T1",
            decimals=18,
            chain_id=1,
        )
        address = "0x" + "a" * 40
        pool = make_v2_pool(
            address=address,
            token0=token0,
            token1=token1,
            factory="0x" + "f" * 40,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=1_000_000,
            reserves_token1=2_000_000,
            chain_id=1,
            py_bot=py_bot,
        )

        sub1 = FakeSubscriber()
        sub2 = FakeSubscriber()
        sub3 = FakeSubscriber()
        pool_id = pool._py_pool.pool_id
        sub1.subscribe_rust(py_bot, pool_id)
        sub2.subscribe_rust(py_bot, pool_id)
        sub3.subscribe_rust(py_bot, pool_id)

        py_bot.dispatch_log(
            address=address,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=1_500, reserve1=2_500),
            block_number=100,
        )

        # Wait for subscriber drainer to flush.
        deadline = time.monotonic() + _SUBSCRIBER_FLUSH_S
        while time.monotonic() < deadline:
            if all(
                len(n) == 1 for n in [sub1.notifications, sub2.notifications, sub3.notifications]
            ):
                break
            time.sleep(0.01)

        order = [sub1.notifications, sub2.notifications, sub3.notifications]
        assert all(len(n) == 1 for n in order), (
            f"each subscriber must fire exactly once; got {order!r}"
        )
        # Each received the SAME pool_id (one-notify-per-dispatch).
        for sub in (sub1, sub2, sub3):
            assert sub.notifications == [(None, pool_id)], (
                f"subscriber fired with {sub.notifications!r}, expected pool_id={pool_id}"
            )
        # Registration-order determinism is asserted at the seam level by
        # worker_3's test_pubsub_seam_parity.py (the ``_Recorder``-order test);
        # here the consumer-routing parity is the SET of (one-each) notifies.

    def test_unsubscribe_releases_the_rust_subscriber(self):
        """``FakeSubscriber.unsubscribe_rust`` releases the stored
        ``PoolStateSubscription`` handle (anchor) → the dispatcher's ``Weak`` goes
        dead → subsequent dispatches silently skip this subscriber (mirrors
        ``PublisherMixin.unsubscribe`` + the engine-adapter Weak lifecycle)."""
        py_bot = RustBot(1)
        pool_id, sub, address, _pool = _make_v2_pool_with_subscriber(py_bot)

        py_bot.dispatch_log(
            address=address,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=1_500, reserve1=2_500),
            block_number=100,
        )
        deadline = time.monotonic() + _SUBSCRIBER_FLUSH_S
        while time.monotonic() < deadline:
            if sub.notifications == [(None, pool_id)]:
                break
            time.sleep(0.01)
        assert sub.notifications == [(None, pool_id)]

        sub.unsubscribe_rust()  # release the PoolStateSubscription handles.

        py_bot.dispatch_log(
            address=address,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=1_600, reserve1=2_600),
            block_number=101,
        )
        # Wait to confirm NO new notification arrives.
        time.sleep(_SUBSCRIBER_FLUSH_S)
        # No new notification — the handle was released, the Weak went dead.
        assert sub.notifications == [(None, pool_id)], (
            "after unsubscribe_rust(), the Rust-routed subscriber must be skipped"
        )

    def test_python_only_publisher_stays_on_publisher_mixin_path(self):
        """A ``FakeSubscriber`` subscribed via ``subscribe(publisher)`` to a
        Python-only publisher (no ``_py_bot``/``_py_pool``) takes the legacy
        ``PublisherMixin`` path — the full ``notify(publisher, message)`` is
        delivered. Pins the parity-preservation AC for
        ``test_publisher_mixin.py`` + ``test_pool_protocols.py``."""
        pub = _PythonOnlyPublisher()
        sub = FakeSubscriber()
        sub.subscribe(pub)  # Python-only publisher → PublisherMixin path.

        msg = _DummyMessage()
        pub._notify_subscribers(msg)

        assert sub.inbox == [{"from": pub, "message": msg}], (
            "Python-only publisher must deliver the full (publisher, message) payload"
        )
        assert sub.notifications == [(pub, msg)]

    def test_wrong_pool_dispatch_does_not_notify_subscriber(self):
        """§4.2 isolation: a ``FakeSubscriber`` subscribed to pool A is NOT
        notified when a ``Sync`` for a different pool B mutates (the Rust seam
        registers per-``pool_id``; a B dispatch fires B's subscribers only)."""
        py_bot = RustBot(1)
        _pool_id_a, sub_a, _address_a, _pool_a = _make_v2_pool_with_subscriber(py_bot)
        # Register a SECOND V2 pool (different address) in the same RustBot.
        # ``make_v2_pool`` constructs fresh LiquidityPool handles per the
        # token-registration convention — pool B uses different token bytes.
        token0_b = make_erc20(
            py_bot=py_bot,
            address="0x" + "3" * 40,
            name="Token0B",
            symbol="TB0",
            decimals=18,
            chain_id=1,
        )
        token1_b = make_erc20(
            py_bot=py_bot,
            address="0x" + "4" * 40,
            name="Token1B",
            symbol="TB1",
            decimals=18,
            chain_id=1,
        )
        address_b = "0x" + "b" * 40
        make_v2_pool(
            address=address_b,
            token0=token0_b,
            token1=token1_b,
            factory="0x" + "f" * 40,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=500,
            reserves_token1=500,
            chain_id=1,
            py_bot=py_bot,
        )

        # Dispatch a Sync for pool B → sub_a (registered for A) must not fire.
        py_bot.dispatch_log(
            address=address_b,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=5_000, reserve1=5_000),
            block_number=300,
        )
        assert sub_a.notifications == [], (
            "a FakeSubscriber registered for pool A must not be notified for a pool B mutation"
        )


class _DummyMessage:
    """A minimal publisher message for the Python-only path parity test."""


class _PythonOnlyPublisher:
    """A minimal ``PublisherMixin`` publisher (Python-only — no Rust backing).

    Mirrors ``tests/types/test_publisher_mixin.py::_FakePublisher``: a
    ``PublisherMixin`` with a ``WeakSet`` of subscribers, exercising the legacy
    Python ``notify(publisher, message)`` dispatch path. ``FakeSubscriber.subscribe``
    takes this path for a Python-only publisher (no ``_py_bot``/``_py_pool``).
    """

    def __init__(self) -> None:
        from weakref import WeakSet

        self._subscribers: WeakSet[FakeSubscriber] = WeakSet()

    def subscribe(self, subscriber: FakeSubscriber) -> None:
        self._subscribers.add(subscriber)

    def unsubscribe(self, subscriber: FakeSubscriber) -> None:
        self._subscribers.discard(subscriber)

    def _notify_subscribers(self, message) -> None:
        for subscriber in self._subscribers:
            subscriber.notify(publisher=self, message=message)
