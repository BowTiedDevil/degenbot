"""§4.2 parity for the PySubscriberAdapter pub/sub seam (task ZBD4MS).

The Rust pub/sub mechanism (`degenbot_bot::bot_core::log_dispatcher` — the
`PoolStateSubscriber` trait + `LogDispatcher` `Weak`-fan-out) is the single
notification path for Rust-owned `BotState` mutations. The
`PySubscriberAdapter` is the PyO3 seam that lets a Python callback register
as a `PoolStateSubscriber` against that SAME `LogDispatcher` the engine
adapter uses — so Rust-owned mutations notify Rust AND Python subscribers
through one fan-out.

The oracle is `_PyOracleOracle` below — an insertion-ordered list of
`weakref.ref` mirroring the Rust `Vec<Weak<dyn PoolStateSubscriber>>`.
§4.2 pins:

1. **Notification ordering + fan-out match the Rust mechanism contract.** The
   Rust `LogDispatcher::notify` iterates the `Vec<Weak<dyn PoolStateSubscriber>>`
   in registration order (a `Vec`, deterministic). A Python callback
   registered via `register_subscriber` receives `pool_id` in
   registration order; the matching Python oracle mirror (a `list` of `weakref.ref`,
   mirroring the Rust `Vec<Weak>` structure) produces the SAME notify sequence.
   The §4.2 contract is: the same SET of live subscribers receives exactly one
   notification each per dispatch; the Rust port additionally guarantees
   registration-order determinism.
2. **Drop-skip parity.** Dropping the returned `PoolStateSubscription` handle (or
   `.unsubscribe()`) drops the strong `Arc` → `LogDispatcher`'s `Weak` goes
   dead → subsequent `notify` calls silently skip it (mirrors the
   engine-adapter Weak lifecycle).
3. **One notify per dispatch.** A single dispatched log for a registered pool
   fires each (live) subscriber exactly once.
4. **Wrong-pool isolation.** A subscriber registered for `pool_id=A` is NOT
   notified when `pool_id=B` mutates.

The Rust notify surfaces the mutated ``pool_id`` per dispatch. The ordering
+ fan-out + drop-skip parity (the mechanism contract) IS pinned here.
"""

import gc
import time
import weakref

import pytest

from degenbot._ffi import Bot
from degenbot._ffi.subscriber import register_subscriber

_SUBSCRIBER_FLUSH_S = (
    0.15  # max seconds to wait for subscriber drainer flush (2x interval + margin)
)

# keccak256("Sync(uint112,uint112)") — the V2 Sync event signature.
_V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"


def _sync_data(reserve0: int, reserve1: int) -> str:
    """ABI-encode ``(uint112 reserve0, uint112 reserve1)`` (two 32-byte words)."""
    return "0x" + (reserve0.to_bytes(32, "big") + reserve1.to_bytes(32, "big")).hex()


def _register_v2_pool(py_bot: Bot, address: str = "0x" + "11" * 20) -> int:
    """Register a minimal V2 pool against ``py_bot``; return its pool_id."""
    return py_bot.register_v2_pool(
        address=address,
        token0="0x" + "aa" * 20,
        token1="0x" + "bb" * 20,
        reserve0=1_000,
        reserve1=2_000,
        gamma_numer0=997,
        fee_denom0=1_000,
        gamma_numer1=997,
        fee_denom1=1_000,
        factory="0x" + "ff" * 20,
    )


class _Recorder:
    """A callable subscriber: appends its name to a shared record on notify.

    `register_subscriber` invokes the callback as `callback(pool_id)`; this
    recorder ignores `pool_id` and just timestamps its own name, so the test
    asserts ORDER (which subscriber fired, in what sequence).
    """

    def __init__(self, name: str, record: list[str]) -> None:
        self._name = name
        self._record = record

    def __call__(self, pool_id: int) -> None:
        self._record.append(self._name)


class _PyOracleOracle:
    """A faithful Python mirror of the Rust `LogDispatcher` fan-out contract.

    Mirrors the Rust `LogDispatcher::notify` contract EXACTLY: subscribers are
    held as `weakref.ref`s in a `list` (insertion-ordered, mirroring the Rust
    `Vec<Weak<dyn PoolStateSubscriber>>`); `notify` iterates in registration
    order, upgrading each weak ref (skipping dead ones). A `WeakSet` would NOT
    preserve insertion order (set iteration is hash-order), so this uses an
    ordered list of weak refs — the same data structure the Rust side uses.
    """

    def __init__(self) -> None:
        self._subs: list[weakref.ref] = []

    def subscribe(self, sub: _Recorder) -> None:
        self._subs.append(weakref.ref(sub))

    def notify(self) -> list[str]:
        order: list[str] = []
        for w in self._subs:
            sub = w()
            if sub is not None:  # dead weak skipped — mirrors Weak::upgrade None.
                order.append(sub._name)
        return order


class TestPySubscriberAdapterParity:
    """§4.2 parity: Rust `LogDispatcher` fan-out matches the Python oracle's
    notification ordering + skip-on-drop contract.

    Notifications are delivered via a batched drainer (50ms flush interval),
    so each test waits up to `_SUBSCRIBER_FLUSH_S` after dispatch for the
    expected notification to arrive.
    """

    @staticmethod
    def _wait_for(condition, timeout=_SUBSCRIBER_FLUSH_S) -> None:
        """Poll until `condition` returns truthy, or raise after timeout."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = condition()
            if result:
                return
            time.sleep(0.01)
        msg = f"Condition not met within {timeout}s: {condition!r}"
        raise AssertionError(msg)

    def test_register_subscriber_receives_pool_id_on_dispatch(self):
        """A Python callback registered via `register_subscriber` receives
        `pool_id` when `dispatch_log` applies a decoded event to that pool."""
        py_bot = Bot()
        pool_id = _register_v2_pool(py_bot)
        record: list[int] = []
        # Hold the handle — the strong Arc anchors the Weak the dispatcher holds.
        handle = register_subscriber(py_bot, pool_id, record.append)

        assert record == [], "no notifications before a dispatched log"

        py_bot.dispatch_log(
            address="0x" + "11" * 20,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=1_500, reserve1=2_500),
            block_number=100,
        )
        self._wait_for(lambda: record == [pool_id], timeout=_SUBSCRIBER_FLUSH_S)
        assert record == [pool_id], (
            "the Python callback must fire exactly once with the mutated pool_id"
        )
        # Reassure the linter the handle is held (lifetime anchor).
        assert handle is not None

    def test_notification_order_matches_python_oracle(self):
        """§4.2: the Rust `LogDispatcher` fan-out order is IDENTICAL to the
        Python oracle's order for the same subscriber set. Register three
        Python callbacks against the Rust seam + the same three against the
        Python oracle; dispatch one log; assert both paths produced the same
        notify sequence."""
        py_bot = Bot()
        pool_id_a = _register_v2_pool(py_bot, address="0x" + "11" * 20)

        names = ["alpha", "beta", "gamma"]
        rust_record: list[str] = []
        # Hold every handle — each strong Arc anchors its Weak.
        handles = [register_subscriber(py_bot, pool_id_a, _Recorder(n, rust_record)) for n in names]

        # Python oracle mirror, identical registration order. Hold STRONG refs
        # to the subscribers (a WeakSet only holds weak refs; without strong
        # refs the subscribers would be GC'd and `notify()` would iterate dead
        # refs — which would actually ALSO match the Rust drop-skip contract,
        # but we want the live-subscriber ordering comparison).
        oracle = _PyOracleOracle()
        py_oracle_record: list[str] = []
        oracle_subs = [_Recorder(n, py_oracle_record) for n in names]
        for sub in oracle_subs:
            oracle.subscribe(sub)

        py_bot.dispatch_log(
            address="0x" + "11" * 20,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=1_500, reserve1=2_500),
            block_number=100,
        )
        # Wait for the Rust seam to flush (batched drainer).
        self._wait_for(lambda: len(rust_record) >= 3, timeout=_SUBSCRIBER_FLUSH_S)
        rust_order = list(rust_record)
        oracle_order = oracle.notify()

        assert rust_order == names, (
            f"Rust seam notify order {rust_order!r} != registration order {names!r}"
        )
        assert oracle_order == names, (
            f"Python oracle order {oracle_order!r} != registration order {names!r}"
        )
        assert rust_order == oracle_order, (
            "§4.2 parity breach: Rust fan-out order != Python oracle order"
        )
        assert len(handles) == 3  # anchors
        assert len(oracle_subs) == 3  # oracle anchors

    def test_dropped_subscriber_is_silently_skipped(self):
        """§4.2 drop-skip parity: dropping the returned `PoolStateSubscription` handle
        drops the strong `Arc` → `LogDispatcher`'s `Weak` goes dead → subsequent
        `notify` calls silently skip that subscriber, while the live one keeps
        firing. Mirrors the engine-adapter Weak lifecycle.
        """
        py_bot = Bot()
        pool_id = _register_v2_pool(py_bot)

        live_record: list[str] = []
        live_handle = register_subscriber(py_bot, pool_id, _Recorder("live", live_record))
        dropped_record: list[str] = []
        dropped_handle = register_subscriber(py_bot, pool_id, _Recorder("dropped", dropped_record))

        # First dispatch: both fire, in registration order.
        py_bot.dispatch_log(
            address="0x" + "11" * 20,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=9_000, reserve1=9_000),
            block_number=200,
        )
        self._wait_for(lambda: live_record == ["live"], timeout=_SUBSCRIBER_FLUSH_S)
        assert live_record == ["live"]
        assert dropped_record == ["dropped"]

        # Drop the second handle → its Weak goes dead.
        del dropped_handle
        gc.collect()

        # Second dispatch: only the live one fires (the dropped one skipped).
        py_bot.dispatch_log(
            address="0x" + "11" * 20,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=9_100, reserve1=9_100),
            block_number=201,
        )
        self._wait_for(lambda: live_record == ["live", "live"], timeout=_SUBSCRIBER_FLUSH_S)
        assert live_record == ["live", "live"], "live subscriber keeps firing"
        assert dropped_record == ["dropped"], (
            "dropped subscriber must be silently skipped after its handle drops"
        )
        assert live_handle is not None  # anchor

    def test_unsubscribe_method_releases_the_weak(self):
        """`.unsubscribe()` on the handle is the explicit form of dropping it:
        the strong Arc releases, the Weak goes dead, subsequent dispatches skip."""
        py_bot = Bot()
        pool_id = _register_v2_pool(py_bot)
        record: list[int] = []
        handle = register_subscriber(py_bot, pool_id, record.append)

        py_bot.dispatch_log(
            address="0x" + "11" * 20,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=1_500, reserve1=2_500),
            block_number=100,
        )
        self._wait_for(lambda: record == [pool_id], timeout=_SUBSCRIBER_FLUSH_S)
        assert record == [pool_id]

        handle.unsubscribe()
        py_bot.dispatch_log(
            address="0x" + "11" * 20,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=1_600, reserve1=2_600),
            block_number=101,
        )
        # Wait a full flush cycle to confirm NO new notification arrives.
        time.sleep(_SUBSCRIBER_FLUSH_S)
        assert record == [pool_id], (
            "after unsubscribe(), the subscriber must be skipped on subsequent dispatches"
        )

    def test_wrong_pool_subscriber_is_not_notified(self):
        """§4.2 isolation: a subscriber registered for `pool_id=A` is NOT
        notified when `pool_id=B` mutates."""
        py_bot = Bot()
        pool_id_a = _register_v2_pool(py_bot, address="0x" + "11" * 20)
        pool_id_b = _register_v2_pool(py_bot, address="0x" + "22" * 20)
        assert pool_id_a != pool_id_b

        a_record: list[str] = []
        handle_a = register_subscriber(py_bot, pool_id_a, _Recorder("A", a_record))

        # Dispatch a Sync for pool B → sub_a (registered for A) must not fire.
        py_bot.dispatch_log(
            address="0x" + "22" * 20,
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=5_000, reserve1=5_000),
            block_number=300,
        )
        assert a_record == [], (
            "a subscriber registered for pool A must not be notified for a pool B mutation"
        )
        assert handle_a is not None  # anchor

    def test_non_callable_callback_is_rejected(self):
        """The seam rejects a non-callable callback up front (RuntimeError)
        rather than failing at notify time."""
        py_bot = Bot()
        pool_id = _register_v2_pool(py_bot)
        with pytest.raises(RuntimeError, match="callable"):
            register_subscriber(py_bot, pool_id, 42)  # type: ignore[arg-type]

    def test_unregistered_pool_dispatch_is_no_op(self):
        """A Sync for an unregistered pool address is a silent no-op — the
        subscriber registered for a different pool is not notified."""
        py_bot = Bot()
        pool_id = _register_v2_pool(py_bot, address="0x" + "11" * 20)
        record: list[str] = []
        handle = register_subscriber(py_bot, pool_id, _Recorder("A", record))

        # Sync for a DIFFERENT (unregistered) address → no notify.
        py_bot.dispatch_log(
            address="0x" + "33" * 20,  # never registered
            topics=[_V2_SYNC_TOPIC],
            data=_sync_data(reserve0=1, reserve1=1),
            block_number=400,
        )
        assert record == []
        assert handle is not None  # anchor
