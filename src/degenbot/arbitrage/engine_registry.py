"""The canonical bot-startup orchestrator (Plan 102, slice 3).

:class:`EngineRegistry` is the **one correct way to start** a
:class:`~degenbot.degenbot_rs.UniswapArbEngine` operator: it runs the
pre-pump startup ritual (``subscribe`` → stream snapshots → ``backfill`` →
verify config) and *stops before* ``resume()``, so the caller can attach its
result consumer before any batches flow. It also maintains the Python pool ↔
Rust ``pool_id`` key maps and registers paths.

Lifted verbatim from ``examples/eth_backrun_v2_v3_v4_rust.py`` — engine-
operation machinery only. Deployment policy (the main loop, dispatcher,
simulation overrides) stays example-side (B-mid scope).
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from degenbot import Bot, UniswapV2Pool
from degenbot.aerodrome.pools import AerodromeV2Pool
from degenbot.degenbot_rs import UniswapArbEngine
from degenbot.logging import logger as bot_logger
from degenbot.uniswap.snapshot_binary import (
    stream_v3_snapshot_to_engine,
    stream_v4_snapshot_to_engine,
)
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

from .hop_info import PathInfo, build_hops_from_pools
from .policy import NoOpPathPredicate, PathCompositionPredicate

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
    from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
    from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot

__all__ = ["EngineRegistry"]


class EngineRegistry:
    """Thin wrapper over the Rust UniswapArbEngine.

    Maintains Python pool ↔ Rust key mappings so events can be routed
    to the right engine pool, and results can be mapped back to Python
    pool objects for encoding.

    The first pool in each path provides the flash borrow: V2 via
    uniswapV2Call, V3 via swapCallback, V4 via unlockCallback.

    V4 pools are keyed by pool_id hex string (sufficient since the bot
    uses a single PoolManager). The Rust engine additionally receives
    the pool_manager address at registration time.
    """

    def __init__(  # noqa: D107
        self,
        bot: Bot | None = None,
        *,
        engine: UniswapArbEngine | None = None,
        path_predicate: PathCompositionPredicate | None = None,
    ) -> None:
        # ADR-006 D1+D4: the engine adopts the Bot's shared BotState, so the
        # engine reads/writes the SAME core that V2 PyLiquidityPool handles
        # share — no dual-BotState split (rust-owned-bot.md §17 closure). The
        # registry takes the Bot directly (Python-side expression of ADR-006 D4:
        # Bot owns the engine; the user drives Bot) — never `bot._py_bot`.
        # `engine` is a testability seam: when omitted, the production engine is
        # constructed against the bot's shared BotState.
        if engine is not None:
            self.engine = engine
        elif bot is None:
            msg = "EngineRegistry requires either `engine` (test path) or `bot` (production)."
            raise ValueError(msg)
        else:
            self.engine = UniswapArbEngine(py_bot=bot._py_bot)  # noqa: SLF001
        self._v2_keys: dict[str, int] = {}  # address → pool_id (shared BotState)
        self._v3_keys: dict[str, int] = {}
        # V4 pools keyed by pool_id hex — for event routing from PoolManager logs
        self._v4_keys: dict[str, int] = {}  # pool_id_hex → pool_id
        self.paths: dict[int, PathInfo] = {}
        # ADR-006 D4 + D7KMQO: a pluggable path-composition predicate enforces
        # deployment policy (token denylist/allowlist, hop-count bounds,
        # min-liquidity, duplicate-pool guard) BEFORE hop building + engine
        # dispatch. Distinct from the Rust core's pool-admission floor
        # (HookedPoolRejectedError / DynamicFeePoolRejectedError). Default:
        # NoOpPathPredicate (accept all).
        self.path_predicate: PathCompositionPredicate = path_predicate or NoOpPathPredicate()
        # Verify config (stashed in start(), consumed by verify_liquidity_maps).
        self._verify_rpc_url: str | None = None
        self._verify_state_view: str | None = None
        # T1 (ADR-006 D4 + two-step verify prep): the snapshot seed block and
        # the backfill target block, stashed in start() for the per-pool
        # two-step verify (T6) to pass to the verify closures — NOT wired into
        # the orphaned engine.set_verify_*_block setters (deleted in T5).
        # snapshot_block = min(snap.newest_block) across supplied snapshots.
        # `None` when no snapshots were supplied (no seed) — T6 guards
        # accordingly. (Pre-fix the registry also stashed a
        # `_verify_backfill_block` constant for step-2 to compare against —
        # removed 2026-06-29: the post-drain pin now carries its OWN block,
        # captured atomically with the drain, so step-2 takes no `block`
        # argument from the registry.)
        self._verify_snapshot_block: int | None = None
        # NOTE: These Python dicts (_v2_keys, _v3_keys, _v4_keys) are plain
        # dicts — NOT thread-safe. All access is on the single asyncio event loop.

    def start(
        self,
        node_http: str,
        node_ws: str,
        *,
        v3_snapshot: UniswapV3LiquiditySnapshot | None = None,
        v4_snapshot: UniswapV4LiquiditySnapshot | None = None,
        verify_state_view: str | None = None,
    ) -> int:
        """Run the pre-pump startup ritual and stop BEFORE resume().

        Sequences: subscribe(ws) → load snapshots → backfill → verify config.
        Stops at EnginePhase::Backfilled so the caller can attach its result
        consumer before batches begin to flow (`resume()` is the single gate
        after which the pump emits one ResultBatch per block into the
        fire-and-forget channel — attaching the consumer after resume risks
        unbounded backlog and stale-batch dispatch).

        DB snapshot (JUCFCB, Shape 2): the V3+V4 DB snapshot is eagerly loaded
        into the core ``BotState`` at ``Bot.__init__`` time via
        ``PyBot.load_snapshot_from_db`` — so the DB path needs NO snapshot
        kwargs here. This method reads ``snapshot_seed_block`` from the engine
        (the shared ``BotState``) and calls ``backfill_from_snapshot`` to close
        the S→WS gap.

        Non-DB snapshots (file/memory): pass ``v3_snapshot``/``v4_snapshot``
        kwargs; they're loaded via ``load_*_from_py`` (which feed the core
        store), then ``snapshot_block = min(s.newest_block)`` drives the
        backfill. These two kwargs are non-DB-only — the DB path constructs the
        Bot with ``config.database.path`` and passes no snapshots here.

        Returns:
            The backfill target (first observed WS block) from ``subscribe``.

        """
        backfill_target = self.engine.subscribe(node_ws)

        # Load snapshots + backfill the snapshot→WS gap. Zero batches are
        # emitted here — the pump isn't running until resume().
        if v3_snapshot is not None or v4_snapshot is not None:
            # Non-DB (file/memory) path — stream_*_to_engine falls back to
            # engine.load_*_from_py for non-DB sources, feeding the core store.
            # (The DB-source branch inside stream_*_to_engine is now dead: the
            # DB path eagerly loads at Bot.__init__ via load_snapshot_from_db,
            # so DB-backed snapshots are never passed here.)
            if v3_snapshot is not None:
                stream_v3_snapshot_to_engine(v3_snapshot, self.engine)
            if v4_snapshot is not None:
                stream_v4_snapshot_to_engine(v4_snapshot, self.engine)
            snapshot_block = min(
                s.newest_block for s in (v3_snapshot, v4_snapshot) if s is not None
            )
        else:
            # DB path (Shape 2): the snapshot was loaded at Bot construction;
            # read S from the core BotState via the engine getter.
            snapshot_block = self.engine.snapshot_seed_block

        if snapshot_block is not None:
            self.engine.backfill_from_snapshot(node_http, snapshot_block)
            # T1: stash the snapshot seed block for the per-pool two-step verify
            # (T6). step-1 compares the pinned seed against on-chain@this
            # block. step-2 captures its OWN block atomically with the drain
            # (the post-drain pin carries `(tick_data, update_block)`), so the
            # registry stashes no backfill-block constant for step-2 (removed
            # 2026-06-29: pre-fix it stashed `backfill_target` and passed it to
            # step-2, fabricating a mismatch on active pools during a slow
            # `build_paths`). Only set when a snapshot was supplied (else
            # there's no seed).
            self._verify_snapshot_block = snapshot_block

        # Verify config (consumer-safe: nothing emits yet).
        self.engine.set_verify_rpc_url(node_http)
        if verify_state_view is not None:
            self.engine.set_verify_state_view(verify_state_view)
        # Stash the verify RPC + StateView for the post-register batch
        # verification (verify_liquidity_maps, T6 per-pool two-step, T7
        # recurring). ADR-006 D3 (T5): the orphaned
        # engine.register_v2/v3/v4_pool + verify_on_register gate is deleted —
        # pools enter BotState via the bot builders (py_bot.register_*) and the
        # verify seam is the registry drain (T6), not the dead register gate.
        self._verify_rpc_url = node_http
        self._verify_state_view = verify_state_view

        # Intentionally NOT calling resume() — the caller attaches its
        # consumer next, then calls resume() as the single batch-flow gate.
        return backfill_target

    async def verify_liquidity_maps(
        self,
        *,
        block_number: int | None = None,
    ) -> None:
        """Verify the engine's V3+V4 liquidity maps against on-chain state.

        Reads BotState directly (``core.v3_pools_snapshot`` /
        ``v4_pools_snapshot``) and compares each pool's tick map to the chain
        via ``TickLens`` (V3) / ``StateView`` (V4), emitting
        ``[verify] V3 + V4 liquidity maps OK at block {}`` on success.

        **Block tag.** The comparison must be engine-state@N vs on-chain@N at
        the *same* block — otherwise a high-activity pool (e.g. USDC/WETH 0.05%)
        will mismatch on liquidity that changed between the engine's anchored
        block and the chain tip. ``block_number=None`` (the default) resolves
        to the engine's ``last_processed_block()`` — the latest block whose
        events the pump has fully applied — so the verify is deterministic.
        Pass an explicit block only to pin a specific checkpoint.

        A pass transitively confirms the snapshot seed: backfill + pump apply
        *deltas* on top of the seed, so a wrong seed cannot converge to a
        matching on-chain state at block N. Run it after paths are built
        (pools registered in BotState) and before the hot loop. Raises
        (fail-fast) on mismatch — do not arb against an unverified engine.

        Args:
            block_number: Block to verify against. ``None`` (default) resolves
                to the engine's ``last_processed_block()`` — deterministic.

        Raises:
            RuntimeError: verify config was never stashed (``start()`` not
                called, or called without ``verify_state_view``).

        """
        if self._verify_rpc_url is None or self._verify_state_view is None:
            msg = (
                "verify_liquidity_maps requires verify config — call "
                "EngineRegistry.start() with verify_state_view set first."
            )
            raise RuntimeError(msg)
        # block_number=None (the default) resolves
        # to the engine's ``last_processed_block()`` — the latest block whose
        # events the pump has fully applied — so the verify is deterministic.
        # Pass an explicit block only to pin a specific checkpoint.
        #
        # Async (`await`) because the engine pyo3 method is `future_into_py` —
        # the sync `runtime.block_on` variant deadlocked the pump (2026-06-24
        # diagnosis). The asyncio loop driving the consumer also drives this.
        resolved_block = block_number
        if resolved_block is None:
            resolved_block = self.engine.last_processed_block()
        # tick_lens_address is unused by the V3 batch path (it calls
        # pool.ticks() directly); a zero address satisfies the parser.
        await self.engine.verify_liquidity_maps(
            rpc_url=self._verify_rpc_url,
            tick_lens_address="0x0000000000000000000000000000000000000000",
            state_view_address=self._verify_state_view,
            block_number=resolved_block,
        )

    async def _verify_pool_seed_at_block(
        self,
        family: str,
        address: str,
        block: int | None,
        *,
        pool_id_hex: str | None = None,
    ) -> None:
        """T6 step-1: verify the pinned snapshot SEED vs on-chain@``block`` BEFORE the drain.

        CBCH6H: compares the pinned seed (the registration-time
        ``tick_data``), NOT engine-current — during a rolling start
        (``resume()`` precedes ``build_paths``) the live pump applies
        Mint/Burn onto engine-current; comparing that vs on-chain@snapshot
        (pre-journal) would false-mismatch on every active pool
        (logs/perm-V2-V3-V2.log). The seed is consumed once so memory is
        bounded.

        Gated by verify config — a no-op when ``start()`` wasn't called
        with ``verify_state_view`` (mirrors the batch
        ``verify_liquidity_maps`` posture). ``block is None`` (no snapshot
        supplied at ``start()``) is also a no-op at this seam — the batch
        verify at ``last_processed_block()`` still covers the pool
        post-``build_paths``. A mismatch raises the engine's
        ``VerificationMismatchError`` at the offending pool — fail-fast,
        surfacing from ``build_paths``.

        Args:
            family: ``"v3"`` or ``"v4"`` — selects the verify method.
            address: the pool address (label only — on-chain state is read
                from the shared BotState snapshot, not the address).
            block: the snapshot block to verify against; ``None`` (no
                snapshot supplied) → no-op at this seam.
            pool_id_hex: required for V4 (the StateView key).

        """
        if self._verify_rpc_url is None or self._verify_state_view is None:
            # No verify config — skip, same as batch verify_liquidity_maps.
            return
        if block is None:
            # No snapshot block (no snapshot supplied at start()) — nothing
            # to verify at this seam; the batch verify at
            # last_processed_block() still runs post-build_paths.
            return
        if family == "v3":
            await self.engine.verify_v3_snapshot_seed(
                address=address,
                rpc_url=self._verify_rpc_url,
                block_number=block,
            )
        elif family == "v4":
            assert pool_id_hex is not None
            await self.engine.verify_v4_snapshot_seed(
                pool_manager_address=address,
                pool_id_hex=pool_id_hex,
                rpc_url=self._verify_rpc_url,
                state_view_address=self._verify_state_view,
                block_number=block,
            )

    async def _verify_pool_post_drain(
        self,
        family: str,
        address: str,
        *,
        pool_id_hex: str | None = None,
    ) -> None:
        """T6 step-2: verify the pinned POST-DRAIN pair vs on-chain@pinned block AFTER the drain.

        The block compared against is the one captured atomically with the
        drain inside ``pin_v3/v4_post_drain_snapshot`` — the ``update_block``
        at pin time (the last drained backfill OR pump event's block, or the
        registration block if neither buffer had events). The registry passes
        NO block: the pin carries its own. Pre-fix the registry passed
        ``verify_backfill_block`` as a constant, which fabricated a mismatch
        on active pools during a slow ``build_paths`` (pump Mint/Burn at blocks
        PAST the backfill boundary advanced the drained state to a later
        block — 2026-06-29 crash). The ``(state, block)`` pair — captured
        under the same write-lock that finished the drain — is
        self-consistent, so the verify is race-free.

        The comparison is drain-state vs on-chain@pinned-block, NOT
        engine-current (drain + pump journal), which would false-mismatch on
        every active pool under a rolling start (``resume()`` precedes
        ``build_paths``). The pin is taken (consumed) once; ``None`` for
        sparse/un-drained pools → no-op Ok (the batch verify at
        ``last_processed_block()`` still covers them post-build_paths).

        Gated by verify config — a no-op when ``start()`` wasn't called
        with ``verify_state_view``.

        Args:
            family: ``"v3"`` or ``"v4"`` — selects the verify method.
            address: the pool address (label only — on-chain state is read
                from the shared BotState snapshot, not the address).
            pool_id_hex: required for V4 (the StateView key).

        """
        if self._verify_rpc_url is None or self._verify_state_view is None:
            # No verify config — skip, same as batch verify_liquidity_maps.
            return
        if family == "v3":
            await self.engine.verify_v3_post_drain_snapshot(
                address=address,
                rpc_url=self._verify_rpc_url,
            )
        elif family == "v4":
            assert pool_id_hex is not None
            await self.engine.verify_v4_post_drain_snapshot(
                pool_manager_address=address,
                pool_id_hex=pool_id_hex,
                rpc_url=self._verify_rpc_url,
                state_view_address=self._verify_state_view,
            )

    def register_v2_pool(self, pool: UniswapV2Pool) -> int:  # noqa: D102
        if pool.address in self._v2_keys:
            return self._v2_keys[pool.address]
        # ADR-006 slice 9: with the engine sharing the bot's BotState, the V2
        # pool is ALREADY registered there by `bot.build_pool` (the V2 builder
        # calls `py_bot.register_v2_pool` + hands back the PyLiquidityPool
        # handle). Re-registering via `engine.register_v2_pool` would panic on
        # the duplicate address. Cache the shared pool_id for path-building;
        # orient via zero_for_one at register_path time (no `fwd_key + 1` shim).
        key = pool._py_pool.pool_id  # noqa: SLF001
        # Note: _fee_token0/_fee_token1 asymmetry warning retained for
        # diagnostics — the engine reads fees from the shared BotState now, so
        # no engine.register_v2_pool call carries a fee here.
        if pool._fee_token0 != pool._fee_token1:  # noqa: SLF001
            bot_logger.warning(
                f"Asymmetric V2 fees detected for {pool.address} "
                f"(fee_token0={pool._fee_token0}, fee_token1={pool._fee_token1}).",  # noqa: SLF001
            )
        self._v2_keys[pool.address] = key
        return key

    def register_aerodrome_pool(self, pool: AerodromeV2Pool) -> int:
        """Register an Aerodrome V2 pool's shared-core key.

        Mirrors :meth:`register_v2_pool` — the pool is already registered in
        the shared ``BotState`` by ``aerodrome_v2_builder``'s call to
        ``py_bot.register_aerodrome_pool``. Cache the ``pool_id`` (the
        engine derives the Solidly hop family from the ``BotState`` identity at
        ``register_path`` time, so no engine-side pre-registration carries a
        family tag).

        Returns:
            The registered pool's engine ``pool_id``.

        """
        if pool.address in self._v2_keys:
            return self._v2_keys[pool.address]
        key = pool._py_pool.pool_id  # noqa: SLF001
        self._v2_keys[pool.address] = key
        return key

    async def register_v3_pool(
        self,
        pool: UniswapV3Pool,
    ) -> int:
        """Register a V3 pool with the Rust engine.

        Tick data is resolved automatically by the Rust engine from
        the streamed snapshot (loaded via stream_v3_snapshot_to_engine).
        The engine applies buffered events on top of stale snapshot data.

        Returns:
            The registered pool's engine ``pool_id``.

        """
        if pool.address in self._v3_keys:
            return self._v3_keys[pool.address]

        # ADR-006 slice 9 / D1: the engine shares the Bot's BotState, so the
        # V3 pool is ALREADY registered there by `bot.build_pool` (the V3
        # builder calls `py_bot.register_v3_pool` + hands back the
        # `PyLiquidityPool` handle). Re-registering via
        # `engine.register_v3_pool` would PANIC the Rust core (``BotCore::
        # register_v3_pool`` panics on duplicate address, unlike V4 which
        # raises a catchable ``ValueError``) — taking the process down. Mirror
        # the V2 path: read the shared-core pool_id from the PyLiquidityPool
        # handle and cache it so subsequent paths short-circuit. (V2's
        # `register_v2_pool` documents the same shared-state invariant.)
        key = pool._py_pool.pool_id  # noqa: SLF001

        # T6 (ADR-006 D4): fail-fast two-step verify at the drain seam — the
        # fail-fast detection the dead `engine.register_v3_pool` +
        # `verify_on_register` gate was meant to provide, now at the method
        # actually called per-pool during `build_paths`. Step 1: snapshot
        # verify (pinned seed vs on-chain @ snapshot block) BEFORE the drain —
        # catches a bad seed/serialization at the offending pool, not 18k pools
        # later. CBCH6H: `verify_seed=True` compares the pinned snapshot seed,
        # NOT engine-current — during a rolling start (`resume()` precedes
        # `build_paths`) the live pump applies Mint/Burn onto engine-current;
        # comparing that vs on-chain@snapshot (pre-journal) would false-
        # mismatch on every active pool. The seed is verified exactly once
        # (consumed) so memory is bounded. V2 has no tick map; V3/V4 only.
        # Async (awaited) — never `block_on` (the ecf576de tokio deadlock).
        # Gated by verify config (same posture as batch `verify_liquidity_maps`).
        await self._verify_pool_seed_at_block(
            "v3",
            pool.address,
            self._verify_snapshot_block,
        )

        # Drain the backfill/pump buffer onto the snapshot seed. *(unchanged)*
        self.engine.apply_buffer_v3(pool.address)

        # Step 2: post-drain verify (pinned `(tick_data, block)` pair vs
        # on-chain @ **the pinned block**) AFTER the drain — catches a bad
        # buffer-apply (e.g. the V4 register->seed clobber at 292101f) at the
        # offending pool. The post-drain pin is captured atomically with
        # `apply_buffer_v3`'s final drain (single core.write() hold) so a pump
        # Mint/Burn landing AFTER the drain cannot corrupt the comparison (the
        # step-2 rolling-start race — logs/verify-race-hotloop.log:
        # `update_block=25396803 > block=25396790`). The pin carries its OWN
        # block (the `update_block` at drain time) — step-2 takes no `block`
        # argument (pre-fix the registry passed `verify_backfill_block` as a
        # constant, which fabricated a mismatch on active pools during a slow
        # `build_paths`: pump Mint/Burn at blocks PAST the backfill boundary
        # advanced the drained state to a later block — 2026-06-29 crash).
        # Per-pool (not batch) + race-free.
        await self._verify_pool_post_drain("v3", pool.address)

        self._v3_keys[pool.address] = key
        return key

    async def register_v4_pool(
        self,
        pool: UniswapV4Pool,
    ) -> int:
        """Register a V4 pool with the Rust engine.

        Tick data is resolved automatically by the Rust engine from
        the streamed snapshot (loaded via stream_v4_snapshot_to_engine).
        The engine applies buffered events on top of stale snapshot data.

        Pool admission (amount-modifying hooks / dynamic fees) is enforced
        by the Rust core as a *correctness floor* — the solver's V3-CL math
        assumes no hook intervention + a fixed fee. A rejection surfaces as a
        typed ``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError``
        (both subclass ``ValueError``); ``build_paths`` classifies by type.

        Returns:
            The registered pool's engine ``pool_id``.

        """
        pool_id_hex = pool.pool_id.to_0x_hex()

        if pool_id_hex in self._v4_keys:
            return self._v4_keys[pool_id_hex]

        # ADR-006 slice 9 / D1: the engine shares the Bot's BotState, so the
        # V4 pool is ALREADY registered there by `bot.build_managed_pool` (the
        # V4 builder calls `py_bot.register_v4_pool` + hands back the
        # `PyLiquidityPool` handle). Re-registering via
        # `engine.register_v4_pool` would raise ValueError("V4 pool already
        # registered") for every V4 hop in every discovered path — and, since
        # the cache below is only set on success, the same pool would trip it
        # repeatedly. Mirror the V2 path: read the shared-core pool_id from the
        # PyLiquidityPool handle and cache it so subsequent paths short-circuit.
        #
        # V4 hook/dynamic-fee admission (HookedPoolRejectedError /
        # DynamicFeePoolRejectedError) is enforced at `bot.build_managed_pool`
        # time — i.e. BEFORE this method is ever called — so it surfaces from
        # the builder, not here.
        key = pool._py_pool.pool_id  # noqa: SLF001

        # T6 (ADR-006 D4): fail-fast two-step verify at the drain seam.
        # Step 1: snapshot verify (pinned seed vs on-chain @ snapshot block).
        # CBCH6H: `verify_seed=True` compares the pinned snapshot seed, not
        # engine-current (rolling-start race fix; see register_v3_pool).
        await self._verify_pool_seed_at_block(
            "v4",
            pool.address,
            self._verify_snapshot_block,
            pool_id_hex=pool_id_hex,
        )

        # Drain backfill/pump buffer — same rationale as register_v3_pool.
        self.engine.apply_buffer_v4(pool.address, pool_id_hex)

        # Step 2: post-drain verify (pinned `(tick_data, block)` pair vs
        # on-chain @ the pinned block). Same race-free contract as V3 step-2
        # (see register_v3_pool) — the pin carries its OWN block; step-2 takes
        # no `block` argument.
        await self._verify_pool_post_drain(
            "v4",
            pool.address,
            pool_id_hex=pool_id_hex,
        )

        self._v4_keys[pool_id_hex] = key
        return key

    def knows_pool(self, address: str) -> bool:
        """Return whether a V2 or V3 pool with `address` is registered.

        Returns:
            True if registered.

        """
        return address in self._v2_keys or address in self._v3_keys

    def knows_v4_pool(self, pool_id_hex: str) -> bool:
        """Return whether a V4 pool with `pool_id_hex` is registered.

        Returns:
            True if registered.

        """
        return pool_id_hex in self._v4_keys

    def register_path(
        self,
        pools_and_zfos: Sequence[tuple[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool, bool]],
    ) -> int:
        """Register a path from concrete pool objects + per-hop directions.

        ``HopInfo`` is built via ``build_hops_from_pools`` from pool
        attributes; each pool's engine key is resolved from this registry's
        key maps. Uses ``register_and_solve_path`` for eager solving so the
        path is immediately included in the next result batch.

        Returns:
            The registered path's ``path_id``.

        Raises:
            ValueError: If any pool in the path has not been registered with
                this registry.

        A path-composition policy rejection (when a predicate is injected)
        surfaces as a typed ``PathRejectedError`` subtype (e.g.
        ``TokenDenylistedError``, ``HopCountExceededError``,
        ``DuplicatePoolError``) BEFORE hop building + engine dispatch —
        distinct from the Rust core's pool-admission floor
        (``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError``).

        """
        # D7KMQO: enforce deployment policy before any work. A rejection
        # raises a PathRejectedError subtype and never reaches the engine —
        # mirrors how V4 admission (HookedPoolRejectedError) is typed.
        self.path_predicate.evaluate(pools_and_zfos)
        hops = build_hops_from_pools(pools_and_zfos)
        engine_hops: list[tuple[int, bool]] = []
        for pool, zfo in pools_and_zfos:
            if isinstance(pool, UniswapV4Pool):
                key = self._v4_keys.get(pool.pool_id.to_0x_hex())
            elif isinstance(pool, AerodromeV2Pool):
                # Aerodrome shares the same address→pool_id map as V2 (pool
                # contract addresses are globally unique). The engine's
                # ``derive_hop_type`` reads the Aerodrome identity off the
                # shared ``BotState`` and classifies a stable pool as the
                # Solidly hop family.
                key = self._v2_keys.get(pool.address)
            elif isinstance(pool, UniswapV2Pool):
                key = self._v2_keys.get(pool.address)
            else:  # V3
                key = self._v3_keys.get(pool.address)
            if key is None:
                msg = f"Pool not registered: {pool}"
                raise ValueError(msg)
            # ADR-006 D3: register_path takes (pool_id, zero_for_one) — the
            # engine derives the family from the Bot. One pool_id per pool;
            # orientation is zero_for_one (the old `fwd_key + 1` reverse-id
            # shim is gone — Bot is 1-id-per-pool post-ADR-003).
            engine_hops.append((key, zfo))

        path_id = self.engine.register_and_solve_path(engine_hops)
        self.paths[path_id] = PathInfo(hops=hops)
        return path_id
