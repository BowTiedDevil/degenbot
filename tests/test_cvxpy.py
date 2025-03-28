# ruff: noqa: E501

from fractions import Fraction
from threading import Lock
from typing import cast
from weakref import WeakSet

import cvxpy
import cvxpy.atoms.geo_mean
import cvxpy.settings
import cvxpy.transforms
import cvxpy.transforms.indicator
import numpy
import pytest
from cvxpy.atoms.affine.binary_operators import multiply as cvxpy_multiply
from cvxpy.atoms.affine.bmat import bmat as cvxpy_bmat
from cvxpy.atoms.affine.sum import sum as cvxpy_sum
from cvxpy.atoms.geo_mean import geo_mean

from degenbot.arbitrage.uniswap_multipool_cycle_testing import _UniswapMultiPoolCycleTesting
from degenbot.cache import get_checksum_address
from degenbot.config import set_web3
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20_token import Erc20Token
from degenbot.uniswap.types import UniswapV2PoolState
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool

WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
LINK_ADDRESS = "0x514910771AF9Ca656af840dff83E8264EcF986CA"
WBTC_WETH_V2_POOL_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
WBTC_WETH_V3_POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"


class MockLiquidityPool(UniswapV2Pool):
    def __init__(self) -> None:
        self._state = UniswapV2PoolState(
            address=ZERO_ADDRESS,
            reserves_token0=0,
            reserves_token1=0,
            block=None,
        )
        self._state_lock = Lock()
        self._subscribers = WeakSet()


@pytest.fixture
def wbtc_token(ethereum_archive_node_web3) -> Erc20Token:
    set_web3(ethereum_archive_node_web3)
    return Erc20Token(WBTC_ADDRESS)


@pytest.fixture
def weth_token(ethereum_archive_node_web3) -> Erc20Token:
    set_web3(ethereum_archive_node_web3)
    return Erc20Token(WETH_ADDRESS)


@pytest.fixture
def link_token(ethereum_archive_node_web3) -> Erc20Token:
    set_web3(ethereum_archive_node_web3)
    return Erc20Token(LINK_ADDRESS)


@pytest.fixture
def weth_base_token(base_full_node_web3) -> Erc20Token:
    set_web3(base_full_node_web3)
    return Erc20Token("0x4200000000000000000000000000000000000006")


@pytest.fixture
def xxx_token(base_full_node_web3) -> Erc20Token:
    set_web3(base_full_node_web3)
    return Erc20Token("0x09C07E80bFeEd81130498516F5C07aA0715794Bb")


@pytest.fixture
def wbtc_pool_a(wbtc_token, weth_token) -> MockLiquidityPool:
    lp = MockLiquidityPool()
    lp.name = "WBTC-WETH (V2, 0.30%)"
    lp.address = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
    lp.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp.fee_token0 = Fraction(3, 1000)
    lp.fee_token1 = Fraction(3, 1000)
    lp._state = UniswapV2PoolState(
        address=lp.address,
        block=None,
        reserves_token0=9000000000,
        reserves_token1=2100000000000000000000,
    )
    lp.token0 = wbtc_token
    lp.token1 = weth_token
    return lp


@pytest.fixture
def wbtc_pool_b(wbtc_token, weth_token) -> MockLiquidityPool:
    lp = MockLiquidityPool()
    lp.name = "WBTC-WETH (V2, 0.30%)"
    lp.address = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D941")
    lp.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp.fee_token0 = Fraction(3, 1000)
    lp.fee_token1 = Fraction(3, 1000)
    lp._state = UniswapV2PoolState(
        address=lp.address,
        block=None,
        reserves_token0=9250000000,
        reserves_token1=2100000000000000000000,
    )
    lp.token0 = wbtc_token
    lp.token1 = weth_token
    return lp


@pytest.fixture
def test_pool_a(xxx_token, weth_base_token) -> MockLiquidityPool:
    lp = MockLiquidityPool()
    lp.name = ""
    lp.address = get_checksum_address("0x214356Cc4aAb907244A791CA9735292860490D5A")
    lp.factory = get_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da")
    lp.fee_token0 = Fraction(3, 1000)
    lp.fee_token1 = Fraction(3, 1000)
    lp._state = UniswapV2PoolState(
        address=lp.address,
        block=None,
        reserves_token0=19643270033194347,
        reserves_token1=406789256841523130269,
    )
    lp.token0 = weth_base_token
    lp.token1 = xxx_token
    return lp


@pytest.fixture
def test_pool_b(xxx_token, weth_base_token) -> MockLiquidityPool:
    lp = MockLiquidityPool()
    lp.name = ""
    lp.address = get_checksum_address("0x404E927b203375779a6aBD52A2049cE0ADf6609B")
    lp.factory = get_checksum_address("0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6")
    lp.fee_token0 = Fraction(3, 1000)
    lp.fee_token1 = Fraction(3, 1000)
    lp._state = UniswapV2PoolState(
        address=lp.address,
        block=None,
        reserves_token0=880450452482804609420,
        reserves_token1=18733831498401825763565574,
    )
    lp.token0 = weth_base_token
    lp.token1 = xxx_token
    return lp


def test_2pool_uniswap_v2_decimal_corrected(
    wbtc_pool_a: MockLiquidityPool,
    wbtc_pool_b: MockLiquidityPool,
    weth_token: Erc20Token,
):
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

    # Indices are arbitrary but must be consistent so amounts are consistent between pools in the
    # various arrays
    pool_hi_index, pool_lo_index = 0, 1

    token0_decimals = pool_hi.token0.decimals
    token1_decimals = pool_hi.token1.decimals

    forward_token = pool_hi.token1 if pool_hi.token0 == profit_token else pool_hi.token0
    forward_token_index = 1 if pool_hi.token0 == profit_token else 0
    profit_token_index = 0 if pool_hi.token0 == profit_token else 1
    assert forward_token_index != profit_token_index

    pool_hi_fees = [pool_hi.fee_token0, pool_hi.fee_token1]
    pool_lo_fees = [pool_lo.fee_token0, pool_lo.fee_token1]
    fee_multiplier = cvxpy_bmat(
        (
            pool_hi_fees,
            pool_lo_fees,
        )
    )

    # Identify the largest value to use as a common divisor.
    compression_factor = max(
        Fraction(pool_hi.state.reserves_token0, 10**token0_decimals),
        Fraction(pool_hi.state.reserves_token1, 10**token1_decimals),
        Fraction(pool_lo.state.reserves_token0, 10**token0_decimals),
        Fraction(pool_lo.state.reserves_token1, 10**token1_decimals),
    )

    # Compress all pool reserves into a 0.0 - 1.0 value range
    _compressed_starting_reserves_pool_hi = (
        Fraction(pool_hi.state.reserves_token0, 10**token0_decimals) / compression_factor,
        Fraction(pool_hi.state.reserves_token1, 10**token1_decimals) / compression_factor,
    )
    _compressed_starting_reserves_pool_lo = (
        Fraction(pool_lo.state.reserves_token0, 10**token0_decimals) / compression_factor,
        Fraction(pool_lo.state.reserves_token1, 10**token1_decimals) / compression_factor,
    )
    compressed_reserves_pre_swap = cvxpy.Parameter(
        name="compressed_reserves_pre_swap",
        shape=(num_pools, num_tokens),
        value=numpy.array(
            (
                _compressed_starting_reserves_pool_hi,
                _compressed_starting_reserves_pool_lo,
            ),
            dtype=numpy.float64,
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

    pool_lo_profit_token_in = cvxpy.Variable(
        name="pool_lo_profit_token_in",
        nonneg=True,
    )
    pool_hi_profit_token_out = cvxpy.Variable(
        name="pool_hi_profit_token_out",
        nonneg=True,
    )
    forward_token_amount = cvxpy.Variable(
        name="forward_token_amount",
        nonneg=True,
    )

    pool_hi_deposits = (
        (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
    )
    pool_lo_deposits = (
        (0, pool_lo_profit_token_in) if forward_token_index == 0 else (pool_lo_profit_token_in, 0)
    )
    deposits = cvxpy_bmat(
        (
            pool_hi_deposits,
            pool_lo_deposits,
        )
    )

    pool_hi_withdrawals = (
        (0, pool_hi_profit_token_out) if forward_token_index == 0 else (pool_hi_profit_token_out, 0)
    )
    pool_lo_withdrawals = (
        (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
    )
    withdrawals = cvxpy_bmat(
        (
            pool_hi_withdrawals,
            pool_lo_withdrawals,
        )
    )

    fees_removed = cvxpy_multiply(fee_multiplier, deposits)

    compressed_reserves_post_swap = (
        compressed_reserves_pre_swap + deposits - withdrawals - fees_removed
    )

    final_reserves = compressed_reserves_post_swap + fees_removed

    pool_hi_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_hi_index])
    pool_lo_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_lo_index])

    pool_hi_final_k = geo_mean(final_reserves[pool_hi_index])
    pool_lo_final_k = geo_mean(final_reserves[pool_lo_index])

    objective = cvxpy.Maximize(cvxpy_sum((withdrawals - deposits)[:, profit_token_index]))
    constraints = [
        # Pool invariant (x*y=k)
        pool_hi_post_swap_k >= pool_hi_pre_swap_k,
        pool_lo_post_swap_k >= pool_lo_pre_swap_k,
        # Withdrawals can't exceed pool reserves
        pool_hi_profit_token_out <= compressed_reserves_pre_swap[pool_hi_index, profit_token_index],
        forward_token_amount <= compressed_reserves_pre_swap[pool_lo_index, forward_token_index],
    ]

    problem = cvxpy.Problem(objective, constraints)
    problem.solve(
        solver=cvxpy.CLARABEL,
        # verbose=True,
    )

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
    uncompressed_deposits = compression_factor * deposits
    uncompressed_withdrawals = compression_factor * withdrawals

    print()
    print("Solved")
    print(
        f"fee_multiplier                        = {[(float(fee[0]), float(fee[1])) for fee in fee_multiplier.value]}"
    )
    print(f"forward_token_amount                  = {uncompressed_forward_token_amount}")
    print(
        f"uncompressed withdrawals (pool_hi)    = {uncompressed_withdrawals[pool_hi_index].value}"
    )
    print(
        f"uncompressed withdrawals (pool_lo)    = {uncompressed_withdrawals[pool_lo_index].value}"
    )
    print(f"uncompressed deposits    (pool_hi)    = {uncompressed_deposits[pool_hi_index].value}")
    print(f"uncompressed deposits    (pool_lo)    = {uncompressed_deposits[pool_lo_index].value}")
    print(
        f"reserves_starting    (pool_hi)        = {[res for res in compressed_reserves_pre_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_ending      (pool_hi)        = {[res for res in compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_final       (pool_hi)        = {[res for res in compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_pre_swap    (pool_lo)        = {[res for res in compressed_reserves_pre_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_post_swap   (pool_lo)        = {[res for res in compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_final       (pool_lo)        = {[res for res in compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(f"pool_lo_pre_swap_k                    = {pool_lo_pre_swap_k.value}")
    print(f"pool_lo_post_swap_k                   = {pool_lo_post_swap_k.value}")
    print(f"pool_lo_final_k                       = {pool_lo_final_k.value}")
    print(f"pool_hi_pre_swap_k                    = {pool_hi_pre_swap_k.value}")
    print(f"pool_hi_post_swap_k                   = {pool_hi_post_swap_k.value}")
    print(f"pool_hi_final_k                       = {pool_hi_final_k.value}")

    # assert pool_hi_final_k.value >= pool_hi_start_k.value
    # assert pool_lo_final_k.value >= pool_lo_start_k.value

    weth_out = pool_hi.calculate_tokens_out_from_tokens_in(
        token_in=forward_token,
        token_in_quantity=uncompressed_forward_token_amount,
    )

    weth_in = pool_lo.calculate_tokens_in_from_tokens_out(
        token_out=forward_token,
        token_out_quantity=uncompressed_forward_token_amount,
    )
    print(f"Actual profit                         = {weth_out - weth_in}")


def test_2pool_uniswap_v2_double_decimal_corrected(
    wbtc_pool_a: MockLiquidityPool,
    wbtc_pool_b: MockLiquidityPool,
    weth_token: Erc20Token,
):
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

    # Indices are arbitrary but must be consistent so amounts are consistent between pools in the
    # various arrays
    pool_hi_index, pool_lo_index = 0, 1

    token0_decimals = pool_hi.token0.decimals
    token1_decimals = pool_hi.token1.decimals

    forward_token = pool_hi.token1 if pool_hi.token0 == profit_token else pool_hi.token0
    forward_token_index = 1 if pool_hi.token0 == profit_token else 0
    profit_token_index = 0 if pool_hi.token0 == profit_token else 1
    assert forward_token_index != profit_token_index

    pool_hi_fees = [pool_hi.fee_token0, pool_hi.fee_token1]
    pool_lo_fees = [pool_lo.fee_token0, pool_lo.fee_token1]
    fee_multiplier = cvxpy_bmat(
        (
            pool_hi_fees,
            pool_lo_fees,
        )
    )

    # Identify the largest value to use as a common divisor for each token.
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
    print(f"compression factor (token0): {float(compression_factor_token0)}")
    print(f"compression factor (token1): {float(compression_factor_token1)}")

    # Compress all pool reserves into a 0.0 - 1.0 value range
    _compressed_starting_reserves_pool_hi = (
        Fraction(pool_hi.state.reserves_token0, 10**token0_decimals) / compression_factor_token0,
        Fraction(pool_hi.state.reserves_token1, 10**token1_decimals) / compression_factor_token1,
    )
    _compressed_starting_reserves_pool_lo = (
        Fraction(pool_lo.state.reserves_token0, 10**token0_decimals) / compression_factor_token0,
        Fraction(pool_lo.state.reserves_token1, 10**token1_decimals) / compression_factor_token1,
    )
    compressed_reserves_pre_swap = cvxpy.Parameter(
        name="compressed_reserves_pre_swap",
        shape=(num_pools, num_tokens),
        value=numpy.array(
            (
                _compressed_starting_reserves_pool_hi,
                _compressed_starting_reserves_pool_lo,
            ),
            dtype=numpy.float64,
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

    pool_lo_profit_token_in = cvxpy.Variable(
        name="pool_lo_profit_token_in",
        nonneg=True,
    )
    pool_hi_profit_token_out = cvxpy.Variable(
        name="pool_hi_profit_token_out",
        nonneg=True,
    )
    forward_token_amount = cvxpy.Variable(
        name="forward_token_amount",
        nonneg=True,
    )

    pool_hi_deposits = (
        (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
    )
    pool_lo_deposits = (
        (0, pool_lo_profit_token_in) if forward_token_index == 0 else (pool_lo_profit_token_in, 0)
    )
    deposits = cvxpy_bmat(
        (
            pool_hi_deposits,
            pool_lo_deposits,
        )
    )

    pool_hi_withdrawals = (
        (0, pool_hi_profit_token_out) if forward_token_index == 0 else (pool_hi_profit_token_out, 0)
    )
    pool_lo_withdrawals = (
        (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
    )
    withdrawals = cvxpy_bmat(
        (
            pool_hi_withdrawals,
            pool_lo_withdrawals,
        )
    )

    fees_removed = cvxpy_multiply(fee_multiplier, deposits)

    compressed_reserves_post_swap = (
        compressed_reserves_pre_swap + deposits - withdrawals - fees_removed
    )

    final_reserves = compressed_reserves_post_swap + fees_removed

    pool_hi_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_hi_index])
    pool_lo_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_lo_index])

    pool_hi_final_k = geo_mean(final_reserves[pool_hi_index])
    pool_lo_final_k = geo_mean(final_reserves[pool_lo_index])

    objective = cvxpy.Maximize(cvxpy_sum((withdrawals - deposits)[:, profit_token_index]))
    constraints = [
        # Pool invariant (x*y=k)
        pool_hi_post_swap_k >= pool_hi_pre_swap_k,
        pool_lo_post_swap_k >= pool_lo_pre_swap_k,
        # Withdrawals can't exceed pool reserves
        pool_hi_profit_token_out <= compressed_reserves_pre_swap[pool_hi_index, profit_token_index],
        forward_token_amount <= compressed_reserves_pre_swap[pool_lo_index, forward_token_index],
    ]

    problem = cvxpy.Problem(objective, constraints)
    problem.solve(
        solver=cvxpy.CLARABEL,
        # verbose=True,
    )

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
    uncompressed_deposits = cvxpy_multiply(
        deposits, numpy.array([compression_factor_token0, compression_factor_token1])
    )
    uncompressed_withdrawals = cvxpy_multiply(
        withdrawals, numpy.array([compression_factor_token1, compression_factor_token1])
    )
    # uncompressed_deposits = compression_factor * deposits
    # uncompressed_withdrawals = compression_factor * withdrawals

    print()
    print("Solved")
    print(
        f"fee_multiplier                        = {[(float(fee[0]), float(fee[1])) for fee in fee_multiplier.value]}"
    )
    print(f"forward_token_amount                  = {uncompressed_forward_token_amount}")
    print(
        f"uncompressed withdrawals (pool_hi)    = {uncompressed_withdrawals[pool_hi_index].value}"
    )
    print(
        f"uncompressed withdrawals (pool_lo)    = {uncompressed_withdrawals[pool_lo_index].value}"
    )
    print(f"uncompressed deposits    (pool_hi)    = {uncompressed_deposits[pool_hi_index].value}")
    print(f"uncompressed deposits    (pool_lo)    = {uncompressed_deposits[pool_lo_index].value}")
    print(
        f"reserves_starting    (pool_hi)        = {[res for res in compressed_reserves_pre_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_ending      (pool_hi)        = {[res for res in compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_final       (pool_hi)        = {[res for res in compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_pre_swap    (pool_lo)        = {[res for res in compressed_reserves_pre_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_post_swap   (pool_lo)        = {[res for res in compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_final       (pool_lo)        = {[res for res in compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(f"pool_lo_pre_swap_k                    = {pool_lo_pre_swap_k.value}")
    print(f"pool_lo_post_swap_k                   = {pool_lo_post_swap_k.value}")
    print(f"pool_lo_final_k                       = {pool_lo_final_k.value}")
    print(f"pool_hi_pre_swap_k                    = {pool_hi_pre_swap_k.value}")
    print(f"pool_hi_post_swap_k                   = {pool_hi_post_swap_k.value}")
    print(f"pool_hi_final_k                       = {pool_hi_final_k.value}")

    # assert pool_hi_final_k.value >= pool_hi_start_k.value
    # assert pool_lo_final_k.value >= pool_lo_start_k.value

    weth_out = pool_hi.calculate_tokens_out_from_tokens_in(
        token_in=forward_token,
        token_in_quantity=uncompressed_forward_token_amount,
    )

    weth_in = pool_lo.calculate_tokens_in_from_tokens_out(
        token_out=forward_token,
        token_out_quantity=uncompressed_forward_token_amount,
    )
    print(f"Actual profit                         = {weth_out - weth_in}")


def test_base_2pool(
    test_pool_a: MockLiquidityPool,
    test_pool_b: MockLiquidityPool,
    weth_base_token: Erc20Token,
):
    # scipy forward amount: 4323768730401916416
    # cvxpy forward amount: 4323770602979739649

    profit_token = weth_base_token

    # profit_token = weth_base_token
    pool_a_roe = test_pool_a.get_absolute_exchange_rate(token=profit_token)
    pool_b_roe = test_pool_b.get_absolute_exchange_rate(token=profit_token)

    if pool_a_roe > pool_b_roe:
        pool_hi = test_pool_a
        pool_lo = test_pool_b
    else:
        pool_hi = test_pool_b
        pool_lo = test_pool_a

    num_pools = 2
    num_tokens = 2

    # Indices are arbitrary but must be consistent so amounts are consistent between pools in the
    # various arrays
    pool_hi_index, pool_lo_index = 0, 1

    token0_decimals = pool_hi.token0.decimals
    token1_decimals = pool_hi.token1.decimals

    forward_token = pool_hi.token1 if pool_hi.token0 == profit_token else pool_hi.token0
    forward_token_index = 1 if pool_hi.token0 == profit_token else 0
    profit_token_index = 0 if pool_hi.token0 == profit_token else 1
    assert forward_token_index != profit_token_index

    pool_hi_fees = [pool_hi.fee_token0, pool_hi.fee_token1]
    pool_lo_fees = [pool_lo.fee_token0, pool_lo.fee_token1]
    fee_multiplier = cvxpy_bmat(
        (
            pool_hi_fees,
            pool_lo_fees,
        )
    )

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

    # Compress all pool reserves into a 0.0 - 1.0 value range
    _compressed_starting_reserves_pool_hi = (
        Fraction(pool_hi.state.reserves_token0, 10**token0_decimals) / compression_factor_token0,
        Fraction(pool_hi.state.reserves_token1, 10**token1_decimals) / compression_factor_token1,
    )
    _compressed_starting_reserves_pool_lo = (
        Fraction(pool_lo.state.reserves_token0, 10**token0_decimals) / compression_factor_token0,
        Fraction(pool_lo.state.reserves_token1, 10**token1_decimals) / compression_factor_token1,
    )
    compressed_reserves_pre_swap = cvxpy.Parameter(
        name="compressed_reserves_pre_swap",
        shape=(num_pools, num_tokens),
        value=numpy.array(
            (
                _compressed_starting_reserves_pool_hi,
                _compressed_starting_reserves_pool_lo,
            ),
            dtype=numpy.float64,
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

    pool_lo_profit_token_in = cvxpy.Variable(
        name="pool_lo_profit_token_in",
        nonneg=True,
    )
    pool_hi_profit_token_out = cvxpy.Variable(
        name="pool_hi_profit_token_out",
        nonneg=True,
    )
    forward_token_amount = cvxpy.Variable(
        name="forward_token_amount",
        nonneg=True,
    )

    pool_hi_deposits = (
        (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
    )
    pool_lo_deposits = (
        (0, pool_lo_profit_token_in) if forward_token_index == 0 else (pool_lo_profit_token_in, 0)
    )
    deposits = cvxpy_bmat(
        (
            pool_hi_deposits,
            pool_lo_deposits,
        )
    )

    pool_hi_withdrawals = (
        (0, pool_hi_profit_token_out) if forward_token_index == 0 else (pool_hi_profit_token_out, 0)
    )
    pool_lo_withdrawals = (
        (forward_token_amount, 0) if forward_token_index == 0 else (0, forward_token_amount)
    )
    withdrawals = cvxpy_bmat(
        (
            pool_hi_withdrawals,
            pool_lo_withdrawals,
        )
    )

    fees_removed = cvxpy_multiply(fee_multiplier, deposits)

    compressed_reserves_post_swap = (
        compressed_reserves_pre_swap + deposits - withdrawals - fees_removed
    )

    final_reserves = compressed_reserves_post_swap + fees_removed

    pool_hi_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_hi_index])
    pool_lo_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_lo_index])

    pool_hi_final_k = geo_mean(final_reserves[pool_hi_index])
    pool_lo_final_k = geo_mean(final_reserves[pool_lo_index])

    objective = cvxpy.Maximize(cvxpy_sum((withdrawals - deposits)[:, profit_token_index]))
    constraints = [
        # Pool invariant (x*y=k)
        pool_hi_post_swap_k >= pool_hi_pre_swap_k,
        pool_lo_post_swap_k >= pool_lo_pre_swap_k,
        # Withdrawals can't exceed pool reserves
        pool_hi_profit_token_out <= compressed_reserves_pre_swap[pool_hi_index, profit_token_index],
        forward_token_amount <= compressed_reserves_pre_swap[pool_lo_index, forward_token_index],
    ]

    problem = cvxpy.Problem(objective, constraints)
    clarabel_tols = 1e-10
    problem.solve(
        solver=cvxpy.CLARABEL,
        verbose=True,
        tol_gap_abs=clarabel_tols,  # absolute duality gap tolerance
        tol_gap_rel=clarabel_tols,  # relative duality gap tolerance
        tol_feas=clarabel_tols,  # feasibility check tolerance (primal and dual)
        tol_infeas_abs=clarabel_tols,  # absolute infeasibility tolerance (primal and dual)
        tol_infeas_rel=clarabel_tols,  # relative infeasibility tolerance (primal and dual)
    )

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
    uncompressed_deposits = cvxpy_multiply(
        deposits, numpy.array([compression_factor_token0, compression_factor_token1])
    )
    uncompressed_withdrawals = cvxpy_multiply(
        withdrawals, numpy.array([compression_factor_token1, compression_factor_token1])
    )
    print()
    print("Solved")
    print(
        f"fee_multiplier                        = {[(float(fee[0]), float(fee[1])) for fee in fee_multiplier.value]}"
    )
    print(f"forward_token_amount                  = {uncompressed_forward_token_amount}")
    print(
        f"uncompressed withdrawals (pool_hi)    = {uncompressed_withdrawals[pool_hi_index].value}"
    )
    print(
        f"uncompressed withdrawals (pool_lo)    = {uncompressed_withdrawals[pool_lo_index].value}"
    )
    print(f"uncompressed deposits    (pool_hi)    = {uncompressed_deposits[pool_hi_index].value}")
    print(f"uncompressed deposits    (pool_lo)    = {uncompressed_deposits[pool_lo_index].value}")
    print(
        f"reserves_pre_swap    (pool_hi)        = {[res for res in compressed_reserves_pre_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_ending      (pool_hi)        = {[res for res in compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_final       (pool_hi)        = {[res for res in compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_pre_swap    (pool_lo)        = {[res for res in compressed_reserves_pre_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_post_swap   (pool_lo)        = {[res for res in compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_final       (pool_lo)        = {[res for res in compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(f"pool_lo_pre_swap_k                    = {pool_lo_pre_swap_k.value}")
    print(f"pool_lo_post_swap_k                   = {pool_lo_post_swap_k.value}")
    print(f"pool_lo_final_k                       = {pool_lo_final_k.value}")
    print(f"pool_hi_pre_swap_k                    = {pool_hi_pre_swap_k.value}")
    print(f"pool_hi_post_swap_k                   = {pool_hi_post_swap_k.value}")
    print(f"pool_hi_final_k                       = {pool_hi_final_k.value}")

    # assert pool_hi_final_k.value >= pool_hi_start_k.value
    # assert pool_lo_final_k.value >= pool_lo_start_k.value

    weth_out = pool_hi.calculate_tokens_out_from_tokens_in(
        token_in=forward_token,
        token_in_quantity=uncompressed_forward_token_amount,
    )

    weth_in = pool_lo.calculate_tokens_in_from_tokens_out(
        token_out=forward_token,
        token_out_quantity=uncompressed_forward_token_amount,
    )
    print(f"Actual profit                         = {weth_out - weth_in}")


def test_base_3pool(
    link_token: Erc20Token,
    wbtc_token: Erc20Token,
    weth_token: Erc20Token,
):
    # fake prices:
    # 1 WTBC = 20 WETH
    # 1 WETH = 200 LINK
    # 1 WBTC = 4000 LINK

    lp_1 = MockLiquidityPool()
    lp_1.name = "Fake V2 Pool: LINK-WETH (0.3%)"
    lp_1.address = get_checksum_address("0x0000000000000000000000000000000000000001")
    lp_1.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp_1.fee_token0 = Fraction(3, 1000)
    lp_1.fee_token1 = Fraction(3, 1000)
    lp_1.token0 = link_token
    lp_1.token1 = weth_token
    lp_1._state = UniswapV2PoolState(
        address=lp_1.address,
        block=None,
        reserves_token0=20_000 * 10**link_token.decimals,
        reserves_token1=100 * 10**weth_token.decimals,
        # total liquidity 10_000 WETH
    )

    lp_2 = MockLiquidityPool()
    lp_2.name = "Fake V2 Pool: LINK-WBTC (0.3%)"
    lp_2.address = get_checksum_address("0x0000000000000000000000000000000000000002")
    lp_2.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp_2.fee_token0 = Fraction(3, 1000)
    lp_2.fee_token1 = Fraction(3, 1000)
    lp_2.token0 = wbtc_token
    lp_2.token1 = link_token
    lp_2._state = UniswapV2PoolState(
        address=lp_2.address,
        block=None,
        # reserves_token0=5 * 10**wbtc_token.decimals, # original balance
        reserves_token0=(
            # dislocate middle pool by decreasing the price of WBTC
            10 * 10**wbtc_token.decimals
        ),
        reserves_token1=20_000 * 10**link_token.decimals,
    )

    lp_3 = MockLiquidityPool()
    lp_3.name = "Fake V2 Pool: WBTC-WETH (0.3%)"
    lp_3.address = get_checksum_address("0x0000000000000000000000000000000000000003")
    lp_3.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp_3.fee_token0 = Fraction(3, 1000)
    lp_3.fee_token1 = Fraction(3, 1000)
    lp_3.token0 = wbtc_token
    lp_3.token1 = weth_token
    lp_3._state = UniswapV2PoolState(
        address=lp_3.address,
        block=None,
        reserves_token0=5 * 10**wbtc_token.decimals,
        reserves_token1=100 * 10**weth_token.decimals,
        # total liquidity 10_000 WETH
    )

    arb = _UniswapMultiPoolCycleTesting(
        input_token=weth_token,
        # swap_pools=[wbtc_pool_b, wbtc_pool_a],
        swap_pools=[lp_1, lp_2, lp_3],  # profitable
        # swap_pools=[lp_3, lp_2, lp_1], # unprofitable
        id="test",
    )

    result = arb.calculate()
    print(f"{result.profit_amount=}")
    # print(f"{result=}")
    # payloads = arb.generate_payloads(
    #     from_address="0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f",
    #     pool_swap_amounts=result.swap_amounts,
    # )
