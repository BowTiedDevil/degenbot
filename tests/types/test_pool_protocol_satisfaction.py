"""
Tests verifying pool protocol declarations are well-formed.

Protocol satisfaction is verified indirectly through integration tests
that construct ArbitragePath with real pool objects.
"""

from degenbot.types.pool_protocols import (
    ArbitrageCapablePool,
    ArbitragePathPool,
    PoolSimulation,
)


class TestProtocolDeclarations:
    def test_pool_simulation_declares_required_interface(self):
        """PoolSimulation declares the required pool interface."""
        attrs = PoolSimulation.__protocol_attrs__  # type: ignore[attr-defined]
        assert "address" in attrs
        assert "simulate_swap" in attrs
        assert "subscribe" in attrs
        assert "unsubscribe" in attrs

    def test_arbitrage_capable_pool_extends_pool_simulation(self):
        """ArbitrageCapablePool extends PoolSimulation with hop/fee methods."""
        attrs = ArbitrageCapablePool.__protocol_attrs__  # type: ignore[attr-defined]
        # From PoolSimulation
        assert "address" in attrs
        assert "simulate_swap" in attrs
        # Additional
        assert "to_hop_state" in attrs
        assert "extract_fee" in attrs

    def test_arbitrage_path_pool_declares_required_interface(self):
        """ArbitragePathPool declares all methods needed by ArbitragePath."""
        attrs = ArbitragePathPool.__protocol_attrs__  # type: ignore[attr-defined]
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
            "build_swap_amount",
        }
        assert expected == attrs
