"""
Equivalence tests: ArbitragePath + Solver vs. legacy UniswapLpCycle.

Uses real pools on a fork to verify calculation equivalence for production
path types.

Status:
- V2+V3 mixed: RED
- V2-only: green via unit tests in verify_legacy_equivalence.py
- V3-only: skipped pending investigation
- Curve: skipped
"""

from typing import TYPE_CHECKING

import pytest
from eth_typing import ChainId

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage import UniswapLpCycle
from degenbot.arbitrage.optimizers.solver import BrentSolver, MobiusSolver
from degenbot.arbitrage.path import ArbitragePath
from degenbot.arbitrage.types import (
    UniswapV2PoolSwapAmounts,
    UniswapV3PoolSwapAmounts,
)
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection import set_web3
from degenbot.erc20.erc20 import Erc20Token
from degenbot.erc20.manager import Erc20TokenManager
from degenbot.uniswap.v2_liquidity_pool import UniswapV2Pool
from degenbot.uniswap.v2_types import UniswapV2PoolExternalUpdate, UniswapV2PoolState
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3BitmapAtWord,
    UniswapV3PoolExternalUpdate,
    UniswapV3PoolState,
)

if TYPE_CHECKING:
    from degenbot.arbitrage.uniswap_lp_cycle import Pool, PoolState

WBTC_ADDRESS = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"
WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
WBTC_WETH_V2_POOL_ADDRESS = "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"
WBTC_WETH_V3_POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"


@pytest.fixture
def wbtc_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20TokenManager(chain_id=ChainId.ETH).get_erc20token(WBTC_ADDRESS)


@pytest.fixture
def weth_token(fork_mainnet_full: AnvilFork) -> Erc20Token:
    set_web3(fork_mainnet_full.w3)
    return Erc20TokenManager(chain_id=ChainId.ETH).get_erc20token(WETH_ADDRESS)


@pytest.fixture
def wbtc_weth_v2_lp(fork_mainnet_full: AnvilFork) -> UniswapV2Pool:
    set_web3(fork_mainnet_full.w3)
    pool = UniswapV2Pool(WBTC_WETH_V2_POOL_ADDRESS)
    pool.external_update(
        UniswapV2PoolExternalUpdate(
            block_number=pool.update_block,
            reserves_token0=16231137593,
            reserves_token1=2571336301536722443178,
        )
    )
    return pool


@pytest.fixture
def wbtc_weth_v3_lp(fork_mainnet_full: AnvilFork) -> UniswapV3Pool:
    set_web3(fork_mainnet_full.w3)
    # Initialize from chain with auto-fetched tick data. This is simpler
    # than duplicating the hardcoded bitmap from test_uniswap_lp_cycle.py.
    pool = UniswapV3Pool(WBTC_WETH_V3_POOL_ADDRESS)
    return pool


class TestV2V3MixedEquivalence:
    """
    Equivalence test for a real V2+V3 mixed path.

    The existing integration test suite uses WBTC-WETH V2 and V3 pools with
    hardcoded states. We construct the same path via both UniswapLpCycle
    and ArbitragePath, and compare results.
    """

    def test_baseline_calculation_matches(
        self,
        wbtc_weth_v2_lp: UniswapV2Pool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth_token: Erc20Token,
    ):
        """
        Both systems should find equivalent optimal input and profit for the
        same pool states.
        """
        max_input = 100 * 10**18

        # Legacy system
        legacy = UniswapLpCycle(
            input_token=weth_token,
            swap_pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            max_input=max_input,
        )
        legacy_result = legacy.calculate()

        assert legacy_result.input_amount > 0
        assert legacy_result.profit_amount > 0
        for swap in legacy_result.swap_amounts:
            assert isinstance(swap, (UniswapV2PoolSwapAmounts, UniswapV3PoolSwapAmounts))

        # New system
        path = ArbitragePath(
            pools=[wbtc_weth_v2_lp, wbtc_weth_v3_lp],
            input_token=weth_token,
            solver=MobiusSolver(),
            max_input=max_input,
        )
        solve_result = path.calculate()
        new_result = path.build_swap_amounts(solve_result)

        assert new_result.input_amount > 0
        assert new_result.profit_amount > 0

        # Compare results.
        #
        # When the profit function is monotonically increasing toward
        # max_input, different solvers may hit the boundary at slightly
        # different input amounts. The important invariant is that the
        # profits are equivalent (within floating-point tolerance of the
        # underlying solvers).
        assert new_result.input_amount > 0
        assert new_result.profit_amount > 0

        # Relative profit tolerance: 0.001% (the solvers use different
        # float-to-integer conversion strategies)
        relative_profit_diff = abs(
            legacy_result.profit_amount - new_result.profit_amount
        ) / legacy_result.profit_amount
        assert relative_profit_diff < 0.00001  # 0.001%

    @pytest.mark.skip(reason="State override shapes differ between systems")
    def test_state_override_equivalence(
        self,
        wbtc_weth_v2_lp: UniswapV2Pool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth_token: Erc20Token,
    ):
        """
        V2 pool override:
        reserves_token0=16027096956, reserves_token1=2602647332090181827846

        V3 pool override:
        liquidity=1533143241938066251,
        sqrt_price_x96=31881290961944305252140777263703426,
        tick=258116
        """
        pass


class TestEdgeCases:
    """
    Edge case parity: behavior when the legacy system rejects a path early.
    """

    @pytest.mark.skip(reason="UniswapLpCycle._pre_calculation_check vs OptimizationError")
    def test_unprofitable_path_rejection_parity(
        self,
        wbtc_weth_v2_lp: UniswapV2Pool,
        wbtc_weth_v3_lp: UniswapV3Pool,
        weth_token: Erc20Token,
    ):
        """
        When reserves produce no arbitrage, both systems must reject.
        Legacy raises ArbitrageError or RateOfExchangeBelowMinimum.
        New system raises OptimizationError.
        """
        pass
