"""The canonical bot-startup orchestrator (Plan 102, slice 3).

:class:`EngineRegistry` is the **one correct way to start** a
:class:`~degenbot._ffi.ArbitrageEngine` operator: it runs the
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
from degenbot.arbitrage import ArbitrageEngine
from degenbot.logging import logger as bot_logger

# XEANMB: the `load_*_from_py` ingestion surface + `_v3_snapshot_to_py_dict`
# / `_v4_snapshot_to_py_dict` converters are retired (the in-memory
# SnapshotStore is gone). Per-pool tick data is read via the Db arm (held tx) or
# the Chain arm (RPC) at registration; `start()` only sets the snapshot seed
# block `S`.
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.utils.bytes import to_0x_hex

from ._claims import AsyncioFutureWake, ClaimRecord, VerifyClaims
from .policy import NoOpPathPredicate, PathCompositionPredicate

if TYPE_CHECKING:
    import asyncio
    from collections.abc import Awaitable, Callable, Sequence

    from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
    from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
    from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot

__all__ = ["EngineRegistry"]


class EngineRegistry:
    """Thin wrapper over the Rust ArbitrageEngine.

    Maintains Python pool ↔ Rust key mappings so events can be routed
    to the right engine pool, and results can be mapped back to Python
    pool objects for encoding.

    The first pool in each path provides the flash borrow: V2 via
    uniswapV2Call, V3 via swapCallback, V4 via unlockCallback.

    V4 pools are keyed by pool_id hex string (sufficient since the bot
    uses a single PoolManager). The Rust engine additionally receives
    the pool_manager address at registration time.
    """

    def __init__(  # ruff:ignore[undocumented-public-init]
        self,
        bot: Bot | None = None,
        *,
        engine: ArbitrageEngine | None = None,
        path_predicate: PathCompositionPredicate | None = None,
    ) -> None:
        # ADR-006 D1+D4: the engine adopts the Bot's shared BotState, so the
        # engine reads/writes the SAME core that V2 Pool handles
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
            self.engine = ArbitrageEngine(py_bot=bot._py_bot)  # ruff:ignore[private-member-access]
        self._v2_keys: dict[str, int] = {}  # address → pool_id (shared BotState)
        self._v3_keys: dict[str, int] = {}
        # V4 pools keyed by pool_id hex — for event routing from PoolManager logs
        self._v4_keys: dict[str, int] = {}  # pool_id_hex → pool_id
        # DMZ3DD (NRHEAC): per-pool in-flight claims that close the
        # register_v3/v4_pool check-then-act TOCTOU under concurrent
        # registration workers. The claim record + the leader/peer/
        # release-on-failure policy live ONCE in `arbitrage._claims`
        # (VerifyClaims); these are the loop-bound asyncio claim tables — one
        # per family (address → record for V3, pool_id_hex → record for V4),
        # read directly by the racing-sibling observers (each record awaits
        # as its wrapped Future). A worker claims the entry BEFORE the
        # blocking-RPC verify awaits; a worker that sees the claim awaits the
        # SAME record instead of re-running the verify, so a pool is verified
        # at most once. V2 is intentionally not covered: `register_v2_pool`
        # is SYNC with no await between check and cache-set, so it is already
        # atomic on the single loop.
        self._v3_inflight: dict[str, ClaimRecord[asyncio.Future[int]]] = {}
        self._v4_inflight: dict[str, ClaimRecord[asyncio.Future[int]]] = {}
        self._v3_claims: VerifyClaims[asyncio.Future[int], int] = VerifyClaims(
            AsyncioFutureWake(),
            self._v3_inflight,
        )
        self._v4_claims: VerifyClaims[asyncio.Future[int], int] = VerifyClaims(
            AsyncioFutureWake(),
            self._v4_inflight,
        )
        # NXM2BF: the Python `PathInfo` relay is retired. `register_path`
        # returns the Rust `path_id`; `DispatchCandidate` resolves the
        # encoder's `composers::PathInfo` from that `path_id` via
        # `PyArbitrageEngine.path_info_for_core`. The `[profit]` hop-detail
        # render reads `outcome.path_infos` (Rust→Py), not a stored Python copy.
        # ADR-006 D4 + D7KMQO: a pluggable path-composition predicate enforces
        # deployment policy (token denylist/allowlist, hop-count bounds,
        # min-liquidity, duplicate-pool guard) BEFORE hop building + engine
        # dispatch. Distinct from the Rust core's pool-admission floor
        # (HookedPoolRejectedError / DynamicFeePoolRejectedError). Default:
        # NoOpPathPredicate (accept all).
        self.path_predicate: PathCompositionPredicate = path_predicate or NoOpPathPredicate()
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

        Sequences: subscribe(ws) → load snapshots → verify config.
        Stops at the snapshot-loaded phase so the caller can attach its result
        consumer before batches begin to flow (`resume()` is the single gate
        after which the pump emits one ResultBatch per block into the
        fire-and-forget channel — attaching the consumer after resume risks
        unbounded backlog and stale-batch dispatch; `resume()` also runs the
        auto-backfill that closes the snapshot→WS gap, J3FMDO).

        DB snapshot (JUCFCB, Shape 2): the V3+V4 DB snapshot is eagerly loaded
        into the core ``BotState`` at ``Bot.__init__`` time via
        ``Bot.load_snapshot_from_db`` — so the DB path needs NO snapshot
        kwargs here. The snapshot seed block ``S`` stays on the shared
        ``BotState``; the per-pool two-step verify (step-1) reads the stashed
        ``_verify_snapshot_block`` (set below from the same source), and the
        snapshot→WS backfill runs automatically inside ``resume()``
        (`BlockPump::resume_from_subscribe`) using the pump's own HTTP provider.

        Non-DB snapshots (file/memory): pass ``v3_snapshot``/``v4_snapshot``
        kwargs; each is converted to a single Python dict and handed to the
        engine via ``load_v3_snapshot_from_py`` / ``load_v4_snapshot_from_py``
        (ONE PyO3 crossing per family — DADWUP retired the per-pool
        ``insert_*_pool_snapshot`` crossings), then
        ``snapshot_block = min(s.newest_block)`` stashes the per-pool step-1
        verify seed. These two kwargs are non-DB-only — the DB path constructs
        the Bot with ``config.database.path`` and passes no snapshots here.

        Returns:
            The first observed WS block from ``subscribe`` (the resume live-loop
            anchor).

        """
        # Compute the snapshot seed block `S` BEFORE `subscribe()` so the
        # core's `after_subscribe` phase transition sees `core_has_snapshot =
        # true` and advances the engine phase to `SnapshotLoaded` (required by
        # `resume()`). XEANMB: the non-DB path no longer fills a
        # `SnapshotStore` via `load_*_from_py` (retired); per-pool tick data is
        # read through the Db arm (held tx) or the Chain arm (RPC) at
        # registration. `S` is the only thing stashed here — it drives the
        # core auto-backfill inside `resume()` (J3FMDO) that closes the
        # snapshot→WS gap.
        if v3_snapshot is not None or v4_snapshot is not None:
            # Non-DB (file/memory) path — `S = min(newest_block)` across the
            # supplied snapshots. The tick-data dicts themselves are NOT
            # ingested here (the Store is retired, epic XEANMB); per-pool tick
            # data is fetched via the Chain arm (RPC) at registration.
            snapshot_block = min(
                s.newest_block for s in (v3_snapshot, v4_snapshot) if s is not None
            )
        else:
            # DB path (Shape 2): snapshot already loaded at Bot construction;
            # read S from the core BotState via the engine getter.
            snapshot_block = self.engine.snapshot_seed_block

        # Only set when a snapshot was supplied (else there's no seed).
        self._verify_snapshot_block = snapshot_block
        # 2SM4Y7: record S on the shared BotState so the core auto-backfill
        # inside `resume()` (J3FMDO) closes the snapshot→WS gap (the pyo3
        # `backfill_from_snapshot` is retired; the non-DB path sets S via the
        # `snapshot_seed_block` property setter — the DB path's
        # `load_snapshot_from_db` already set S).
        if snapshot_block is not None:
            self.engine.snapshot_seed_block = snapshot_block

        backfill_target = self.engine.subscribe(node_ws)

        # Verify config (consumer-safe: nothing emits yet).
        self.engine.set_verify_rpc_url(node_http)
        if verify_state_view is not None:
            self.engine.set_verify_state_view(verify_state_view)

        # Intentionally NOT calling resume() — the caller attaches its
        # consumer next, then calls resume() as the single batch-flow gate.
        return backfill_target

    def register_v2_pool(self, pool: UniswapV2Pool) -> int:  # ruff:ignore[undocumented-public-method]
        if pool.address in self._v2_keys:
            return self._v2_keys[pool.address]
        # ADR-006 slice 9: with the engine sharing the bot's BotState, the V2
        # pool is ALREADY registered there by `bot.build_pool` (the V2 builder
        # calls `py_bot.register_v2_pool` + hands back the Pool
        # handle). Re-registering via `engine.register_v2_pool` would panic on
        # the duplicate address. Cache the shared pool_id for path-building;
        # orient via zero_for_one at register_path time (no `fwd_key + 1` shim).
        key = pool._py_pool.pool_id  # ruff:ignore[private-member-access]
        # Note: _fee_token0/_fee_token1 asymmetry warning retained for
        # diagnostics — the engine reads fees from the shared BotState now, so
        # no engine.register_v2_pool call carries a fee here.
        if pool._fee_token0 != pool._fee_token1:  # ruff:ignore[private-member-access]
            bot_logger.warning(
                f"Asymmetric V2 fees detected for {pool.address} "
                f"(fee_token0={pool._fee_token0}, fee_token1={pool._fee_token1}).",  # ruff:ignore[private-member-access]
            )
        self._v2_keys[pool.address] = key
        return key

    def register_aerodrome_pool(self, pool: AerodromeV2Pool) -> int:
        """Register an Aerodrome V2 pool's shared-core key.

        Mirrors :meth:`register_v2_pool` — the pool is already registered in
        the shared ``BotState`` by the delegated ``build_aerodrome_v2`` path's call to
        ``py_bot.register_aerodrome_pool``. Cache the ``pool_id`` (the
        engine derives the Solidly hop family from the ``BotState`` identity at
        ``register_path`` time, so no engine-side pre-registration carries a
        family tag).

        Returns:
            The registered pool's engine ``pool_id``.

        """
        if pool.address in self._v2_keys:
            return self._v2_keys[pool.address]
        key = pool._py_pool.pool_id  # ruff:ignore[private-member-access]
        self._v2_keys[pool.address] = key
        return key

    async def register_v3_pool(
        self,
        pool: UniswapV3Pool,
    ) -> int:
        """Register a V3 pool with the Rust engine.

        Tick data is resolved automatically by the Rust engine from
        the loaded snapshot (fed via load_v3_snapshot_from_py or the DB
        path's load_snapshot_from_db). The engine applies buffered events on
        top of stale snapshot data.

        Returns:
            The registered pool's engine ``pool_id``.

        """
        # ADR-006 slice 9 / D1: the engine shares the Bot's BotState, so the V3
        # pool is ALREADY registered there by `bot.build_pool` (the V3 builder
        # calls `py_bot.register_v3_pool` + hands back the Pool
        # handle). Re-registering via `engine.register_v3_pool` would PANIC the
        # Rust core on the duplicate address — taking the process down. Mirror
        # the V2 path: read the shared-core pool_id off the handle and cache it
        # so subsequent paths short-circuit.
        return await self._register_with_claim(
            claims=self._v3_claims,
            keys=self._v3_keys,
            cache_key=pool.address,
            key_of=lambda: pool._py_pool.pool_id,  # ruff:ignore[private-member-access]
            lifecycle=lambda: self.engine.run_v3_registration_lifecycle(
                pool.address,
                self._verify_snapshot_block,
            ),
        )

    async def register_v4_pool(
        self,
        pool: UniswapV4Pool,
    ) -> int:
        """Register a V4 pool with the Rust engine.

        Tick data is resolved automatically by the Rust engine from
        the loaded snapshot (fed via load_v4_snapshot_from_py or the DB
        path's load_snapshot_from_db). The engine applies buffered events on
        top of stale snapshot data.

        Pool admission (amount-modifying hooks / dynamic fees) is enforced
        by the Rust core as a *correctness floor* — the solver's V3-CL math
        assumes no hook intervention + a fixed fee. A rejection surfaces as a
        typed ``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError``
        (both subclass ``ValueError``); ``build_paths`` classifies by type.

        Returns:
            The registered pool's engine ``pool_id``.

        """
        # ADR-006 slice 9 / D1: the engine shares the Bot's BotState, so the V4
        # pool is ALREADY registered there by `bot.build_managed_pool` (the V4
        # builder calls `py_bot.register_v4_pool` + hands back the
        # Pool handle). Re-registering via `engine.register_v4_pool`
        # would raise ValueError("V4 pool already registered") for every V4 hop
        # in every discovered path — and, since the cache is only set on
        # success, the same pool would trip it repeatedly. Mirror the V2 path:
        # read the shared-core pool_id and cache it. V4 hook/dynamic-fee
        # admission is enforced at `bot.build_managed_pool` time — BEFORE this
        # method is ever called — so it surfaces from the builder, not here.
        pool_id_hex = to_0x_hex(pool.pool_id)
        return await self._register_with_claim(
            claims=self._v4_claims,
            keys=self._v4_keys,
            cache_key=pool_id_hex,
            key_of=lambda: pool._py_pool.pool_id,  # ruff:ignore[private-member-access]
            lifecycle=lambda: self.engine.run_v4_registration_lifecycle(
                pool.address,
                pool_id_hex,
                self._verify_snapshot_block,
            ),
        )

    @staticmethod
    async def _register_with_claim(
        *,
        claims: VerifyClaims[asyncio.Future[int], int],
        keys: dict[str, int],
        cache_key: str,
        key_of: Callable[[], int],
        lifecycle: Callable[[], Awaitable[object]],
    ) -> int:
        """Run ONE family's DMZ3DD registration dance (the V3/V4 twins, NRHEAC).

        The key-cache short-circuit leads; everything after it is the
        at-most-once claim dance of `arbitrage._claims` (claim-if-absent /
        await-if-present / release-on-failure + the unretrieved-exception
        hygiene band) driven over the asyncio adapter — stated once there,
        not per family. Family differences are parameters: the claim key
        shape (``cache_key`` — address for V3, pool_id hex for V4), the key
        map, and the core-owned lifecycle call.

        The at-most-once claim matters because the lifecycle (IKGQ6F /
        ADR-022 D1, core-owned) sequences quarantine (6N7XVR) → seed-verify
        @ snapshot block → drain+pin (single core.write() hold) →
        post-drain-verify @ the pin's own block → set_live, with the
        mismatch tripwire as the final gate; double-running it is wasted RPC
        and, on a tight post-drain-verify, can false-trip the tripwire if
        the first run's pin moved the anchor (sparse pools are immediate
        no-ops; tracked pools are Live only after verification).

        The leader settles the claim with the family key exactly where the
        retired twins called `claim.set_result(key)` (after the lifecycle);
        the key-cache set now lands after the claim settles — both it and
        the retired pre-`set_result` cache set sit in the same no-await tail
        on the loop, so no peer or fresh worker can observe the swap. The
        family key is read inside the claim window, before the lifecycle (a
        plain int property read — a dead `_py_pool` handle would surface as
        a failed, retriable claim instead of a pre-claim raise;
        pathological only).

        Returns:
            The registered pool's engine `pool_id` (peers receive the
            leader's settled key from the shared claim).

        """
        if cache_key in keys:
            return keys[cache_key]

        async def _lifecycle_then_key() -> int:
            key = key_of()
            await lifecycle()
            return key

        key = await claims.run(cache_key, _lifecycle_then_key)
        keys[cache_key] = key
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

    @property
    def verify_snapshot_block(self) -> int | None:
        """The seeded snapshot block for the two-step verify (T1/T6).

        PRG-5 seat-thread surface: the crawl units run on fleet seats (no
        asyncio loop) and read this once per unit to pass into the blocking
        lifecycle FFI — the same value the async register path stashes.
        """
        return self._verify_snapshot_block

    def run_v3_verify_lifecycle_sync(self, address: str) -> None:
        """Drive a V3 pool's core-owned verify lifecycle, BLOCKING (PRG-5).

        The seat-thread twin of the lifecycle inside :meth:`register_v3_pool`:
        same core choreography and the same snapshot seed block — only the
        park shape differs (a fleet seat owns no asyncio loop). The retry
        contract (VerificationRpcError retried; VerificationMismatchError
        fatal) is applied by the CALLER — the unit wraps this with the
        pipeline's policy (the registry has none).

        The loop-bound bookkeeping of :meth:`register_v3_pool` (the key cache
        + the asyncio in-flight claims, DMZ3DD) is NOT touched here — those
        structures remain single-loop state for the operator surface; the
        crawl's own at-most-once verify lifecycle is the pipeline's
        thread-safe seat claims table.
        """
        self.engine.run_v3_registration_lifecycle_sync(
            address,
            self._verify_snapshot_block,
        )

    def run_v4_verify_lifecycle_sync(
        self,
        pool_manager: str,
        pool_id_hex: str,
    ) -> None:
        """V4 seat-thread twin of :meth:`run_v3_verify_lifecycle_sync`."""
        self.engine.run_v4_registration_lifecycle_sync(
            pool_manager,
            pool_id_hex,
            self._verify_snapshot_block,
        )

    def register_crawl_path(
        self,
        engine_hops: Sequence[tuple[int, bool]],
    ) -> tuple[int, bool]:
        """Register a resolved hop list straight into the engine (PRG-5).

        The seat-thread pathRegistration used by the crawl units: unlike
        :meth:`register_path` it takes ALREADY-RESOLVED ``(pool_id,
        zero_for_one)`` hops (the unit gets the ids off the build handles —
        the loop-bound ``_vN_keys`` caches are not consulted or populated).
        The D7KMQO path predicate is evaluated by the caller over the concrete
        pools BEFORE hop building. Returns ``(path_id, created)`` with the
        same semantics as :meth:`register_path` (dedup by construction in the
        engine, PRG-4; the cap refusal surfaces as typed
        :class:`PathRegistryFullError`).

        Returns:
            ``(path_id, created)`` — `created` is False when the engine's
            signature dedup answered (PRG-4).

        """
        return self.engine.register_and_solve_path(list(engine_hops))

    def register_path(
        self,
        pools_and_zfos: Sequence[tuple[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool, bool]],
    ) -> tuple[int, bool]:
        """Register a path from concrete pool objects + per-hop directions.

        Each pool's engine key is resolved from this registry's key maps +
        dispatched as a ``(key, zero_for_one)`` tuple to the engine's
        ``register_and_solve_path`` (eager solve — the path is immediately
        included in the next result batch). NXM2BF: the Python ``PathInfo``
        relay is retired — ``DispatchCandidate`` resolves the encoder's
        ``composers::PathInfo`` from the returned ``path_id`` via
        ``PyArbitrageEngine.path_info_for_core`` (no Python hop build, no
        stored copy).

        Returns:
            ``(path_id, created)`` — `created` is `False` when the engine's
            own signature dedup answered with an existing `path_id` (PRG-4:
            dedup is by construction core-side; the Python dedup set
            retired).

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
        engine_hops: list[tuple[int, bool]] = []
        for pool, zfo in pools_and_zfos:
            if isinstance(pool, UniswapV4Pool):
                key = self._v4_keys.get(to_0x_hex(pool.pool_id))
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

        return self.engine.register_and_solve_path(engine_hops)
