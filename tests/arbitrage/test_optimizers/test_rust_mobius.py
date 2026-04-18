"""Tests for the Rust Möbius optimizer Python bindings."""


from itertools import starmap

from degenbot._rs import mobius as rs_mobius

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
        result = rs_mobius.py_mobius_solve(PROFIT_HOPS_2)
        assert result.success
        assert result.optimal_input > 0
        assert result.profit > 0
        assert result.iterations == 0

    def test_two_hop_not_profitable(self):
        result = rs_mobius.py_mobius_solve(FLAT_HOPS_2)
        assert not result.success

    def test_three_hop_profitable(self):
        result = rs_mobius.py_mobius_solve(PROFIT_HOPS_3)
        assert result.success
        assert result.optimal_input > 0
        assert result.profit > 0

    def test_max_input_constraint(self):
        rs_mobius.py_mobius_solve(PROFIT_HOPS_2)
        result_constrained = rs_mobius.py_mobius_solve(PROFIT_HOPS_2, max_input=100.0)
        assert result_constrained.optimal_input <= 100.0

    def test_matches_python(self):
        """Rust solver should produce identical results to Python solver."""
        from degenbot.arbitrage.optimizers.mobius import HopState
        from degenbot.arbitrage.optimizers.mobius import mobius_solve as py_solve

        hops_data = [
            (1_000_000.0, 5_000_000.0, 0.003),
            (1_500_000.0, 3_000_000.0, 0.003),
        ]
        py_hops = list(starmap(HopState, hops_data))
        rust_hops = list(starmap(rs_mobius.RustHopState, hops_data))

        py_x, py_profit, py_iters = py_solve(py_hops)
        rust_result = rs_mobius.py_mobius_solve(rust_hops)

        assert abs(py_x - rust_result.optimal_input) < 1e-6
        assert abs(py_profit - rust_result.profit) < 1e-6
        assert py_iters == rust_result.iterations == 0


class TestRustSimulatePath:
    def test_basic_simulation(self):
        output = rs_mobius.py_simulate_path(1000.0, PROFIT_HOPS_2)
        assert output > 0

    def test_zero_input(self):
        hops = [rs_mobius.RustHopState(1_000_000.0, 1_050_000.0, 0.003)]
        output = rs_mobius.py_simulate_path(0.0, hops)
        assert output == 0.0


class TestRustMobiusCoefficients:
    def test_two_hop_profitable(self):
        coeffs = rs_mobius.py_compute_mobius_coefficients(PROFIT_HOPS_2)
        assert coeffs.is_profitable
        assert coeffs.coeff_K > 0
        assert coeffs.coeff_M > 0
        assert coeffs.coeff_N > 0

    def test_optimal_input(self):
        coeffs = rs_mobius.py_compute_mobius_coefficients(PROFIT_HOPS_2)
        x_opt = coeffs.optimal_input()
        assert x_opt > 0

    def test_profit_at(self):
        coeffs = rs_mobius.py_compute_mobius_coefficients(PROFIT_HOPS_2)
        x_opt = coeffs.optimal_input()
        profit = coeffs.profit_at(x_opt)
        assert profit > 0

    def test_path_output(self):
        coeffs = rs_mobius.py_compute_mobius_coefficients(PROFIT_HOPS_2)
        output = coeffs.path_output(1000.0)
        assert output > 0

    def test_not_profitable(self):
        coeffs = rs_mobius.py_compute_mobius_coefficients(FLAT_HOPS_2)
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


class TestRustMobiusOptimizer:
    def test_solve(self):
        optimizer = rs_mobius.RustMobiusOptimizer()
        result = optimizer.solve(PROFIT_HOPS_2)
        assert result.success
        assert result.optimal_input > 0
        assert result.profit > 0

    def test_batch_solve(self):
        optimizer = rs_mobius.RustMobiusOptimizer()
        # 2 paths × 2 hops
        hops_array = [
            1_000_000.0, 5_000_000.0, 0.003,  # path 0, hop 0
            1_500_000.0, 3_000_000.0, 0.003,  # path 0, hop 1
            2_000_000.0, 10_000_000.0, 0.003,  # path 1, hop 0
            3_000_000.0, 6_000_000.0, 0.003,  # path 1, hop 1
        ]
        max_inputs = [float("inf"), float("inf")]
        result = optimizer.solve_batch(hops_array, 2, max_inputs)
        assert "optimal_input" in result
        assert "profit" in result
        assert "is_profitable" in result
        assert len(result["optimal_input"]) == 2

    def test_estimate_v3_final_sqrt_price(self):
        optimizer = rs_mobius.RustMobiusOptimizer()
        v3 = rs_mobius.RustV3TickRangeHop(
            liquidity=1e18,
            sqrt_price_current=1000.0,
            sqrt_price_lower=900.0,
            sqrt_price_upper=1100.0,
            fee=0.003,
            zero_for_one=True,
        )
        # Use a small amount so it stays in range
        final_price = optimizer.estimate_v3_final_sqrt_price(1e10, v3)
        assert final_price < 1000.0  # Price decreases for zero_for_one
        assert final_price > 900.0   # Should stay in range for small input
