"""The canonical bot-startup orchestrator (Plan 102, slice 3).

:class:`EngineRegistry` is the **one correct way to start** a
:class:`~degenbot._ffi.ArbitrageEngine` operator: it runs the
pre-pump startup ritual (``subscribe`` → stream snapshots → ``backfill`` →
verify config) and *stops before* ``resume()``, so the caller can attach its
result consumer before any batches flow. It also registers pools and paths.

It is a **thin adapter**: every decision it used to own now belongs to the
Rust core. Pool identity is derived, not mirrored — a pool's engine
``pool_id`` comes from the shared ``BotState`` through
``ArbitrageEngine.pool_id_for_pool`` / ``pool_id_for_v4_pool`` (or straight
off the pool's own core handle), so this module holds no address → id map
that could disagree with the state owner. The at-most-once verify claim is
likewise core-owned: ``run_v*_registration_lifecycle`` enters the session's
``VerifyClaims`` table inside the driver (ADR-022), so concurrent
registration workers share one lifecycle run without a Python claim table.

The name stays ``EngineRegistry`` because it remains the public registration
interface for operators; the registration *state* it used to own is gone.

Lifted from ``examples/eth_backrun_v2_v3_v4_rust.py`` — engine-operation
machinery only. Deployment policy (the main loop, dispatcher, simulation
overrides) stays example-side (B-mid scope).
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

from .policy import NoOpPathPredicate, PathCompositionPredicate

if TYPE_CHECKING:
    from collections.abc import Sequence

    from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
    from degenbot.runner.config import VerificationRetryPolicy
    from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
    from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot

__all__ = ["EngineRegistry"]

#: The core family tag each Python pool class registers under in the shared
#: ``BotState``. Used to ask the core for a pool's engine ``pool_id`` by
#: identity; the engine derives the hop family from that same registration, so
#: the tag is the join between a Python pool object and the core's identity.
_V2_FAMILY = "v2"
_AERODROME_FAMILY = "aerodrome-v2"
_V3_FAMILY = "v3"


class EngineRegistry:
    """Thin adapter over the Rust engine: the public registration interface.

    Registration is three questions, all answered by the core:

    * **which id does this pool have?** — the shared ``BotState``, either off
      the pool's own core handle or, for a caller holding only an identity,
      via :meth:`ArbitrageEngine.pool_id_for_pool` /
      :meth:`ArbitrageEngine.pool_id_for_v4_pool`.
    * **has this pool been verified?** — the core's own registration verify
      lifecycle, run at most once per live claim window and skipped entirely
      once it has completed (ADR-022 D1). The durable fact lives on the shared
      driver, never here.
    * **what is this path's id?** — the engine's signature dedup, via
      :meth:`register_and_solve_path`.

    The first pool in each path provides the flash borrow: V2 via
    uniswapV2Call, V3 via swapCallback, V4 via unlockCallback. V4 pools are
    keyed by the ``(PoolManager, pool_id)`` pair — the engine receives the
    pool_manager address at registration time.
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

        Delegates the core ordering to the engine's one-call startup ritual
        (the Rust ``EngineDriver::start`` mirror — ``subscribe(ws)`` then
        verify-config); the Python side keeps only the snapshot seed ``S``
        resolution and the non-DB ``set_snapshot_seed_block`` call.
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
        (``BlockPump::resume_from_subscribe``) using the pump's own HTTP provider.

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
        # `SnapshotStore` via `load_*_from_py` (retired); per-pool tick
        # data is read through the Db arm (held tx) or the Chain arm (RPC) at
        # registration. `S` is the only thing stashed here — it drives the
        # core auto-backfill inside `resume()` (J3FMDO) that closes the
        # snapshot→WS gap (the pyo3 `backfill_from_snapshot` is retired; the
        # non-DB path sets S via the `snapshot_seed_block` property setter —
        # the DB path's `load_snapshot_from_db` already set S).
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

        # The one-call startup ritual (the Rust `EngineDriver::start` mirror):
        # subscribe(ws) -> verify-config(http, view), consumer-safe (it stops
        # before `resume()`, so nothing emits yet). `S` is already on the shared
        # `BotState` above; the driver reads it internally while subscribing.
        # Intentionally NOT calling resume() — the caller attaches its
        # consumer next, then calls resume() as the single batch-flow gate.
        return self.engine.start(node_http, node_ws, verify_state_view)

    @staticmethod
    def register_v2_pool(pool: UniswapV2Pool) -> int:
        """Return the shared-core ``pool_id`` of a V2 pool.

        ADR-006 slice 9: with the engine sharing the bot's BotState, the V2
        pool is ALREADY registered there by `bot.build_pool` (the V2 builder
        calls `py_bot.register_v2_pool` + hands back the Pool
        handle), and re-registering via `engine.register_v2_pool` would panic
        on the duplicate address. The id is read off the pool's own core
        handle — the state owner's answer, not a Python cache — and
        orientation is decided at register_path time (no `fwd_key + 1` shim).

        Returns:
            The pool's engine ``pool_id``.

        """
        # Note: _fee_token0/_fee_token1 asymmetry warning retained for
        # diagnostics — the engine reads fees from the shared BotState now, so
        # no engine.register_v2_pool call carries a fee here.
        if pool._fee_token0 != pool._fee_token1:  # ruff:ignore[private-member-access]
            bot_logger.warning(
                f"Asymmetric V2 fees detected for {pool.address} "
                f"(fee_token0={pool._fee_token0}, fee_token1={pool._fee_token1}).",  # ruff:ignore[private-member-access]
            )
        return pool._py_pool.pool_id  # ruff:ignore[private-member-access]

    @staticmethod
    def register_aerodrome_pool(pool: AerodromeV2Pool) -> int:
        """Return the shared-core ``pool_id`` of an Aerodrome V2 pool.

        The V2 twin of :meth:`register_v2_pool` — the pool is already
        registered in the shared ``BotState`` by the delegated
        ``build_aerodrome_v2`` path's call to ``py_bot.register_aerodrome_pool``,
        and the engine derives the Solidly hop family from that ``BotState``
        identity at ``register_path`` time, so no engine-side pre-registration
        carries a family tag.

        Returns:
            The registered pool's engine ``pool_id``.

        """
        return pool._py_pool.pool_id  # ruff:ignore[private-member-access]

    async def register_v3_pool(
        self,
        pool: UniswapV3Pool,
    ) -> int:
        """Run a V3 pool's core-owned verify lifecycle and return its ``pool_id``.

        Tick data is resolved by the Rust engine from the loaded snapshot
        (fed via load_v3_snapshot_from_py or the DB path's load_snapshot_from_db).
        The engine applies buffered events on top of stale snapshot data.

        ADR-006 slice 9 / D1: the engine shares the Bot's BotState, so the V3
        pool is ALREADY registered there by `bot.build_pool` (the V3 builder
        calls `py_bot.register_v3_pool` + hands back the Pool
        handle). Re-registering via `engine.register_v3_pool` would PANIC the
        Rust core on the duplicate address — taking the process down — so this
        reads the shared-core pool_id off the handle and runs the lifecycle.

        The at-most-once policy and the durable verify-once fact both live in
        the CORE: the lifecycle call enters the driver's session claim table
        (ADR-022 D1), so N concurrent workers registering one pool run the
        choreography once and each receive its outcome, and a completed
        lifecycle is recorded on the driver so a later registration of the
        same identity is a no-op. There is deliberately no Python claim table
        or verified-pool set here — a caller that raced on its own map would
        have re-run the verify.

        Returns:
            The registered pool's engine ``pool_id``.

        """
        await self.engine.run_v3_registration_lifecycle(
            pool.address,
            self._verify_snapshot_block,
        )
        return pool._py_pool.pool_id  # ruff:ignore[private-member-access]

    async def register_v4_pool(
        self,
        pool: UniswapV4Pool,
    ) -> int:
        """Run a V4 pool's core-owned verify lifecycle and return its ``pool_id``.

        Tick data is resolved by the Rust engine from the loaded snapshot
        (fed via load_v4_snapshot_from_py or the DB path's load_snapshot_from_db).
        The engine applies buffered events on top of stale snapshot data.

        Pool admission (amount-modifying hooks / dynamic fees) is enforced
        by the Rust core as a *correctness floor* — the solver's V3-CL math
        assumes no hook intervention + a fixed fee. A rejection surfaces as a
        typed ``HookedPoolRejectedError`` / ``DynamicFeePoolRejectedError``
        (both subclass ``ValueError``); ``build_paths`` classifies by type.

        ADR-006 slice 9 / D1: the pool is ALREADY registered in the shared
        ``BotState`` by `bot.build_managed_pool`; re-registering would raise
        ``ValueError("V4 pool already registered")`` for every V4 hop in every
        discovered path. V4 hook/dynamic-fee admission is enforced at
        `bot.build_managed_pool` time — BEFORE this method is ever called — so
        it surfaces from the builder, not here. As with V3, the at-most-once
        verify claim is the driver's (ADR-022 D1), keyed by the
        ``(PoolManager, pool_id)`` pair.

        Returns:
            The registered pool's engine ``pool_id``.

        """
        await self.engine.run_v4_registration_lifecycle(
            pool.address,
            to_0x_hex(pool.pool_id),
            self._verify_snapshot_block,
        )
        return pool._py_pool.pool_id  # ruff:ignore[private-member-access]

    def pool_id(self, pool: UniswapV2Pool | AerodromeV2Pool | UniswapV3Pool | UniswapV4Pool) -> int:
        """Resolve the engine ``pool_id`` of a pool from its canonical identity.

        The read the retired per-family key maps used to serve, now asked of
        the shared ``BotState``: a family tag plus the pool's own address for
        the address-keyed families, and the ``(PoolManager, pool_id)`` pair
        for V4. Identity is the key, so a pool object and the core can never
        disagree about which id a hop means.

        Returns:
            The pool's engine ``pool_id``.

        Raises:
            ValueError: If no pool with that identity is registered in the
                shared ``BotState``.

        """
        if isinstance(pool, UniswapV4Pool):
            key = self.engine.pool_id_for_v4_pool(pool.address, to_0x_hex(pool.pool_id))
        elif isinstance(pool, AerodromeV2Pool):
            # Aerodrome registers under its own family tag; the engine derives
            # the Solidly hop family from the same `BotState` identity at
            # `register_path` time.
            key = self.engine.pool_id_for_pool(_AERODROME_FAMILY, pool.address)
        elif isinstance(pool, UniswapV2Pool):
            key = self.engine.pool_id_for_pool(_V2_FAMILY, pool.address)
        else:  # V3
            key = self.engine.pool_id_for_pool(_V3_FAMILY, pool.address)
        if key is None:
            msg = f"Pool not registered: {pool}"
            raise ValueError(msg)
        return key

    def knows_pool(self, address: str) -> bool:
        """Return whether a V2 or V3 pool with `address` is registered.

        The core-derived question ("does the shared ``BotState`` hold a pool
        with this address in this family?") — no Python map answers it.

        Returns:
            True if registered.

        """
        return (
            self.engine.pool_id_for_pool(_V2_FAMILY, address) is not None
            or self.engine.pool_id_for_pool(_V3_FAMILY, address) is not None
        )

    def knows_v4_pool(self, pool_manager: str, pool_id_hex: str) -> bool:
        """Return whether a V4 pool is registered for the given pair.

        A V4 pool is identified by its ``(PoolManager, pool_id)`` pair, so the
        manager is part of the question: one manager hosts many pools.

        Returns:
            True if registered.

        """
        return self.engine.pool_id_for_v4_pool(pool_manager, pool_id_hex) is not None

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
        same core choreography, the same session claim table (the seat and the
        operator loop share one driver, so they share one at-most-once window),
        and the same snapshot seed block — only the park shape differs (a fleet
        seat owns no asyncio loop). The retry contract
        (VerificationRpcError retried; VerificationMismatchError fatal) is
        applied by the CALLER — the unit wraps this with the pipeline's
        policy.

        A failed lifecycle releases the claim, so a LATER caller re-runs it;
        a completed one records a durable verified-pool fact on the shared
        driver, so a later call for the same identity is a no-op.
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

    def run_v3_verify_lifecycle_sync_with_retry(
        self,
        address: str,
        policy: VerificationRetryPolicy,
    ) -> None:
        """V3 seat-thread verify under the core-owned bounded retry dance.

        The retry classification (transient RPC/provider vs fatal mismatch) and
        the backoff dance are the core's; this adapter injects the driver's
        resolved policy and passes the stashed snapshot seed block. A transient
        failure releases the lifecycle claim, so a retry re-runs the whole
        choreography.
        """
        self.engine.run_v3_registration_lifecycle_with_retry_sync(
            address,
            self._verify_snapshot_block,
            policy.max_attempts,
            policy.base_delay,
            policy.max_delay,
            policy.jitter,
        )

    def run_v4_verify_lifecycle_sync_with_retry(
        self,
        pool_manager: str,
        pool_id_hex: str,
        policy: VerificationRetryPolicy,
    ) -> None:
        """V4 twin of :meth:`run_v3_verify_lifecycle_sync_with_retry`."""
        self.engine.run_v4_registration_lifecycle_with_retry_sync(
            pool_manager,
            pool_id_hex,
            self._verify_snapshot_block,
            policy.max_attempts,
            policy.base_delay,
            policy.max_delay,
            policy.jitter,
        )

    def register_crawl_path(
        self,
        engine_hops: Sequence[tuple[int, bool]],
    ) -> tuple[int, bool]:
        """Register a resolved hop list straight into the engine (PRG-5).

        The seat-thread pathRegistration used by the crawl units: unlike
        :meth:`register_path` it takes ALREADY-RESOLVED ``(pool_id,
        zero_for_one)`` hops (the unit gets the ids off the build handles).
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

        Each pool's engine key is DERIVED from the shared ``BotState`` by
        canonical identity (family + address, or the V4
        ``(PoolManager, pool_id)`` pair) and dispatched as a
        ``(key, zero_for_one)`` tuple to the engine's
        ``register_and_solve_path`` (eager solve — the path is immediately
        included in the next result batch). NXM2BF: the Python ``PathInfo``
        relay is retired — ``DispatchCandidate`` resolves the encoder's
        ``composers::PathInfo`` from the returned ``path_id`` via
        ``PyArbitrageEngine.path_info_for_core`` (no Python hop build, no
        stored copy).

        A hop whose pool is not registered in the shared ``BotState`` is
        refused by :meth:`pool_id` with a ``ValueError`` before the engine is
        reached.

        Returns:
            ``(path_id, created)`` — `created` is `False` when the engine's
            own signature dedup answered with an existing `path_id` (PRG-4:
            dedup is by construction core-side; the Python dedup set
            retired).

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
            # ADR-006 D3: register_path takes (pool_id, zero_for_one) — the
            # engine derives the hop family from the shared `BotState`
            # identity, so the key is resolved BY IDENTITY here rather than
            # from a per-family Python map. One pool_id per pool; orientation
            # is zero_for_one (the old `fwd_key + 1` reverse-id shim is gone —
            # Bot is 1-id-per-pool post-ADR-003).
            engine_hops.append((self.pool_id(pool), zfo))

        return self.engine.register_and_solve_path(engine_hops)
