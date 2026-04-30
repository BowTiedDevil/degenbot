"""
Tests for ArbitragePath.build_swap_amounts.

Validates that swap amount construction produces correct pool-specific
swap amounts for V2 pools.
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage.optimizers.hop_types import SolveResult, SolverMethod
from degenbot.arbitrage.optimizers.solver import MobiusSolver
from degenbot.arbitrage.path import ArbitragePath, PathValidationError
from degenbot.arbitrage.types import UniswapV2PoolSwapAmounts

from .conftest import FakeToken, FakeUniswapV2Pool, _make_v2_pool

FEE_03 = Fraction(3, 1000)


@pytest.fixture
def token_a():
    return FakeToken("0xtokenA")


@pytest.fixture
def token_b():
    return FakeToken("0xtokenB")


@pytest.fixture
def token_c():
    return FakeToken("0xtokenC")


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
) -> FakeUniswapV2Pool:
    pool = _make_v2_pool(token0, token1, reserve0=reserve0, reserve1=reserve1, fee=fee)
    pool.address = address

    def _swap(token_in, token_in_quantity, override_state=None):
        if token_in == pool.token0:
            return _constant_product_swap(reserve0, reserve1, token_in_quantity, fee)
        return _constant_product_swap(reserve1, reserve0, token_in_quantity, fee)

    pool.calculate_tokens_out_from_tokens_in = _swap
    return pool


class TestBuildSwapAmountsV2V2:
    def _make_path(self, token_a, token_b):
        pool0 = _make_pool_with_swap(
            token_a, token_b, reserve0=2_000_000, reserve1=1_000_000_000, address="0xpool0"
        )
        pool1 = _make_pool_with_swap(
            token_b, token_a, reserve0=1_500_000, reserve1=800_000_000, address="0xpool1"
        )
        solver = MobiusSolver()
        return ArbitragePath(
            pools=[pool0, pool1],
            input_token=token_a,
            solver=solver,
        )

    def test_build_swap_amounts_returns_result(self, token_a, token_b):
        path = self._make_path(token_a, token_b)
        result = path.calculate()

        arb_result = path.build_swap_amounts(result)
        assert arb_result.input_amount > 0
        assert arb_result.profit_amount > 0
        assert len(arb_result.swap_amounts) == 2

    def test_swap_amounts_are_v2_type(self, token_a, token_b):
        path = self._make_path(token_a, token_b)
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        for sa in arb_result.swap_amounts:
            assert isinstance(sa, UniswapV2PoolSwapAmounts)

    def test_first_swap_input_matches_optimal(self, token_a, token_b):
        path = self._make_path(token_a, token_b)
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        first_swap = arb_result.swap_amounts[0]
        assert max(first_swap.amounts_in) == result.optimal_input

    def test_profit_matches_swap_amounts(self, token_a, token_b):
        path = self._make_path(token_a, token_b)
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        first_input = max(arb_result.swap_amounts[0].amounts_in)
        last_output = max(arb_result.swap_amounts[-1].amounts_out)
        assert last_output - first_input == arb_result.profit_amount

    def test_direction_encoding(self, token_a, token_b):
        path = self._make_path(token_a, token_b)
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        first_swap = arb_result.swap_amounts[0]
        assert first_swap.amounts_in[0] > 0
        assert first_swap.amounts_in[1] == 0
        assert first_swap.amounts_out[0] == 0
        assert first_swap.amounts_out[1] > 0

    def test_rejects_unprofitable_result(self, token_a, token_b):
        path = self._make_path(token_a, token_b)
        unprofitable = SolveResult(
            optimal_input=0,
            profit=0,
            iterations=0,
            method=SolverMethod.MOBIUS,
        )
        with pytest.raises(PathValidationError, match="output of zero"):
            path.build_swap_amounts(unprofitable)

    def test_pool_addresses_set(self, token_a, token_b):
        path = self._make_path(token_a, token_b)
        result = path.calculate()
        arb_result = path.build_swap_amounts(result)

        assert arb_result.swap_amounts[0].pool == "0xpool0"
        assert arb_result.swap_amounts[1].pool == "0xpool1"


class TestBuildSwapAmountsThreeHop:
    def test_three_hop_v2(self, token_a, token_b, token_c):
        # Reserves chosen so the three-hop path is profitable:
        # pool0: tokenA -> tokenB (cheap to buy tokenB)
        # pool1: tokenB -> tokenC (cheap to buy tokenC)
        # pool2: tokenC -> tokenA (expensive to sell tokenC)
        pool0 = _make_pool_with_swap(
            token_a, token_b, reserve0=10_000_000, reserve1=20_000_000, address="0xp0"
        )
        pool1 = _make_pool_with_swap(
            token_b, token_c, reserve0=20_000_000, reserve1=30_000_000, address="0xp1"
        )
        pool2 = _make_pool_with_swap(
            token_c, token_a, reserve0=30_000_000, reserve1=40_000_000, address="0xp2"
        )

        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1, pool2],
            input_token=token_a,
            solver=solver,
        )

        result = path.calculate()
        assert result.profit > 0, "Three-hop path should be profitable with these reserves"

        arb_result = path.build_swap_amounts(result)
        assert len(arb_result.swap_amounts) == 3
        assert arb_result.profit_amount > 0

        for sa in arb_result.swap_amounts:
            assert isinstance(sa, UniswapV2PoolSwapAmounts)
