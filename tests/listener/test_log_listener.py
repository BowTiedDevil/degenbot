"""Tests for the LogListener dispatch registry."""

from __future__ import annotations

import pytest
from eth_abi import encode as abi_encode
from hexbytes import HexBytes

from degenbot.listener import LogListener
from degenbot.uniswap.log_decoders import V2_SYNC_TOPIC, decode_v2_sync
from tests.fakes.pools import FakeV2Pool


def _noop(_log: dict) -> None:
    """No-op handler for registration tests."""
    return


class TestLogListenerRegistration:
    """Tests for register/unregister behavior."""

    def test_register_adds_handler(self) -> None:
        listener = LogListener()
        listener.register("0xabc", "0xdef", _noop)
        assert listener.handler_count() == 1
        assert listener.key_count() == 1

    def test_register_same_key_multiple_handlers(self) -> None:
        listener = LogListener()

        def h1(_log: dict) -> None:
            return None

        def h2(_log: dict) -> None:
            return None

        listener.register("0xabc", "0xdef", h1)
        listener.register("0xabc", "0xdef", h2)
        assert listener.handler_count() == 2
        assert listener.key_count() == 1

    def test_register_duplicate_handler_is_noop(self) -> None:
        listener = LogListener()
        listener.register("0xabc", "0xdef", _noop)
        listener.register("0xabc", "0xdef", _noop)
        assert listener.handler_count() == 1

    def test_register_non_callable_raises(self) -> None:
        listener = LogListener()
        with pytest.raises(TypeError, match="callable"):
            listener.register("0xabc", "0xdef", "not_a_callable")  # type: ignore[arg-type]

    def test_unregister_removes_handler(self) -> None:
        listener = LogListener()
        listener.register("0xabc", "0xdef", _noop)
        listener.unregister("0xabc", "0xdef", _noop)
        assert listener.handler_count() == 0
        assert listener.key_count() == 0

    def test_unregister_nonexistent_handler_is_noop(self) -> None:
        listener = LogListener()
        listener.unregister("0xabc", "0xdef", _noop)
        assert listener.handler_count() == 0

    def test_unregister_one_of_two_leaves_other(self) -> None:
        listener = LogListener()

        def h1(_log: dict) -> None:
            return None

        def h2(_log: dict) -> None:
            return None

        listener.register("0xabc", "0xdef", h1)
        listener.register("0xabc", "0xdef", h2)
        listener.unregister("0xabc", "0xdef", h1)
        assert listener.handler_count() == 1


class TestLogListenerDispatch:
    """Tests for dispatch behavior."""

    def test_dispatch_calls_matching_handler(self) -> None:
        listener = LogListener()
        received: list[dict] = []

        def handler(log: dict) -> None:
            received.append(log)

        listener.register("0xabc", "0xdef", handler)

        log = {"address": "0xabc", "topics": ["0xdef"]}
        listener.dispatch(log)
        assert received == [log]

    def test_dispatch_no_match_is_silent(self) -> None:
        listener = LogListener()
        listener.register("0xabc", "0xdef", _noop)

        # Different address
        listener.dispatch({"address": "0x999", "topics": ["0xdef"]})
        # Different topic
        listener.dispatch({"address": "0xabc", "topics": ["0x999"]})

    def test_dispatch_calls_multiple_handlers(self) -> None:
        listener = LogListener()
        r1: list[dict] = []
        r2: list[dict] = []

        def h1(log: dict) -> None:
            r1.append(log)

        def h2(log: dict) -> None:
            r2.append(log)

        listener.register("0xabc", "0xdef", h1)
        listener.register("0xabc", "0xdef", h2)

        log = {"address": "0xabc", "topics": ["0xdef"]}
        listener.dispatch(log)
        assert r1 == [log]
        assert r2 == [log]

    def test_dispatch_sequential_order(self) -> None:
        """Handlers are called in insertion order."""
        listener = LogListener()
        order: list[int] = []

        def h1(_log: dict) -> None:
            order.append(1)

        def h2(_log: dict) -> None:
            order.append(2)

        listener.register("0xabc", "0xdef", h1)
        listener.register("0xabc", "0xdef", h2)

        listener.dispatch({"address": "0xabc", "topics": ["0xdef"]})
        assert order == [1, 2]

    def test_dispatch_exception_propagates(self) -> None:
        """Handler exceptions propagate — fail loudly."""
        listener = LogListener()

        def bad_handler(_log: dict) -> None:
            msg = "Handler failed"
            raise RuntimeError(msg)

        listener.register("0xabc", "0xdef", bad_handler)

        with pytest.raises(RuntimeError, match="Handler failed"):
            listener.dispatch({"address": "0xabc", "topics": ["0xdef"]})

    def test_dispatch_missing_address_is_silent(self) -> None:
        listener = LogListener()
        listener.register("0xabc", "0xdef", _noop)
        listener.dispatch({"topics": ["0xdef"]})

    def test_dispatch_missing_topics_is_silent(self) -> None:
        listener = LogListener()
        listener.register("0xabc", "0xdef", _noop)
        listener.dispatch({"address": "0xabc"})

    def test_dispatch_empty_topics_is_silent(self) -> None:
        listener = LogListener()
        listener.register("0xabc", "0xdef", _noop)
        listener.dispatch({"address": "0xabc", "topics": []})

    def test_dispatch_checksummed_address_matches_lowercase(self) -> None:
        """Address normalization: checksummed and lowercase match."""
        listener = LogListener()
        received: list[dict] = []

        def handler(log: dict) -> None:
            received.append(log)

        # Register with checksummed
        listener.register("0xABCdef1234567890aBCDef1234567890ABCdEF12", "0xEventTopic", handler)

        # Dispatch with lowercase
        log = {"address": "0xabcdef1234567890abcdef1234567890abcdef12", "topics": ["0xeventtopic"]}
        listener.dispatch(log)
        assert received == [log]

    def test_dispatch_checksummed_topic_matches_lowercase(self) -> None:
        """Topic normalization: checksummed and lowercase match."""
        listener = LogListener()
        received: list[dict] = []

        def handler(log: dict) -> None:
            received.append(log)

        # Register with mixed case topic
        listener.register("0xabc", "0xAbCdEf", handler)

        # Dispatch with lowercase topic
        log = {"address": "0xabc", "topics": ["0xabcdef"]}
        listener.dispatch(log)
        assert received == [log]

    def test_dispatch_uses_exact_match_only(self) -> None:
        """Different addresses don't match even with same topic."""
        listener = LogListener()
        received: list[dict] = []

        def handler(log: dict) -> None:
            received.append(log)

        listener.register("0xabc", "0xdef", handler)

        # Different address — should not fire
        listener.dispatch({"address": "0x999", "topics": ["0xdef"]})
        assert received == []


class TestLogListenerRepr:
    """Tests for __repr__."""

    def test_repr_empty(self) -> None:
        listener = LogListener()
        assert repr(listener) == "LogListener(keys=0, handlers=0)"

    def test_repr_with_handlers(self) -> None:
        listener = LogListener()
        listener.register("0xabc", "0xdef", _noop)
        assert repr(listener) == "LogListener(keys=1, handlers=1)"


class TestLogListenerWithPoolHandlers:
    """Test the full wiring pattern: pool.LOG_HANDLERS → LogListener."""

    def test_wire_v2_pool_handlers(self) -> None:
        listener = LogListener()
        pool = FakeV2Pool()

        # Wire: for each topic in LOG_HANDLERS, create a handler
        # that decodes and applies
        for topic, decoder in {V2_SYNC_TOPIC: decode_v2_sync}.items():

            def make_handler(d, p):
                def handler(log: dict) -> None:
                    d(log)(p)

                return handler

            addr = pool.address if hasattr(pool, "address") else "0xabc"
            listener.register(addr, topic, make_handler(decoder, pool))

        # Simulate a V2 Sync log
        data = abi_encode(["uint112", "uint112"], [1000, 2000])
        log = {
            "address": "0xabc",
            "topics": [HexBytes(bytes.fromhex(V2_SYNC_TOPIC[2:]))],
            "data": HexBytes(data),
            "blockNumber": 42,
        }

        listener.dispatch(log)
        assert pool.last_update is not None
        assert pool.last_update.reserves_token0 == 1000
        assert pool.last_update.reserves_token1 == 2000
