"""Tests for the Rust Möbius optimizer Python bindings."""

from fractions import Fraction
from itertools import starmap

import degenbot.degenbot_rs as rs_mobius
from degenbot.arbitrage.optimizers._solver_utils import _compute_mobius_coefficients
from degenbot.arbitrage.optimizers._mobius_math import MobiusFloatHop
from degenbot.arbitrage.optimizers._mobius_math import mobius_solve as py_solve
from degenbot.types.hop_types import ConstantProductHop

# Reserve pairs that are profitable after fees.
# For 2-hop arbitrage: K = γ²·s₁·s₂, M = r₁·r₂. Need K > M.
# With same-product pools, K/M = γ² < 1 (never profitable).
# Need pools with different product constants (asymmetric reserves).
PROFIT_HOPS_2 = [
    rs_mobius.RustHopState(1_000_000.0, 5_000_000.0, 0.003),  # pool A: token1 cheap
    rs_mobius.RustHopState(1_500_000.0, 3_000_000.0, 0.003),  # pool B: token0 cheaper
]

PROFIT_HOPS_3 = [
    rs_mobius.RustHopState(2_000_000.0, 2_200_000.0, 0.003),
    rs_mobius.RustHopState(2_000_000.0, 2_100_000.0, 0.003),
    rs_mobius.RustHopState(2_100_000.0, 2_000_000.0, 0.003),
]

FLAT_HOPS_2 = [
    rs_mobius.RustHopState(1_000_000.0, 1_000_000.0, 0.003),
    rs_mobius.RustHopState(1_000_000.0, 1_000_000.0, 0.003),
]


def _to_cp_hops(rust_hops: list) -> tuple[ConstantProductHop, ...]:
    """Convert RustHopState list to ConstantProductHop tuple for Python utils."""
    return tuple(
        ConstantProductHop(
            reserve_in=int(h.reserve_in),
            reserve_out=int(h.reserve_out),
            fee=Fraction(round(h.fee * 1e6), 1_000_000),
        )
        for h in rust_hops
    )


def _sim_path_float(x: float, hops: list) -> float:
    """Pure Python simulate_path using RustHopState-like objects."""
    amount = x
    for hop in hops:
        r_in = hop.reserve_in
        r_out = hop.reserve_out
        fee = hop.fee
        gamma = 1.0 - fee
        denom = r_in + amount * gamma
        if denom <= 0:
            return 0.0
        amount = amount * gamma * r_out / denom
    return amount


class TestRustHopState:
    def test_creation(self):
        hop = rs_mobius.RustHopState(1_000_000.0, 1_050_000.0, 0.003)
        assert hop.reserve_in == 1_000_000.0
        assert hop.reserve_out == 1_050_000.0
        assert hop.fee == 0.003

    def test_repr(self):
        hop = rs_mobius.RustHopState(100.0, 200.0, 0.003)
        assert "RustHopState" in repr(hop)


class TestRustMobiusSolve:
    def test_two_hop_profitable(self):
        result = rs_mobius.RustArbSolver().solve(PROFIT_HOPS_2)
        assert result.optimal_input > 0
        assert result.profit > 0
        assert result.success

    def test_two_hop_not_profitable(self):
        result = rs_mobius.RustArbSolver().solve(FLAT_HOPS_2)
        assert not result.success

    def test_three_hop_profitable(self):
        result = rs_mobius.RustArbSolver().solve(PROFIT_HOPS_3)
        assert result.optimal_input > 0
        assert result.profit > 0

    def test_max_input_constraint(self):
        rs_mobius.RustArbSolver().solve(PROFIT_HOPS_2)
        result_constrained = rs_mobius.RustArbSolver().solve(PROFIT_HOPS_2, max_input=100.0)
        assert result_constrained.optimal_input <= 100.0

    def test_matches_python(self):
        """Rust solver should produce identical results to Python solver."""

        hops_data = [
            (1_000_000.0, 5_000_000.0, 0.003),
            (1_500_000.0, 3_000_000.0, 0.003),
        ]
        py_hops = list(starmap(MobiusFloatHop, hops_data))
        rust_hops = list(starmap(rs_mobius.RustHopState, hops_data))

        py_x, py_profit, _py_iters = py_solve(py_hops)
        rust_result = rs_mobius.RustArbSolver().solve(rust_hops)

        assert abs(py_x - rust_result.optimal_input) < 1e-6
        assert abs(py_profit - rust_result.profit) < 1e-6


class TestRustSimulatePath:
    def test_basic_simulation(self):
        output = _sim_path_float(1000.0, PROFIT_HOPS_2)
        assert output > 0

    def test_zero_input(self):
        hops = [rs_mobius.RustHopState(1_000_000.0, 1_050_000.0, 0.003)]
        output = _sim_path_float(0.0, hops)
        assert output == 0.0


class TestRustMobiusCoefficients:
    """Test Möbius coefficients via Python implementation (RustMobiusCoefficients removed)."""

    def test_two_hop_profitable(self):
        cp_hops = _to_cp_hops(PROFIT_HOPS_2)
        coeffs = _compute_mobius_coefficients(cp_hops)
        assert coeffs.is_profitable
        assert coeffs.K > 0
        assert coeffs.M > 0
        assert coeffs.N > 0

    def test_optimal_input(self):
        cp_hops = _to_cp_hops(PROFIT_HOPS_2)
        coeffs = _compute_mobius_coefficients(cp_hops)
        x_opt = coeffs.optimal_input()
        assert x_opt > 0

    def test_profit_at(self):
        cp_hops = _to_cp_hops(PROFIT_HOPS_2)
        coeffs = _compute_mobius_coefficients(cp_hops)
        x_opt = coeffs.optimal_input()
        profit = coeffs.profit_at(x_opt)
        assert profit > 0

    def test_path_output(self):
        cp_hops = _to_cp_hops(PROFIT_HOPS_2)
        coeffs = _compute_mobius_coefficients(cp_hops)
        output = coeffs.path_output(1000.0)
        assert output > 0

    def test_not_profitable(self):
        cp_hops = _to_cp_hops(FLAT_HOPS_2)
        coeffs = _compute_mobius_coefficients(cp_hops)
        assert not coeffs.is_profitable
        assert coeffs.optimal_input() == 0.0


class TestRustV3TickRangeHop:
    def test_creation(self):
        v3 = rs_mobius.RustV3TickRangeHop(
            liquidity=1e18,
            sqrt_price_current=1000.0,
            sqrt_price_lower=900.0,
            sqrt_price_upper=1100.0,
            fee=0.003,
            zero_for_one=True,
        )
        assert v3.liquidity == 1e18
        assert v3.sqrt_price_current == 1000.0
        assert v3.fee == 0.003
        assert v3.zero_for_one is True

    def test_alpha_beta(self):
        v3 = rs_mobius.RustV3TickRangeHop(
            liquidity=1e18,
            sqrt_price_current=1000.0,
            sqrt_price_lower=900.0,
            sqrt_price_upper=1100.0,
            fee=0.003,
            zero_for_one=True,
        )
        assert abs(v3.alpha() - 1e18 / 1100.0) < 1.0
        assert abs(v3.beta() - 1e18 * 900.0) < 1.0

    def test_to_hop_state(self):
        v3 = rs_mobius.RustV3TickRangeHop(
            liquidity=1e18,
            sqrt_price_current=1000.0,
            sqrt_price_lower=900.0,
            sqrt_price_upper=1100.0,
            fee=0.003,
            zero_for_one=True,
        )
        hop = v3.to_hop_state()
        assert abs(hop.reserve_in - 1e15) < 1.0
        assert abs(hop.reserve_out - 1e21) < 1.0

    def test_contains_sqrt_price(self):
        v3 = rs_mobius.RustV3TickRangeHop(
            liquidity=1e18,
            sqrt_price_current=1000.0,
            sqrt_price_lower=900.0,
            sqrt_price_upper=1100.0,
            fee=0.003,
            zero_for_one=True,
        )
        assert v3.contains_sqrt_price(1000.0)
        assert v3.contains_sqrt_price(900.0)
        assert v3.contains_sqrt_price(1100.0)
        assert not v3.contains_sqrt_price(899.0)
        assert not v3.contains_sqrt_price(1101.0)
