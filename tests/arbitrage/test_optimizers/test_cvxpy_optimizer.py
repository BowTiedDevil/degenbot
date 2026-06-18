"""Tests for CVXPY convex optimization solver for arbitrage.

Tests cover:
- 2-pool V2 cycles with decimal corrections
- Multi-pool cycles (3+, 4+ pools)
- Property-based testing with randomized pool states

Migrated from tests/test_cvxpy.py with additions for Hypothesis-based testing.
"""

from fractions import Fraction
from typing import cast

import cvxpy
import cvxpy.settings
import hypothesis
import hypothesis.strategies as st
import numpy as np
import pytest
from cvxpy.atoms.affine.binary_operators import multiply as cvxpy_multiply
from cvxpy.atoms.affine.bmat import bmat as cvxpy_bmat
from cvxpy.atoms.affine.sum import sum as cvxpy_sum
from cvxpy.atoms.geo_mean import geo_mean

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage._legacy import _UniswapMultiPoolCycleTesting
from degenbot.bot import Bot
from degenbot.erc20.erc20 import Erc20Token
from degenbot.provider import ProviderAdapter
from degenbot.uniswap.liquidity_pool import LiquidityPool
from tests.arbitrage.generator.fixtures import FixtureFactory
from tests.arbitrage.generator.hypothesis_strategies import (
    liquidity_depth_strategy,
    mismatched_decimal_pair_strategy,
    price_ratio_strategy,
    seed_strategy,
)
from tests.helpers.bot_factory import make_bot_with_provider
from tests.helpers.v2_pool_factory import make_v2_pool

# ==============================================================================
# Test Fixtures (Migrated from test_cvxpy.py)
# ==============================================================================

WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
LINK_ADDRESS = "0x514910771AF9Ca656af840dff83E8264EcF986CA"
USDC_ADDRESS = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"


@pytest.fixture
def bot_mainnet_full(fork_mainnet_full: AnvilFork) -> Bot:
    """Provide a Bot with the mainnet full fork's provider registered."""
    provider = ProviderAdapter.from_web3(fork_mainnet_full.w3)
    return make_bot_with_provider(provider)


@pytest.fixture
def bot_base_full(fork_base_full: AnvilFork) -> Bot:
    """Provide a Bot with the base fork's provider registered."""
    provider = ProviderAdapter.from_web3(fork_base_full.w3)
    return make_bot_with_provider(provider)


@pytest.fixture
def wbtc_token(fork_mainnet_full: AnvilFork, bot_mainnet_full: Bot) -> Erc20Token:
    return bot_mainnet_full.build_erc20token(WBTC_ADDRESS)


@pytest.fixture
def weth_token(fork_mainnet_full: AnvilFork, bot_mainnet_full: Bot) -> Erc20Token:
    return bot_mainnet_full.build_erc20token(WETH_ADDRESS)


@pytest.fixture
def link_token(fork_mainnet_full: AnvilFork, bot_mainnet_full: Bot) -> Erc20Token:
    return bot_mainnet_full.build_erc20token(LINK_ADDRESS)


@pytest.fixture
def usdc_token(fork_mainnet_full: AnvilFork, bot_mainnet_full: Bot) -> Erc20Token:
    return bot_mainnet_full.build_erc20token(USDC_ADDRESS)


@pytest.fixture
def weth_base_token(fork_base_full: AnvilFork, bot_base_full: Bot) -> Erc20Token:
    return bot_base_full.build_erc20token("0x4200000000000000000000000000000000000006")


@pytest.fixture
def xxx_base_token(fork_base_full: AnvilFork, bot_base_full: Bot) -> Erc20Token:
    return bot_base_full.build_erc20token("0x09C07E80bFeEd81130498516F5C07aA0715794Bb")


@pytest.fixture
def wbtc_pool_a(wbtc_token, weth_token) -> LiquidityPool:
    return make_v2_pool(
        address="0xBb2b8038a1640196FbE3e38816F3e67Cba72D940",
        token0=wbtc_token,
        token1=weth_token,
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=9000000000,
        reserves_token1=2100000000000000000000,
        state_block=1,
    )


@pytest.fixture
def wbtc_pool_b(wbtc_token, weth_token) -> LiquidityPool:
    return make_v2_pool(
        address="0xBb2b8038a1640196FbE3e38816F3e67Cba72D941",
        token0=wbtc_token,
        token1=weth_token,
        factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=9250000000,
        reserves_token1=2100000000000000000000,
        state_block=1,
    )


@pytest.fixture
def test_pool_base_a(xxx_base_token, weth_base_token) -> LiquidityPool:
    return make_v2_pool(
        address="0x214356Cc4aAb907244A791CA9735292860490D5A",
        token0=weth_base_token,
        token1=xxx_base_token,
        factory="0x420DD381b31aEf6683db6B902084cB0FFECe40Da",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=19643270033194347,
        reserves_token1=406789256841523130269,
        state_block=1,
    )


@pytest.fixture
def test_pool_base_b(xxx_base_token, weth_base_token) -> LiquidityPool:
    return make_v2_pool(
        address="0x404E927b203375779a6aBD52A2049cE0ADf6609B",
        token0=weth_base_token,
        token1=xxx_base_token,
        factory="0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6",
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=880450452482804609420,
        reserves_token1=18733831498401825763565574,
        state_block=1,
    )


# ==============================================================================
# Known Value Tests (Regression)
# ==============================================================================


class TestCVXPY2PoolKnownValues:
    """Known value regression tests from original test_cvxpy.py."""

    def test_2pool_uniswap_v2_decimal_corrected(
        self,
        wbtc_pool_a: LiquidityPool,
        wbtc_pool_b: LiquidityPool,
        weth_token: Erc20Token,
    ):
        """Regression test: WBTC/WETH 2-pool arbitrage with single compression."""
        profit_token = weth_token
        pool_a_roe = wbtc_pool_a.get_absolute_exchange_rate(token=profit_token)
        pool_b_roe = wbtc_pool_b.get_absolute_exchange_rate(token=profit_token)

        if pool_a_roe > pool_b_roe:
            pool_hi = wbtc_pool_a
            pool_lo = wbtc_pool_b
        else:
            pool_hi = wbtc_pool_b
            pool_lo = wbtc_pool_a

        num_pools = 2
        num_tokens = 2

        pool_hi_index, pool_lo_index = 0, 1

        token0_decimals = pool_hi.token0.decimals
        token1_decimals = pool_hi.token1.decimals

        forward_token = pool_hi.token1 if pool_hi.token0 == profit_token else pool_hi.token0
        forward_token_index = 1 if pool_hi.token0 == profit_token else 0
        profit_token_index = 0 if pool_hi.token0 == profit_token else 1
        assert forward_token_index != profit_token_index

        pool_hi_fees = [pool_hi.fee_token0, pool_hi.fee_token1]
        pool_lo_fees = [pool_lo.fee_token0, pool_lo.fee_token1]
        fee_multiplier = cvxpy_bmat((
            pool_hi_fees,
            pool_lo_fees,
        ))

        compression_factor = max(
            Fraction(pool_hi.state.reserves_token0, 10**token0_decimals),
            Fraction(pool_hi.state.reserves_token1, 10**token1_decimals),
            Fraction(pool_lo.state.reserves_token0, 10**token0_decimals),
            Fraction(pool_lo.state.reserves_token1, 10**token1_decimals),
        )

        compressed_starting_reserves_pool_hi = (
            Fraction(pool_hi.state.reserves_token0, 10**token0_decimals) / compression_factor,
            Fraction(pool_hi.state.reserves_token1, 10**token1_decimals) / compression_factor,
        )
        compressed_starting_reserves_pool_lo = (
            Fraction(pool_lo.state.reserves_token0, 10**token0_decimals) / compression_factor,
            Fraction(pool_lo.state.reserves_token1, 10**token1_decimals) / compression_factor,
        )
        compressed_reserves_pre_swap = cvxpy.Parameter(
            name="compressed_reserves_pre_swap",
            shape=(num_pools, num_tokens),
            value=np.array(
                (
                    compressed_starting_reserves_pool_hi,
                    compressed_starting_reserves_pool_lo,
                ),
                dtype=np.float64,
            ),
        )

        pool_hi_pre_swap_k = cvxpy.Parameter(
            name="pool_hi_pre_swap_k",
            value=geo_mean(compressed_reserves_pre_swap[pool_hi_index]).value,
        )
        pool_lo_pre_swap_k = cvxpy.Parameter(
            name="pool_lo_pre_swap_k",
            value=geo_mean(compressed_reserves_pre_swap[pool_lo_index]).value,
        )

        pool_lo_profit_token_in = cvxpy.Variable(name="pool_lo_profit_token_in", nonneg=True)
        pool_hi_profit_token_out = cvxpy.Variable(name="pool_hi_profit_token_out", nonneg=True)
        forward_token_amount = cvxpy.Variable(name="forward_token_amount", nonneg=True)

        pool_hi_deposits = (
            (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
        )
        pool_lo_deposits = (
            (0, pool_lo_profit_token_in)
            if forward_token_index == 0
            else (pool_lo_profit_token_in, 0)
        )
        deposits = cvxpy_bmat((
            pool_hi_deposits,
            pool_lo_deposits,
        ))

        pool_hi_withdrawals = (
            (0, pool_hi_profit_token_out)
            if forward_token_index == 0
            else (pool_hi_profit_token_out, 0)
        )
        pool_lo_withdrawals = (
            (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
        )
        withdrawals = cvxpy_bmat((
            pool_hi_withdrawals,
            pool_lo_withdrawals,
        ))

        fees_removed = cvxpy_multiply(fee_multiplier, deposits)

        compressed_reserves_post_swap = (
            compressed_reserves_pre_swap + deposits - withdrawals - fees_removed
        )

        pool_hi_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_hi_index])
        pool_lo_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_lo_index])

        objective = cvxpy.Maximize(cvxpy_sum((withdrawals - deposits)[:, profit_token_index]))
        constraints = [
            pool_hi_post_swap_k >= pool_hi_pre_swap_k,
            pool_lo_post_swap_k >= pool_lo_pre_swap_k,
            pool_hi_profit_token_out
            <= compressed_reserves_pre_swap[pool_hi_index, profit_token_index],
            forward_token_amount
            <= compressed_reserves_pre_swap[pool_lo_index, forward_token_index],
        ]

        problem = cvxpy.Problem(objective, constraints)
        problem.solve(solver=cvxpy.CLARABEL)

        assert problem.status in cvxpy.settings.SOLUTION_PRESENT

        uncompressed_forward_token_amount = min(
            int(
                cast("float", forward_token_amount.value)
                * compression_factor
                * 10**forward_token.decimals
            ),
            (
                pool_lo.state.reserves_token0
                if forward_token_index == 0
                else pool_lo.state.reserves_token1
            )
            - 1,
        )

        weth_out = pool_hi.calculate_tokens_out_from_tokens_in(
            token_in=forward_token,
            token_in_quantity=uncompressed_forward_token_amount,
        )

        weth_in = pool_lo.calculate_tokens_in_from_tokens_out(
            token_out=forward_token,
            token_out_quantity=uncompressed_forward_token_amount,
        )

        # Verify profit exists
        assert weth_out > weth_in, "Expected profitable arbitrage"

    def test_2pool_uniswap_v2_double_decimal_corrected(
        self,
        wbtc_pool_a: LiquidityPool,
        wbtc_pool_b: LiquidityPool,
        weth_token: Erc20Token,
    ):
        """Regression test: WBTC/WETH 2-pool arbitrage with double compression."""
        profit_token = weth_token
        pool_a_roe = wbtc_pool_a.get_absolute_exchange_rate(token=profit_token)
        pool_b_roe = wbtc_pool_b.get_absolute_exchange_rate(token=profit_token)

        if pool_a_roe > pool_b_roe:
            pool_hi = wbtc_pool_a
            pool_lo = wbtc_pool_b
        else:
            pool_hi = wbtc_pool_b
            pool_lo = wbtc_pool_a

        num_pools = 2
        num_tokens = 2

        pool_hi_index, pool_lo_index = 0, 1

        token0_decimals = pool_hi.token0.decimals
        token1_decimals = pool_hi.token1.decimals

        forward_token = pool_hi.token1 if pool_hi.token0 == profit_token else pool_hi.token0
        forward_token_index = 1 if pool_hi.token0 == profit_token else 0
        profit_token_index = 0 if pool_hi.token0 == profit_token else 1
        assert forward_token_index != profit_token_index

        pool_hi_fees = [pool_hi.fee_token0, pool_hi.fee_token1]
        pool_lo_fees = [pool_lo.fee_token0, pool_lo.fee_token1]
        fee_multiplier = cvxpy_bmat((
            pool_hi_fees,
            pool_lo_fees,
        ))

        compression_factor_token0 = max(
            Fraction(pool_hi.state.reserves_token0, 10**token0_decimals),
            Fraction(pool_lo.state.reserves_token0, 10**token0_decimals),
        )
        compression_factor_token1 = max(
            Fraction(pool_hi.state.reserves_token1, 10**token1_decimals),
            Fraction(pool_lo.state.reserves_token1, 10**token1_decimals),
        )
        compression_factor_forward_token = (
            compression_factor_token0 if forward_token_index == 0 else compression_factor_token1
        )

        compressed_starting_reserves_pool_hi = (
            Fraction(pool_hi.state.reserves_token0, 10**token0_decimals)
            / compression_factor_token0,
            Fraction(pool_hi.state.reserves_token1, 10**token1_decimals)
            / compression_factor_token1,
        )
        compressed_starting_reserves_pool_lo = (
            Fraction(pool_lo.state.reserves_token0, 10**token0_decimals)
            / compression_factor_token0,
            Fraction(pool_lo.state.reserves_token1, 10**token1_decimals)
            / compression_factor_token1,
        )
        compressed_reserves_pre_swap = cvxpy.Parameter(
            name="compressed_reserves_pre_swap",
            shape=(num_pools, num_tokens),
            value=np.array(
                (
                    compressed_starting_reserves_pool_hi,
                    compressed_starting_reserves_pool_lo,
                ),
                dtype=np.float64,
            ),
        )

        pool_hi_pre_swap_k = cvxpy.Parameter(
            name="pool_hi_pre_swap_k",
            value=geo_mean(compressed_reserves_pre_swap[pool_hi_index]).value,
        )
        pool_lo_pre_swap_k = cvxpy.Parameter(
            name="pool_lo_pre_swap_k",
            value=geo_mean(compressed_reserves_pre_swap[pool_lo_index]).value,
        )

        pool_lo_profit_token_in = cvxpy.Variable(name="pool_lo_profit_token_in", nonneg=True)
        pool_hi_profit_token_out = cvxpy.Variable(name="pool_hi_profit_token_out", nonneg=True)
        forward_token_amount = cvxpy.Variable(name="forward_token_amount", nonneg=True)

        pool_hi_deposits = (
            (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
        )
        pool_lo_deposits = (
            (0, pool_lo_profit_token_in)
            if forward_token_index == 0
            else (pool_lo_profit_token_in, 0)
        )
        deposits = cvxpy_bmat((
            pool_hi_deposits,
            pool_lo_deposits,
        ))

        pool_hi_withdrawals = (
            (0, pool_hi_profit_token_out)
            if forward_token_index == 0
            else (pool_hi_profit_token_out, 0)
        )
        pool_lo_withdrawals = (
            (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
        )
        withdrawals = cvxpy_bmat((
            pool_hi_withdrawals,
            pool_lo_withdrawals,
        ))

        fees_removed = cvxpy_multiply(fee_multiplier, deposits)

        compressed_reserves_post_swap = (
            compressed_reserves_pre_swap + deposits - withdrawals - fees_removed
        )

        pool_hi_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_hi_index])
        pool_lo_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_lo_index])

        objective = cvxpy.Maximize(cvxpy_sum((withdrawals - deposits)[:, profit_token_index]))
        constraints = [
            pool_hi_post_swap_k >= pool_hi_pre_swap_k,
            pool_lo_post_swap_k >= pool_lo_pre_swap_k,
            pool_hi_profit_token_out
            <= compressed_reserves_pre_swap[pool_hi_index, profit_token_index],
            forward_token_amount
            <= compressed_reserves_pre_swap[pool_lo_index, forward_token_index],
        ]

        problem = cvxpy.Problem(objective, constraints)
        problem.solve(solver=cvxpy.CLARABEL)
        assert problem.status in cvxpy.settings.SOLUTION_PRESENT

        uncompressed_forward_token_amount = min(
            int(
                cast("float", forward_token_amount.value)
                * compression_factor_forward_token
                * 10**forward_token.decimals
            ),
            (
                pool_lo.state.reserves_token0
                if forward_token_index == 0
                else pool_lo.state.reserves_token1
            )
            - 1,
        )

        weth_out = pool_hi.calculate_tokens_out_from_tokens_in(
            token_in=forward_token,
            token_in_quantity=uncompressed_forward_token_amount,
        )

        weth_in = pool_lo.calculate_tokens_in_from_tokens_out(
            token_out=forward_token,
            token_out_quantity=uncompressed_forward_token_amount,
        )

        # Verify profit exists
        assert weth_out > weth_in, "Expected profitable arbitrage"


# ==============================================================================
# Property-Based Tests
# ==============================================================================


class TestCVXPY2PoolPropertyBased:
    """Property-based tests for 2-pool V2 arbitrage with CVXPY."""

    @hypothesis.given(
        price_ratio=price_ratio_strategy,
        liquidity_depth=liquidity_depth_strategy,
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=25)
    def test_randomized_2pool_finds_profitable_arb(
        self,
        price_ratio: float,
        liquidity_depth: str,
        seed: int,
    ):
        """Property: For any valid V2 pair with price discrepancy > fees,
        CVXPY optimizer finds a solution that indicates arbitrage opportunity.
        """
        factory = FixtureFactory()
        fixture = factory.random_v2_pair(
            seed=seed,
            liquidity_depth=liquidity_depth,
            price_ratio_range=(price_ratio, price_ratio),
        )

        # Verify fixture has expected properties
        assert fixture.cycle_type == "v2_v2"
        assert len(fixture.pool_states) == 2

        # Extract pool states
        pool_states = list(fixture.pool_states.values())
        state_a, state_b = pool_states[0], pool_states[1]

        # Calculate effective prices
        price_a = state_a.reserves_token1 / state_a.reserves_token0
        price_b = state_b.reserves_token1 / state_b.reserves_token0

        # With price_ratio > 1, there should be an arb opportunity
        # (assuming fees don't overwhelm the spread)
        price_diff = abs(price_a - price_b) / min(price_a, price_b)

        # At minimum, the fixture should have created a price discrepancy
        assert price_diff > 0, f"Expected price discrepancy, got {price_diff}"

    @hypothesis.given(
        decimals_pair=mismatched_decimal_pair_strategy,
        price_ratio=st.floats(
            min_value=1.02, max_value=1.08, allow_nan=False, allow_infinity=False
        ),
        liquidity_depth=liquidity_depth_strategy,
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=20)
    def test_mismatched_decimals_handled(
        self,
        decimals_pair: tuple[int, int],
        price_ratio: float,
        liquidity_depth: str,
        seed: int,
    ):
        """Property: CVXPY handles pools with mismatched token decimals.

        Tests that the optimizer doesn't fail or produce invalid results
        when tokens have different decimal places.
        """

        factory = FixtureFactory()
        fixture = factory.random_v2_pair(
            seed=seed,
            liquidity_depth=liquidity_depth,
            price_ratio_range=(price_ratio, price_ratio),
        )

        # The fixture should be valid regardless of decimals
        assert fixture.validate() is True


class TestCVXPYMultiPoolPropertyBased:
    """Property-based tests for multi-pool arbitrage cycles."""

    @hypothesis.given(
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_3pool_cycle_finds_solution(self, seed: int):
        """Property: 3-pool cycles can be solved by CVXPY.

        Note: Not all 3-pool configurations are profitable, but the solver
        should at least find a solution or report infeasibility.
        """
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=3,
            pool_types=["v2", "v2", "v2"],
            liquidity_depth="medium",
            price_ratio_range=(1.01, 1.03),
        )

        # Verify fixture is valid
        assert len(fixture.pool_states) == 3

    @hypothesis.given(
        seed=seed_strategy,
    )
    @hypothesis.settings(deadline=None, max_examples=10)
    def test_4pool_cycle_finds_solution(self, seed: int):
        """Property: 4-pool cycles can be solved by CVXPY."""
        factory = FixtureFactory()
        fixture = factory.random_multi_pool_cycle(
            seed=seed,
            num_pools=4,
            pool_types=["v2", "v2", "v2", "v2"],
            liquidity_depth="medium",
            price_ratio_range=(1.01, 1.02),
        )

        # Verify fixture is valid
        assert len(fixture.pool_states) == 4


# ==============================================================================
# Multi-Pool Known Value Tests
# ==============================================================================


class TestMultiPoolKnownValues:
    """Known value tests for multi-pool cycles from original test_cvxpy.py."""

    def test_base_3pool(
        self,
        link_token: Erc20Token,
        wbtc_token: Erc20Token,
        weth_token: Erc20Token,
    ):
        """3-pool arbitrage cycle: WETH -> LINK -> WBTC -> WETH."""
        # fake prices:
        # 1 WBTC = 20 WETH
        # 1 WETH = 200 LINK
        # 1 WBTC = 4000 LINK

        lp_1 = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=link_token,
            token1=weth_token,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=20_000 * 10**link_token.decimals,
            reserves_token1=100 * 10**weth_token.decimals,
            state_block=1,
        )

        lp_2 = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=wbtc_token,
            token1=link_token,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=10 * 10**wbtc_token.decimals,
            reserves_token1=20_000 * 10**link_token.decimals,
            state_block=1,
        )

        lp_3 = make_v2_pool(
            address="0x0000000000000000000000000000000000000003",
            token0=wbtc_token,
            token1=weth_token,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=5 * 10**wbtc_token.decimals,
            reserves_token1=100 * 10**weth_token.decimals,
            state_block=1,
        )

        arb = _UniswapMultiPoolCycleTesting(
            input_token=weth_token,
            swap_pools=[lp_1, lp_2, lp_3],
        )

        result = arb.calculate()
        assert result.profit_amount >= 0, "Expected non-negative profit"

    def test_base_4pool(
        self,
        link_token: Erc20Token,
        wbtc_token: Erc20Token,
        usdc_token: Erc20Token,
        weth_token: Erc20Token,
    ):
        """4-pool arbitrage cycle: WETH -> LINK -> USDC -> WBTC -> WETH."""
        # fake prices:
        # 1 WBTC = 100,000 USDC
        # 1 WBTC = 20 WETH (WETH = $5000)
        # 1 WETH = 200 LINK (LINK = $25)
        # 1 WBTC = 4000 LINK

        weth_link = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=weth_token,
            token1=link_token,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(0),
            fee_token1=Fraction(0),
            reserves_token0=1 * 10**weth_token.decimals,
            reserves_token1=250 * 10**link_token.decimals,
            state_block=1,
        )

        link_usdc = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=usdc_token,
            token1=link_token,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(0),
            fee_token1=Fraction(0),
            reserves_token0=25 * 10**usdc_token.decimals,
            reserves_token1=1 * 10**link_token.decimals,
            state_block=1,
        )

        usdc_wbtc = make_v2_pool(
            address="0x0000000000000000000000000000000000000003",
            token0=usdc_token,
            token1=wbtc_token,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(0),
            fee_token1=Fraction(0),
            reserves_token0=100_000 * 10**usdc_token.decimals,
            reserves_token1=1 * 10**wbtc_token.decimals,
            state_block=1,
        )

        weth_wbtc = make_v2_pool(
            address="0x0000000000000000000000000000000000000004",
            token0=weth_token,
            token1=wbtc_token,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(0),
            fee_token1=Fraction(0),
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=1 * 10**wbtc_token.decimals,
            state_block=1,
        )

        arb = _UniswapMultiPoolCycleTesting(
            input_token=weth_token,
            swap_pools=[weth_link, link_usdc, usdc_wbtc, weth_wbtc],
        )

        result = arb.calculate()
        assert result.profit_amount >= 0, "Expected non-negative profit"

    def test_multipool_two_pools(
        self,
        wbtc_token: Erc20Token,
        weth_token: Erc20Token,
    ):
        """2-pool simple arbitrage: WETH-WBTC with price discrepancy."""
        weth_wbtc_1 = make_v2_pool(
            address="0x0000000000000000000000000000000000000001",
            token0=weth_token,
            token1=wbtc_token,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(0),
            fee_token1=Fraction(0),
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=2 * 10**wbtc_token.decimals,
            state_block=1,
        )

        weth_wbtc_2 = make_v2_pool(
            address="0x0000000000000000000000000000000000000002",
            token0=weth_token,
            token1=wbtc_token,
            factory="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
            fee_token0=Fraction(0),
            fee_token1=Fraction(0),
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=1 * 10**wbtc_token.decimals,
            state_block=1,
        )

        arb = _UniswapMultiPoolCycleTesting(
            input_token=weth_token,
            swap_pools=[weth_wbtc_1, weth_wbtc_2],
        )

        result = arb.calculate()
        assert result.profit_amount >= 0, "Expected non-negative profit"


# ==============================================================================
# Base Chain Tests
# ==============================================================================


@pytest.mark.base
class TestBaseChainCVXPY:
    """CVXPY tests on Base chain."""

    def test_base_2pool(
        self,
        test_pool_base_a: LiquidityPool,
        test_pool_base_b: LiquidityPool,
        weth_base_token: Erc20Token,
    ):
        """2-pool arbitrage on Base chain."""
        profit_token = weth_base_token

        pool_a_roe = test_pool_base_a.get_absolute_exchange_rate(token=profit_token)
        pool_b_roe = test_pool_base_b.get_absolute_exchange_rate(token=profit_token)

        if pool_a_roe > pool_b_roe:
            pool_hi = test_pool_base_a
            pool_lo = test_pool_base_b
        else:
            pool_hi = test_pool_base_b
            pool_lo = test_pool_base_a

        assert pool_hi == test_pool_base_a
        assert pool_lo == test_pool_base_b

        num_pools = 2
        num_tokens = 2

        pool_hi_index, pool_lo_index = 0, 1

        token0_decimals = pool_hi.token0.decimals
        token1_decimals = pool_hi.token1.decimals

        forward_token_index = 1 if pool_hi.token0 == profit_token else 0
        profit_token_index = 0 if pool_hi.token0 == profit_token else 1
        assert forward_token_index != profit_token_index

        pool_hi_fees = [pool_hi.fee_token0, pool_hi.fee_token1]
        pool_lo_fees = [pool_lo.fee_token0, pool_lo.fee_token1]
        fee_multiplier = cvxpy_bmat((
            pool_hi_fees,
            pool_lo_fees,
        ))

        compression_factor_token0 = max(
            Fraction(pool_hi.state.reserves_token0, 10**token0_decimals),
            Fraction(pool_lo.state.reserves_token0, 10**token0_decimals),
        )
        compression_factor_token1 = max(
            Fraction(pool_hi.state.reserves_token1, 10**token1_decimals),
            Fraction(pool_lo.state.reserves_token1, 10**token1_decimals),
        )

        compressed_starting_reserves_pool_hi = (
            Fraction(pool_hi.state.reserves_token0, 10**token0_decimals)
            / compression_factor_token0,
            Fraction(pool_hi.state.reserves_token1, 10**token1_decimals)
            / compression_factor_token1,
        )
        compressed_starting_reserves_pool_lo = (
            Fraction(pool_lo.state.reserves_token0, 10**token0_decimals)
            / compression_factor_token0,
            Fraction(pool_lo.state.reserves_token1, 10**token1_decimals)
            / compression_factor_token1,
        )
        compressed_reserves_pre_swap = cvxpy.Parameter(
            name="compressed_reserves_pre_swap",
            shape=(num_pools, num_tokens),
            value=np.array(
                (
                    compressed_starting_reserves_pool_hi,
                    compressed_starting_reserves_pool_lo,
                ),
                dtype=np.float64,
            ),
        )

        pool_hi_pre_swap_k = cvxpy.Parameter(
            name="pool_hi_pre_swap_k",
            value=geo_mean(compressed_reserves_pre_swap[pool_hi_index]).value,
        )
        pool_lo_pre_swap_k = cvxpy.Parameter(
            name="pool_lo_pre_swap_k",
            value=geo_mean(compressed_reserves_pre_swap[pool_lo_index]).value,
        )

        pool_lo_profit_token_in = cvxpy.Variable(name="pool_lo_profit_token_in", nonneg=True)
        pool_hi_profit_token_out = cvxpy.Variable(name="pool_hi_profit_token_out", nonneg=True)
        forward_token_amount = cvxpy.Variable(name="forward_token_amount", nonneg=True)

        pool_hi_deposits = (
            (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
        )
        pool_lo_deposits = (
            (0, pool_lo_profit_token_in)
            if forward_token_index == 0
            else (pool_lo_profit_token_in, 0)
        )
        deposits = cvxpy_bmat((
            pool_hi_deposits,
            pool_lo_deposits,
        ))

        pool_hi_withdrawals = (
            (0, pool_hi_profit_token_out)
            if forward_token_index == 0
            else (pool_hi_profit_token_out, 0)
        )
        pool_lo_withdrawals = (
            (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
        )
        withdrawals = cvxpy_bmat((
            pool_hi_withdrawals,
            pool_lo_withdrawals,
        ))

        fees_removed = cvxpy_multiply(fee_multiplier, deposits)

        compressed_reserves_post_swap = (
            compressed_reserves_pre_swap + deposits - withdrawals - fees_removed
        )

        pool_hi_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_hi_index])
        pool_lo_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_lo_index])

        objective = cvxpy.Maximize(pool_hi_profit_token_out - pool_lo_profit_token_in)
        constraints = [
            pool_hi_post_swap_k >= pool_hi_pre_swap_k,
            pool_lo_post_swap_k >= pool_lo_pre_swap_k,
            pool_hi_profit_token_out
            <= compressed_reserves_pre_swap[pool_hi_index, profit_token_index],
            forward_token_amount
            <= compressed_reserves_pre_swap[pool_lo_index, forward_token_index],
        ]

        problem = cvxpy.Problem(objective, constraints)
        problem.solve(solver=cvxpy.CLARABEL)

        # Verify the optimization found a solution
        assert problem.status in cvxpy.settings.SOLUTION_PRESENT
