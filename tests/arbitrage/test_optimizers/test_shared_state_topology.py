"""ADR-006 slice 9 overall-acceptance: shared-state topology round-trip.

Proves the ADR-006 single-``Bot``-per-chain topology end-to-end, the structural
closure of the ``rust-owned-bot.md`` §17 stale-state caveat:

- ``UniswapArbEngine(py_bot=core)`` adopts ``core``'s shared ``BotState`` (one
  state, not the engine's private copy).
- Pools register ONCE — via ``PyBot.register_v2_pool`` (the same path
  ``Bot.build_pool`` takes). The engine does NOT re-register them; it reads
  them through the shared state via ``register_and_solve_path``.
- A live-state write through the ``PyLiquidityPool`` handle
  (``sync_reserves``) is immediately visible to a subsequent engine re-solve
  (``solve_all_paths``) — the engine reads the *current* shared state, not a
  stale copy. The dual-``BotState`` split (the §17 root cause) is gone.
"""

from __future__ import annotations

from degenbot.degenbot_rs import PyBot, UniswapArbEngine

USDC = 10**6
WETH = 10**18

V2_POOL_A = "0x" + "11" * 20
V2_POOL_B = "0x" + "22" * 20
TOKEN0 = "0x" + "aa" * 20
TOKEN1 = "0x" + "bb" * 20
FACTORY = "0x" + "ff" * 20


def _register_balanced_v2_pair(core: PyBot) -> tuple[int, int]:
    """Register two balanced V2 pools (A: USDC→WETH, B: WETH→USDC) at ~1:1875.

    Returns the (pool_id_a, pool_id_b) pair. The cycle is initially ~balanced.
    """
    pool_id_a = core.register_v2_pool(
        address=V2_POOL_A,
        token0=TOKEN0,
        token1=TOKEN1,
        reserve0=1_500_000 * USDC,
        reserve1=800 * WETH,
        gamma_numer0=997,
        fee_denom0=1000,
        gamma_numer1=997,
        fee_denom1=1000,
        factory=FACTORY,
    )
    pool_id_b = core.register_v2_pool(
        address=V2_POOL_B,
        token0=TOKEN1,  # reversed token order so the cycle closes
        token1=TOKEN0,
        reserve0=800 * WETH,
        reserve1=1_600_000 * USDC,
        gamma_numer0=997,
        fee_denom0=1000,
        gamma_numer1=997,
        fee_denom1=1000,
        factory=FACTORY,
    )
    return pool_id_a, pool_id_b


class TestSharedStateTopology:
    """``UniswapArbEngine(py_bot=)`` shares the bot's ``BotState`` (ADR-006 D1+D4)."""

    def test_engine_adopts_shared_bot_state(self) -> None:
        """The engine reads pools registered on the shared PyBot — no re-registration."""
        core = PyBot()
        pool_id_a, pool_id_b = _register_balanced_v2_pair(core)
        engine = UniswapArbEngine(py_bot=core)

        # The engine can build a path from pool_ids it never registered itself —
        # proof it reads the shared BotState, not a private copy.
        path_id = engine.register_and_solve_path([(pool_id_a, True), (pool_id_b, True)])
        assert path_id == 1
        assert engine.v2_pool_count() == 2
        assert engine.path_count() == 1

    def test_live_state_write_is_visible_to_engine_re_solve(self) -> None:
        """A ``PyLiquidityPool`` write is immediately read by the next engine solve.

        This is the §17 stale-state root cause's structural closure: with one
        shared ``BotState``, the engine re-solve reads the *current* state the
        handle just wrote — not a stale copy.
        """
        core = PyBot()
        pool_id_a, pool_id_b = _register_balanced_v2_pair(core)
        engine = UniswapArbEngine(py_bot=core)
        engine.register_and_solve_path([(pool_id_a, True), (pool_id_b, True)])

        engine.solve_all_paths(1)
        results_1, _ = engine.latest_results()
        profit_before = results_1[0][2]
        assert profit_before > 0, "the initial mispricing should be profitable"

        # Live write through the pool handle — updates the shared BotState.
        pool_handle = core.get_pool(pool_id_a)
        assert pool_handle is not None
        assert pool_handle.reserve0 == 1_500_000 * USDC
        pool_handle.sync_reserves(1_000_000 * USDC, 800 * WETH, block_number=2)
        # The handle reads the live shared state immediately.
        assert pool_handle.reserve0 == 1_000_000 * USDC

        # Engine re-solve reads the UPDATED shared state → different profit.
        engine.solve_all_paths(2)
        results_2, _ = engine.latest_results()
        profit_after = results_2[0][2]
        assert profit_after != profit_before, (
            "engine re-solve did NOT read the live shared state (§17 regression!)"
        )
