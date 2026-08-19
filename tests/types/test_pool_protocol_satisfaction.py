"""Tests verifying pool protocol declarations are well-formed.

Protocol satisfaction is verified indirectly through integration tests
that construct paths with real pool objects.
"""

from degenbot.types.pool_protocols import (
    PoolSimulation,
)


class TestProtocolDeclarations:
    def test_pool_simulation_declares_required_interface(self):
        """PoolSimulation declares the required pool interface."""
        attrs = PoolSimulation.__protocol_attrs__
        assert "address" in attrs
