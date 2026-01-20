# ruff: noqa: E501

from collections import deque
from fractions import Fraction
from threading import Lock
from typing import cast
from weakref import WeakSet

import cvxpy
import cvxpy.settings
import numpy as np
import pytest
from cvxpy.atoms.affine.binary_operators import multiply as cvxpy_multiply
from cvxpy.atoms.affine.bmat import bmat as cvxpy_bmat
from cvxpy.atoms.affine.sum import sum as cvxpy_sum
from cvxpy.atoms.geo_mean import geo_mean

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage.uniswap_multipool_cycle_testing import (
    _UniswapMultiPoolCycleTesting,  # noqa: PLC2701
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import set_web3
from degenbot.constants import ZERO_ADDRESS
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate, UniswapV2PoolState

WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
LINK_ADDRESS = "0x514910771AF9Ca656af840dff83E8264EcF986CA"
USDC_ADDRESS = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
WBTC_WETH_V2_POOL_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
WBTC_WETH_V3_POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"


class MockLiquidityPool(UniswapV2Pool):
    def __init__(self) -> None:
        self._state_cache = deque([
            UniswapV2PoolState(
                address=ZERO_ADDRESS,
                reserves_token0=0,
                reserves_token1=0,
                block=0,
            )
        ])
        self._state_lock = Lock()
        self._subscribers = WeakSet()


@pytest.fixture
def wbtc_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20Token(WBTC_ADDRESS)


@pytest.fixture
def weth_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20Token(WETH_ADDRESS)


@pytest.fixture
def link_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20Token(LINK_ADDRESS)


@pytest.fixture
def usdc_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20Token(USDC_ADDRESS)


@pytest.fixture
def weth_base_token(fork_base_full: AnvilFork) -> Erc20Token:
    set_web3(fork_base_full.w3)
    return Erc20Token("0x4200000000000000000000000000000000000006")


@pytest.fixture
def xxx_base_token(fork_base_full: AnvilFork) -> Erc20Token:
    set_web3(fork_base_full.w3)
    return Erc20Token("0x09C07E80bFeEd81130498516F5C07aA0715794Bb")


@pytest.fixture
def wbtc_pool_a(wbtc_token, weth_token) -> MockLiquidityPool:
    lp = MockLiquidityPool()
    lp.name = "WBTC-WETH (V2, 0.30%)"
    lp.address = get_checksum_address("0xBb2b8038a1640196FbE3e38816F3e67Cba72D940")
    lp.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp.fee_token0 = Fraction(3, 1000)
    lp.fee_token1 = Fraction(3, 1000)
    lp.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=9000000000,
            reserves_token1=2100000000000000000000,
        )
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
    lp.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=9250000000,
            reserves_token1=2100000000000000000000,
        )
    )
    lp.token0 = wbtc_token
    lp.token1 = weth_token
    return lp


@pytest.fixture
def test_pool_base_a(xxx_base_token, weth_base_token) -> MockLiquidityPool:
    lp = MockLiquidityPool()
    lp.name = ""
    lp.address = get_checksum_address("0x214356Cc4aAb907244A791CA9735292860490D5A")
    lp.factory = get_checksum_address("0x420DD381b31aEf6683db6B902084cB0FFECe40Da")
    lp.fee_token0 = Fraction(3, 1000)
    lp.fee_token1 = Fraction(3, 1000)
    lp.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=19643270033194347,
            reserves_token1=406789256841523130269,
        )
    )
    lp.token0 = weth_base_token
    lp.token1 = xxx_base_token
    return lp


@pytest.fixture
def test_pool_base_b(xxx_base_token, weth_base_token) -> MockLiquidityPool:
    lp = MockLiquidityPool()
    lp.name = ""
    lp.address = get_checksum_address("0x404E927b203375779a6aBD52A2049cE0ADf6609B")
    lp.factory = get_checksum_address("0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6")
    lp.fee_token0 = Fraction(3, 1000)
    lp.fee_token1 = Fraction(3, 1000)
    lp.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=880450452482804609420,
            reserves_token1=18733831498401825763565574,
        )
    )
    lp.token0 = weth_base_token
    lp.token1 = xxx_base_token
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
    fee_multiplier = cvxpy_bmat((
        pool_hi_fees,
        pool_lo_fees,
    ))

    # Identify the largest value to use as a common divisor.
    compression_factor = max(
        Fraction(pool_hi.state.reserves_token0, 10**token0_decimals),
        Fraction(pool_hi.state.reserves_token1, 10**token1_decimals),
        Fraction(pool_lo.state.reserves_token0, 10**token0_decimals),
        Fraction(pool_lo.state.reserves_token1, 10**token1_decimals),
    )

    # Compress all pool reserves into a 0.0 - 1.0 value range
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
    deposits = cvxpy_bmat((
        pool_hi_deposits,
        pool_lo_deposits,
    ))

    pool_hi_withdrawals = (
        (0, pool_hi_profit_token_out) if forward_token_index == 0 else (pool_hi_profit_token_out, 0)
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
        f"reserves_starting    (pool_hi)        = {[compressed_reserves_pre_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_ending      (pool_hi)        = {[compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_final       (pool_hi)        = {[compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_pre_swap    (pool_lo)        = {[compressed_reserves_pre_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_post_swap   (pool_lo)        = {[compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_final       (pool_lo)        = {[compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(f"pool_lo_pre_swap_k                    = {pool_lo_pre_swap_k.value}")
    print(f"pool_lo_post_swap_k                   = {pool_lo_post_swap_k.value}")
    print(f"pool_lo_final_k                       = {pool_lo_final_k.value}")
    print(f"pool_hi_pre_swap_k                    = {pool_hi_pre_swap_k.value}")
    print(f"pool_hi_post_swap_k                   = {pool_hi_post_swap_k.value}")
    print(f"pool_hi_final_k                       = {pool_hi_final_k.value}")

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
    fee_multiplier = cvxpy_bmat((
        pool_hi_fees,
        pool_lo_fees,
    ))

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
    compressed_starting_reserves_pool_hi = (
        Fraction(pool_hi.state.reserves_token0, 10**token0_decimals) / compression_factor_token0,
        Fraction(pool_hi.state.reserves_token1, 10**token1_decimals) / compression_factor_token1,
    )
    compressed_starting_reserves_pool_lo = (
        Fraction(pool_lo.state.reserves_token0, 10**token0_decimals) / compression_factor_token0,
        Fraction(pool_lo.state.reserves_token1, 10**token1_decimals) / compression_factor_token1,
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
    deposits = cvxpy_bmat((
        pool_hi_deposits,
        pool_lo_deposits,
    ))

    pool_hi_withdrawals = (
        (0, pool_hi_profit_token_out) if forward_token_index == 0 else (pool_hi_profit_token_out, 0)
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
    uncompressed_deposits = cvxpy_multiply(
        deposits, np.array([compression_factor_token0, compression_factor_token1])
    )
    uncompressed_withdrawals = cvxpy_multiply(
        withdrawals, np.array([compression_factor_token0, compression_factor_token1])
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
        f"reserves_starting    (pool_hi)        = {[compressed_reserves_pre_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_ending      (pool_hi)        = {[compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_final       (pool_hi)        = {[compressed_reserves_post_swap[pool_hi_index].value]}"
    )
    print(
        f"reserves_pre_swap    (pool_lo)        = {[compressed_reserves_pre_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_post_swap   (pool_lo)        = {[compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(
        f"reserves_final       (pool_lo)        = {[compressed_reserves_post_swap[pool_lo_index].value]}"
    )
    print(f"pool_lo_pre_swap_k                    = {pool_lo_pre_swap_k.value}")
    print(f"pool_lo_post_swap_k                   = {pool_lo_post_swap_k.value}")
    print(f"pool_lo_final_k                       = {pool_lo_final_k.value}")
    print(f"pool_hi_pre_swap_k                    = {pool_hi_pre_swap_k.value}")
    print(f"pool_hi_post_swap_k                   = {pool_hi_post_swap_k.value}")
    print(f"pool_hi_final_k                       = {pool_hi_final_k.value}")

    weth_out = pool_hi.calculate_tokens_out_from_tokens_in(
        token_in=forward_token,
        token_in_quantity=uncompressed_forward_token_amount,
    )

    weth_in = pool_lo.calculate_tokens_in_from_tokens_out(
        token_out=forward_token,
        token_out_quantity=uncompressed_forward_token_amount,
    )
    print(f"Actual profit                         = {weth_out - weth_in}")


@pytest.mark.base
def test_base_2pool(
    test_pool_base_a: MockLiquidityPool,
    test_pool_base_b: MockLiquidityPool,
    weth_base_token: Erc20Token,
):
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

    # Indices are arbitrary but must be consistent so amounts are consistent between pools in the
    # various arrays
    pool_hi_index, pool_lo_index = 0, 1

    token0_decimals = pool_hi.token0.decimals
    token1_decimals = pool_hi.token1.decimals

    forward_token = pool_hi.token1 if pool_hi.token0 == profit_token else pool_hi.token0
    forward_token_index = 1 if pool_hi.token0 == profit_token else 0
    profit_token_index = 0 if pool_hi.token0 == profit_token else 1
    assert forward_token_index != profit_token_index

    profit_token_decimals = token0_decimals if profit_token_index == 0 else token1_decimals

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
    compression_factor_profit_token = (
        compression_factor_token0 if profit_token_index == 0 else compression_factor_token1
    )

    # Compress all pool reserves into a 0.0 - 1.0 value range
    compressed_starting_reserves_pool_hi = (
        Fraction(pool_hi.state.reserves_token0, 10**token0_decimals) / compression_factor_token0,
        Fraction(pool_hi.state.reserves_token1, 10**token1_decimals) / compression_factor_token1,
    )
    compressed_starting_reserves_pool_lo = (
        Fraction(pool_lo.state.reserves_token0, 10**token0_decimals) / compression_factor_token0,
        Fraction(pool_lo.state.reserves_token1, 10**token1_decimals) / compression_factor_token1,
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
    deposits = cvxpy_bmat((
        pool_hi_deposits,
        pool_lo_deposits,
    ))

    pool_hi_withdrawals = (
        (0, pool_hi_profit_token_out) if forward_token_index == 0 else (pool_hi_profit_token_out, 0)
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

    final_reserves = compressed_reserves_post_swap + fees_removed

    pool_hi_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_hi_index])
    pool_lo_post_swap_k = geo_mean(compressed_reserves_post_swap[pool_lo_index])

    pool_hi_final_k = geo_mean(final_reserves[pool_hi_index])
    pool_lo_final_k = geo_mean(final_reserves[pool_lo_index])

    objective = cvxpy.Maximize(pool_hi_profit_token_out - pool_lo_profit_token_in)
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
        verbose=True,
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
        deposits, np.array([compression_factor_token0, compression_factor_token1])
    )
    uncompressed_withdrawals = cvxpy_multiply(
        withdrawals, np.array([compression_factor_token0, compression_factor_token1])
    )
    # fmt:off
    print()
    print("Solved")
    print(f"fee_multiplier                        = {[(float(fee[0]), float(fee[1])) for fee in fee_multiplier.value]}")
    print(f"forward_token_amount                  = {uncompressed_forward_token_amount}")
    print(f"uncompressed withdrawals (pool_hi)    = {uncompressed_withdrawals[pool_hi_index].value}")
    print(f"uncompressed withdrawals (pool_lo)    = {uncompressed_withdrawals[pool_lo_index].value}")
    print(f"uncompressed deposits    (pool_hi)    = {uncompressed_deposits[pool_hi_index].value}")
    print(f"uncompressed deposits    (pool_lo)    = {uncompressed_deposits[pool_lo_index].value}")
    print(f"compressed withdrawals   (pool_hi)    = {withdrawals[pool_hi_index].value}")
    print(f"compressed withdrawals   (pool_lo)    = {withdrawals[pool_lo_index].value}")
    print(f"compressed deposits      (pool_hi)    = {deposits[pool_hi_index].value}")
    print(f"compressed deposits      (pool_lo)    = {deposits[pool_lo_index].value}")
    print(f"reserves_pre_swap    (pool_hi)        = {[compressed_reserves_pre_swap[pool_hi_index].value]}")
    print(f"reserves_ending      (pool_hi)        = {[compressed_reserves_post_swap[pool_hi_index].value]}")
    print(f"reserves_final       (pool_hi)        = {[compressed_reserves_post_swap[pool_hi_index].value]}")
    print(f"reserves_pre_swap    (pool_lo)        = {[compressed_reserves_pre_swap[pool_lo_index].value]}")
    print(f"reserves_post_swap   (pool_lo)        = {[compressed_reserves_post_swap[pool_lo_index].value]}")
    print(f"reserves_final       (pool_lo)        = {[compressed_reserves_post_swap[pool_lo_index].value]}")
    print(f"pool_lo_pre_swap_k                    = {pool_lo_pre_swap_k.value}")
    print(f"pool_lo_post_swap_k                   = {pool_lo_post_swap_k.value}")
    print(f"pool_lo_final_k                       = {pool_lo_final_k.value}")
    print(f"pool_hi_pre_swap_k                    = {pool_hi_pre_swap_k.value}")
    print(f"pool_hi_post_swap_k                   = {pool_hi_post_swap_k.value}")
    print(f"pool_hi_final_k                       = {pool_hi_final_k.value}")
    # fmt:on

    weth_out = pool_hi.calculate_tokens_out_from_tokens_in(
        token_in=forward_token,
        token_in_quantity=uncompressed_forward_token_amount,
    )

    weth_in = pool_lo.calculate_tokens_in_from_tokens_out(
        token_out=forward_token,
        token_out_quantity=uncompressed_forward_token_amount,
    )
    # fmt: off
    print(f"Fees (pool_hi)                        = {fees_removed[pool_hi_index, :].value}")
    print(f"Fees (pool_lo)                        = {fees_removed[pool_lo_index, :].value}")
    print(f"WETH out (approximate)                = {int(compression_factor_profit_token * 10**profit_token_decimals * pool_hi_profit_token_out.value)}")
    print(f"WETH out (actual)                     = {weth_out}")
    print(f"WETH in  (approximate)                = {int(compression_factor_profit_token * 10**profit_token_decimals * pool_lo_profit_token_in.value)}")
    print(f"WETH in  (actual)                     = {weth_in}")
    print(f"Profit (approximate)                  = {int(compression_factor_profit_token * 10**profit_token_decimals * pool_hi_profit_token_out.value) - int(compression_factor_profit_token * 10**profit_token_decimals * pool_lo_profit_token_in.value)}")
    print(f"Profit (actual)                       = {weth_out - weth_in}")
    # fmt:on


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
    lp_1.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=20_000 * 10**link_token.decimals,
            reserves_token1=100 * 10**weth_token.decimals,
            # total liquidity 10_000 WETH
        )
    )

    lp_2 = MockLiquidityPool()
    lp_2.name = "Fake V2 Pool: LINK-WBTC (0.3%)"
    lp_2.address = get_checksum_address("0x0000000000000000000000000000000000000002")
    lp_2.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp_2.fee_token0 = Fraction(3, 1000)
    lp_2.fee_token1 = Fraction(3, 1000)
    lp_2.token0 = wbtc_token
    lp_2.token1 = link_token
    lp_2.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=(
                # dislocate middle pool by decreasing the price of WBTC
                10 * 10**wbtc_token.decimals
            ),
            reserves_token1=20_000 * 10**link_token.decimals,
        )
    )

    lp_3 = MockLiquidityPool()
    lp_3.name = "Fake V2 Pool: WBTC-WETH (0.3%)"
    lp_3.address = get_checksum_address("0x0000000000000000000000000000000000000003")
    lp_3.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    lp_3.fee_token0 = Fraction(3, 1000)
    lp_3.fee_token1 = Fraction(3, 1000)
    lp_3.token0 = wbtc_token
    lp_3.token1 = weth_token
    lp_3.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=5 * 10**wbtc_token.decimals,
            reserves_token1=100 * 10**weth_token.decimals,
            # total liquidity 10_000 WETH
        )
    )

    arb = _UniswapMultiPoolCycleTesting(
        input_token=weth_token,
        swap_pools=[lp_1, lp_2, lp_3],  # profitable
    )

    result = arb.calculate()
    print(f"{result.profit_amount=}")


def test_base_4pool(
    link_token: Erc20Token,
    wbtc_token: Erc20Token,
    usdc_token: Erc20Token,
    weth_token: Erc20Token,
):
    # fake prices:
    # 1 WBTC = 100,000 USDC
    # 1 WTBC = 20 WETH      (WETH = $5000)
    # 1 WETH = 200 LINK     (LINK = $25)
    # 1 WBTC = 4000 LINK

    weth_link = MockLiquidityPool()
    weth_link.name = "Fake V2 Pool: WETH-LINK (0%)"
    weth_link.address = get_checksum_address("0x0000000000000000000000000000000000000001")
    weth_link.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    weth_link.fee_token0 = Fraction(0)
    weth_link.fee_token1 = Fraction(0)
    weth_link.token0 = weth_token
    weth_link.token1 = link_token
    weth_link.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=1 * 10**weth_token.decimals,
            reserves_token1=(
                # skew the ratio of this pool to create the arbitrage opportunity
                250 * 10**link_token.decimals
            ),
        )
    )

    link_usdc = MockLiquidityPool()
    link_usdc.name = "Fake V2 Pool: LINK-USDC (0%)"
    link_usdc.address = get_checksum_address("0x0000000000000000000000000000000000000002")
    link_usdc.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    link_usdc.fee_token0 = Fraction(0)
    link_usdc.fee_token1 = Fraction(0)
    link_usdc.token0 = usdc_token
    link_usdc.token1 = link_token
    link_usdc.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=25 * 10**usdc_token.decimals,
            reserves_token1=1 * 10**link_token.decimals,
        )
    )

    usdc_wbtc = MockLiquidityPool()
    usdc_wbtc.name = "Fake V2 Pool: USDC-WBTC (0%)"
    usdc_wbtc.address = get_checksum_address("0x0000000000000000000000000000000000000003")
    usdc_wbtc.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    usdc_wbtc.fee_token0 = Fraction(0)
    usdc_wbtc.fee_token1 = Fraction(0)
    usdc_wbtc.token0 = usdc_token
    usdc_wbtc.token1 = wbtc_token
    usdc_wbtc.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=100_000 * 10**usdc_token.decimals,
            reserves_token1=1 * 10**wbtc_token.decimals,
        )
    )

    weth_wbtc = MockLiquidityPool()
    weth_wbtc.name = "Fake V2 Pool: WETH-WBTC (0%)"
    weth_wbtc.address = get_checksum_address("0x0000000000000000000000000000000000000004")
    weth_wbtc.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    weth_wbtc.fee_token0 = Fraction(0)
    weth_wbtc.fee_token1 = Fraction(0)
    weth_wbtc.token0 = weth_token
    weth_wbtc.token1 = wbtc_token
    weth_wbtc.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=1 * 10**wbtc_token.decimals,
        )
    )

    arb = _UniswapMultiPoolCycleTesting(
        input_token=weth_token,
        swap_pools=[weth_link, link_usdc, usdc_wbtc, weth_wbtc],  # profitable
    )

    result = arb.calculate()
    print(f"{result.profit_amount=}")


def test_multipool_two_pools(
    wbtc_token: Erc20Token,
    weth_token: Erc20Token,
):
    # fake prices:
    # 1 WTBC = 20 WETH      (WETH = $5000)

    weth_wbtc_1 = MockLiquidityPool()
    weth_wbtc_1.name = "Fake V2 Pool: WETH-WBTC (0%)"
    weth_wbtc_1.address = get_checksum_address("0x0000000000000000000000000000000000000001")
    weth_wbtc_1.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    weth_wbtc_1.fee_token0 = Fraction(0)
    weth_wbtc_1.fee_token1 = Fraction(0)
    weth_wbtc_1.token0 = weth_token
    weth_wbtc_1.token1 = wbtc_token
    weth_wbtc_1.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=2 * 10**wbtc_token.decimals,
        )
    )

    weth_wbtc_2 = MockLiquidityPool()
    weth_wbtc_2.name = "Fake V2 Pool: WETH-WBTC (0%)"
    weth_wbtc_2.address = get_checksum_address("0x0000000000000000000000000000000000000002")
    weth_wbtc_2.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    weth_wbtc_2.fee_token0 = Fraction(0)
    weth_wbtc_2.fee_token1 = Fraction(0)
    weth_wbtc_2.token0 = weth_token
    weth_wbtc_2.token1 = wbtc_token
    weth_wbtc_2.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=1 * 10**wbtc_token.decimals,
        )
    )

    arb = _UniswapMultiPoolCycleTesting(
        input_token=weth_token,
        swap_pools=[
            weth_wbtc_1,
            weth_wbtc_2,
        ],
    )

    result = arb.calculate()
    print(f"{result.profit_amount=}")


@pytest.mark.xfail(reason="Multi-pool with shared token pairs are WIP", strict=True)
def test_base_4pool_repeated_pair(
    wbtc_token: Erc20Token,
    weth_token: Erc20Token,
):
    # fake prices:
    # 1 WTBC = 20 WETH      (WETH = $5000)

    weth_wbtc_1 = MockLiquidityPool()
    weth_wbtc_1.name = "Fake V2 Pool: WETH-WBTC (0%)"
    weth_wbtc_1.address = get_checksum_address("0x0000000000000000000000000000000000000001")
    weth_wbtc_1.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    weth_wbtc_1.fee_token0 = Fraction(0)
    weth_wbtc_1.fee_token1 = Fraction(0)
    weth_wbtc_1.token0 = weth_token
    weth_wbtc_1.token1 = wbtc_token
    weth_wbtc_1.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=2 * 10**wbtc_token.decimals,
        )
    )

    weth_wbtc_2 = MockLiquidityPool()
    weth_wbtc_2.name = "Fake V2 Pool: WETH-WBTC (0%)"
    weth_wbtc_2.address = get_checksum_address("0x0000000000000000000000000000000000000002")
    weth_wbtc_2.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    weth_wbtc_2.fee_token0 = Fraction(0)
    weth_wbtc_2.fee_token1 = Fraction(0)
    weth_wbtc_2.token0 = weth_token
    weth_wbtc_2.token1 = wbtc_token
    weth_wbtc_2.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=1 * 10**wbtc_token.decimals,
        )
    )

    weth_wbtc_3 = MockLiquidityPool()
    weth_wbtc_3.name = "Fake V2 Pool: WETH-WBTC (0%)"
    weth_wbtc_3.address = get_checksum_address("0x0000000000000000000000000000000000000003")
    weth_wbtc_3.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    weth_wbtc_3.fee_token0 = Fraction(0)
    weth_wbtc_3.fee_token1 = Fraction(0)
    weth_wbtc_3.token0 = weth_token
    weth_wbtc_3.token1 = wbtc_token
    weth_wbtc_3.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=1 * 10**wbtc_token.decimals,
        )
    )

    weth_wbtc_4 = MockLiquidityPool()
    weth_wbtc_4.name = "Fake V2 Pool: WETH-WBTC (0%)"
    weth_wbtc_4.address = get_checksum_address("0x0000000000000000000000000000000000000004")
    weth_wbtc_4.factory = get_checksum_address("0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f")
    weth_wbtc_4.fee_token0 = Fraction(0)
    weth_wbtc_4.fee_token1 = Fraction(0)
    weth_wbtc_4.token0 = weth_token
    weth_wbtc_4.token1 = wbtc_token
    weth_wbtc_4.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=1,
            reserves_token0=20 * 10**weth_token.decimals,
            reserves_token1=1 * 10**wbtc_token.decimals,
        )
    )

    arb = _UniswapMultiPoolCycleTesting(
        input_token=weth_token,
        swap_pools=[
            weth_wbtc_1,
            weth_wbtc_2,
            weth_wbtc_3,
            weth_wbtc_4,
        ],
    )

    result = arb.calculate()
    print(f"{result.profit_amount=}")
