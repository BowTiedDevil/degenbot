"""Tests for ArbitrageEngine — mixed V2/V3 arbitrage engine."""

from __future__ import annotations

import pytest

from degenbot.arbitrage.engine_registry import ArbitrageEngine
from degenbot.dispatch import Dispatcher, SessionConsumer, SessionConsumerError
from tests.helpers.session_drive import offline_executor

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
        claim; resuming it must fail loud (the shared loud-abort mapping) rather
        than drive a stream nobody gave it.
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
