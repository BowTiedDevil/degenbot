"""
Tests for solver types, protocol compliance, and basic construction.

Validates that all hop state types are constructable, frozen, and have
correct property behavior. Also validates MobiusSolveResult construction
and SolverProtocol compliance checks.
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.solver.protocol import SolverProtocol
from degenbot.arbitrage.solver.types import (
    ConcentratedLiquidityHopState,
    MobiusHopState,
    MobiusSolveResult,
    SolverMethod,
    TickRangeState,
)


class TestMobiusHopState:
    def test_construction(self):
        hop = MobiusHopState(
            reserve_in=2_000_000_000_000,
            reserve_out=1_000_000_000_000_000_000,
            fee=Fraction(3, 1000),
        )
        assert hop.reserve_in == 2_000_000_000_000
        assert hop.reserve_out == 1_000_000_000_000_000_000
        assert hop.fee == Fraction(3, 1000)

    def test_gamma(self):
        hop = MobiusHopState(
            reserve_in=1000,
            reserve_out=2000,
            fee=Fraction(3, 1000),
        )
        assert hop.gamma == pytest.approx(0.997)

    def test_frozen(self):
        hop = MobiusHopState(
            reserve_in=1000,
            reserve_out=2000,
            fee=Fraction(3, 1000),
        )
        with pytest.raises(AttributeError):
            hop.reserve_in = 500

    def test_zero_fee(self):
        hop = MobiusHopState(
            reserve_in=1000,
            reserve_out=2000,
            fee=Fraction(0),
        )
        assert hop.gamma == 1.0

    def test_large_reserves(self):
        hop = MobiusHopState(
            reserve_in=2**128,
            reserve_out=2**128,
            fee=Fraction(3, 1000),
        )
        assert hop.reserve_in == 2**128


class TestTickRangeState:
    def test_construction(self):
        tr = TickRangeState(
            tick_lower=-100,
            tick_upper=100,
            liquidity=10**18,
            sqrt_price_lower=79228162514264337593543950336,
            sqrt_price_upper=80000000000000000000000000000,
        )
        assert tr.tick_lower == -100
        assert tr.tick_upper == 100
        assert tr.liquidity == 10**18

    def test_frozen(self):
        tr = TickRangeState(
            tick_lower=-100,
            tick_upper=100,
            liquidity=10**18,
            sqrt_price_lower=0,
            sqrt_price_upper=0,
        )
        with pytest.raises(AttributeError):
            tr.liquidity = 0


class TestConcentratedLiquidityHopState:
    def test_inherits_mobius_hop_state(self):
        hop = ConcentratedLiquidityHopState(
            reserve_in=1000,
            reserve_out=2000,
            fee=Fraction(3, 1000),
            liquidity=10**18,
            sqrt_price=79228162514264337593543950336,
            tick_lower=-100,
            tick_upper=100,
        )
        assert isinstance(hop, MobiusHopState)
        assert hop.gamma == pytest.approx(0.997)

    def test_single_range_no_tick_ranges(self):
        hop = ConcentratedLiquidityHopState(
            reserve_in=1000,
            reserve_out=2000,
            fee=Fraction(3, 1000),
            liquidity=10**18,
            sqrt_price=79228162514264337593543950336,
            tick_lower=-100,
            tick_upper=100,
        )
        assert not hop.has_multi_range

    def test_multi_range(self):
        ranges = (
            TickRangeState(-200, -100, 10**18, 0, 0),
            TickRangeState(-100, 100, 10**18, 0, 0),
        )
        hop = ConcentratedLiquidityHopState(
            reserve_in=1000,
            reserve_out=2000,
            fee=Fraction(3, 1000),
            liquidity=10**18,
            sqrt_price=79228162514264337593543950336,
            tick_lower=-100,
            tick_upper=100,
            tick_ranges=ranges,
            current_range_index=1,
        )
        assert hop.has_multi_range
        assert hop.current_range_index == 1

    def test_frozen(self):
        hop = ConcentratedLiquidityHopState(
            reserve_in=1000,
            reserve_out=2000,
            fee=Fraction(3, 1000),
            liquidity=10**18,
            sqrt_price=0,
            tick_lower=-100,
            tick_upper=100,
        )
        with pytest.raises(AttributeError):
            hop.liquidity = 0


class TestMobiusSolveResult:
    def test_profitable_result(self):
        result = MobiusSolveResult(
            optimal_input=10**18,
            profit=10**15,
            is_profitable=True,
            method=SolverMethod.MOBIUS,
        )
        assert result.is_profitable
        assert result.iterations == 0
        assert result.error is None

    def test_unprofitable_result(self):
        result = MobiusSolveResult(
            optimal_input=0,
            profit=0,
            is_profitable=False,
            method=SolverMethod.MOBIUS,
            error="K/M <= 1",
        )
        assert not result.is_profitable
        assert result.error == "K/M <= 1"

    def test_piecewise_result(self):
        result = MobiusSolveResult(
            optimal_input=10**17,
            profit=10**14,
            is_profitable=True,
            method=SolverMethod.PIECEWISE_MOBIUS,
            iterations=25,
        )
        assert result.method == SolverMethod.PIECEWISE_MOBIUS
        assert result.iterations == 25

    def test_frozen(self):
        result = MobiusSolveResult(
            optimal_input=0,
            profit=0,
            is_profitable=False,
            method=SolverMethod.MOBIUS,
        )
        with pytest.raises(AttributeError):
            result.profit = 100


class TestSolverProtocol:
    def test_cannot_instantiate(self):
        with pytest.raises(TypeError):
            SolverProtocol()

    def test_subclass_must_implement_solve(self):
        class IncompleteSolver(SolverProtocol):
            def supports(self, hops):
                return True

        with pytest.raises(TypeError):
            IncompleteSolver()

    def test_subclass_must_implement_supports(self):
        class IncompleteSolver(SolverProtocol):
            def solve(self, hops, max_input=None):
                pass

        with pytest.raises(TypeError):
            IncompleteSolver()

    def test_complete_subclass_instantiates(self):
        class FakeSolver(SolverProtocol):
            def solve(self, hops, max_input=None):
                return MobiusSolveResult(
                    optimal_input=0,
                    profit=0,
                    is_profitable=False,
                    method=SolverMethod.MOBIUS,
                )

            def supports(self, hops):
                return True

        solver = FakeSolver()
        assert solver.supports([])
        result = solver.solve([])
        assert not result.is_profitable
