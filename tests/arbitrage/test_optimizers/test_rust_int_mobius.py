"""Tests for the Rust integer Möbius optimizer (EVM-exact)."""

from degenbot._rs import mobius as rs_mobius


class TestRustIntHopState:
    def test_creation_u64(self):
        hop = rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000)
        assert int(hop.reserve_in) == 1_000_000
        assert int(hop.reserve_out) == 5_000_000
        assert hop.fee_numer == 997
        assert hop.fee_denom == 1000

    def test_creation_bigint(self):
        """Accept arbitrary Python ints for uint256-scale reserves."""
        usdc = 100_000_000 * 10**6  # 100M USDC in 6 decimals
        weth = 50_000 * 10**18  # 50K WETH in 18 decimals
        hop = rs_mobius.RustIntHopState(usdc, weth, 997, 1000)
        assert int(hop.reserve_in) == usdc
        assert int(hop.reserve_out) == weth


class TestIntMobiusSolve:
    def test_profitable_2hop(self):
        hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        result = rs_mobius.py_int_mobius_solve(hops)
        assert result.success
        assert int(result.optimal_input) > 0
        assert int(result.profit) > 0

    def test_not_profitable_same_product(self):
        """Same-product V2-V2 is never profitable after fees (K/M = γ² < 1)."""
        hops = [
            rs_mobius.RustIntHopState(100_000, 50, 997, 1000),
            rs_mobius.RustIntHopState(50, 100_000, 997, 1000),
        ]
        result = rs_mobius.py_int_mobius_solve(hops)
        assert not result.success

    def test_profitable_3hop(self):
        hops = [
            rs_mobius.RustIntHopState(2_000_000, 2_100_000, 997, 1000),
            rs_mobius.RustIntHopState(2_000_000, 2_050_000, 997, 1000),
            rs_mobius.RustIntHopState(2_050_000, 2_000_000, 997, 1000),
        ]
        result = rs_mobius.py_int_mobius_solve(hops)
        assert result.success
        assert int(result.profit) > 0

    def test_full_scale_reserves(self):
        """Full uint256-scale reserves (USDC 6-dec, WETH 18-dec)."""
        r0_a = 100_000_000 * 10**6   # 100M USDC
        r1_a = 60_000 * 10**18       # 60K WETH
        r1_b = 40_000 * 10**18       # 40K WETH
        r0_b = 80_000_000 * 10**6    # 80M USDC

        hops = [
            rs_mobius.RustIntHopState(r0_a, r1_a, 997, 1000),
            rs_mobius.RustIntHopState(r1_b, r0_b, 997, 1000),
        ]
        result = rs_mobius.py_int_mobius_solve(hops)
        assert result.success
        assert int(result.profit) > 0

    def test_evm_profit_matches_float(self):
        """Integer EVM-exact profit should match float profit within 1 wei."""
        hops_int = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        hops_float = [
            rs_mobius.RustHopState(1_000_000.0, 5_000_000.0, 0.003),
            rs_mobius.RustHopState(1_500_000.0, 3_000_000.0, 0.003),
        ]

        int_result = rs_mobius.py_int_mobius_solve(hops_int)
        float_result = rs_mobius.py_mobius_solve(hops_float)

        assert abs(int(int_result.profit) - float_result.profit) < 2.0

    def test_evm_simulation_verified(self):
        """EVM simulation at solver's x_opt should give matching profit."""
        hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        result = rs_mobius.py_int_mobius_solve(hops)
        x_opt = int(result.optimal_input)
        output = int(rs_mobius.py_int_simulate_path(x_opt, hops))
        evm_profit = output - x_opt
        assert evm_profit == int(result.profit)


class TestIntSimulatePath:
    def test_basic(self):
        hops = [
            rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000),
            rs_mobius.RustIntHopState(1_500_000, 3_000_000, 997, 1000),
        ]
        output = rs_mobius.py_int_simulate_path(1000, hops)
        assert int(output) > 0

    def test_zero_input(self):
        hops = [rs_mobius.RustIntHopState(1_000_000, 5_000_000, 997, 1000)]
        output = rs_mobius.py_int_simulate_path(0, hops)
        assert int(output) == 0

    def test_full_scale(self):
        """EVM simulation with full uint256-scale reserves."""
        r0 = 100_000_000 * 10**6
        r1 = 50_000 * 10**18
        hops = [rs_mobius.RustIntHopState(r0, r1, 997, 1000)]
        output = rs_mobius.py_int_simulate_path(1_000_000_000_000, hops)
        assert int(output) > 0
