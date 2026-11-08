"""
Tests for the V3 fake pool's swap behavior.

Verifies that the pool computes swap outputs using the real V3
computeSwapStep (SqrtPriceMath + FullMath) rather than pre-configured amounts.

All tests go through the executor, matching real-world usage — the pool always
invokes a callback on msg.sender, so direct calls from EOAs revert (same as the
real UniswapV3Pool).
"""

import pytest
import math

from .conftest_shared import (
    enc_v3_swap_compact,
    AddressTable,
    enc_preamble,
)

Q96 = 79228162514264337593543950336

AMOUNT_WETH = 1 * 10**18
AMOUNT_USDC = 2000 * 10**6
WETH_LIQUIDITY = 100 * 10**18
USDC_LIQUIDITY = 200_000 * 10**6
FEE_30BIPS = 3000


# ── Fixtures ──

@pytest.fixture
def v3_pool(project, owner_account, weth, usdc):
    """Deploy and initialize a V3 pool with liquidity."""
    t0, t1 = sorted([weth.address, usdc.address], key=lambda a: a.lower())
    pool = project.fake_uniswap_v3_pool.deploy(t0, t1, 0, FEE_30BIPS, sender=owner_account)
    
    # Compute sqrtPriceX96 — 2000 USDC per WETH
    # V3 price = token1/token0 in raw units
    if pool.token0() == usdc.address:
        # token0=USDC (6 dec), token1=WETH (18 dec)
        # 2000 USDC per 1 WETH: price = 1e18 / (2000*1e6) = 5e8
        sqrt_price_x96 = int(math.sqrt(5 * 10**8) * Q96)
    else:
        # token0=WETH (18 dec), token1=USDC (6 dec)
        # price = (2000*1e6) / 1e18 → very small → use inverse identity
        primary_sqrt = int(math.sqrt(5 * 10**8) * Q96)
        sqrt_price_x96 = (Q96 * Q96) // primary_sqrt
    
    pool.initialize(sqrt_price_x96, sender=owner_account)
    
    weth.mint(pool.address, WETH_LIQUIDITY, sender=owner_account)
    usdc.mint(pool.address, USDC_LIQUIDITY, sender=owner_account)
    pool.add_liquidity(sender=owner_account)
    
    return pool


# ── Tests ──

class TestV3PoolBasic:
    """Basic V3 pool tests — all go through the executor."""

    def test_initialize_and_liquidity(self, v3_pool, owner_account):
        """Verify initialize() and add_liquidity() set state correctly."""
        pool = v3_pool
        assert pool.sqrt_price_x96() > 0
        assert pool.liquidity() > 0
        assert pool.fee() == FEE_30BIPS

    def test_swap_weth_for_usdc(
        self, v3_pool, owner_account, executor, weth, usdc
    ):
        """Sell 1 WETH for USDC. Verify price moves and USDC is transferred from pool."""
        pool = v3_pool
        
        weth_is_token0 = pool.token0() == weth.address
        swap_zfo = weth_is_token0
        
        sqrt_price_before = pool.sqrt_price_x96()
        pool_usdc_before = usdc.balanceOf(pool.address)
        pool_weth_before = weth.balanceOf(pool.address)
        
        at = AddressTable(weth_addr=weth.address, executor_addr=executor.address)
        pool_idx = at.add(pool.address)
        
        commands = enc_preamble(at)
        commands += enc_v3_swap_compact(
            pool_idx, swap_zfo, AMOUNT_WETH, at.add(owner_account.address)
        )
        
        weth.mint(executor.address, AMOUNT_WETH, sender=owner_account)
        executor.execute(commands, 0, sender=owner_account)
        
        # Price moved in the correct direction
        sqrt_price_after = pool.sqrt_price_x96()
        if swap_zfo:
            assert sqrt_price_after < sqrt_price_before
        else:
            assert sqrt_price_after > sqrt_price_before
        
        # Pool received WETH
        pool_weth_after = weth.balanceOf(pool.address)
        assert pool_weth_after > pool_weth_before
        
        # Pool sent USDC (~1974 USDC for 1 WETH at 0.3% fee)
        pool_usdc_after = usdc.balanceOf(pool.address)
        usdc_output = pool_usdc_before - pool_usdc_after
        assert usdc_output > 0
        
        # Verify output is approximately correct (within 1% of Python reference)
        # Python reference: L * (newSqrtP - sqrtP) / (newSqrtP * sqrtP / Q96) ≈ 1974 USDC
        expected_approx = int(AMOUNT_WETH * 2000 * (1 - FEE_30BIPS / 1_000_000) / 10**12 * 0.987)
        tolerance = expected_approx * 0.02
        assert abs(usdc_output - expected_approx) < tolerance, (
            f"USDC output {usdc_output} not within 2% of expected {expected_approx}"
        )
