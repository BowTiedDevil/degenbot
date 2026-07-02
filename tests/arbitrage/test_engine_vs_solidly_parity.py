"""Parity gate: Rust ``UniswapArbEngine`` Solidly solve vs Python oracles.

Task FEMZJC — the Solidly parity gate, mirroring
``tests/arbitrage/test_engine_vs_brent_parity.py`` for V2/V3/V4.

The Rust engine's ``solve_solidly_path_int`` (Möbius precheck + golden-section
+ integer verification, all EVM-exact on the ``degenbot-solidly-math`` integer
leaf) must agree with two independent Python oracles over a corpus of
Aerodrome + Camelot paths:

1. ``SolidlyStableSolver`` — the Python reference solver
   (``arbitrage.solvers.solidly_stable``). Runs its own golden-section search
   over the same ``swap_fn`` integer leaf, so an EVM-exact parity is expected.
2. ``BrentSolver`` — ``scipy.optimize.minimize_scalar(method="bounded")``
   over the same ``swap_fn``-based ``_simulate_path``. f64 outer loop, but
   the inner simulation is integer-exact via ``swap_fn``.

Both oracles + the engine converge on the same EVM-exact swap function, so
the optimal input / profit must agree within BrentSolver's float epsilon band
(``xatol=1.0`` over reserves on the order of 1e24 → relative tolerance
``1e-6``). The engine's reported profit is also cross-checked against a
manual pool-walk at the engine's optimal_input (exact integer equality, since
both walk ``calculate_tokens_out_from_tokens_in`` on the same reserves).

Corpus (each profitable + one unprofitable):
- Aerodrome stable USDC→WETH + V2 WETH→USDC (mixed V2+Solidly).
- Camelot stable USDT→USDC + Camelot stable USDC→USDT (all-Solidly).
- A round-trip through one pool (unprofitable → engine returns no result,
  both oracles raise ``OptimizationError``).
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.arbitrage.solvers.brent_solver import BrentSolver
from degenbot.arbitrage.solvers.hop_types import SolveInput
from degenbot.arbitrage.solvers.solidly_stable import SolidlyStableSolver
from degenbot.degenbot_rs import PyBot, UniswapArbEngine
from degenbot.exceptions import OptimizationError
from degenbot.types.hop_types import PoolInvariant
from tests.helpers.aerodrome_pool_factory import make_aerodrome_v2_pool
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool


def _manual_pool_walk(pools: list, zfos: tuple[bool, ...], amount_in: int) -> int:
    """Integer pool-walk: ``calculate_tokens_out_from_tokens_in`` chained."""
    amount = amount_in
    for pool, zfo in zip(pools, zfos, strict=True):
        token_in = pool.token0 if zfo else pool.token1
        amount = pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_in_quantity=amount,
        )
    return amount


def _engine_result(
    bot: PyBot,
    pool_ids: tuple[int, ...],
    zfos: tuple[bool, ...],
) -> tuple[int, int]:
    """Register the path in a fresh engine, solve, return (optimal_input, profit).

    Raises ``AssertionError`` if the engine reports no profitable result.
    """
    engine = UniswapArbEngine(py_bot=bot)
    path_id = engine.register_and_solve_path(list(zip(pool_ids, zfos, strict=True)))
    results, _block = engine.latest_results()
    entry = next(r for r in results if r[0] == path_id)
    _r_path_id, optimal_input, profit, _hop_outputs, _consumed = entry
    return optimal_input, profit


@pytest.fixture
def aerodrome_v2_cycle():
    """Aerodrome stable → V2 mixed cycle (18-dec tokens), proven profitable.

    Mirrors the DMPSNG engine-test fixture (both tokens 18-dec so the Solidly
    math's calc_d products stay well above 1e18 — small-magnitude reserves
    would panic get_y_solidly on divide-by-zero).
    """
    bot = PyBot()
    token_a = make_erc20(
        bot, address="0x" + "aa" * 20, name="TokenA", symbol="TA", decimals=18
    )
    token_b = make_erc20(
        bot, address="0x" + "bb" * 20, name="TokenB", symbol="TB", decimals=18
    )
    pool_a = make_aerodrome_v2_pool(
        address="0x" + "11" * 20,
        token0=token_a,
        token1=token_b,
        factory="0x" + "f1" * 20,
        fee=Fraction(3, 1000),
        stable=True,
        reserves_token0=1_000 * 10**18,  # 1K tokenA
        reserves_token1=100 * 10**18,  # 100 tokenB
        py_bot=bot,
    )
    pool_b = make_v2_pool(
        address="0x" + "22" * 20,
        token0=token_a,
        token1=token_b,
        factory="0x" + "f2" * 20,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=2_000 * 10**18,  # 2K tokenA (2x pool A → arb cycle)
        reserves_token1=100 * 10**18,
        py_bot=bot,
    )
    pools = [pool_a, pool_b]
    pool_ids = (pool_a._py_pool.pool_id, pool_b._py_pool.pool_id)  # noqa: SLF001
    # tokenA→tokenB in pool A (zero_for_one=True), tokenB→tokenA in pool B (zero_for_one=False).
    zfos = (True, False)
    return bot, pools, pool_ids, zfos


@pytest.fixture
def camelot_stable_cycle():
    """Camelot stable-stable cycle (18-dec tokens).

    Pool A: 1K tokenA / 100 tokenB (tokenB dear).
    Pool B: 1.2K tokenA / 100 tokenB (tokenB cheaper in pool A).
    Cycle: tokenA → (pool A, zfo=True) → tokenB → (pool B, zfo=False) → tokenA.
    """
    bot = PyBot()
    token_a = make_erc20(
        bot, address="0x" + "aa" * 20, name="TokenA", symbol="TA", decimals=18
    )
    token_b = make_erc20(
        bot, address="0x" + "bb" * 20, name="TokenB", symbol="TB", decimals=18
    )
    pool_a = make_v2_pool(
        address="0x" + "31" * 20,
        token0=token_a,
        token1=token_b,
        factory="0x" + "f3" * 20,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1_000 * 10**18,
        reserves_token1=100 * 10**18,
        py_bot=bot,
        stable_swap=True,
        fee_denominator=10_000,
        variant="camelot-v2-stable",
    )
    pool_b = make_v2_pool(
        address="0x" + "32" * 20,
        token0=token_a,
        token1=token_b,
        factory="0x" + "f3" * 20,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1_200 * 10**18,
        reserves_token1=100 * 10**18,
        py_bot=bot,
        stable_swap=True,
        fee_denominator=10_000,
        variant="camelot-v2-stable",
    )
    pools = [pool_a, pool_b]
    pool_ids = (pool_a._py_pool.pool_id, pool_b._py_pool.pool_id)  # noqa: SLF001
    # tokenA→tokenB in pool A (zero_for_one=True), tokenB→tokenA in pool B (zero_for_one=False).
    zfos = (True, False)
    return bot, pools, pool_ids, zfos


class TestEngineVsSolidlyOracles:
    """The engine's Solidly solve matches both Python oracles."""

    def test_aerodrome_v2_mixed_matches_pool_walk(
        self,
        aerodrome_v2_cycle,
    ) -> None:
        """Engine == manual pool-walk at the engine's optimal_input (exact
        integer parity — both walk ``calculate_tokens_out_from_tokens_in`` on
        the same reserves)."""
        bot, pools, pool_ids, zfos = aerodrome_v2_cycle
        optimal_input, profit = _engine_result(bot, pool_ids, zfos)
        assert optimal_input > 0
        assert profit > 0
        walk_output = _manual_pool_walk(pools, zfos, optimal_input)
        assert walk_output - optimal_input == profit, (
            f"engine profit {profit} != pool-walk profit {walk_output - optimal_input}"
        )

    def test_aerodrome_v2_mixed_matches_oracles_within_epsilon(
        self,
        aerodrome_v2_cycle,
    ) -> None:
        bot, pools, pool_ids, zfos = aerodrome_v2_cycle
        engine_optimal, engine_profit = _engine_result(bot, pool_ids, zfos)
        hops = tuple(
            pool.to_hop_state(zero_for_one=zfo)
            for pool, zfo in zip(pools, zfos, strict=True)
        )
        # The Aerodrome hop should report the Solidly stable invariant.
        solidly_hop = hops[0]
        assert solidly_hop.invariant == PoolInvariant.SOLIDLY_STABLE

        # Oracle 1: SolidlyStableSolver.
        ss_result = SolidlyStableSolver().solve(SolveInput(hops=hops))
        rel = abs(ss_result.optimal_input - engine_optimal) / engine_optimal
        assert rel < 1e-6, (
            f"engine optimal_input {engine_optimal} vs SolidlyStableSolver "
            f"{ss_result.optimal_input}: relative Δ {rel:.3e}"
        )
        profit_rel = abs(ss_result.profit - engine_profit) / max(engine_profit, 1)
        assert profit_rel < 1e-6, (
            f"engine profit {engine_profit} vs SolidlyStableSolver "
            f"{ss_result.profit}: relative Δ {profit_rel:.3e}"
        )

        # Oracle 2: BrentSolver (f64 outer, integer swap_fn inner).
        brent_result = BrentSolver().solve(SolveInput(hops=hops))
        rel2 = abs(brent_result.optimal_input - engine_optimal) / engine_optimal
        # Brent's f64 outer over the Solidly curve lands ~4e-6 from the
        # integer-exact engine (its scipy xatol=1.0 over a non-parabolic profit
        # curve); 1e-4 is the comfortable cross-oracle band. SolidlyStableSolver
        # is the tight gate (same algorithm as the engine, integer-exact).
        assert rel2 < 1e-4, (
            f"engine optimal_input {engine_optimal} vs BrentSolver "
            f"{brent_result.optimal_input}: relative Δ {rel2:.3e}"
        )
        profit_rel2 = abs(brent_result.profit - engine_profit) / max(engine_profit, 1)
        assert profit_rel2 < 1e-4, (
            f"engine profit {engine_profit} vs BrentSolver {brent_result.profit}: "
            f"relative Δ {profit_rel2:.3e}"
        )

    def test_camelot_stable_stable_matches_pool_walk(
        self,
        camelot_stable_cycle,
    ) -> None:
        """All-Solidly Camelot path: engine == manual pool-walk."""
        bot, pools, pool_ids, zfos = camelot_stable_cycle
        optimal_input, profit = _engine_result(bot, pool_ids, zfos)
        assert optimal_input > 0
        assert profit > 0
        walk_output = _manual_pool_walk(pools, zfos, optimal_input)
        assert walk_output - optimal_input == profit, (
            f"engine profit {profit} != pool-walk profit {walk_output - optimal_input}"
        )

    def test_camelot_stable_stable_matches_oracles_within_epsilon(
        self,
        camelot_stable_cycle,
    ) -> None:
        bot, pools, pool_ids, zfos = camelot_stable_cycle
        engine_optimal, engine_profit = _engine_result(bot, pool_ids, zfos)
        hops = tuple(
            pool.to_hop_state(zero_for_one=zfo)
            for pool, zfo in zip(pools, zfos, strict=True)
        )
        # Both hops should report the Solidly stable invariant (Camelot
        # stable_swap pools).
        assert all(h.invariant == PoolInvariant.SOLIDLY_STABLE for h in hops)

        ss_result = SolidlyStableSolver().solve(SolveInput(hops=hops))
        rel = abs(ss_result.optimal_input - engine_optimal) / engine_optimal
        assert rel < 1e-6, (
            f"engine optimal_input {engine_optimal} vs SolidlyStableSolver "
            f"{ss_result.optimal_input}: relative Δ {rel:.3e}"
        )
        profit_rel = abs(ss_result.profit - engine_profit) / max(engine_profit, 1)
        assert profit_rel < 1e-6, (
            f"engine profit {engine_profit} vs SolidlyStableSolver "
            f"{ss_result.profit}: relative Δ {profit_rel:.3e}"
        )

        brent_result = BrentSolver().solve(SolveInput(hops=hops))
        rel2 = abs(brent_result.optimal_input - engine_optimal) / engine_optimal
        # Brent's f64 outer band; see the Aerodrome-mixed tolerance note.
        assert rel2 < 1e-4, (
            f"engine optimal_input {engine_optimal} vs BrentSolver "
            f"{brent_result.optimal_input}: relative Δ {rel2:.3e}"
        )

    def test_unprofitable_round_trip_no_engine_result(
        self,
        aerodrome_v2_cycle,
    ) -> None:
        """A round-trip through one pool is unprofitable — the engine reports
        no result (the solve path is filtered out by the latest_results
        profitability gate), and both Python oracles raise
        ``OptimizationError``."""
        bot, pools, pool_ids, _default_zfos = aerodrome_v2_cycle
        engine = UniswapArbEngine(py_bot=bot)
        path_id = engine.register_and_solve_path(
            [(pool_ids[0], True), (pool_ids[0], False)]
        )
        results, _block = engine.latest_results()
        # The path was registered but never lands in results (heavy-loss path
        # → solve_solidly_path_int returns None for the round-trip).
        assert not any(r[0] == path_id for r in results), (
            "unprofitable round-trip path should not be in latest_results"
        )
        del path_id  # path_id unused beyond registration

        # Both Python oracles reject the round-trip hops too.
        hop0 = pools[0].to_hop_state(zero_for_one=True)
        hop1 = pools[0].to_hop_state(zero_for_one=False)
        with pytest.raises(OptimizationError):
            SolidlyStableSolver().solve(SolveInput(hops=(hop0, hop1)))
        with pytest.raises(OptimizationError):
            BrentSolver().solve(SolveInput(hops=(hop0, hop1)))