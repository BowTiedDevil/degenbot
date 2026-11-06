"""Tests for ArbitrageEngine — mixed V2/V3 arbitrage engine."""

from __future__ import annotations

import pytest

from degenbot.arbitrage.engine_registry import ArbitrageEngine
from degenbot.dispatch import Dispatcher, SessionConsumer, SessionConsumerError
from tests.helpers.session_drive import block_event, offline_executor
from tests.helpers.v2_cycle_engine import build_two_hop_v2_engine

# The prefix every mapped leaf failure carries. A slot-local refusal is
# prefix-free by construction, so its presence in a slot message means that
# refusal was routed through the loud-abort mapping.
LOUD_ABORT_PREFIX = "sim-submit-pipeline leaf task failed - aborting the consumer loudly: "

# sqrt price at tick 0 (1:1 price for 18-decimal tokens)
SQRT_PRICE_TICK_0 = 79228162514264337593543950336

USDC = 10**6
WETH = 10**18

# Common pool addresses used in tests
V2_POOL_A = "0x" + "11" * 20
V2_POOL_B = "0x" + "12" * 20
V3_POOL = "0x" + "22" * 20

# V4 PoolManager address (mainnet)
V4_PM = "0x000000000004444c5dc75cB358380D2e3De08A90"


def _make_pool_id(suffix: int) -> str:
    """Generate a 32-byte pool ID as hex string."""
    return "0x" + (b"\x00" * 31 + bytes([suffix])).hex()


class TestEventBufferControl:
    def test_set_event_buffer_max_age(self):
        engine = ArbitrageEngine()
        engine.set_event_buffer_max_age(max_age=None)
        engine.set_event_buffer_max_age(max_age=100)

    def test_flush_event_buffer(self):
        engine = ArbitrageEngine()
        engine.flush_event_buffer()


class TestLifecycleMoves:
    def test_connect_and_close_are_the_engine_lifecycle_moves(self):
        """The engine exposes the connect/close pair (they will not connect in tests).

        The arm move belongs to the session consumer (`resume` is the claim's
        one use), so the engine carries only the two start-flow halves the run
        ritual calls in order around that resume.
        """
        engine = ArbitrageEngine()
        assert hasattr(engine, "connect")
        assert hasattr(engine, "close")
        assert hasattr(engine, "enable_facets")
        assert hasattr(engine, "start_hosted_session")

    async def test_resume_before_attach_refuses_on_the_claim(self):
        """`resume` consumes the consumer claim, so an unattached consumer refuses.

        A consumer built but never handed the engine's event hand-off holds no
        claim; resuming it is refused for the hand-off that was never attached,
        in the refusal's own words. No pipeline leaf ran, so the mapped error is
        prefix-free — the loud-abort prefix is a leaf failure's label and only
        that — while the typed facts behind the refusal (`batch_index`) ride the
        error itself.
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer(executor, allow_quiet_end=True)
        with pytest.raises(SessionConsumerError, match="no attached session") as refusal:
            await consumer.resume()
        assert refusal.value.batch_index == 0

    def test_connect_signature_takes_the_endpoint_only(self):
        """Configuration is not a lifecycle argument (ADR-093 D1)."""
        engine = ArbitrageEngine()
        import inspect

        sig = inspect.signature(engine.connect)
        params = list(sig.parameters.keys())
        assert "rpc_url" in params
        assert "buffer_event_types" not in params


class TestEngineStartRefusals:
    """Real-FFI pins for ``ArbitrageEngine.start`` — the one-call ritual
    (connect → verify-config) the shell composes.

    The suite reaches the driver's refusal paths without a live node, exactly
    as :class:`TestSubscribeResume` does: a fresh engine's phase admits
    ``subscribe``, so the first refusal comes from the real WS transport, and
    a stopped driver refuses before any transport is opened. The driver's
    ``already subscribed`` branch is only reachable after a successful WS
    subscribe, so it stays pinned against the fake
    (``test_start_propagates_driver_session_error_on_double_start``) rather
    than by booting a live stream here.
    """

    def test_start_on_a_fresh_engine_reaches_the_driver_ws_refusal(self):
        """A bare ``start`` delegates to the real driver and stops at the
        transport: the phase gate admits a fresh engine's ``subscribe``, so
        the RuntimeError is the driver's typed subscribe failure, not a Python
        precondition error."""
        engine = ArbitrageEngine()
        with pytest.raises(RuntimeError, match="WsIngestor"):
            engine.start("http://127.0.0.1:1", "ws://127.0.0.1:1")

    def test_start_after_stop_refuses_with_the_driver_session_error(self):
        """Re-entry through the one-call ritual surfaces the driver's typed
        session refusal (``RuntimeError``) instead of re-authoring the
        sequence — the real twin of the fake's double-start pin."""
        engine = ArbitrageEngine()
        engine.close()
        with pytest.raises(RuntimeError, match="session is stopped"):
            engine.start("http://127.0.0.1:1", "ws://127.0.0.1:1")


class TestConsumerSlotTransitions:
    """The session consumer's lifecycle slot: one consumer, one origin, one move.

    ``SessionConsumer`` holds a slot over the core consumer, and the consumer
    moves together with its event origin: a scripted consumer's refused move
    restores the scripted receiver, so the follow-on ``run`` still consumes the
    script the caller supplied. A move the slot cannot make is refused by name
    (the slot's own transition-naming message, outside the core's mapping), and
    that refusal is pinned exactly: a bare ``RuntimeError``, carrying none of
    the loud-abort prefix the mapped errors wear, so routing a slot refusal
    through the mapping fails the test instead of passing under a broader
    ``RuntimeError`` match. The unattached, claim-spent and driver refusals stay
    the core's, so a test that reaches them pins the occurrence and the
    follow-on product, not the text.
    """

    async def test_a_refused_resume_keeps_the_scripted_origin(self):
        """A scripted consumer's refused ``resume`` leaves the script armed.

        ``resume`` takes the consumer out for its await and restores it - with
        its origin. A scripted consumer holds no session to resume, so the core
        refuses; the restore must keep the scripted receiver, so the follow-on
        ``run`` consumes the scripted stream rather than the (absent) live arm.
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer.scripted(executor, [block_event(100)], allow_quiet_end=True)

        with pytest.raises(SessionConsumerError, match="no attached session"):
            await consumer.resume()

        report = await consumer.run()
        assert report["blocks"] == 1, "the refused resume kept the scripted stream"
        assert report["end_of_stream"] == 1
        assert report["end"] == "quiet-end"

    async def test_a_driver_refused_resume_then_run_consumes_the_closed_session(self):
        """A driver-refused resume leaves the consumer able to ``run``.

        The core's ``run`` deliberately drops the claim ``resume`` uses, so a
        session whose driver refused the resume is still consumed to its typed
        end verdict by the next move (a never-connected driver's stream is
        already closed).
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer(executor, allow_quiet_end=False)
        consumer.attach(ArbitrageEngine())

        with pytest.raises(SessionConsumerError):
            await consumer.resume()

        report = await consumer.run()
        assert report["end"] == "pump-stopped"
        assert report["end_of_stream"] == 1
        assert report["blocks"] == 0

    async def test_a_claim_spent_resume_then_run_consumes_the_closed_session(self):
        """The claim-spent refusal is reachable through a driver refusal.

        The first ``resume`` is driver-refused and takes the claim with it, so a
        second ``resume`` is refused claim-spent; the slot restores after both,
        and ``run`` still consumes the closed session's stream to its typed end.
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer(executor, allow_quiet_end=False)
        consumer.attach(ArbitrageEngine())

        with pytest.raises(SessionConsumerError):
            await consumer.resume()
        with pytest.raises(SessionConsumerError, match="claim was already used"):
            await consumer.resume()

        report = await consumer.run()
        assert report["end"] == "pump-stopped"
        assert report["end_of_stream"] == 1
        assert report["blocks"] == 0

    def test_a_second_attach_is_refused_and_keeps_the_hand_off(self):
        """One session has one consumer: a second attach is refused by name.

        The refusal fires on the slot BEFORE the second engine's hand-off is
        taken, so a refused attach destroys nothing: that engine's stream is
        still claimable by a consumer of its own. It is the slot's own
        refusal, so its shape is exact: a bare ``RuntimeError``, prefix-free.
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer(executor, allow_quiet_end=True)
        consumer.attach(ArbitrageEngine())
        second = ArbitrageEngine()

        with pytest.raises(RuntimeError, match="one session has one consumer") as refusal:
            consumer.attach(second)

        assert type(refusal.value) is RuntimeError, (
            "the slot refuses in its own type; a SessionConsumerError here means "
            "the refusal was routed through the loud-abort mapping"
        )
        assert LOUD_ABORT_PREFIX not in str(refusal.value), (
            "the slot's refusal is prefix-free, not a mapped leaf failure"
        )

        SessionConsumer(executor, allow_quiet_end=True).attach(second)

    async def test_a_scripted_consumer_refuses_attach(self):
        """A scripted consumer cannot attach an engine hand-off.

        The refused attach leaves both sides intact: the scripted receiver stays
        armed for the consumer's ``run``, and the engine's hand-off is still
        claimable. It is the slot's own refusal, so its shape is exact: a bare
        ``RuntimeError``, prefix-free.
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer.scripted(executor, [block_event(100)], allow_quiet_end=True)
        engine = ArbitrageEngine()

        with pytest.raises(RuntimeError, match="a scripted consumer cannot attach") as refusal:
            consumer.attach(engine)

        assert type(refusal.value) is RuntimeError, (
            "the slot refuses in its own type; a SessionConsumerError here means "
            "the refusal was routed through the loud-abort mapping"
        )
        assert LOUD_ABORT_PREFIX not in str(refusal.value), (
            "the slot's refusal is prefix-free, not a mapped leaf failure"
        )

        SessionConsumer(executor, allow_quiet_end=True).attach(engine)
        report = await consumer.run()
        assert report["blocks"] == 1, "the refused attach kept the scripted stream"

    async def test_an_unattached_run_reports_the_no_session_refusal(self):
        """A never-attached consumer's ``run`` reports the core's own refusal.

        The slot's ``Pending`` origin guards the attach transition only: an
        unattached ``run`` still delegates, so ADR-093 6.6's no-session text
        survives at the FFI. The refused move restores the slot fresh, so the
        next move delegates again instead of reading as spent.
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer(executor, allow_quiet_end=True)

        with pytest.raises(SessionConsumerError, match="no attached session to run") as refusal:
            await consumer.run()
        assert refusal.value.batch_index == 0

        with pytest.raises(SessionConsumerError, match="no attached session to resume"):
            await consumer.resume()


class TestAbortMappingByKind:
    """The abort mapping classifies by the core's typed kind.

    A core ``ConsumerError`` carries a ``kind``. The three REFUSAL kinds — no
    attached session, a spent claim, the driver refusing the move — map WITHOUT
    the loud-abort prefix, carrying the refusal's own text, because no pipeline
    leaf ran; only ``LeafFailure`` wears the prefix, with the leaf's detail
    under it. The slot's own refusals are a fourth shape entirely: a bare
    ``RuntimeError``, pinned by ``TestConsumerSlotTransitions``.
    """

    async def test_an_unattached_resume_maps_the_no_session_refusal_without_the_prefix(self):
        """NoSession: the no-session text, prefix-free.

        Nothing was attached, so no leaf ran: the refusal keeps its own words
        and the mapped error carries none of the loud-abort prefix.
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer(executor, allow_quiet_end=True)

        with pytest.raises(SessionConsumerError) as refusal:
            await consumer.resume()

        message = str(refusal.value)
        assert LOUD_ABORT_PREFIX not in message, (
            "a refusal maps prefix-free; the prefix here means the mapping "
            "labelled a no-session refusal as a leaf failure"
        )
        assert "no attached session to resume" in message
        assert refusal.value.batch_index == 0
        assert refusal.value.report["batches"] == 0

    async def test_a_claim_spent_resume_maps_the_spent_claim_refusal_without_the_prefix(self):
        """ClaimSpent: the truthful claim text, prefix-free.

        The first ``resume`` is driver-refused and takes the claim with it, so
        the second is refused for the SPENT CLAIM — and says so. It must not
        borrow the no-session words: that message named the wrong transition.
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer(executor, allow_quiet_end=False)
        consumer.attach(ArbitrageEngine())

        with pytest.raises(SessionConsumerError):
            await consumer.resume()
        with pytest.raises(SessionConsumerError) as refusal:
            await consumer.resume()

        message = str(refusal.value)
        assert LOUD_ABORT_PREFIX not in message
        assert message == "the consumer's claim was already used; resume refused", (
            "a spent claim names its own transition"
        )

    async def test_a_driver_refused_resume_maps_the_driver_refusal_without_the_prefix(self):
        """DriverRefusedTheMove: the driver's own refusal text, prefix-free.

        The session's driver refused the move (a never-connected driver cannot
        resume), and the mapped error is that refusal — not a leaf failure.
        """
        executor = offline_executor(Dispatcher.for_block(12_345))
        consumer = SessionConsumer(executor, allow_quiet_end=False)
        consumer.attach(ArbitrageEngine())

        with pytest.raises(SessionConsumerError) as refusal:
            await consumer.resume()

        message = str(refusal.value)
        assert LOUD_ABORT_PREFIX not in message
        assert "cannot resume from detached" in message, "the driver's refusal carries its own text"
        assert "resume requires subscribed" in message
        assert refusal.value.batch_index == 0

    async def test_a_leaf_failure_maps_under_the_loud_abort_prefix(self):
        """LeafFailure: the prefix plus the leaf's detail (the one prefixed kind).

        A batch whose row cannot be assembled (one hop output where the path has
        two) fails the pipeline's leaf; the loop re-raises it in the caller's
        frame under the shared prefix, with the leaf's own words after it.
        """
        engine, path_id = build_two_hop_v2_engine()
        executor = offline_executor(Dispatcher.for_block(0), engine=engine)
        events = [
            block_event(100),
            {
                "kind": "batch",
                "solve_block": 100,
                "timestamp": 0,
                "base_fee_per_gas": None,
                "gas_used": 0,
                "gas_limit": 0,
                "fresh": [(path_id, 1_000, 100, (100,), (1_000,), (1,))],
                "updated": [],
                "expired": [],
                "removed": [],
                "payloads": {},
            },
        ]
        consumer = SessionConsumer.scripted(executor, events, allow_quiet_end=True)

        with pytest.raises(SessionConsumerError) as refusal:
            await consumer.run()

        message = str(refusal.value)
        assert message.startswith(LOUD_ABORT_PREFIX), (
            "a leaf failure keeps the shared loud-abort prefix"
        )
        assert "hop_outputs length (1)" in message
        assert refusal.value.batch_index == 1
