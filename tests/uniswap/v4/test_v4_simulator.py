"""Tests for v4_simulator — pure swap calculation extracted from V4Pool.

These tests verify that ``v4_simulator.calculate_swap`` produces *identical*
results to ``UniswapV4Pool._calculate_swap``. Uses real on-chain pool data
via the ``eth_usdc_v4`` fixture.
"""

import pytest
from hexbytes import HexBytes

from degenbot.anvil_fork import AnvilFork
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import set_web3
from degenbot.constants import ZERO_ADDRESS
from degenbot.registry import managed_pool_registry
from degenbot.uniswap.concentrated.liquidity_map import LiquidityMapSnapshot
from degenbot.uniswap.concentrated.v4_simulator import calculate_swap
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

USDC_CONTRACT_ADDRESS = get_checksum_address("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48")
NATIVE_CURRENCY_ADDRESS = ZERO_ADDRESS
ETH_USDC_V4_POOL_ID = "0x21c67e77068de97969ba93d4aab21826d33ca12bb9f565d8496e8fda8a82ca27"
ETH_USDC_V4_POOL_FEE = 500
ETH_USDC_V4_POOL_TICK_SPACING = 10
V4_POOL_MANAGER_ADDRESS = get_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
STATE_VIEW_ADDRESS = get_checksum_address("0x7fFE42C4a5DEeA5b0feC41C94C136Cf115597227")


@pytest.fixture
def eth_usdc_v4(fork_mainnet_full: AnvilFork) -> UniswapV4Pool:
    """Same fixture as test_uniswap_v4_liquidity_pool.py."""
    set_web3(fork_mainnet_full.w3)
    if (
        pool := managed_pool_registry.get(
            chain_id=fork_mainnet_full.w3.eth.chain_id,
            pool_manager_address=V4_POOL_MANAGER_ADDRESS,
            pool_id=ETH_USDC_V4_POOL_ID,
        )
    ) is None:
        return UniswapV4Pool(
            pool_id=HexBytes(ETH_USDC_V4_POOL_ID),
            pool_manager_address=V4_POOL_MANAGER_ADDRESS,
            state_view_address=STATE_VIEW_ADDRESS,
            tokens=[USDC_CONTRACT_ADDRESS, NATIVE_CURRENCY_ADDRESS],
            fee=ETH_USDC_V4_POOL_FEE,
            tick_spacing=ETH_USDC_V4_POOL_TICK_SPACING,
        )
    assert isinstance(pool, UniswapV4Pool)
    return pool


class TestV4SimulatorMatchesPool:
    def test_exact_input_zero_for_one(self, eth_usdc_v4: UniswapV4Pool) -> None:
        """ETH → USDC (zero_for_one=True, amount_specified < 0 in V4 sign convention)."""
        pool = eth_usdc_v4
        amount_in = 1 * 10**18  # 1 ETH

        old = pool._calculate_swap(
            zero_for_one=True,
            amount_specified=-amount_in,
            sqrt_price_x96_limit=4295128740,
        )

        snap = LiquidityMapSnapshot.from_pool(pool)
        new = calculate_swap(
            snapshot=snap,
            zero_for_one=True,
            amount_specified=-amount_in,
            sqrt_price_x96_limit=4295128740,
            lp_fee=pool.lp_fee,
            protocol_fee=pool.protocol_fee.zero_for_one,
            liquidity_start=pool.liquidity,
            sqrt_price_x96_start=pool.sqrt_price_x96,
            tick_start=pool.tick,
        )

        assert new.amount0 == old[0].currency0
        assert new.amount1 == old[0].currency1
        assert new.sqrt_price_x96 == old[3].sqrt_price_x96
        assert new.liquidity == old[3].liquidity
        assert new.tick == old[3].tick

    def test_exact_input_one_for_zero(self, eth_usdc_v4: UniswapV4Pool) -> None:
        """USDC → ETH (zero_for_one=False, amount_specified < 0)."""
        pool = eth_usdc_v4
        amount_in = 1000 * 10**6  # 1000 USDC

        old = pool._calculate_swap(
            zero_for_one=False,
            amount_specified=-amount_in,
            sqrt_price_x96_limit=1461446703485210103287273052203988822378723970341,
        )

        snap = LiquidityMapSnapshot.from_pool(pool)
        new = calculate_swap(
            snapshot=snap,
            zero_for_one=False,
            amount_specified=-amount_in,
            sqrt_price_x96_limit=1461446703485210103287273052203988822378723970341,
            lp_fee=pool.lp_fee,
            protocol_fee=pool.protocol_fee.one_for_zero,
            liquidity_start=pool.liquidity,
            sqrt_price_x96_start=pool.sqrt_price_x96,
            tick_start=pool.tick,
        )

        assert new.amount0 == old[0].currency0
        assert new.amount1 == old[0].currency1
        assert new.sqrt_price_x96 == old[3].sqrt_price_x96
        assert new.liquidity == old[3].liquidity
        assert new.tick == old[3].tick

    def test_zero_specified_returns_zero_delta(self, eth_usdc_v4: UniswapV4Pool) -> None:
        """V4: amount_specified == 0 returns zero delta (does not revert)."""
        pool = eth_usdc_v4
        snap = LiquidityMapSnapshot.from_pool(pool)

        new = calculate_swap(
            snapshot=snap,
            zero_for_one=True,
            amount_specified=0,
            sqrt_price_x96_limit=4295128740,
            lp_fee=pool.lp_fee,
            protocol_fee=pool.protocol_fee.zero_for_one,
            liquidity_start=pool.liquidity,
            sqrt_price_x96_start=pool.sqrt_price_x96,
            tick_start=pool.tick,
        )
        assert new.amount0 == 0
        assert new.amount1 == 0
        assert new.sqrt_price_x96 == pool.sqrt_price_x96

    def test_exact_output_zero_for_one(self, eth_usdc_v4: UniswapV4Pool) -> None:
        """Want exactly 1000 USDC out (zero_for_one=True, amount_specified > 0 in V4)."""
        pool = eth_usdc_v4
        amount_out = 1000 * 10**6

        old = pool._calculate_swap(
            zero_for_one=True,
            amount_specified=amount_out,
            sqrt_price_x96_limit=4295128740,
        )

        snap = LiquidityMapSnapshot.from_pool(pool)
        new = calculate_swap(
            snapshot=snap,
            zero_for_one=True,
            amount_specified=amount_out,
            sqrt_price_x96_limit=4295128740,
            lp_fee=pool.lp_fee,
            protocol_fee=pool.protocol_fee.zero_for_one,
            liquidity_start=pool.liquidity,
            sqrt_price_x96_start=pool.sqrt_price_x96,
            tick_start=pool.tick,
        )

        assert new.amount0 == old[0].currency0
        assert new.amount1 == old[0].currency1
        assert new.sqrt_price_x96 == old[3].sqrt_price_x96
        assert new.liquidity == old[3].liquidity
        assert new.tick == old[3].tick

    def test_exact_output_one_for_zero(self, eth_usdc_v4: UniswapV4Pool) -> None:
        """Want exactly 0.1 ETH out (zero_for_one=False, amount_specified > 0)."""
        pool = eth_usdc_v4
        amount_out = 1 * 10**17  # 0.1 ETH

        old = pool._calculate_swap(
            zero_for_one=False,
            amount_specified=amount_out,
            sqrt_price_x96_limit=1461446703485210103287273052203988822378723970341,
        )

        snap = LiquidityMapSnapshot.from_pool(pool)
        new = calculate_swap(
            snapshot=snap,
            zero_for_one=False,
            amount_specified=amount_out,
            sqrt_price_x96_limit=1461446703485210103287273052203988822378723970341,
            lp_fee=pool.lp_fee,
            protocol_fee=pool.protocol_fee.one_for_zero,
            liquidity_start=pool.liquidity,
            sqrt_price_x96_start=pool.sqrt_price_x96,
            tick_start=pool.tick,
        )

        assert new.amount0 == old[0].currency0
        assert new.amount1 == old[0].currency1
        assert new.sqrt_price_x96 == old[3].sqrt_price_x96
        assert new.liquidity == old[3].liquidity
        assert new.tick == old[3].tick
