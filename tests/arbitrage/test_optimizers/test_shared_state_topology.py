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

import threading

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

# ─── V4 topology round-trip fixtures (RAJ3PP public-interface regression) ─
# A V4 pool registered via UniswapArbEngine.register_v4_pool (with its core
# shared from a PyBot) and read back through a PyLiquidityPool handle. The
# handle is family-agnostic — the same getters/apply methods used for V3.
V4_POOL_MANAGER = "0x" + "ee" * 20
V4_POOL_ID_HEX = "0x" + "01" * 32
V4_FEE = 3000
V4_TICK_SPACING = 60
V4_SQRT_PRICE = V3_SQRT_PRICE
V4_LIQUIDITY = V3_LIQUIDITY
V4_TICK = V3_TICK


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

    def test_v3_handle_snapshot_is_atomic_under_one_read(self) -> None:
        """``snapshot_v3()`` returns spl/liquidity/tick/block atomically.

        The Python companion's ``state`` property builds a ``UniswapV3PoolState``
        from one snapshot tuple — all four fields are coherent (read under one
        ``BotState`` guard). Matches the V2 ``snapshot()`` atomicity contract.
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

        snap = handle.snapshot_v3()
        assert snap is not None
        spx, liq, tick, block = snap
        assert spx == V3_SQRT_PRICE
        assert liq == V3_LIQUIDITY
        assert tick == V3_TICK
        assert block == 0

    def test_v3_handle_snapshot_returns_none_for_v2_pool(self) -> None:
        """snapshot_v3() returns None on non-V3/V4 pools (no false data)."""
        core = PyBot()
        v2_pool_id = core.register_v2_pool(
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
        v2_handle = core.get_pool(v2_pool_id)
        assert v2_handle is not None
        assert v2_handle.snapshot_v3() is None

    def test_v3_handle_restore_before_block_round_trips_journal(self) -> None:
        """apply_swap journals priors; restore_before_block rolls back.

        Mirrors the V2 ``restore_before_block`` contract — the journal lands
        one delta per V3 swap (scalar priors), and restoring to a block before
        the swap reverts the scalars in-place. The reader immediately sees the
        rolled-back values.
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

        handle.apply_swap(
            sqrt_price_x96=V3_SQRT_PRICE + 1,
            liquidity=V3_LIQUIDITY + 100,
            tick=V3_TICK + 1,
            block_number=10,
        )

        # Restore to block 5 → in-place revert of the swap scalars.
        handle.restore_v3_before_block(5)

        assert handle.sqrt_price_x96 == V3_SQRT_PRICE
        assert handle.liquidity == V3_LIQUIDITY
        assert handle.tick == V3_TICK

    def test_v3_handle_apply_liquidity_update_inits_ticks(self) -> None:
        """A V3 Mint via ``apply_liquidity_update`` initializes tick entries.

        The handle mutates ``BotState.tick_data`` (pool_id-keyed) — the same
        state the engine solver's ``compute_tick_ranges`` reads. Brackets the
        current tick with a lower/upper tick pair to set up a bounded liquidity
        range.
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

        tick_lower = V3_TICK - V3_TICK_SPACING
        tick_upper = V3_TICK + V3_TICK_SPACING
        delta = 10_000
        applied = handle.apply_liquidity_update(
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity_delta=delta,
            block_number=3,
        )
        assert applied, "apply_liquidity_update should succeed on a registered V3 pool"

        # Applies to a non-V3 pool (V2) silently return False — don't corrupt.
        v2_pool_id = core.register_v2_pool(
            address=V2_POOL_B,
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
        v2_handle = core.get_pool(v2_pool_id)
        assert v2_handle is not None
        bad = v2_handle.apply_liquidity_update(
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity_delta=delta,
            block_number=4,
        )
        assert bad is False

    def test_v3_handle_update_tick_data_replaces_tick_map(self) -> None:
        """A V3 ``update_tick_data`` full-sync replaces the pool's tick_data map.

        The Python sparse-map fetcher (``make_tick_data_fetcher``) writes
        fetched ticks back through ``pool.update_tick_data(tick_bitmap,
        tick_data, block)``. In the post-8b topology the companion delegates
        here; this pins the Rust write path the delegation targets. Symmetric
        with ``tick_data_snapshot``: the dict shape written in is the shape read
        back out. The ``tick_bitmap`` arg is redundant in Rust (the bitmap is
        derived from ``tick_data`` keys — see ``tick_bitmap_snapshot``);
        accepted for API parity with the Python caller signature, ignored.
        Scalars (sqrt_price/liquidity/tick) are UNCHANGED — ``update_tick_data``
        is tick-only (a wholesale replace from a fetched snapshot, not a
        journalled event delta).
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
        assert handle.tick_data_snapshot() == {}

        # Seed two ticks via apply_liquidity_update (the Mint/Burn event path).
        seeded_lower = V3_TICK - V3_TICK_SPACING
        seeded_upper = V3_TICK + V3_TICK_SPACING
        handle.apply_liquidity_update(
            tick_lower=seeded_lower,
            tick_upper=seeded_upper,
            liquidity_delta=7_500,
            block_number=3,
        )
        assert set(handle.tick_data_snapshot().keys()) == {seeded_lower, seeded_upper}

        # Full-sync a DIFFERENT tick set via update_tick_data — REPLACES the map.
        new_tick_a = V3_TICK - 2 * V3_TICK_SPACING
        new_tick_b = V3_TICK + 2 * V3_TICK_SPACING
        new_tick_data: dict[int, tuple[int, int, int]] = {
            new_tick_a: (12_000, 12_000, 9),
            new_tick_b: (12_000, -12_000, 9),
        }
        applied = handle.update_tick_data(
            tick_bitmap={},  # redundant in Rust (derived from tick_data keys)
            tick_data=new_tick_data,
            block=9,
        )
        assert applied, "update_tick_data should succeed on a registered V3 pool"

        # The tick map is REPLACED (the two apply_liquidity_update ticks gone).
        snap = handle.tick_data_snapshot()
        assert set(snap.keys()) == {new_tick_a, new_tick_b}
        gross_a, net_a, block_a = snap[new_tick_a]
        gross_b, net_b, _block_b = snap[new_tick_b]
        assert gross_a == 12_000
        assert net_a == 12_000
        assert gross_b == 12_000
        assert net_b == -12_000
        assert block_a == 9

        # Scalars UNCHANGED (update_tick_data is tick-only, not a scalar event).
        assert handle.sqrt_price_x96 == V3_SQRT_PRICE
        assert handle.liquidity == V3_LIQUIDITY
        assert handle.tick == V3_TICK
        # update_block advanced to the new block (full-sync moves it forward).
        assert handle.update_block == 9

        # tick_bitmap_snapshot reflects the new tick keys (derived from keys).
        def word_of(tick: int) -> int:
            return (tick // V3_TICK_SPACING) >> 8

        bitmap = handle.tick_bitmap_snapshot()
        assert set(bitmap.keys()) == {word_of(new_tick_a), word_of(new_tick_b)}

        # An older block does NOT rewind update_block (monotonic).
        applied_older = handle.update_tick_data(
            tick_bitmap={},
            tick_data={new_tick_a: (1, 1, 1)},
            block=2,
        )
        assert applied_older
        assert handle.update_block == 9, "update_block must not rewind"
        # The tick map is still replaced (full-sync semantics, even for old block).
        assert set(handle.tick_data_snapshot().keys()) == {new_tick_a}

        # A V2 apply returns False silently (don't corrupt) — mirrors the
        # apply_liquidity_update V2-rejection contract (a V2 pool has no ticks).
        v2_pool_id = core.register_v2_pool(
            address=V2_POOL_B,
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
        v2_handle = core.get_pool(v2_pool_id)
        assert v2_handle is not None
        bad = v2_handle.update_tick_data(
            tick_bitmap={},
            tick_data={new_tick_a: (1, 1, 1)},
            block=1,
        )
        assert bad is False

    def test_v3_handle_tick_data_snapshot_returns_dict(self) -> None:
        """tick_data_snapshot() returns {tick: (gross, net, block)} dict.

        The Python companion's ``tick_data`` property builds the immutable
        ``LiquidityAtTick(liquidity_net, liquidity_gross, block)`` per tick from
        this Rust-side snapshot — the structured mirror of V2 ``reserve0``/
        ``reserve1`` scalar reads, lifted to the tick_data HashMap.
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

        # Empty at registration.
        assert handle.tick_data_snapshot() == {}

        # Apply a liquidity update — two ticks gain gross/net entries.
        tick_lower = V3_TICK - V3_TICK_SPACING
        tick_upper = V3_TICK + V3_TICK_SPACING
        delta = 7_500
        handle.apply_liquidity_update(
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity_delta=delta,
            block_number=7,
        )

        snap = handle.tick_data_snapshot()
        assert set(snap.keys()) == {tick_lower, tick_upper}
        gross_lower, net_lower, block_lower = snap[tick_lower]
        gross_upper, net_upper, _block_upper = snap[tick_upper]
        assert gross_lower == delta
        assert gross_upper == delta
        # net: lower tick +delta, upper tick -delta (Solidity Tick.update).
        assert net_lower == delta
        assert net_upper == -delta
        assert block_lower == 7

    def test_v3_handle_tick_bitmap_snapshot_pairs_with_tick_data(self) -> None:
        """tick_bitmap_snapshot() mirrors tick_data keys per word.

        Rust's V3PoolState doesn't store a separate tick bitmap — the bitmap is
        derivable from tick_data keys (which ticks are initialized). The handle
        returns a `{word_position: (bitmap_int, block)}` dict whose bitmap int
        has a bit set for each tick (within the 256-tick word) initialized at
        that word. Uninitialized words are absent.
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

        assert handle.tick_bitmap_snapshot() == {}

        tick_lower = V3_TICK - V3_TICK_SPACING
        tick_upper = V3_TICK + V3_TICK_SPACING
        handle.apply_liquidity_update(
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity_delta=10_000,
            block_number=7,
        )

        bitmap = handle.tick_bitmap_snapshot()
        from degenbot.calculations.evm_math import evm_divide
        from degenbot.uniswap.v3_libraries.tick_bitmap import position as _position

        word_lower, bit_lower = _position(evm_divide(tick_lower, V3_TICK_SPACING))
        word_upper, bit_upper = _position(evm_divide(tick_upper, V3_TICK_SPACING))
        assert word_lower != word_upper or bit_lower != bit_upper
        assert word_lower in bitmap, "tick_lower word missing"
        assert word_upper in bitmap, "tick_upper word missing"
        gross_lower_word_bits, _block = bitmap[word_lower]
        gross_upper_word_bits, _block = bitmap[word_upper]
        assert gross_lower_word_bits & (1 << bit_lower), "lower bit not set"
        assert gross_upper_word_bits & (1 << bit_upper), "upper bit not set"
        if word_lower == word_upper:
            expected_bits = (1 << bit_lower) | (1 << bit_upper)
            assert gross_lower_word_bits == expected_bits


class TestSharedStateTopologyV4:
    """Uniswap V4 over PyLiquidityPool — V4-specific RAJ3PP closure.

    Mirrors ``TestSharedStateTopologyV3`` for the V4 family. The headline
    regression: before RAJ3PP, ``PyLiquidityPool.apply_swap``/
    ``apply_liquidity_update`` routed unconditionally into V3-only
    ``apply_v3_*_by_pool_id`` methods that match ``PoolEntry::V3`` only and
    return ``None`` for ``PoolEntry::V4`` — so every Python-side V4 update
    (snapshots, manual ``external_update`` flows driven through the handle)
    was **silently dropped**. The fix is a family-dispatching
    ``apply_swap_by_pool_id``/``apply_liquidity_update_by_pool_id`` on
    ``BotState``; these tests pin that dispatch through the *public* Python
    handle surface (the exact layer that was broken).
    """

    @staticmethod
    def _register_v4(core: PyBot, engine: UniswapArbEngine) -> int:
        """Register a V4 pool via the engine's shared core; return its handle key."""
        return engine.register_v4_pool(
            pool_manager=V4_POOL_MANAGER,
            pool_id_hex=V4_POOL_ID_HEX,
            currency0=TOKEN0,
            currency1=TOKEN1,
            fee=V4_FEE,
            tick_spacing=V4_TICK_SPACING,
            hook_flags=0,
            sqrt_price_x96=V4_SQRT_PRICE,
            liquidity=V4_LIQUIDITY,
            tick=V4_TICK,
        )

    def test_v4_handle_apply_swap_is_visible_to_handle_reads(self) -> None:
        """A V4 ``apply_swap`` through the handle lands on the shared BotState.

        RAJ3PP headline regression via the public interface: pre-fix,
        ``PyLiquidityPool.apply_swap`` called ``apply_v3_swap_by_pool_id``
        unconditionally, which no-op'd on a ``PoolEntry::V4`` and silently left
        the V4 scalars at their registration values. After the family-dispatch
        fix, the write is visible to the next handle read — the V4 family of
        the V3 ``apply_swap → sqrt_price_x96`` visibility contract
        (``test_v3_handle_apply_swap_is_visible_to_handle_reads``).
        """
        core = PyBot()
        engine = UniswapArbEngine(py_bot=core)
        pool_id = self._register_v4(core, engine)
        handle = core.get_pool(pool_id)
        assert handle is not None

        new_spx = V4_SQRT_PRICE + 1
        new_liq = V4_LIQUIDITY + 100
        new_tick = V4_TICK + 1
        handle.apply_swap(
            sqrt_price_x96=new_spx,
            liquidity=new_liq,
            tick=new_tick,
            block_number=5,
        )

        # All four written fields are immediately visible — proving the handle
        # dispatched to the V4 apply path (not the V3-only no-op).
        assert handle.sqrt_price_x96 == new_spx
        assert handle.liquidity == new_liq
        assert handle.tick == new_tick
        assert handle.update_block == 5

    def test_v4_handle_apply_liquidity_update_inits_ticks(self) -> None:
        """A V4 ``apply_liquidity_update`` through the handle inits tick entries.

        The other half of RAJ3PP: pre-fix ``PyLiquidityPool.apply_liquidity_update``
        routed to ``apply_v3_liquidity_update_by_pool_id`` unconditionally, which
        no-op'd on ``PoolEntry::V4`` and silently dropped the ModifyLiquidity
        tick mutation. After the fix the dispatch reaches the V4 path and the
        tick_data map gains entries — the V4 family of the V3
        ``apply_liquidity_update`` contract
        (``test_v3_handle_apply_liquidity_update_inits_ticks``).
        """
        core = PyBot()
        engine = UniswapArbEngine(py_bot=core)
        pool_id = self._register_v4(core, engine)
        handle = core.get_pool(pool_id)
        assert handle is not None

        assert handle.tick_data_snapshot() == {}, "V4 pool starts with no ticks"

        tick_lower = V4_TICK - V4_TICK_SPACING
        tick_upper = V4_TICK + V4_TICK_SPACING
        delta = 10_000
        applied = handle.apply_liquidity_update(
            tick_lower=tick_lower,
            tick_upper=tick_upper,
            liquidity_delta=delta,
            block_number=3,
        )
        assert applied, "apply_liquidity_update should succeed on a registered V4 pool"

        snap = handle.tick_data_snapshot()
        assert set(snap.keys()) == {tick_lower, tick_upper}
        gross_lower, net_lower, block_lower = snap[tick_lower]
        gross_upper, net_upper, _block_upper = snap[tick_upper]
        assert gross_lower == delta
        assert gross_upper == delta
        assert net_lower == delta
        assert net_upper == -delta
        assert block_lower == 3

    def test_v4_handle_update_tick_data_replaces_tick_map(self) -> None:
        """A V4 ``update_tick_data`` full-sync replaces the pool's tick_data map.

        The family-agnostic twin of
        ``test_v3_handle_update_tick_data_replaces_tick_map``: the V4 arm of
        ``BotState::sync_tick_data_by_pool_id`` mirrors the V3 path (both
        store an identical ``tick_data: HashMap<i32, TickInfo>`` — J63J3N).
        Pins that the V3-paid-for family-agnostic write path actually reaches
        a V4 pool (a future maintainer narrowing it to V3-only would re-open
        the RAJ3PP silent-drop footgun shape).
        """
        core = PyBot()
        engine = UniswapArbEngine(py_bot=core)
        pool_id = self._register_v4(core, engine)
        handle = core.get_pool(pool_id)
        assert handle is not None
        assert handle.tick_data_snapshot() == {}

        # Seed two ticks via the V4 Mint/Burn event path.
        seeded_lower = V4_TICK - V4_TICK_SPACING
        seeded_upper = V4_TICK + V4_TICK_SPACING
        handle.apply_liquidity_update(
            tick_lower=seeded_lower,
            tick_upper=seeded_upper,
            liquidity_delta=7_500,
            block_number=3,
        )
        assert set(handle.tick_data_snapshot().keys()) == {seeded_lower, seeded_upper}

        # Full-sync a DIFFERENT tick set — REPLACES the map (V4 family).
        new_tick_a = V4_TICK - 2 * V4_TICK_SPACING
        new_tick_b = V4_TICK + 2 * V4_TICK_SPACING
        new_tick_data: dict[int, tuple[int, int, int]] = {
            new_tick_a: (12_000, 12_000, 9),
            new_tick_b: (12_000, -12_000, 9),
        }
        applied = handle.update_tick_data(
            tick_bitmap={},
            tick_data=new_tick_data,
            block=9,
        )
        assert applied, "update_tick_data should succeed on a registered V4 pool"

        snap = handle.tick_data_snapshot()
        assert set(snap.keys()) == {new_tick_a, new_tick_b}
        gross_a, net_a, block_a = snap[new_tick_a]
        assert gross_a == 12_000
        assert net_a == 12_000
        assert block_a == 9

        # V4 scalars UNCHANGED; update_block advanced.
        assert handle.sqrt_price_x96 == V4_SQRT_PRICE
        assert handle.liquidity == V4_LIQUIDITY
        assert handle.tick == V4_TICK
        assert handle.update_block == 9


class TestSharedStateTopologyConcurrency:
    """ADR-006 slice 10 acceptance: the deadlock surface + atomic-read invariant.

    The lock-ordering invariant is **engine-then-core**: every pump path holds
    the engine ``Mutex<UniswapEngine>`` and nests ``core.write()``/``core.read()``
    inside; ``PyBot``/``PyLiquidityPool`` methods take ``core`` alone and never
    call into the engine (ADR-003's rule keeping the deadlock surface empty).
    These tests characterize that invariant under concurrent writer/reader
    threads — the pump-side write (``apply_swap``) interleaved with companion
    reads (``snapshot_v3``).

    Note on the GIL: under CPython's GIL these threads don't create true
    parallel lock contention, but they DO exercise the lock-acquire/release
    paths + document the atomic-read contract the free-threaded future will
    exercise in earnest. The pump itself runs off the GIL (a tokio task), so
    a Python read genuinely can interleave with a pump-side write.
    """

    _ITERATIONS = 2_000
    _READERS = 4

    def test_concurrent_apply_swap_writes_complete_without_deadlock(self) -> None:
        """A writer thread looping ``apply_swap`` joins within a sane timeout.

        Regression guard for the engine-then-core lock ordering: if a path
        ever nested core-then-engine, the writer (which takes ``core.write()``
        via the handle) would block the pump and the join would time out.
        """
        core = PyBot()
        core.register_v3_pool(
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
        handle = core.get_pool(1)
        assert handle is not None

        errors: list[BaseException] = []

        def writer() -> None:
            try:
                for i in range(self._ITERATIONS):
                    handle.apply_swap(
                        sqrt_price_x96=V3_SQRT_PRICE + i,
                        liquidity=V3_LIQUIDITY + i,
                        tick=V3_TICK + (i % 7),
                        block_number=i + 1,
                    )
            except BaseException as exc:  # noqa: BLE001
                errors.append(exc)

        thread = threading.Thread(target=writer)
        thread.start()
        thread.join(timeout=30.0)
        assert not thread.is_alive(), (
            "writer thread deadlocked (engine-then-core lock ordering broken)"
        )
        assert not errors, errors
        # The final write is visible (live read after the pump-side writes).
        assert handle.update_block == self._ITERATIONS

    def test_snapshot_v3_is_atomic_under_concurrent_writes(self) -> None:
        """``snapshot_v3()`` returns 4 coherent fields — no torn reads.

        The writer writes a tagged tuple per iteration: ``spx = base + i``,
        ``liq = base + i``, ``tick = base + i``, ``block = i`` — all four fields
        encode the same iteration index ``i``. A reader's ``snapshot_v3()``
        must return four fields that all agree on the same ``i`` (i.e.
        ``spx == V3_SQRT_PRICE + block`` AND ``liq == V3_LIQUIDITY + block``
        AND ``tick == V3_TICK + block``). A torn read (e.g. new ``spx`` from
        iteration N+1 with the old ``block`` from N) would fail this — proving
        the single-``core.read()`` guard around the snapshot tuple.
        """
        core = PyBot()
        core.register_v3_pool(
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
        handle = core.get_pool(1)
        assert handle is not None

        errors: list[str] = []
        stop = threading.Event()

        def writer() -> None:
            i = 0
            while not stop.is_set() and i < self._ITERATIONS:
                i += 1
                handle.apply_swap(
                    sqrt_price_x96=V3_SQRT_PRICE + i,
                    liquidity=V3_LIQUIDITY + i,
                    tick=V3_TICK + i,
                    block_number=i,
                )

        def reader() -> None:
            while not stop.is_set():
                snap = handle.snapshot_v3()
                if snap is None:
                    errors.append("snapshot_v3() returned None on a V3 pool")
                    return
                spx, liq, tick, block = snap
                # All four fields must encode the SAME write index `block`.
                if spx != V3_SQRT_PRICE + block:
                    errors.append(
                        f"torn read: spx={spx} != base+block={V3_SQRT_PRICE + block}"
                    )
                    return
                if liq != V3_LIQUIDITY + block:
                    errors.append(
                        f"torn read: liq={liq} != base+block={V3_LIQUIDITY + block}"
                    )
                    return
                if tick != V3_TICK + block:
                    errors.append(
                        f"torn read: tick={tick} != base+block={V3_TICK + block}"
                    )
                    return

        writer_thread = threading.Thread(target=writer)
        readers = [threading.Thread(target=reader) for _ in range(self._READERS)]
        writer_thread.start()
        for t in readers:
            t.start()
        writer_thread.join(timeout=30.0)
        assert not writer_thread.is_alive(), "writer deadlocked"
        stop.set()
        for t in readers:
            t.join(timeout=10.0)
            assert not t.is_alive(), "reader deadlocked"
        assert not errors, errors

    def test_concurrent_engine_solve_and_handle_reads_do_not_deadlock(self) -> None:
        """Engine-path solve (engine lock + core read) + handle reads (core
        read) interleave without deadlock.

        The pump's drain→``solve_all_paths`` takes the engine ``Mutex`` then
        reads ``core``; the companion's getters take ``core`` alone. This
        stresses the engine-then-core ordering from BOTH sides concurrently —
        if the engine ever tried to re-enter ``core`` while holding a
        reader-held core guard (or vice versa), this would deadlock or panic.
        """
        core = PyBot()
        pool_id_a, pool_id_b = _register_balanced_v2_pair(core)
        engine = UniswapArbEngine(py_bot=core)
        engine.register_and_solve_path([(pool_id_a, True), (pool_id_b, True)])

        errors: list[BaseException] = []
        done = threading.Event()

        def solver() -> None:
            try:
                for i in range(1, self._ITERATIONS + 1):
                    if done.is_set():
                        return
                    engine.solve_all_paths(i)
            except BaseException as exc:  # noqa: BLE001
                errors.append(exc)

        def reader() -> None:
            handle_a = core.get_pool(pool_id_a)
            assert handle_a is not None
            try:
                while not done.is_set():
                    _ = handle_a.reserve0
                    _ = handle_a.reserve1
                    _ = handle_a.update_block
            except BaseException as exc:  # noqa: BLE001
                errors.append(exc)

        solver_thread = threading.Thread(target=solver)
        reader_threads = [
            threading.Thread(target=reader) for _ in range(self._READERS)
        ]
        solver_thread.start()
        for t in reader_threads:
            t.start()
        solver_thread.join(timeout=60.0)
        assert not solver_thread.is_alive(), "solver deadlocked"
        done.set()
        for t in reader_threads:
            t.join(timeout=10.0)
            assert not t.is_alive(), "reader deadlocked"
        assert not errors, errors
