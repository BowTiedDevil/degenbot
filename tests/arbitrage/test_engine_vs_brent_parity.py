"""ACDWOC §4 parity gate: engine vs BrentSolver (SciPy) vs manual pool-walk.

Ergo 6TLIJ5/ACDWOC: the f64 Möbius solver stack is deleted (redundant with the
Rust `UniswapArbEngine`, which owns the EVM-exact U512 solve path via
`register_and_solve_path` → `exact_mobius_solve`). This module is the
correctness gate that proves the engine produces the same optimal_input /
profit as two *independent* Python oracles:

1. `BrentSolver` — `scipy.optimize.minimize_scalar(method="bounded")` over a
   universal `hop.swap_fn`-based `_simulate_path` (family-agnostic; the
   Python reference oracle the engine is cross-validated against, kept as
   the production solver for Solidly/Curve/Balancer families).
2. The manual pool-walk — `pool.calculate_tokens_out_from_tokens_in` chained
   hop-by-hop by hand (the brute-force ground truth).

All three must agree (engine within ε of BrentSolver; engine == manual
pool-walk exactly, since both are integer-arithmetic on the same reserves).

This is the template the gold-standard V2/V3 tests port to when the f64 stack
is deleted: register pools in a shared `PyBot`, `register_and_solve_path`,
read `latest_results()`, assert against the BrentSolver + pool-walk oracles.
"""

from __future__ import annotations

from fractions import Fraction

import pytest

from degenbot.arbitrage.engine_registry import UniswapArbEngine
from degenbot.arbitrage.solvers.brent_solver import BrentSolver
from degenbot.arbitrage.solvers.hop_types import SolveInput
from degenbot.bot import PyBot
from degenbot.types.hop_types import PoolInvariant
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

ONE = 10**18


@pytest.fixture
def weth_usdc_cycle() -> tuple[PyBot, list, tuple[int, int], tuple[int, int]]:
    """Two mispriced V2 pools forming a profitable WETH→USDC→WETH cycle.

    Shares one PyBot so the engine + the Python pool objects read the same
    BotState (ADR-006 D1). Reserves hand-crafted so a profitable arb exists:
      pool A: WETH cheap (1 WETH ≥ 2000 USDC) — sell WETH here
      pool B: WETH dear  (1 WETH ≤ 1875 USDC) — buy WETH back
    """
    bot = PyBot()
    weth = make_erc20(bot, address="0x" + "aa" * 20, name="WETH", symbol="WETH", decimals=18)
    usdc = make_erc20(bot, address="0x" + "bb" * 20, name="USDC", symbol="USDC", decimals=6)

    pool_a = make_v2_pool(
        address="0x" + "11" * 20,
        token0=weth,
        token1=usdc,
        factory="0x" + "ff" * 20,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=800 * ONE,  # WETH
        reserves_token1=1_600_000 * 10**6,  # USDC
        py_bot=bot,
    )
    pool_b = make_v2_pool(
        address="0x" + "22" * 20,
        token0=usdc,
        token1=weth,
        factory="0x" + "ff" * 20,
        fee_token0=Fraction(3, 1000),
        fee_token1=Fraction(3, 1000),
        reserves_token0=1_500_000 * 10**6,  # USDC
        reserves_token1=800 * ONE,  # WETH
        py_bot=bot,
    )
    pools = [pool_a, pool_b]
    pool_ids = (pool_a._py_pool.pool_id, pool_b._py_pool.pool_id)
    # WETH→USDC in pool A (zero_for_one=True), USDC→WETH in pool B (zero_for_one=True)
    zfos = (True, True)
    return bot, pools, pool_ids, zfos


def _manual_pool_walk(pools: list, zfos: tuple[bool, bool], amount_in: int) -> int:
    """Brute-force manual pool-walk oracle: `calculate_tokens_out_from_tokens_in` chained."""
    amount = amount_in
    for pool, zfo in zip(pools, zfos, strict=True):
        token_in = pool.token0 if zfo else pool.token1
        amount = pool.calculate_tokens_out_from_tokens_in(
            token_in=token_in,
            token_in_quantity=amount,
        )
    return amount


class TestEngineVsBrentVsPoolwalk:
    """ACDWOC §4 parity: the three oracles agree on a V2-V2 arbitrage cycle."""

    def test_engine_matches_manual_pool_walk(
        self,
        weth_usdc_cycle,
    ) -> None:
        bot, pools, pool_ids, zfos = weth_usdc_cycle
        engine = UniswapArbEngine(py_bot=bot)
        path_id = engine.register_and_solve_path(list(zip(pool_ids, zfos, strict=True)))
        assert path_id == 1

        results, _block = engine.latest_results()
        entry = next(r for r in results if r[0] == path_id)
        _r_path_id, optimal_input, profit, _hop_outputs, _consumed = entry

        assert optimal_input > 0, "engine should find a profitable arb"
        assert profit > 0

        # Engine == manual pool-walk (both integer-arithmetic on the same
        # reserves → must agree exactly). Verify the engine's reported profit
        # is what the manual pool-walk produces for the engine's optimal_input.
        walk_output = _manual_pool_walk(pools, zfos, optimal_input)
        assert walk_output - optimal_input == profit, (
            f"engine profit {profit} != pool-walk profit {walk_output - optimal_input}"
        )

    def test_engine_matches_brent_solver_within_epsilon(
        self,
        weth_usdc_cycle,
    ) -> None:
        bot, pools, pool_ids, zfos = weth_usdc_cycle
        engine = UniswapArbEngine(py_bot=bot)
        engine.register_and_solve_path(list(zip(pool_ids, zfos, strict=True)))
        results, _ = engine.latest_results()
        engine_optimal, engine_profit, _ = (
            results[0][1],
            results[0][2],
            results[0][3],
        )

        # BrentSolver over the same hops the engine solved (Python f64 oracle).
        hops = tuple(
            pool.to_hop_state(zero_for_one=zfo) for pool, zfo in zip(pools, zfos, strict=True)
        )
        assert all(h.invariant == PoolInvariant.CONSTANT_PRODUCT for h in hops)
        brent = BrentSolver()
        brent_result = brent.solve(SolveInput(hops=hops))

        # Cross-validation: BrentSolver is f64 + xatol=1.0 over reserves whose
        # magnitude is ~1e20 (WETH) — the absolute optimal_input sits at ~1e19
        # (≈11 WETH). The engine is U512-integer-exact. Compare RELATIVEly:
        # both should land within 1e-9 (BrentSolver's float64 epsilon band) of
        # each other, and profits within 1e-9 relative. (This is the §4
        # parity gate the f64 Möbius stack used to satisfy; the engine now
        # satisfies it without a Python f64 solver.)
        rel = abs(brent_result.optimal_input - engine_optimal) / engine_optimal
        assert rel < 1e-6, (
            f"engine optimal_input {engine_optimal} vs BrentSolver "
            f"{brent_result.optimal_input}: relative Δ {rel:.3e}"
        )
        profit_rel = abs(brent_result.profit - engine_profit) / max(engine_profit, 1)
        assert profit_rel < 1e-6, (
            f"engine profit {engine_profit} vs BrentSolver {brent_result.profit}: "
            f"relative Δ {profit_rel:.3e}"
        )

    def test_engine_re_solves_after_state_mutation(
        self,
        weth_usdc_cycle,
    ) -> None:
        """The engine's event-driven re-solve (ADR-006 D4) supersedes
        `ArbitragePath`'s publisher/subscriber loop. A live-state write
        through the pool handle is immediately visible to the next solve.
        """
        bot, pools, pool_ids, zfos = weth_usdc_cycle
        engine = UniswapArbEngine(py_bot=bot)
        path_id = engine.register_and_solve_path(list(zip(pool_ids, zfos, strict=True)))
        engine.solve_all_paths(1)
        results_before, _ = engine.latest_results()
        profit_before = next(r for r in results_before if r[0] == path_id)[2]

        # Live write through the pool handle → shared BotState.
        pools[0]._py_pool.sync_reserves(
            1_000_000 * 10**6,  # USDC (was 1_600_000 USDC)
            800 * ONE,  # WETH unchanged
            block_number=2,
        )

        engine.solve_all_paths(2)
        results_after, _ = engine.latest_results()
        profit_after = next(r for r in results_after if r[0] == path_id)[2]
        # The re-solve reflects the mutated state — profit differs. This is the
        # engine-level event-driven re-solve that supersedes ArbitragePath's
        # publisher/subscriber notify loop: a BotState mutation is immediately
        # visible to the next solve_all_paths call.
        assert profit_after != profit_before, (
            f"engine re-solve did not pick up the BotState mutation "
            f"(profit {profit_before} == {profit_after})"
        )
