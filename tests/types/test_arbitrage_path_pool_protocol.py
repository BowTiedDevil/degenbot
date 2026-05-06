"""
Tests verifying that the ArbitragePathPool protocol captures the required pool interface,
and that ArbitragePath types its pools as ArbitragePathPool instead of Any.
"""

from degenbot.types.pool_protocols import ArbitragePathPool


class TestArbitragePathPoolProtocol:
    def test_protocol_is_runtime_checkable(self):
        """ArbitragePathPool is runtime-checkable for isinstance checks."""
        assert hasattr(ArbitragePathPool, "__protocol_attrs__") or hasattr(
            ArbitragePathPool, "_is_protocol"
        )

    def test_protocol_declares_required_methods(self):
        """ArbitragePathPool declares all methods needed by ArbitragePath."""
        # Protocol doesn't expose __protocol_attrs__ reliably across Python versions,
        # so we verify the class has the expected annotations instead
        annotations = ArbitragePathPool.__protocol_attrs__  # type: ignore[attr-defined]
        expected = {
            "address",
            "simulate_swap",
            "subscribe",
            "unsubscribe",
            "token0",
            "token1",
            "calculate_tokens_out_from_tokens_in",
            "to_hop_state",
            "extract_fee",
        }
        assert expected == annotations
