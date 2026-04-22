"""
Tests for ArbitragePath construction, validation, and hop state extraction.

Uses lightweight fake pool objects to avoid blockchain dependencies.
"""

from fractions import Fraction
from unittest.mock import MagicMock

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.arbitrage.path import ArbitragePath, PathValidationError, SwapVector
from degenbot.arbitrage.path.arbitrage_path import (
    PoolCompatibility,
    _check_pool_compatibility,
    _extract_fee,
    _pool_to_hop_state,
    _v3_virtual_reserves,
)
from degenbot.arbitrage.solver import MobiusSolver
from degenbot.arbitrage.solver.types import (
    ConcentratedLiquidityHopState,
    MobiusHopState,
    MobiusSolveResult,
)
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

from .conftest import _make_token, _make_v2_pool, _make_v3_pool

FEE_03 = Fraction(3, 1000)


class TestSwapVector:
    def test_construction(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        sv = SwapVector(t0, t1, zero_for_one=True)
        assert sv.token_in == t0
        assert sv.token_out == t1
        assert sv.zero_for_one

    def test_equality(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        sv1 = SwapVector(t0, t1, zero_for_one=True)
        sv2 = SwapVector(t0, t1, zero_for_one=True)
        assert sv1 == sv2

    def test_inequality(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        t2 = _make_token("0xt2")
        sv1 = SwapVector(t0, t1, zero_for_one=True)
        sv2 = SwapVector(t0, t2, zero_for_one=True)
        assert sv1 != sv2


class TestPoolCompatibility:
    def test_v2_compatible(self):
        pool = MagicMock(spec=UniswapV2Pool)
        assert _check_pool_compatibility(pool) == PoolCompatibility.COMPATIBLE

    def test_v3_compatible(self):
        pool = MagicMock(spec=UniswapV3Pool)
        assert _check_pool_compatibility(pool) == PoolCompatibility.COMPATIBLE

    def test_v4_compatible(self):
        pool = MagicMock(spec=UniswapV4Pool)
        assert _check_pool_compatibility(pool) == PoolCompatibility.COMPATIBLE

    def test_aerodrome_volatile_compatible(self):
        pool = MagicMock(spec=AerodromeV2Pool)
        pool.stable = False
        assert _check_pool_compatibility(pool) == PoolCompatibility.COMPATIBLE

    def test_aerodrome_stable_incompatible(self):
        pool = MagicMock(spec=AerodromeV2Pool)
        pool.stable = True
        assert _check_pool_compatibility(pool) == PoolCompatibility.INCOMPATIBLE_INVARIANT

    def test_unknown_incompatible(self):
        pool = MagicMock()
        assert _check_pool_compatibility(pool) == PoolCompatibility.INCOMPATIBLE_INVARIANT


class TestFeeExtraction:
    def test_v3_fee(self):
        pool = MagicMock(spec=UniswapV3Pool)
        pool.fee = 3000
        pool.FEE_DENOMINATOR = 1_000_000
        fee = _extract_fee(pool, True)
        assert fee == Fraction(3000, 1_000_000)

    def test_v2_fee_zero_for_one(self):
        pool = MagicMock(spec=UniswapV2Pool)
        pool.fee_token0 = Fraction(3, 1000)
        pool.fee_token1 = Fraction(5, 1000)
        fee = _extract_fee(pool, True)
        assert fee == Fraction(3, 1000)

    def test_v2_fee_one_for_zero(self):
        pool = MagicMock(spec=UniswapV2Pool)
        pool.fee_token0 = Fraction(3, 1000)
        pool.fee_token1 = Fraction(5, 1000)
        fee = _extract_fee(pool, False)
        assert fee == Fraction(5, 1000)


class TestPoolToHopState:
    def test_v2_produces_mobius_hop_state(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = _make_v2_pool(t0, t1)
        hop = _pool_to_hop_state(pool, True)
        assert isinstance(hop, MobiusHopState)
        assert hop.reserve_in == 10**18
        assert hop.reserve_out == 2 * 10**18
        assert hop.fee == FEE_03

    def test_v3_produces_concentrated_hop_state(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = _make_v3_pool(t0, t1)
        hop = _pool_to_hop_state(pool, True)
        assert isinstance(hop, ConcentratedLiquidityHopState)

    def test_v2_direction(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = _make_v2_pool(t0, t1, reserve0=1000, reserve1=2000)
        hop_forward = _pool_to_hop_state(pool, True)
        assert hop_forward.reserve_in == 1000
        assert hop_forward.reserve_out == 2000

        hop_reverse = _pool_to_hop_state(pool, False)
        assert hop_reverse.reserve_in == 2000
        assert hop_reverse.reserve_out == 1000


class TestArbitragePathConstruction:
    def _make_cyclic_v2_pools(self):
        t0 = _make_token("0xtokenA")
        t1 = _make_token("0xtokenB")
        pool0 = _make_v2_pool(t0, t1, reserve0=2_000_000, reserve1=1_000_000_000)
        pool0.address = "0xpool0"
        pool1 = _make_v2_pool(t1, t0, reserve0=1_500_000, reserve1=800_000_000)
        pool1.address = "0xpool1"
        return t0, t1, pool0, pool1

    def test_basic_construction(self):
        t0, t1, pool0, pool1 = self._make_cyclic_v2_pools()
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )
        assert len(path.pools) == 2
        assert path.input_token == t0
        assert len(path.swap_vectors) == 2
        assert path.swap_vectors[0].token_in == t0
        assert path.swap_vectors[0].token_out == t1
        assert path.swap_vectors[0].zero_for_one
        assert path.swap_vectors[1].token_in == t1
        assert path.swap_vectors[1].token_out == t0

    def test_subscribes_to_pools(self):
        t0, _t1, pool0, pool1 = self._make_cyclic_v2_pools()
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )
        pool0.subscribe.assert_called_once_with(path)
        pool1.subscribe.assert_called_once_with(path)

    def test_hop_states_extracted(self):
        t0, _t1, pool0, pool1 = self._make_cyclic_v2_pools()
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )
        assert len(path.hop_states) == 2
        assert isinstance(path.hop_states[0], MobiusHopState)
        assert isinstance(path.hop_states[1], MobiusHopState)

    def test_calculate_profitable(self):
        t0, _t1, pool0, pool1 = self._make_cyclic_v2_pools()
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )
        result = path.calculate()
        assert isinstance(result, MobiusSolveResult)
        assert result.is_profitable

    def test_calculate_updates_last_result(self):
        t0, _t1, pool0, pool1 = self._make_cyclic_v2_pools()
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )
        assert path.last_result is None
        path.calculate()
        assert path.last_result is not None
        assert path.last_result.is_profitable

    def test_max_input_property(self):
        t0, _t1, pool0, pool1 = self._make_cyclic_v2_pools()
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
            max_input=10**18,
        )
        assert path.max_input == 10**18

    def test_max_input_setter(self):
        t0, _t1, pool0, pool1 = self._make_cyclic_v2_pools()
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )
        path.max_input = 10**15
        assert path.max_input == 10**15

    def test_set_solver(self):
        t0, _t1, pool0, pool1 = self._make_cyclic_v2_pools()
        solver1 = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver1,
        )
        solver2 = MobiusSolver()
        path.set_solver(solver2)
        assert path.solver is solver2


class TestArbitragePathValidation:
    def test_rejects_single_pool(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool = _make_v2_pool(t0, t1)
        solver = MobiusSolver()
        with pytest.raises(PathValidationError, match="at least 2"):
            ArbitragePath(pools=[pool], input_token=t0, solver=solver)

    def test_rejects_broken_token_chain(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        t2 = _make_token("0xt2")
        pool0 = _make_v2_pool(t0, t1)
        pool0.address = "0xpool0"
        pool1 = _make_v2_pool(t1, t2)
        pool1.address = "0xpool1"
        solver = MobiusSolver()
        with pytest.raises(PathValidationError, match="not cyclic"):
            ArbitragePath(pools=[pool0, pool1], input_token=t0, solver=solver)

    def test_rejects_input_token_not_in_first_pool(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        t2 = _make_token("0xt2")
        pool0 = _make_v2_pool(t0, t1)
        pool0.address = "0xpool0"
        pool1 = _make_v2_pool(t1, t2)
        pool1.address = "0xpool1"
        solver = MobiusSolver()
        with pytest.raises(PathValidationError):
            ArbitragePath(
                pools=[pool0, pool1],
                input_token=t2,
                solver=solver,
            )

    def test_rejects_incompatible_pool(self):
        t0 = _make_token("0xt0")
        t1 = _make_token("0xt1")
        pool0 = _make_v2_pool(t0, t1)
        pool0.address = "0xpool0"
        pool1 = MagicMock()
        pool1.token0 = t1
        pool1.token1 = t0
        pool1.address = "0xpool1"
        pool1.subscribe = MagicMock()
        solver = MobiusSolver()
        with pytest.raises(PathValidationError, match="not Mobius-compatible"):
            ArbitragePath(
                pools=[pool0, pool1],
                input_token=t0,
                solver=solver,
            )


class TestArbitragePathCalculate:
    def test_cross_validates_vs_arb_solver(self):
        from degenbot.arbitrage.optimizers.solver import (
            ArbSolver,
            ConstantProductHop,
            SolveInput,
        )

        t0, _t1, pool0, pool1 = TestArbitragePathConstruction._make_cyclic_v2_pools(self)
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )
        new_result = path.calculate()

        old_solver = ArbSolver()
        old_result = old_solver.solve(
            SolveInput(
                hops=(
                    ConstantProductHop(
                        reserve_in=2_000_000,
                        reserve_out=1_000_000_000,
                        fee=FEE_03,
                    ),
                    ConstantProductHop(
                        reserve_in=1_500_000,
                        reserve_out=800_000_000,
                        fee=FEE_03,
                    ),
                )
            )
        )

        assert new_result.optimal_input == old_result.optimal_input
        assert new_result.profit == old_result.profit
        assert new_result.is_profitable == old_result.success

    def test_calculate_with_state_override(self):
        t0, _t1, pool0, pool1 = TestArbitragePathConstruction._make_cyclic_v2_pools(self)
        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )

        original_result = path.calculate()

        override_state = MagicMock()
        override_state.reserves_token0 = 5_000_000
        override_state.reserves_token1 = 2_000_000_000

        override_result = path.calculate_with_state_override({pool0: override_state})

        assert override_result.optimal_input != original_result.optimal_input

        assert path.hop_states[0].reserve_in == 2_000_000


class TestV3VirtualReservesIntegerMath:
    def test_price_one_symmetric(self):
        Q96 = 2**96
        x, y = _v3_virtual_reserves(liquidity=10**18, sqrt_price_x96=Q96, zero_for_one=True)
        assert x == 10**18 * Q96
        assert y == 10**18 * Q96

    def test_price_one_reversed(self):
        Q96 = 2**96
        x, y = _v3_virtual_reserves(liquidity=10**18, sqrt_price_x96=Q96, zero_for_one=False)
        assert x == 10**18 * Q96
        assert y == 10**18 * Q96

    def test_price_four(self):
        Q96 = 2**96
        sqrt_p = 2 * Q96
        x, y = _v3_virtual_reserves(liquidity=10**18, sqrt_price_x96=sqrt_p, zero_for_one=True)
        assert x == 10**18 * Q96 * Q96 // (2 * Q96)
        assert y == 10**18 * 2 * Q96

    def test_direction_swap(self):
        Q96 = 2**96
        sqrt_p = 2 * Q96
        x_zfo, y_zfo = _v3_virtual_reserves(10**18, sqrt_p, zero_for_one=True)
        x_ofz, y_ofz = _v3_virtual_reserves(10**18, sqrt_p, zero_for_one=False)
        assert x_zfo == y_ofz
        assert y_zfo == x_ofz

    def test_product_equals_liquidity_squared_scaled(self):
        Q96 = 2**96
        L = 10**18
        sqrt_p = 79228162514264337593543950336
        x, y = _v3_virtual_reserves(L, sqrt_p, zero_for_one=True)
        assert x * y == L * L * Q96 * Q96

    def test_large_liquidity_no_precision_loss(self):
        Q96 = 2**96
        L = 2**100
        sqrt_p = Q96
        x, y = _v3_virtual_reserves(L, sqrt_p, zero_for_one=True)
        assert x == L * Q96
        assert y == L * Q96

    def test_matches_float_for_typical_values(self):
        Q96 = 2**96
        L = 10**18
        sqrt_price_x96 = 79228162514264337593543950336

        x_int, y_int = _v3_virtual_reserves(L, sqrt_price_x96, zero_for_one=True)

        sqrt_price = sqrt_price_x96 / Q96
        x_float = round(L / sqrt_price * Q96)
        y_float = round(L * sqrt_price * Q96)

        assert abs(x_int - x_float) <= 1
        assert abs(y_int - y_float) <= 1


class TestArbitragePathClose:
    def test_close_unsubscribes_from_pools(self):
        t0 = _make_token("0xtokenA")
        t1 = _make_token("0xtokenB")
        pool0 = _make_v2_pool(t0, t1, reserve0=2_000_000, reserve1=1_000_000_000)
        pool0.address = "0xpool0"
        pool1 = _make_v2_pool(t1, t0, reserve0=1_500_000, reserve1=800_000_000)
        pool1.address = "0xpool1"

        pool0.unsubscribe = MagicMock()
        pool1.unsubscribe = MagicMock()

        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )

        subscriber = MagicMock()
        path.subscribe(subscriber)

        path.close()

        pool0.unsubscribe.assert_called_once_with(path)
        pool1.unsubscribe.assert_called_once_with(path)

    def test_close_clears_subscribers(self):
        t0 = _make_token("0xtokenA")
        t1 = _make_token("0xtokenB")
        pool0 = _make_v2_pool(t0, t1)
        pool0.address = "0xpool0"
        pool1 = _make_v2_pool(t1, t0)
        pool1.address = "0xpool1"
        pool0.unsubscribe = MagicMock()
        pool1.unsubscribe = MagicMock()

        solver = MobiusSolver()
        path = ArbitragePath(
            pools=[pool0, pool1],
            input_token=t0,
            solver=solver,
        )

        subscriber = MagicMock()
        path.subscribe(subscriber)
        assert len(path._subscribers) == 1

        path.close()
        assert len(path._subscribers) == 0
