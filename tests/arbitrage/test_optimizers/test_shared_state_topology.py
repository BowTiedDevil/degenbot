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

# Keccak256 of `Sync(uint112,uint112)` — the V2 Sync event signature.
V2_SYNC_TOPIC = "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1"

# ─── V3 topology round-trip fixtures (plan-101 slice 8a) ────────────────
# A V3 pool whose sqrt_price ~ 1 USDC per WETH-wei-fractal priced at ~2000.
# Tick −76020 corresponds to ~2000 USDC per WETH on a 0.3% (fee=3000) V3 pool.
V3_POOL_A = "0x" + "33" * 20
V3_POOL_B = "0x" + "44" * 20
V3_FEE = 3000
V3_TICK_SPACING = 60
V3_SQRT_PRICE = 2_198_666_895_605_149_686_863  # ~2000 USDC per WETH
V3_TICK = -76020
V3_LIQUIDITY = 1_234_567_890


def _v2_sync_log_data(reserve0: int, reserve1: int) -> str:
    """ABI-encode `Sync(uint112, uint112)` data: two 32-byte left-padded slots."""
    return "0x" + (reserve0.to_bytes(32, "big") + reserve1.to_bytes(32, "big")).hex()


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

    def test_dispatch_log_drives_full_pump_to_solve_loop(self) -> None:
        """A synthetic WS Sync log via `PyBot.dispatch_log` reaches the engine.

        This is the *full* §17 closure: not just that a `PyLiquidityPool` write
        is visible to the engine (the prior test), but that the WS pump's own
        path — `dispatch_log` → `LogDispatcher` decode/apply → notify
        `EngineSubscriber` → engine dirties the pool → `solve_all_paths`
        re-solves — reads the post-Sync shared state. `sync_reserves` short-
        circuits straight to the core; `dispatch_log` exercises the pump's
        decoder/notify wiring, proving the hot loop (not just the eager entry)
        reads live state.
        """
        core = PyBot()
        pool_id_a, pool_id_b = _register_balanced_v2_pair(core)
        engine = UniswapArbEngine(py_bot=core)
        engine.register_and_solve_path([(pool_id_a, True), (pool_id_b, True)])

        engine.solve_all_paths(1)
        results_1, _ = engine.latest_results()
        profit_before = results_1[0][2]
        assert profit_before > 0, "the initial mispricing should be profitable"

        # Drive a synthetic V2 Sync log through the pump path: decode →
        # apply to BotState → notify the attached EngineSubscriber. This is
        # exactly what BlockPump calls per WS log (ADR-006 D4, slice 5).
        core.dispatch_log(
            address=V2_POOL_A,
            topics=[V2_SYNC_TOPIC],
            data=_v2_sync_log_data(1_000_000 * USDC, 800 * WETH),
            block_number=2,
        )

        # Confirm the synthetic log mutated the shared state via the decoder —
        # the pool handle reads the dispatched reserves, not the registered ones.
        pool_handle = core.get_pool(pool_id_a)
        assert pool_handle is not None
        assert pool_handle.reserve0 == 1_000_000 * USDC, (
            "dispatch_log did not route through the LogDispatcher to BotState"
        )

        # Engine re-solve reads the dispatched state → different profit.
        engine.solve_all_paths(2)
        results_2, _ = engine.latest_results()
        profit_after = results_2[0][2]
        assert profit_after != profit_before, (
            "dispatch_log → engine re-solve did not read the live shared state "
            "(§17 regression on the pump path!)"
        )


class TestSharedStateTopologyV3:
    """UniswapV3Pool over PyLiquidityPool — V3-specific §17 closure (plan-101 slice 8a).

    Mirrors the V2 topology tests but for the V3 family: a pool registered via
    ``PyBot.register_v3_pool`` is read through a ``PyLiquidityPool`` handle the
    engine shares — the V3 scalar state (sqrt_price_x96/liquidity/tick/update_block)
    lives in ``BotState``, not a Python-side ``ConcentratedLiquidityStateManager``.
    """

    def test_v3_handle_reads_registered_scalars(self) -> None:
        """A V3 pool registered on the shared PyBot is read back via a PyLiquidityPool handle.

        The handle's V3 getters (``sqrt_price_x96``/``liquidity``/``tick``/
        ``update_block``/``fee``/``tick_spacing``) read the authoritative
        ``BotState`` scalars set at registration — structural mirror of V2's
        ``reserve0``/``reserve1`` getters.
        """
        core = PyBot()
        pool_id = core.register_v3_pool(
            address=V3_POOL_A,
            token0=TOKEN0,
            token1=TOKEN1,
            fee=V3_FEE,
            tick_spacing=V3_TICK_SPACING,
            factory=FACTORY,
            sqrt_price_x96=V3_SQRT_PRICE,
            liquidity=V3_LIQUIDITY,
            tick=V3_TICK,
        )

        # The handle is family-agnostic — get_pool returns a PyLiquidityPool
        # for a V3 pool_id the same way it does for V2.
        handle = core.get_pool(pool_id)
        assert handle is not None
        assert handle.pool_id == pool_id

        # V3 scalar getters read the shared BotState — not a Python-side state mgr.
        assert handle.sqrt_price_x96 == V3_SQRT_PRICE
        assert handle.liquidity == V3_LIQUIDITY
        assert handle.tick == V3_TICK
        assert handle.update_block == 0  # register_v3_pool hardcodes update_block=0 today
        assert handle.fee == V3_FEE
        assert handle.tick_spacing == V3_TICK_SPACING

    def test_v3_handle_apply_swap_is_visible_to_handle_reads(self) -> None:
        """A V3 ``apply_swap`` write through the handle is immediately readable.

        This is the deepest assertion of slice 8a: a write through
        ``PyLiquidityPool.apply_swap`` lands on the shared ``BotState`` and the
        next getter read sees the new scalars — the V3 family of the V2
        ``sync_reserves → reserve0`` visibility contract.
        """
        core = PyBot()
        pool_id = core.register_v3_pool(
            address=V3_POOL_A,
            token0=TOKEN0,
            token1=TOKEN1,
            fee=V3_FEE,
            tick_spacing=V3_TICK_SPACING,
            factory=FACTORY,
            sqrt_price_x96=V3_SQRT_PRICE,
            liquidity=V3_LIQUIDITY,
            tick=V3_TICK,
        )
        handle = core.get_pool(pool_id)
        assert handle is not None

        new_spx = V3_SQRT_PRICE + 1
        new_liq = V3_LIQUIDITY + 100
        new_tick = V3_TICK + 1

        handle.apply_swap(
            sqrt_price_x96=new_spx,
            liquidity=new_liq,
            tick=new_tick,
            block_number=5,
        )

        # All four written fields are immediately visible — proving the handle
        # reads the same shared BotState the mutation wrote to.
        assert handle.sqrt_price_x96 == new_spx
        assert handle.liquidity == new_liq
        assert handle.tick == new_tick
        assert handle.update_block == 5
