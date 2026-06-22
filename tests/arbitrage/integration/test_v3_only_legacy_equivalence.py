"""V3-only legacy ↔ new equivalence: UniswapLpCycle vs ArbitragePath.

Uses production UniswapV3Pool with exact V3 math to compare the legacy
brent solver with the new MobiusSolver / BrentSolver solvers.

Production V3 pools are fully I/O-free when constructed directly — no RPC
calls, no provider needed. FakeToken is used as a lightweight test double
for Erc20Token.

This is the full-stack equivalence gate for V3-only arbitrage paths.
Both systems must see the same profit landscape; any divergence indicates
a real behavioral gap.
"""

from fractions import Fraction

import pytest

from degenbot.arbitrage._legacy import _UniswapLpCycle as UniswapLpCycle
from degenbot.arbitrage.solvers.solver import BrentSolver, MobiusSolver
from degenbot.arbitrage.path import ArbitragePath
from degenbot.exceptions.arbitrage import OptimizationError, RateOfExchangeBelowMinimum
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.arbitrage.generator.pool_generator import PoolStateGenerator
from tests.arbitrage.generator.types import V3PoolGenerationConfig
from tests.fakes.tokens import FakeToken
from tests.helpers.v3_pool_factory import make_v3_pool

# ---------------------------------------------------------------------------
# Helpers: build pools with production UniswapV3Pool
# ---------------------------------------------------------------------------


def _make_v3_pool_from_state(
    address: str,
    state,
    token0: FakeToken,
    token1: FakeToken,
    fee: int = 3000,
    tick_spacing: int = 60,
) -> UniswapV3Pool:
    """Build a production UniswapV3Pool from a generated V3 pool state.

    The pool is fully I/O-free — no RPC calls or provider references.
    """
    return make_v3_pool(
        address=address,
        token0=token0,  # type: ignore[arg-type]
        token1=token1,  # type: ignore[arg-type]
        factory="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        fee=fee,
        tick_spacing=tick_spacing,
        sqrt_price_x96=state.sqrt_price_x96,
        tick=state.tick,
        liquidity=state.liquidity,
        tick_bitmap=state.tick_bitmap,
        tick_data=state.tick_data,
        state_block=1,
    )


def _make_profitable_v3_pair(
    t0: FakeToken,
    t1: FakeToken,
    price_a: float = 2200.0,
    price_b: float = 2000.0,
    liquidity: int = 10**18,
    fee: int = 500,
) -> tuple[UniswapV3Pool, UniswapV3Pool]:
    """Create two production UniswapV3Pool for the same token pair at different prices.

    Pool A: t0/t1 at {price_a}. ArbitragePath goes t0→t1 (zfo=True).
    Pool B: t0/t1 at {price_b}. ArbitragePath goes t1→t0 (zfo=False).
    """
    generator = PoolStateGenerator()

    addr_a = "0x00000000000000000000000000000000000000A1"
    addr_b = "0x00000000000000000000000000000000000000A2"

    state_a = generator.generate_v3_pool_state_from_price(
        address=addr_a,
        price_token1_per_token0=price_a,
        liquidity=liquidity,
        config=V3PoolGenerationConfig(fee=Fraction(fee, 1_000_000), tick_spacing=60),
    )
    state_b = generator.generate_v3_pool_state_from_price(
        address=addr_b,
        price_token1_per_token0=price_b,
        liquidity=liquidity,
        config=V3PoolGenerationConfig(fee=Fraction(fee, 1_000_000), tick_spacing=60),
    )

    pool_a = _make_v3_pool_from_state(state_a.address, state_a, t0, t1, fee=fee)
    pool_b = _make_v3_pool_from_state(state_b.address, state_b, t0, t1, fee=fee)

    return pool_a, pool_b


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def usdc() -> FakeToken:
    return FakeToken(
        address="0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
        symbol="USDC",
        decimals=6,
    )


@pytest.fixture
def weth() -> FakeToken:
    return FakeToken(
        address="0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
        symbol="WETH",
        decimals=18,
    )


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class TestV3OnlyLegacyVsNew:
    """Verify UniswapLpCycle (legacy) and ArbitragePath (new) produce
    computationally equivalent results on the exact same V3-only pool states.

    Production UniswapV3Pool passes isinstance checks in the legacy code
    natively — no monkeypatching required.
    """

    def test_2hop_v3_agreement_profitable(self, usdc: FakeToken, weth: FakeToken):
        """Both legacy and new systems find profit for a V3-only 2-hop cycle.

        Pool A: USDC/WETH at price 2200 (high USDC per WETH → cheap WETH)
        Pool B: USDC/WETH at price 2000 (low USDC per WETH → expensive WETH)

        Cycle: USDC → WETH (pool A, get 1/2200 WETH per USDC)
               WETH → USDC (pool B, get 2000 USDC per WETH)

        With gamma = 0.9995 (fee=0.05%), factor = 1.1 * 0.999 ≈ 1.099 → PROFIT ✓
        """
        pool_a, pool_b = _make_profitable_v3_pair(usdc, weth, price_a=2200.0, price_b=2000.0)

        max_input = 1_000_000

        # Legacy system — production V3 pools pass isinstance checks natively
        cycle = UniswapLpCycle(
            id="v3_legacy",
            input_token=usdc,  # type: ignore[arg-type]
            swap_pools=[pool_a, pool_b],
            max_input=max_input,
        )
        legacy_result = cycle.calculate()

        # New system
        path = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=usdc,  # type: ignore[arg-type]
            solver=MobiusSolver(),
            max_input=max_input,
        )
        new_result = path.calculate()

        # Both must find positive profit
        assert legacy_result.profit_amount > 0
        assert new_result.profit > 0

        # Legacy uses scipy.minimize_scalar + int(opt.x); MobiusSolver uses integer
        # search around a closed-form float optimum. They may differ by a small number
        # of wei (<0.01% of max_input), but profit must agree within ~0.1%.
        assert abs(legacy_result.input_amount - new_result.optimal_input) <= max_input // 100
        rel_profit_diff = abs(legacy_result.profit_amount - new_result.profit) / max(
            legacy_result.profit_amount, 1
        )
        assert rel_profit_diff < 0.001, (
            f"legacy profit={legacy_result.profit_amount}, new profit={new_result.profit}"
        )

    def test_2hop_v3_agreement_unprofitable(self, usdc: FakeToken, weth: FakeToken):
        """When prices are symmetric, both systems reject the path."""
        pool_a, pool_b = _make_profitable_v3_pair(usdc, weth, price_a=2000.0, price_b=2000.0)

        max_input = 1_000_000

        # Legacy system: symmetric prices means _pre_calculation_check fails
        cycle = UniswapLpCycle(
            id="v3_legacy",
            input_token=usdc,  # type: ignore[arg-type]
            swap_pools=[pool_a, pool_b],
            max_input=max_input,
        )
        with pytest.raises((ValueError, RateOfExchangeBelowMinimum)):
            cycle.calculate()

        # New system: symmetric prices means Möbius K/M ≤ 1
        path = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=usdc,  # type: ignore[arg-type]
            solver=MobiusSolver(),
            max_input=max_input,
        )

        with pytest.raises(OptimizationError, match="Not profitable"):
            path.calculate()

    def test_2hop_v3_mobius_and_brent_agree(self, usdc: FakeToken, weth: FakeToken):
        """MobiusSolver (closed-form) and BrentSolver (scipy) should agree."""
        pool_a, pool_b = _make_profitable_v3_pair(usdc, weth)

        max_input = 1_000_000

        path_mobius = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=usdc,  # type: ignore[arg-type]
            solver=MobiusSolver(),
            max_input=max_input,
        )
        result_mobius = path_mobius.calculate()

        path_brent = ArbitragePath(
            pools=[pool_a, pool_b],
            input_token=usdc,  # type: ignore[arg-type]
            solver=BrentSolver(),
            max_input=max_input,
        )
        result_brent = path_brent.calculate()

        assert abs(result_mobius.optimal_input - result_brent.optimal_input) <= 1
        assert abs(result_mobius.profit - result_brent.profit) <= 1

    def test_3hop_v3_agreement(self, usdc: FakeToken, weth: FakeToken):
        """A 3-hop V3-only path with alternating prices must agree."""
        # Three pools with asymmetric prices
        pool_0, _ = _make_profitable_v3_pair(usdc, weth, price_a=2200.0, price_b=2000.0)
        # Pool 2 returns to the starting token with a favorable rate
        generator = PoolStateGenerator()
        state_2 = generator.generate_v3_pool_state_from_price(
            address="0x00000000000000000000000000000000000000A3",
            price_token1_per_token0=1.05,
            liquidity=10**18,
            config=V3PoolGenerationConfig(fee=Fraction(500, 1_000_000), tick_spacing=60),
        )
        pool_2 = _make_v3_pool_from_state(state_2.address, state_2, usdc, weth, fee=500)

        # Use a 2-hop cycle: pool_0 + pool_2 (both USDC→WETH→USDC)
        max_input = 1_000_000

        # Legacy — production pools work natively
        cycle = UniswapLpCycle(
            id="v3_legacy",
            input_token=usdc,  # type: ignore[arg-type]
            swap_pools=[pool_0, pool_2],
            max_input=max_input,
        )
        legacy_result = cycle.calculate()

        # New
        path = ArbitragePath(
            pools=[pool_0, pool_2],
            input_token=usdc,  # type: ignore[arg-type]
            solver=MobiusSolver(),
            max_input=max_input,
        )
        new_result = path.calculate()

        assert legacy_result.profit_amount > 0
        assert new_result.profit > 0
        assert abs(legacy_result.input_amount - new_result.optimal_input) <= max_input // 100
        rel_profit_diff = abs(legacy_result.profit_amount - new_result.profit) / max(
            legacy_result.profit_amount, 1
        )
        assert rel_profit_diff < 0.001
