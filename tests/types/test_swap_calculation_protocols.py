"""
Tests verifying swap calculation protocol declarations and normalization.
"""

import inspect

from degenbot.balancer.pools import BalancerV2Pool
from degenbot.types.pool_protocols import MultiTokenSwapCalculation, TwoTokenSwapCalculation


class TestSwapCalculationProtocols:
    def test_two_token_swap_calculation_declares_interface(self):
        """TwoTokenSwapCalculation declares the 2-token pool swap interface."""
        attrs = TwoTokenSwapCalculation.__protocol_attrs__  # type: ignore[attr-defined]
        assert "calculate_tokens_out_from_tokens_in" in attrs

    def test_multi_token_swap_calculation_declares_interface(self):
        """MultiTokenSwapCalculation declares the N-token pool swap interface."""
        attrs = MultiTokenSwapCalculation.__protocol_attrs__  # type: ignore[attr-defined]
        assert "calculate_tokens_out_from_tokens_in" in attrs


class TestBalancerParameterOrder:
    def test_balancer_accepts_token_out_before_quantity(self):
        """Balancer's calculate_tokens_out_from_tokens_in has token_out before token_in_quantity."""

        sig = inspect.signature(BalancerV2Pool.calculate_tokens_out_from_tokens_in)
        params = list(sig.parameters.keys())
        # self, token_in, token_out, token_in_quantity, override_state
        assert params.index("token_out") < params.index("token_in_quantity")
