"""
Tests for ArbitragePath.build_swap_amounts.

Validates that swap amount construction produces correct pool-specific
swap amounts for V2 pools.
"""

from fractions import Fraction
from unittest.mock import MagicMock

import pytest

from degenbot.arbitrage.path import ArbitragePath, PathValidationError
from degenbot.arbitrage.solver import MobiusSolver
from degenbot.arbitrage.solver.types import MobiusSolveResult, SolverMethod
from degenbot.arbitrage.types import UniswapV2PoolSwapAmounts

from .conftest import FakeToken, _make_v2_pool

FEE_03 = Fraction(3, 1000)


def _constant_product_swap(
    reserve_in: int,
    reserve_out: int,
    amount_in: int,
    fee: Fraction,
) -> int:
    amount_in_with_fee = amount_in * (fee.denominator - fee.numerator)
    denominator = reserve_in * fee.denominator + amount_in_with_fee
    return reserve_out * amount_in_with_fee // denominator


def _make_pool_with_swap(
    token0,
    token1,
    reserve0: int,
    reserve1: int,
    fee: Fraction = FEE_03,
    address: str = "0xpool",
) -> MagicMock:
    pool = _make_v2_pool(token0, token1, reserve0=reserve0, reserve1=reserve1, fee=fee)
    pool.address = address

    def _swap(token_in, token_in_quantity, override_state=None):
        if token_in == pool.token0:
            return _constant_product_swap(reserve0, reserve1, token_in_quantity, fee)
        return _constant_product_swap(reserve1, reserve0, token_in_quantity, fee)

    pool.calculate_tokens_out_from_tokens_in = _swap
    return pool


class TestBuildSwapAmountsV2V2:
    def _make_path(self):
        t0 = FakeToken("0xtokenA")
        t1 = FakeToken("0xtokenB")
        pool0 = _make_pool_with_swap(
            t0, t1, reserve0=2_000_000, reserve1=1_000_000_000, address="0xpool0"
        )
        pool1 = _make_pool_with_swap(
            t1, t0, reserve0=1_500_000, reserve1=800_000_000, address="0xpool1"
        )
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )
        return path

    def test_build_swap_amounts_returns_result(self):
        path = self._make_path()
        result = path.calculate()
        assert result.is_profitable

        arb_result = path.build_swap_amounts(result)
        assert arb_result.input_amount > 0
        assert arb_result.profit_amount > 0
        assert len(arb_result.swap_amounts) == 2

    def test_swap_amounts_are_v2_type(self):
        path = self._make_path()
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        for sa in arb_result.swap_amounts:
            assert isinstance(sa, UniswapV2PoolSwapAmounts)

    def test_first_swap_input_matches_optimal(self):
        path = self._make_path()
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        first_swap = arb_result.swap_amounts[0]
        assert max(first_swap.amounts_in) == result.optimal_input

    def test_profit_matches_swap_amounts(self):
        path = self._make_path()
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        first_input = max(arb_result.swap_amounts[0].amounts_in)
        last_output = max(arb_result.swap_amounts[-1].amounts_out)
        assert last_output - first_input == arb_result.profit_amount

    def test_direction_encoding(self):
        path = self._make_path()
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        first_swap = arb_result.swap_amounts[0]
        assert first_swap.amounts_in[0] > 0
        assert first_swap.amounts_in[1] == 0
        assert first_swap.amounts_out[0] == 0
        assert first_swap.amounts_out[1] > 0

    def test_rejects_unprofitable_result(self):
        path = self._make_path()
        unprofitable = MobiusSolveResult(
            optimal_input=0,
            profit=0,
            is_profitable=False,
            method=SolverMethod.MOBIUS,
        )
        with pytest.raises(PathValidationError, match="unprofitable"):
            path.build_swap_amounts(unprofitable)

    def test_pool_addresses_set(self):
        path = self._make_path()
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        assert arb_result.swap_amounts[0].pool == "0xpool0"
        assert arb_result.swap_amounts[1].pool == "0xpool1"


class TestBuildSwapAmountsThreeHop:
    def test_three_hop_v2(self):
        t0 = FakeToken("0xtokenA")
        t1 = FakeToken("0xtokenB")
        t2 = FakeToken("0xtokenC")

        pool0 = _make_pool_with_swap(t0, t1, reserve0=10**18, reserve1=2 * 10**18, address="0xp0")
        pool1 = _make_pool_with_swap(t1, t2, reserve0=3 * 10**18, reserve1=10**18, address="0xp1")
        pool2 = _make_pool_with_swap(
            t2, t0, reserve0=2 * 10**18, reserve1=4 * 10**18, address="0xp2"
        )

        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1, pool2],
            input_token=t0,
            solver=solver,
        )

        result = path.calculate()
        if not result.is_profitable:
            pytest.skip("Three-hop path not profitable with these reserves")

        arb_result = path.build_swap_amounts(result)
        assert len(arb_result.swap_amounts) == 3
        assert arb_result.profit_amount > 0

        for sa in arb_result.swap_amounts:
            assert isinstance(sa, UniswapV2PoolSwapAmounts)
