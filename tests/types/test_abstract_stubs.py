"""
Tests verifying that empty abstract stubs have been deepened or removed.
"""

import dataclasses

from degenbot.types.abstract import AbstractSimulationResult


class TestAbstractSimulationResultDeepened:
    def test_abstract_simulation_result_has_fields(self):
        """AbstractSimulationResult is a dataclass with the shared simulation fields."""
        assert dataclasses.is_dataclass(AbstractSimulationResult)
        field_names = {f.name for f in dataclasses.fields(AbstractSimulationResult)}
        assert field_names == {"amount0_delta", "amount1_delta", "initial_state", "final_state"}

    def test_abstract_simulation_result_is_frozen(self):
        """AbstractSimulationResult is frozen to match the existing dataclass contract."""
        assert AbstractSimulationResult.__dataclass_params__.frozen  # type: ignore[attr-defined]


class TestRemovedStubs:
    def test_abstract_pool_update_removed(self):
        """AbstractPoolUpdate has been removed."""
        from degenbot.types import abstract

        assert not hasattr(abstract, "AbstractPoolUpdate")

    def test_abstract_transaction_removed(self):
        """AbstractTransaction has been removed."""
        from degenbot.types import abstract

        assert not hasattr(abstract, "AbstractTransaction")

    def test_abstract_manager_removed(self):
        """AbstractManager has been removed."""
        from degenbot.types import abstract

        assert not hasattr(abstract, "AbstractManager")
