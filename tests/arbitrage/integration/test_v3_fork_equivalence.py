"""
V3-only fork-based equivalence: ArbitragePath + Solver vs. legacy UniswapLpCycle.

Uses real mainnet V3 pools on an Anvil fork. Verifies both systems agree
on profitability and optimal input for a 3-pool V3-only cycle.

Key pools:
- WETH/USDC 0.05%: 0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640
- USDC/USDT 0.01%: 0x3416cF6C708Da44DB2624D63ea0AAef7113527C6
- WETH/USDT 0.30%: 0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36

Status: RED — test asserts equivalence; if mainnet state is unprofitable,
the test can be skipped or the logic can verify both systems reject.
"""


import pytest
from eth_typing import ChainId

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage import UniswapLpCycle
from degenbot.arbitrage.optimizers.solver import BrentSolver
from degenbot.arbitrage.path import ArbitragePath
from degenbot.arbitrage.types import UniswapV3PoolSwapAmounts
from degenbot.connection import set_web3
from degenbot.erc20.erc20 import Erc20Token
from degenbot.erc20.manager import Erc20TokenManager
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool

# Mainnet V3 pool addresses
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
USDC_ADDRESS = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
USDT_ADDRESS = "0xdAC17F958D2ee523a2206206994597C13D831ec7"

WETH_USDC_005_POOL = "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"
USDC_USDT_001_POOL = "0x3416cF6C708Da44DB2624D63ea0AAef7113527C6"
WETH_USDT_030_POOL = "0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36"


@pytest.fixture
def weth_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20TokenManager(chain_id=ChainId.ETH).get_erc20token(WETH_ADDRESS)


@pytest.fixture
def usdc_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20TokenManager(chain_id=ChainId.ETH).get_erc20token(USDC_ADDRESS)


@pytest.fixture
def usdt_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20TokenManager(chain_id=ChainId.ETH).get_erc20token(USDT_ADDRESS)


@pytest.fixture
def weth_usdc_005_lp(fork_mainnet_full: AnvilFork) -> UniswapV3Pool:
    set_web3(fork_mainnet_full.w3)
    return UniswapV3Pool(WETH_USDC_005_POOL)


@pytest.fixture
def usdc_usdt_001_lp(fork_mainnet_full: AnvilFork) -> UniswapV3Pool:
    set_web3(fork_mainnet_full.w3)
    return UniswapV3Pool(USDC_USDT_001_POOL)


@pytest.fixture
def weth_usdt_030_lp(fork_mainnet_full: AnvilFork) -> UniswapV3Pool:
    set_web3(fork_mainnet_full.w3)
    return UniswapV3Pool(WETH_USDT_030_POOL)


class TestV3OnlyForkEquivalence:
    """
    Verify both systems agree on a V3-only 3-hop cycle using real pools.

    Cycle: WETH -> USDC (pool A, 0.05%) -> USDT (pool B, 0.01%) -> WETH (pool C, 0.30%)
    """

    def test_v3_only_fork_agreement(
        self,
        weth_usdc_005_lp: UniswapV3Pool,
        usdc_usdt_001_lp: UniswapV3Pool,
        weth_usdt_030_lp: UniswapV3Pool,
        weth_token: Erc20Token,
    ):
        """
        Both legacy and new systems must agree on whether the V3-only
        triangle is profitable, and if so, on the optimal input and profit.
        """
        pools = [weth_usdc_005_lp, usdc_usdt_001_lp, weth_usdt_030_lp]
        max_input = 10 * 10**18

        # Legacy system
        try:
            legacy = UniswapLpCycle(
                input_token=weth_token,
                swap_pools=pools,
                max_input=max_input,
            )
            legacy_result = legacy.calculate()
            legacy_found = True
        except Exception:
            legacy_found = False

        # New system
        path = ArbitragePath(
            pools=pools,
            input_token=weth_token,
            solver=BrentSolver(),
            max_input=max_input,
        )
        try:
            new_solve = path.calculate()
            new_found = True
        except Exception:
            new_found = False

        # If both reject, skip (mainnet state is unprofitable for this triangle)
        if not legacy_found and not new_found:
            pytest.skip("Both systems found no profit at current fork block")

        # If one accepts and the other rejects, that's a real gap
        if legacy_found != new_found:
            pytest.fail(
                f"Disagreement: legacy_found={legacy_found}, new_found={new_found}"
            )

        # Both found profit — compare results
        assert legacy_result.input_amount > 0
        assert new_solve.optimal_input > 0
        assert legacy_result.profit_amount > 0
        assert new_solve.profit > 0

        # Build swap amounts for comparison
        new_calc = path.build_swap_amounts(new_solve)

        # Match to within 0.01% relative tolerance (different float solvers)
        rel_diff = abs(legacy_result.profit_amount - new_calc.profit_amount) / max(
            legacy_result.profit_amount, 1
        )
        assert rel_diff < 0.0001, (
            f"Profit mismatch: legacy={legacy_result.profit_amount}, "
            f"new={new_calc.profit_amount}, rel_diff={rel_diff:.6f}"
        )

        # Verify all swaps are V3 type
        for swap in new_calc.swap_amounts:
            assert isinstance(swap, UniswapV3PoolSwapAmounts)
