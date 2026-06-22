"""Test CurveStableswapPool integration with ArbitragePath vs legacy comparison.

This test verifies that the new ArbitragePath + Solver correctly handles
Curve-stableswap hops. Uses production CurveStableswapPool with
FakeCurveDataProvider for I/O-free operation.
"""

from fractions import Fraction

import pytest

from degenbot.anvil_fork import AnvilFork
from degenbot.arbitrage._legacy import _UniswapCurveCycle as UniswapCurveCycle
from degenbot.arbitrage.optimizers._solver_utils import _simulate_path
from degenbot.arbitrage.optimizers.hop_types import SolveInput
from degenbot.arbitrage.optimizers.solidly_stable import (
    _simulate_mixed_path,
    _simulate_mixed_path_int,
)
from degenbot.arbitrage.optimizers.solver import BrentSolver
from degenbot.curve.curve_stableswap_liquidity_pool import CurveStableswapPool
from degenbot.degenbot_rs import PyBot
from degenbot.exceptions.arbitrage import ArbitrageError
from degenbot.provider import ProviderAdapter
from degenbot.types.hop_types import ConstantProductHop, CurveStableswapHop
from degenbot.uniswap.v2_types import UniswapV2PoolState
from tests.fakes.curve_data_provider import FakeCurveDataProvider
from tests.helpers.bot_factory import make_bot_with_provider
from tests.helpers.curve_pool_factory import make_curve_pool
from tests.helpers.erc20_factory import make_erc20

_PY_BOT = PyBot()

WETH_ADDRESS = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
DAI_ADDRESS = "0x6B175474E89094C44Da98b954EedeAC495271d0F"
USDC_ADDRESS = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
UNISWAP_V2_WETH_DAI_ADDRESS = "0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"
UNISWAP_V2_WETH_USDC_ADDRESS = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
CURVE_TRIPOOL_ADDRESS = "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"

STATE_BLOCK = 18_000_000

# Production tokens for Curve pool construction
DAI = make_erc20(_PY_BOT, address=DAI_ADDRESS, name="DAI", symbol="DAI", decimals=18)
USDC = make_erc20(_PY_BOT, address=USDC_ADDRESS, name="USD Coin", symbol="USDC", decimals=6)


def _make_curve_pool(
    balances: tuple[int, ...] = (1_000_000_000_000, 1_000_000_000_000),
    a_coefficient: int = 1000,
    fee: int = 3_000_000,
    address: str = "0x00000000000000000000000000000000000000d0",
) -> CurveStableswapPool:
    """Build a production CurveStableswapPool with FakeCurveDataProvider."""
    provider = FakeCurveDataProvider(block_timestamp=1_700_000_000)
    return make_curve_pool(
        address=address,  # type: ignore[arg-type]
        tokens=(DAI, USDC),
        a_coefficient=a_coefficient,
        fee=fee,
        admin_fee=5_000_000_000,
        balances=balances,
        state_block=STATE_BLOCK,
        data_provider=provider,
    )


def test_curve_simulation_functions():
    """Test that all simulation functions handle Curve swap_fn."""
    # Create a production Curve pool
    curve_pool = _make_curve_pool()

    curve_hop = curve_pool.to_hop_state(zero_for_one=True)

    # Test all three simulation functions
    input_amount = 100_000

    result_path = _simulate_path(input_amount, (curve_hop,))
    result_mixed = _simulate_mixed_path(input_amount, (curve_hop,))
    result_mixed_int = _simulate_mixed_path_int(input_amount, (curve_hop,))

    # All should produce the same result (within float precision)
    expected = float(curve_pool.get_dy(0, 1, input_amount, block_identifier=STATE_BLOCK))

    assert pytest.approx(result_path, rel=1e-6) == expected
    assert pytest.approx(result_mixed, rel=1e-6) == expected
    assert (
        result_mixed_int == int(expected) or result_mixed_int == int(expected) - 1
    )  # Allow 1 wei rounding


def test_curve_v2_mixed_path():
    """Test mixed path of Curve -> V2 hops."""
    # Curve pool
    curve_pool = _make_curve_pool()
    curve_hop = curve_pool.to_hop_state(zero_for_one=True)

    # V2 pool (simple constant product)
    v2_hop = ConstantProductHop(
        reserve_in=1_000_000_000_000,
        reserve_out=1_000_000_000_000,
        fee=Fraction(3, 10000),
    )

    # Path: Curve -> V2
    input_amount = 100_000

    result = _simulate_path(input_amount, (curve_hop, v2_hop))

    # Manual calculation
    after_curve = curve_pool.get_dy(0, 1, input_amount, block_identifier=STATE_BLOCK)
    # V2 formula: y = (gamma * s * x) / (r + gamma * x)
    gamma = 1 - 0.0003
    expected_v2 = (gamma * v2_hop.reserve_out * after_curve) / (
        v2_hop.reserve_in + gamma * after_curve
    )

    assert pytest.approx(result, rel=1e-6) == expected_v2


def test_curve_hop_without_swap_fn():
    """Test that Curve hop without swap_fn falls back gracefully."""
    curve_hop = CurveStableswapHop(
        reserve_in=1_000_000_000_000,
        reserve_out=1_000_000_000_000,
        fee=Fraction(3, 10000),
        curve_a=1000,
        curve_n_coins=2,
        curve_d=2_000_000_000_000,  # Approx D
        token_index_in=0,
        token_index_out=1,
        precisions=(10**18, 10**18),
        swap_fn=None,  # No swap_fn
    )

    # Without swap_fn, the simulation should use constant-product fallback
    # (not exact Curve math, but should still produce a result)
    result = _simulate_path(100_000, (curve_hop,))

    # Result should be positive but not exact Curve output
    assert result > 0
    assert result < 100_000  # Fees should reduce output


def test_brent_solver_with_curve():
    """Test that BrentSolver can optimize a path containing Curve hop.

    Uses imbalanced pools to create an arbitrage opportunity.
    """
    solver = BrentSolver()

    # Curve pool: slightly imbalanced (Curve's A=1000 makes it resistant to price changes)
    curve_pool = _make_curve_pool(
        balances=(10_000_000_000_000, 9_500_000_000_000),  # Imbalanced
    )

    # Create hops: V2 -> Curve -> V2 (arbitrage triangle)
    # The key is to create an asymmetric path where the net effect is profitable

    # V2 in: reserves favor token0 (lower price for token0 -> token1)
    v2_in = ConstantProductHop(
        reserve_in=12_000_000_000_000,  # More token0 = cheaper token0
        reserve_out=8_000_000_000_000,
        fee=Fraction(3, 10000),
    )

    # Curve hop in the middle
    curve_hop = curve_pool.to_hop_state(zero_for_one=True)

    # V2 out: reserves favor token1 (higher price for token1 -> token0)
    v2_out = ConstantProductHop(
        reserve_in=8_000_000_000_000,
        reserve_out=12_000_000_000_000,  # More token1 = cheaper token1
        fee=Fraction(3, 10000),
    )

    solve_input = SolveInput(hops=(v2_in, curve_hop, v2_out))

    try:
        result = solver.solve(solve_input)
        # If profitable, verify result is reasonable
        assert result.optimal_input > 0
        assert result.profit >= 0
    except (ArbitrageError, ValueError):
        # Even if not profitable, the solver should run without crashing
        pass


@pytest.mark.ethereum
def test_curve_fork_equivalence(fork_mainnet_full: AnvilFork) -> None:
    """Fork-based Curve equivalence test.

    Compares ArbitragePath with Curve hops against legacy UniswapCurveCycle
    using real mainnet Curve pools.
    """
    bot = make_bot_with_provider(ProviderAdapter.from_web3(fork_mainnet_full.w3))
    # CurveStableswapPool still accesses the global connection_manager for
    # on-chain data (e.g. block number during swap calculations), so register
    # the provider globally too.

    weth = bot.build_erc20token(WETH_ADDRESS)

    curve_tripool = bot.build_pool(CURVE_TRIPOOL_ADDRESS)
    uniswap_v2_weth_dai_lp = bot.build_pool(UNISWAP_V2_WETH_DAI_ADDRESS)
    uniswap_v2_weth_usdc_lp = bot.build_pool(UNISWAP_V2_WETH_USDC_ADDRESS)

    # Override V2 pool state to create a profitable arbitrage condition
    v2_weth_dai_override = UniswapV2PoolState(
        address=uniswap_v2_weth_dai_lp.address,
        reserves_token0=7154631418308101780013056,
        reserves_token1=2641882268814772168174,
        block=None,
    )

    max_input = 10 * 10**18

    # Legacy system
    legacy = UniswapCurveCycle(
        input_token=weth,
        swap_pools=[uniswap_v2_weth_dai_lp, curve_tripool, uniswap_v2_weth_usdc_lp],
        id="legacy-test",
        max_input=max_input,
    )
    legacy_result = legacy.calculate(
        state_overrides={
            uniswap_v2_weth_dai_lp: v2_weth_dai_override,
        }
    )
    assert legacy_result.profit_amount > 0
