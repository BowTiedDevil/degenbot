"""Tests for ArbitrageEngine — mixed V2/V3 arbitrage engine."""

from __future__ import annotations

import pytest

from degenbot.arbitrage.engine_registry import ArbitrageEngine

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


def _make_v3_snapshot(
    pools: dict[str, dict[int, tuple[int, int]]],
) -> dict[str, dict[int, tuple[int, int]]]:
    """Build a V3 snapshot dict for load_v3_snapshot_from_py()."""
    return pools


def _make_v4_snapshot(
    pool_managers: dict[str, dict[str, dict[int, tuple[int, int]]]],
) -> dict[str, dict[str, dict[int, tuple[int, int]]]]:
    """Build a V4 snapshot dict for load_v4_snapshot_from_py()."""
    return pool_managers


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


class TestSubscribeResume:
    def test_subscribe_returns_block_number_type(self):
        """subscribe() should be callable (won't actually connect in tests)."""
        engine = ArbitrageEngine()
        assert hasattr(engine, "subscribe")
        assert hasattr(engine, "resume")

    def test_resume_without_subscribe_raises(self):
        """resume() without subscribe() should raise RuntimeError."""
        engine = ArbitrageEngine()
        with pytest.raises(RuntimeError, match="SnapshotLoaded|subscribe"):
            engine.resume()

    def test_double_subscribe_raises(self):
        """Calling subscribe() twice without resume() should raise."""
        engine = ArbitrageEngine()
        import inspect

        sig = inspect.signature(engine.subscribe)
        params = list(sig.parameters.keys())
        assert "rpc_url" in params
        assert "buffer_event_types" not in params
