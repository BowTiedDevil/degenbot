"""Tests verifying that empty abstract stubs have been deepened."""

import dataclasses

import degenbot.types.abstract


class TestAbstractSimulationResultDeepened:
    def test_abstract_simulation_result_has_fields(self):
        """AbstractSimulationResult is a dataclass with the shared simulation fields."""
        assert dataclasses.is_dataclass(degenbot.types.abstract.AbstractSimulationResult)
        field_names = {
            f.name for f in dataclasses.fields(degenbot.types.abstract.AbstractSimulationResult)
        }
        assert field_names == {"amount0_delta", "amount1_delta", "initial_state", "final_state"}
