"""BrentSolver profitability tests (ACDWOC port).

Verifies that ``BrentSolver.solve()`` (the kept SciPy `minimize_scalar`
reference oracle) produces results consistent with a manual pool walk for
V2-V2, V2-V3, and V3-V3 pool configurations.

Previously these tests exercised ``ArbSolver`` / ``MobiusSolver`` /
``ArbitragePath`` (the f64 Möbius dispatcher + event-driven path helper).
ACDWOC retired that stack — the Rust ``UniswapArbEngine`` is the production
solve surface (cross-validated against ``BrentSolver`` in
``test_engine_vs_brent_parity.py``), and ``BrentSolver`` is the kept Python
reference oracle + the production solver for Solidly/Curve/Balancer families.

The two oracle considerations:
- ``BrentSolver.solve`` returns ``SolveResult(optimal_input, profit, ...)``
  directly — the ``ArbitragePath.calculate`` indirection is gone; the tests
  build ``SolveInput(hops, max_input)`` directly from the same ``to_hop_state``
  calls ``ArbitragePath`` used internally.
- The manual pool-walk check (per-pool
  ``calculate_tokens_out_from_tokens_in`` chained) is unchanged — it remains
  the ground-truth profitability gate.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.arbitrage.solvers.brent_solver import BrentSolver
from degenbot.arbitrage.solvers.hop_types import SolveInput
from degenbot.degenbot_rs import PyBot
from degenbot.erc20.erc20 import Erc20Token
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from tests.arbitrage.generator.pool_generator import PoolStateGenerator
from tests.arbitrage.generator.types import V3PoolGenerationConfig
from tests.fakes.tokens import FakeToken
from tests.helpers.aerodrome_pool_factory import make_aerodrome_v2_pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool
from tests.helpers.v3_pool_factory import make_v3_pool

_PY_BOT = PyBot()


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

    Pool A: t0/t1 at {price_a}. Cycle goes t0→t1 (zfo=True).
    Pool B: t0/t1 at {price_b}. Cycle goes t1→t0 (zfo=False).
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
# Unused fixtures and class preserved (kept for selectsolidly-stable parity suite importers)
# ---------------------------------------------------------------------------
_USDC_ADDR = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
_WETH_ADDR = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
_FACTORY = "0x5C69bEe701ef814E44274f655e7632cB715C14B6"


@pytest.fixture
def usdc() -> Erc20Token:
    return make_erc20(
        _PY_BOT,
        address=_USDC_ADDR,
        name="USD Coin",
        symbol="USDC",
        decimals=6,
    )


@pytest.fixture
def usdt() -> Erc20Token:
    return make_erc20(
        _PY_BOT,
        address="0xdAC17F958D2ee523a2206206994597C13D831ec7",
        name="Tether USD",
        symbol="USDT",
        decimals=6,
    )


@pytest.fixture
def aerodrome_stable_pool(usdc: Erc20Token, usdt: Erc20Token) -> AerodromeV2Pool:
    return make_aerodrome_v2_pool(
        address="0x1234567890123456789012345678901234567890",
        token0=usdc,
        token1=usdt,
        factory="0xABCDEF1234567890ABCDEF1234567890ABCDEF12",
        fee=Fraction(1, 1000),
        stable=True,
        reserves_token0=10_000_000 * 10**6,
        reserves_token1=10_020_000 * 10**6,
    )


# ---------------------------------------------------------------------------
# Tests: BrentSolver vs manual pool walk for V2-V2, V2-V3, V3-V3 cycles
# ---------------------------------------------------------------------------


class TestBrentSolverParityWithLpCycle:
    """``BrentSolver.solve()`` should produce optimal input amounts whose
    profit is confirmed by a manual pool walk through the same pool sequence.

    ACDWOC: this is the kept Python reference oracle (SciPy minimize_scalar
    over ``hop.swap_fn`` via ``_simulate_path``); the f64 Möbius dispatcher
    (``ArbSolver``) and the event-driven path wrapper (``ArbitragePath``) are
    retired — the engine is the production solve surface, cross-validated
    against this oracle in ``test_engine_vs_brent_parity.py``.
    """

    def test_v2_v2_pair_parity(self):
        """BrentSolver should find profitable V2-V2 arbitrage."""
        usdc = make_erc20(
            _PY_BOT,
            address=_USDC_ADDR,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
        )
        weth = make_erc20(
            _PY_BOT,
            address=_WETH_ADDR,
            name="Wrapped Ether",
            symbol="WETH",
            decimals=18,
        )

        pool_a = make_v2_pool(
            address="0xAAAA1111222233334444555566667777888899a1",
            token0=usdc,
            token1=weth,
            factory=_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_000_000 * 10**6,
            reserves_token1=1_000 * 10**18,
        )
        pool_b = make_v2_pool(
            address="0xBBBB1111222233334444555566667777888899b2",
            token0=usdc,
            token1=weth,
            factory=_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_100_000 * 10**6,
            reserves_token1=1_000 * 10**18,
        )

        solver = BrentSolver()
        hops = (
            pool_a.to_hop_state(zero_for_one=True),
            pool_b.to_hop_state(zero_for_one=False),
        )
        solve_input = SolveInput(hops=hops, max_input=1_000 * 10**6)
        solver_result = solver.solve(solve_input)

        assert solver_result.profit > 0
        # Verify by walking through pools manually
        out_a = pool_a.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=solver_result.optimal_input,
        )
        out_b = pool_b.calculate_tokens_out_from_tokens_in(
            token_in=weth,
            token_in_quantity=out_a,
        )
        assert out_b > solver_result.optimal_input

    def test_v2_v3_pair_parity(self):
        """BrentSolver should find profitable V2-V3 arbitrage."""
        usdc = FakeToken(
            address=_USDC_ADDR,
            symbol="USDC",
            decimals=6,
        )
        weth = FakeToken(
            address=_WETH_ADDR,
            symbol="WETH",
            decimals=18,
        )

        v2_pool = make_v2_pool(
            address="0xAAAA1111222233334444555566667777888899a1",
            token0=make_erc20(
                _PY_BOT,
                address=_USDC_ADDR,
                name="USD Coin",
                symbol="USDC",
                decimals=6,
            ),
            token1=make_erc20(
                _PY_BOT,
                address=_WETH_ADDR,
                name="Wrapped Ether",
                symbol="WETH",
                decimals=18,
            ),
            factory=_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_000_000 * 10**6,
            reserves_token1=1_000 * 10**18,
        )

        _v3_pool_a, v3_pool_b = _make_profitable_v3_pair(usdc, weth, price_a=2200.0, price_b=2000.0)

        max_input = 1_000_000

        # ACDWOC: solve directly via BrentSolver + SolveInput, replacing the
        # ArbitragePath wrapper.
        hops = (
            v2_pool.to_hop_state(zero_for_one=True),
            v3_pool_b.to_hop_state(zero_for_one=False),
        )
        result = BrentSolver().solve(SolveInput(hops=hops, max_input=max_input))

        # Verify profit via manual pool walk
        assert result.profit > 0
        token_in_qty = result.optimal_input
        # Walk V2 pool
        v2_out = v2_pool.calculate_tokens_out_from_tokens_in(
            token_in=v2_pool.token0,
            token_in_quantity=token_in_qty,
        )
        # Walk V3 pool
        v3_out = v3_pool_b.calculate_tokens_out_from_tokens_in(
            token_in=v3_pool_b.token1,
            token_in_quantity=v2_out,
        )
        manual_profit = v3_out - token_in_qty
        assert manual_profit > 0, "Pool walk must show positive profit"

    def test_v3_v3_pair_parity(self):
        """BrentSolver should find profitable V3-V3 arbitrage."""
        usdc = FakeToken(
            address=_USDC_ADDR,
            symbol="USDC",
            decimals=6,
        )
        weth = FakeToken(
            address=_WETH_ADDR,
            symbol="WETH",
            decimals=18,
        )

        pool_a, pool_b = _make_profitable_v3_pair(usdc, weth, price_a=2200.0, price_b=2000.0)
        max_input = 1_000_000

        # ACDWOC: solve via BrentSolver + SolveInput (was ArbitragePath +
        # MobiusSolver).
        hops = (
            pool_a.to_hop_state(zero_for_one=True),
            pool_b.to_hop_state(zero_for_one=False),
        )
        result = BrentSolver().solve(SolveInput(hops=hops, max_input=max_input))
        assert result.profit > 0

        # Verify profit by walking pools manually
        token_in_qty = result.optimal_input
        out_a = pool_a.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=token_in_qty,
        )
        out_b = pool_b.calculate_tokens_out_from_tokens_in(
            token_in=weth,
            token_in_quantity=out_a,
        )
        manual_profit = out_b - token_in_qty
        assert manual_profit == result.profit, (
            f"Pool walk profit {manual_profit} != solver profit {result.profit}"
        )
        assert manual_profit > 0

    def test_v2_v2_profit_verified_by_pool_walk(self):
        """Walk V2 pools manually to verify the solver's optimal input yields real profit."""
        usdc = make_erc20(
            _PY_BOT,
            address=_USDC_ADDR,
            name="USD Coin",
            symbol="USDC",
            decimals=6,
        )
        weth = make_erc20(
            _PY_BOT,
            address=_WETH_ADDR,
            name="Wrapped Ether",
            symbol="WETH",
            decimals=18,
        )

        pool_a = make_v2_pool(
            address="0xAAAA1111222233334444555566667777888899a1",
            token0=usdc,
            token1=weth,
            factory=_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_000_000 * 10**6,
            reserves_token1=1_000 * 10**18,
        )
        pool_b = make_v2_pool(
            address="0xBBBB1111222233334444555566667777888899b2",
            token0=usdc,
            token1=weth,
            factory=_FACTORY,
            fee_token0=Fraction(3, 1000),
            fee_token1=Fraction(3, 1000),
            reserves_token0=2_100_000 * 10**6,
            reserves_token1=1_000 * 10**18,
        )

        solver = BrentSolver()
        hops = (
            pool_a.to_hop_state(zero_for_one=True),
            pool_b.to_hop_state(zero_for_one=False),
        )
        solve_input = SolveInput(hops=hops, max_input=1_000 * 10**6)
        result = solver.solve(solve_input)

        # Walk through pools manually to verify profit
        token_in_qty = result.optimal_input
        out_a = pool_a.calculate_tokens_out_from_tokens_in(
            token_in=usdc,
            token_in_quantity=token_in_qty,
        )
        out_b = pool_b.calculate_tokens_out_from_tokens_in(
            token_in=weth,
            token_in_quantity=out_a,
        )
        manual_profit = out_b - token_in_qty
        assert manual_profit == result.profit, (
            f"Pool walk profit {manual_profit} != result profit {result.profit}"
        )
        assert manual_profit > 0
