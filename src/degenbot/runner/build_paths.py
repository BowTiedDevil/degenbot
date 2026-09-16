"""Path discovery + registration for the settlement-arbitrage ``BotRunner``.

Extracted from ``examples/eth_backrun_v2_v3_v4_rust.py`` (epic 5TSYKN, task
JKYVST). Owns ``build_paths`` and its registration machinery:
:class:`ConstructionContext` (registration-owned construction resources kept
out of the main-loop trim), :class:`PathRegistrationPipeline` (the reusable,
pump-concurrent per-path registration / verify / dedup), and the bounded
producer/consumer helper that drives it.

The driver is Python-companion orchestration (``stays-python``): it registers
paths with the Rust-owned engine (``EngineRegistry``) but owns no pool state.
"""

from __future__ import annotations

import asyncio
import os
import time
from collections import Counter, deque
from collections.abc import AsyncGenerator, AsyncIterable, Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, cast

from sqlalchemy import text
from sqlalchemy.orm import Session

from degenbot import Bot, UniswapV2Pool, UniswapV3Pool, UniswapV4Pool, get_checksum_address
from degenbot.arbitrage._claims import ThreadEventWake, VerifyClaims
from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.arbitrage.verification_retry import (
    VerificationRetryPolicy,
    retry_verification_call,
)
from degenbot.database.models.pools import (
    UniswapV2PoolTableBase,
    UniswapV3PoolTableBase,
    UniswapV4PoolTable,
    UniswapV4PoolTableBase,
)
from degenbot.exceptions import (
    DirectionResolutionError,
    PathRegistryFullError,
    PathRejectedError,
    VerificationMismatchError,
    VerificationRpcError,
)
from degenbot.logging import logger as bot_logger
from degenbot.pathfinding import discovery_batch_size as _rust_discovery_batch_size
from degenbot.pathfinding import find_paths_async
from degenbot.runner._driver_constants import (
    ALLOWED_INTERMEDIATE_TOKENS,
    PANCAKESWAP_V3_MAINNET_FACTORY,
    SUSHISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V4_POOL_MANAGER_ADDRESS,
    WETH_ADDRESS,
)
from degenbot.runner._registration_ledger import (
    RegistrationLedger,
    RegistrationOutcome,
)
from degenbot.uniswap.trackers import UniswapV3PoolTracker
from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS
from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot
from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    import threading


def _discovery_batch_size() -> int:
    """Read the typed pathfinding.discovery_batch_size (4IOEVT).

    The Rust config loader is the only env reader; the value is
    positive-clamped there and find_paths_async clamps to >= 1, so every
    batch_size forwards straight to the Rust batched async iterator.

    Returns:
        The effective discovery delivery batch size.

    """
    return max(1, int(_rust_discovery_batch_size()))


# ──────────────────────────────────────────────────────────────────
# Permutation filter helpers
# ──────────────────────────────────────────────────────────────────


def _concrete_pool_types(base_type: type) -> list[type]:
    """Expand an abstract pool table base into its concrete subclasses."""
    if not getattr(base_type, "__abstract__", False):
        return [base_type]
    subs = base_type.__subclasses__()
    if not subs:
        return [base_type]
    result: list[type] = []
    for s in subs:
        result.extend(_concrete_pool_types(s))
    return result


_POOL_VERSION_MAP: dict[str, list[type]] = {
    "V2": _concrete_pool_types(UniswapV2PoolTableBase),
    "V3": _concrete_pool_types(UniswapV3PoolTableBase),
    "V4": [UniswapV4PoolTable],
}


def _parse_permutation_filter(
    perms: set[str] | None,
) -> list[set[type] | None] | None:
    """Convert a set of permutation strings like {'V3-V4-V3'} into a
    pool_type_per_depth list suitable for find_paths_async.

    Returns None if perms is None/empty (no filter).
    Returns a list of sets, one per depth, where each set contains the
    allowed pool table types at that depth. If all permutations agree
    that any type is allowed at a depth, that entry is None.
    """
    if not perms:
        return None
    parsed: list[list[str]] = []
    for perm in perms:
        parts = perm.split("-")
        if not all(p in _POOL_VERSION_MAP for p in parts):
            msg = f"Invalid permutation '{perm}': unknown version tag"
            raise ValueError(msg)
        parsed.append(parts)
    if len({len(p) for p in parsed}) != 1:
        msg = f"All permutations must have the same depth, got: {perms}"
        raise ValueError(msg)
    max_depth = len(parsed[0])
    result: list[set[type] | None] = []
    for depth in range(max_depth):
        allowed_this_depth: set[type] = set()
        for perm_parts in parsed:
            allowed_this_depth.update(_POOL_VERSION_MAP[perm_parts[depth]])
        result.append(allowed_this_depth or None)
    return result


def _pool_types_from_filter(perms: set[str] | None) -> list[type]:
    """Derive the pool_types list from the permutation filter.

    When a permutation filter is set, only include pool table types for
    the version tags mentioned in the permutations. When the filter is
    None/empty, include all V2/V3/V4 types so every permutation is
    discoverable.
    """
    if not perms:
        types: set[type] = set()
        for version_types in _POOL_VERSION_MAP.values():
            types.update(version_types)
        return list(types)

    versions_needed: set[str] = set()
    for perm in perms:
        versions_needed.update(perm.split("-"))

    types = set()
    for version in versions_needed:
        types.update(_POOL_VERSION_MAP[version])
    return list(types)


# ──────────────────────────────────────────────────────────────────
# Direction resolver
# ──────────────────────────────────────────────────────────────────


#: Hard cap on registered paths per process. Registration stops accepting
#: new paths once ``path_count`` reaches this value (each capped candidate is
#: counted as a ``path-cap`` skip); the engine then reaches steady state with
#: a bounded path universe so solve performance is observable without ongoing
#: registration load. Override with DEGENBOT_MAX_PATHS (0 = uncapped). The
#: code default and the running environment may diverge (devcontainers export
#: their own value), so the pipeline announces the effective cap at startup.
MAX_REGISTERED_PATHS = int(os.environ.get("DEGENBOT_MAX_PATHS", "100000"))


#: Paths legally in flight on the fleet intake at once (PRG-5). The bounded
#: producer/consumer QUEUE retired with the crawl shell; the backpressure is
#: now this submission window over the fleet's duty-counted seats: the crawl
#: submits a unit, and only when the OLDEST receipt resolves does the next
#: submission leave the window — discovery can never outrun registration by
#: more than the window, and the fiscal bound lives in the fleet's own
#: queue_cap (2x pool_state_updater_slots). 8x the default seat count: wide
#: enough to keep every seat fed through a long verify, small enough that a
#: path-cap stop holds only a handful of in-flight units.
REG_INTAKE_WINDOW = 32


@dataclass
class RegistrationUnitOutcome:
    """The per-path unit outcome, reported back to the driver.

    The units run on fleet seats (plain threads, possibly concurrent), so
    they NEVER touch the pipeline counters — they return one of these and
    the single-loop driver folds it into the summary counters exactly as the
    retired inline ``_consume`` did (counter parity is the PRG-3/5 bar).
    """

    #: "skip" (build/direction benign skip) | "reject" (engine registration
    #: refusal, counted as engine_reject) | "registered" | "cap" (the benign
    #: registered-path-cap stop, PRG-4).
    kind: str
    #: The stable skip/reject tag (never an interpolated address) for
    #: ``_record_skip`` and the engine-reject log line.
    tag: str | None = None
    #: "registered": the engine path registry created a NEW path (False = the
    #: signature dedup answered an existing id, PRG-4).
    created: bool = False
    #: V4 pools that entered the registration stage of this unit (the parity
    #: witness for ``v4_pool_count`` — counted on registered AND rejected
    #: outcomes, matching the retired inline increment-before-verify).
    v4_hops: int = 0
    #: Whether the skip adds to ``skip_count`` (V4 admission refusals do not
    #: — they carry their own counters; byte-parity with the retired body).
    counts_as_skip: bool = True
    #: The exception text (build/registration detail) for the driver-side
    #: first-few-occurrences skip log (the retired body logged the exception
    # object at the skip site; the seat keeps no counters, so the text rides
    #: the outcome).
    detail: str | None = None


class _SeatVerifyClaims:
    """At-most-once seat-thread verify lifecycle claims (the DMZ3DD twin).

    The asyncio in-flight claims in ``EngineRegistry.register_v3/v4_pool``
    are single-loop state (they are the OPERATOR surface's dedup); the crawl
    units run on fleet seats — plain threads — so the same check-then-act
    window needs thread primitives. NRHEAC: the claim record + the
    leader/peer/release-on-failure policy (first unit claims, peers park,
    a failed claim is released for a LATER unit to re-run) live ONCE in
    ``degenbot.arbitrage._claims`` (:class:`VerifyClaims`); this class is
    the thin ``threading.Event`` adapter shell — seat-thread construction
    plus the ``run_exclusive`` name the pipeline call sites use. The first unit
    to claim a pool's verify runs the lifecycle; a concurrent peer parks
    on the claim's event and re-raises the leader's exact exception (a
    failed lifecycle stays retriable: the failed claim is released, a
    LATER unit re-runs it).
    """

    def __init__(self) -> None:
        self._claims: VerifyClaims[threading.Event, None] = VerifyClaims(
            ThreadEventWake(),
        )

    def run_exclusive(self, key: str, run: Callable[[], object]) -> None:
        """Run ``run()`` at most once per live claim window; peers wait it."""
        self._claims.run_sync(key, run)


def resolve_directions(
    pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool],
    input_token_address: str,
) -> list[bool] | None:
    """Determine zero_for_one for each hop so the cycle closes.

    The cycle: input_token → hop_0 → intermediate → hop_1 → ... → input_token.
    Returns a list of zfo values (one per hop), or None if the cycle cannot
    close (token mismatch).

    V4 pools use NATIVE_CURRENCY_ADDRESS (address(0)) for ETH. For direction
    resolution, we treat NATIVE_CURRENCY_ADDRESS as equivalent to WETH — since
    our profit token is always WETH.
    """
    addr = get_checksum_address(input_token_address)
    start_addr = addr
    zfo_list: list[bool] = []

    for i, pool in enumerate(pools):
        token0_addr = get_checksum_address(pool.token0.address)
        token1_addr = get_checksum_address(pool.token1.address)

        # V4: treat NATIVE_CURRENCY_ADDRESS as WETH for matching
        if token0_addr == NATIVE_CURRENCY_ADDRESS:
            token0_addr = WETH_ADDRESS
        if token1_addr == NATIVE_CURRENCY_ADDRESS:
            token1_addr = WETH_ADDRESS

        if token0_addr == addr:
            zfo = True  # selling token0 (input) for token1
        elif token1_addr == addr:
            zfo = False  # selling token1 (input) for token0
        else:
            # Fatal: the pathfinder contract guarantees every yielded path's
            # hops chain from a requested boundary token, so a mid-path token
            # mismatch means the constructed pool object disagrees with the DB
            # subgraph edge (wrong pool built, stale subgraph, or a builder
            # bug). This is an invariant violation — skip-and-continue would
            # silently drop every path touching that pool (observed live:
            # 85k skips, 0 registrations), so fail-stop loudly instead.
            raise DirectionResolutionError(
                message=(
                    f"hop {i}/{len(pools)}: pool {pool} has "
                    f"token0={token0_addr} token1={token1_addr}; expected either "
                    f"to carry the tracked input token {addr} "
                    f"(path starts at {start_addr})"
                )
            )

        addr = token1_addr if zfo else token0_addr
        zfo_list.append(zfo)

    if addr != get_checksum_address(input_token_address):
        raise DirectionResolutionError(
            message=(
                f"cycle does not close: final output {addr} != input "
                f"{start_addr}; pools={[str(pool) for pool in pools]}"
            )
        )

    return zfo_list


@dataclass
class ConstructionContext:
    """Registration-owned construction resources, kept out of run()'s trim.

    Bundles everything ``build_paths`` needs to construct and register pools,
    so the registration task owns them as a single self-contained context for
    its lifetime. ``BotRunner.run()`` trims *main-loop* state
    (``release_python_state()`` + ``self.bot = None``); the context is a
    *separate* identity that a background registration task holds and that the
    trim never severs — the decoupling seam for Sub-B (background registration
    on the pump runtime).

    The three V3 trackers + the WETH token are built once here (at
    :meth:`for_bot`), not re-derived per pool.
    """

    bot: Bot
    chain_id: int
    db: Any
    uniswap_v3_tracker: UniswapV3PoolTracker
    sushiswap_v3_tracker: UniswapV3PoolTracker
    pancakeswap_v3_tracker: UniswapV3PoolTracker
    weth: Any  # Erc20Token (WETH)

    @classmethod
    def for_bot(
        cls,
        bot: Bot,
        v3_snapshot: UniswapV3LiquiditySnapshot | None,
    ) -> ConstructionContext:
        """Build the construction context for a bot, creating the trackers + WETH once."""
        uniswap_v3_tracker = bot.add_tracker(
            UniswapV3PoolTracker,
            factory_address=UNISWAP_V3_MAINNET_FACTORY,
            snapshot=v3_snapshot,
        )
        sushiswap_v3_tracker = bot.add_tracker(
            UniswapV3PoolTracker,
            factory_address=SUSHISWAP_V3_MAINNET_FACTORY,
            snapshot=v3_snapshot,
        )
        pancakeswap_v3_tracker = bot.add_tracker(
            UniswapV3PoolTracker,
            factory_address=PANCAKESWAP_V3_MAINNET_FACTORY,
            snapshot=v3_snapshot,
        )
        weth = bot.build_erc20token(WETH_ADDRESS)
        return cls(
            bot=bot,
            chain_id=bot.chain_id,
            db=bot.db,
            uniswap_v3_tracker=uniswap_v3_tracker,
            sushiswap_v3_tracker=sushiswap_v3_tracker,
            pancakeswap_v3_tracker=pancakeswap_v3_tracker,
            weth=weth,
        )


class PathRegistrationPipeline:
    """Reusable, pump-concurrent registration pipeline (NWTUM3 / D1c).

    Owns the per-path registration work that ``build_paths`` previously ran
    inline: construction (through the retained ``ConstructionContext`` — the
    Rust ``PoolBuilder``), engine registration + verification, direction
    resolution, registered-path dedup, per-path release, and the summary
    counters.

    It is LONG-LIVED by design: it keeps the ``ConstructionContext`` AND the
    ``engine_registry`` for the session's lifetime, so an operator can add a
    specific path (``enqueue_path``) or trigger a bounded on-demand discovery
    (``trigger_discovery``) at ANY time — including after ``run()`` trims the
    main-loop bot. The context survives the trim (Sub-A seam), so these
    methods never need the dropped Python ``bot``. The pipeline never awaits
    the pump, so adds/discovery cannot block update/solve/dispatch.

    The fail-fast tripwire is preserved: a fatal ``VerificationMismatchError``
    / ``VerificationRpcError`` is NOT swallowed here — it propagates out of the
    worker and must abort the pipeline loudly.
    """

    def __init__(
        self,
        *,
        context: ConstructionContext,
        engine_registry: EngineRegistry,
        retry_policy: VerificationRetryPolicy | None = None,
    ) -> None:
        self.constr_ctx = context
        self.constr_bot = context.bot
        self.constr_chain_id = context.chain_id
        self.constr_db = context.db
        self.uniswap_v3_tracker = context.uniswap_v3_tracker
        self.sushiswap_v3_tracker = context.sushiswap_v3_tracker
        self.pancakeswap_v3_tracker = context.pancakeswap_v3_tracker
        self.weth = context.weth
        self.engine_registry = engine_registry
        self.retry_policy_obj = retry_policy or VerificationRetryPolicy()

        # PRG-5 hard cutover (IRUMXD): the crawl shell (the bounded
        # producer/consumer queue + the bounded offload executor) retired.
        # The construction home is the fleet's duty-counted `PoolStateUpdater`
        # intake (census row fleet_pool_state_updater_slots; Deferrable
        # cordon class) — unconditionally, with no parallel implementation.
        # The stance is read ONCE here (construction-time, never per call).
        self._fleet_intake = bool(
            getattr(self.constr_bot, "registration_fleet_hosted", lambda: False)()
        )
        if not self._fleet_intake:
            msg = (
                "registration is fleet-hosted only (PRG-5 hard cutover, epic "
                "IRUMXD): the legacy crawl shell (bounded queue + offload "
                "executor) is retired and the worker fleet is the only "
                "behavior (CQLMM2 stance cutover) — the fleet intake boot "
                "descriptor is missing, so the engine was not constructed "
                "or its fleet boot failed; check the worker-census boot "
                "table for fleet_pool_state_updater_slots."
            )
            raise RuntimeError(msg)
        # The seat-thread at-most-once verify-claims table (the DMZ3DD twin
        # for units running concurrently on seats — the loop-bound asyncio
        # claims in EngineRegistry serve the operator surface only).
        self._verify_claims = _SeatVerifyClaims()

        # Configured discovery inputs (set by the driver before discovery runs).
        self.pool_types: list[type] = []
        self.pool_type_per_depth: list[set[type] | None] | None = None

        # PRG-4: the registered-path budget transfers to the engine path
        # registry (MAX_REGISTERED_PATHS, 0 = uncapped). Counters keep the
        # Progress summary; the dedup set and the pre-count cap gate retire.
        # (Pipeline tests run with engine_registry=None.)
        py_engine = getattr(self.engine_registry, "engine", None)
        if py_engine is not None and hasattr(py_engine, "set_path_cap"):
            py_engine.set_path_cap(MAX_REGISTERED_PATHS or None)
        bot_logger.info(
            f"[build_paths] registered-path cap: "
            f"{MAX_REGISTERED_PATHS or 'uncapped'} "
            f"({'DEGENBOT_MAX_PATHS' if os.environ.get('DEGENBOT_MAX_PATHS') else 'code default'})"
        )

        # Summary counters.
        self.path_count = 0
        self.cap_skip_count = 0
        self.skip_count = 0
        self.token_filter_count = 0
        self.engine_reject_count = 0
        self.dup_count = 0
        self.register_fail_count = 0
        self.v4_pool_count = 0
        self.v4_hook_rejected = 0
        self.v4_dynamic_fee_rejected = 0
        self.other_exc_count = 0
        # The registration outcome ledger owns the four memos that used to
        # live here ad-hoc — the registered-path dup fast-path, the verify-once
        # pool fact, the unregistrable-pool stable refusals, and the
        # deterministic rejected-path deny — plus the typed build-refusal
        # classification and the bounded metric tag vocabulary. A transient
        # failure is never memoized (a raced build or blip stays retryable).
        # See _registration_ledger.
        self._ledger = RegistrationLedger()
        # INN6TK observability: reason-tagged skip breakdown + time-throttled
        # progress emission. The legacy `[build_paths] Progress` line only fires
        # when `path_count` crosses each 1000-boundary; a discovery-heavy crawl
        # that registers few paths never prints it, hiding the skip/dup/reject
        # reasons. We record a reason tag per skip and emit the same summary on
        # a wall-clock cadence so the cause stays visible mid-crawl.
        self._skip_reasons: Counter[str] = Counter()
        self._last_progress_ts = 0.0
        # PRG-4: the benign registered-path-cap stop witness (the retired
        # DiscoveryCrawlComplete unwind exception became this flag).
        self.capped = False
        # Structural edition of the DB subgraph for which a discovery sweep
        # last ran to NATURAL completion (not bound-truncated, not capped):
        # once latched, a later trigger with the same edition stops BEFORE
        # enumerating — the unchanged structure can only re-yield paths the
        # pipeline already saw. See trigger_discovery.
        self._sweep_completed_edition: tuple[int, int, int, int] | None = None

    #: Seconds between periodic registration-progress summaries (time-based, so
    #: they fire even when ``path_count`` never reaches the 1000 print gate).
    _PROGRESS_INTERVAL_S = float(os.environ.get("DEGENBOT_REG_PROGRESS_SECS", "30"))

    def _registration_unit(
        self,
        path_steps: Any,
        directions: list[bool] | None = None,
    ) -> RegistrationUnitOutcome:
        """The per-path registration unit — SYNC, runs on a fleet seat (PRG-5).

        The whole ``_consume`` body of the retired crawl shell, re-homed: the
        hop builds ride the SAME Rust single-flighted build path (PRG-1), the
        V3/V4 verify lifecycles run BLOCKING on the shared tokio runtime
        (the seat owns no loop — the async twins serve the operator surface)
        under the seat-claims at-most-once table (DMZ3DD), and the path Las
        registers straight through the engine FFI (the dedup + cap are the
        engine's, PRG-4). One unit = one path = one receipt; counters are
        NEVER mutated here (concurrent seats) — the outcome travels back to
        the single-loop driver, which folds it into the summary (the
        counter-parity contract).
        """
        steps = list(path_steps)
        pool_type_strs: list[str] = []
        for step in steps:
            if issubclass(step.type, UniswapV2PoolTableBase):
                pool_type_strs.append("V2")
            elif issubclass(step.type, UniswapV3PoolTableBase):
                pool_type_strs.append("V3")
            elif issubclass(step.type, UniswapV4PoolTableBase):
                pool_type_strs.append("V4")
            else:
                pool_type_strs.append("")

        # Unregistrable-pool memo: a hop whose STABLE build refusal was already
        # recorded answers here, before any build or verify — the pathological
        # DFS region re-yielded one refused pool alongside thousands of
        # candidate paths. The ledger owns the record.
        for step, pt in zip(steps, pool_type_strs, strict=True):
            memo = self._ledger.unregistrable_record(self._ledger.pool_memo_key(step, pt))
            if memo is not None:
                return RegistrationUnitOutcome(
                    kind="skip",
                    tag=memo.outcome.value,
                    counts_as_skip=memo.counts_as_skip,
                )

        pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool] = []
        for step, pt in zip(steps, pool_type_strs, strict=True):
            if pt not in {"V2", "V3", "V4"}:
                return RegistrationUnitOutcome(
                    kind="skip",
                    tag=RegistrationOutcome.UNKNOWN_POOL_TYPE.value,
                )
            if pt == "V4" and not step.hash:
                return RegistrationUnitOutcome(
                    kind="skip",
                    tag=RegistrationOutcome.V4_NO_HASH.value,
                )
            try:
                # PRG-1: the Rust build path single-flights duplicate builds
                # and answers already-registered identities from the registry
                # of record; PRG-2: the V4 admission gate (dynamic-fee /
                # fee-encoder-limit / hooked) is answered pre-RPC by the FFI
                # build call as a typed refusal.
                if pt == "V2":
                    pool = self.constr_bot.build_pool(step.address, silent=True)
                elif pt == "V3":
                    pool = self._build_v3_fallback_chain(step.address)
                else:
                    pool = self.constr_bot.build_managed_pool(
                        address=UNISWAP_V4_POOL_MANAGER_ADDRESS,
                        pool_id=step.hash,
                        silent=True,
                    )
            except Exception as exc:
                # Ledger ask: typed stable-vs-transient classification (never
                # the exception class name). A stable refusal is a pool fact —
                # memoize its hop identity; a transient failure stays
                # retryable (CXKACI: PoolAlreadyRegisteredError is a race
                # artifact, never a pool fact).
                refusal = self._ledger.classify_build_refusal(exc, pool_type=pt)
                if refusal.stable:
                    self._ledger.memoize_unregistrable(
                        self._ledger.pool_memo_key(step, pt),
                        refusal.outcome,
                        counts_as_skip=refusal.counts_as_skip,
                    )
                return RegistrationUnitOutcome(
                    kind="skip",
                    tag=refusal.outcome.value,
                    counts_as_skip=refusal.counts_as_skip,
                    detail=refusal.detail,
                )
            pools.append(cast("UniswapV2Pool | UniswapV3Pool | UniswapV4Pool", pool))

        # Every hop is built (or was answered by the registry): the pools that
        # reached the registration stage are the v4_pool_count parity witness.
        v4_hops = sum(1 for pt in pool_type_strs if pt == "V4")

        reg = self.engine_registry

        # ── Registration stage (seat-thread) ──
        # Pool registration INTO the engine already happened inside the build
        # (PRG-1: the builders publish into the shared BotState). What remains
        # per CL pool is the verify choreography (quarantine → seed-verify →
        # drain+pin → post-drain-verify → set_live, IKGQ6F/ADR-022 D1), then
        # the path registration (D7KMQO predicate + engine hop list). The sync
        # lifecycle twins run parked on the shared tokio runtime inside
        # py.detach; the seat claims table keeps them at-most-once per live
        # window (concurrent paths sharing a pool would otherwise verify
        # twice — wasted RPC and a false-mismatch tripwire risk).
        #
        # Fatal contract preserved byte-for-byte from the retired body: a
        # VerificationMismatchError / VerificationRpcError is NOT swallowed —
        # it propagates out of the seat (through the receipt) and aborts the
        # pipeline loudly. Every OTHER registration-stage exception is the
        # counted engine-reject path.
        #
        # Order within the try: directions → policy gate → hop ids → the dup
        # fast-path → verify → register. The D7KMQO docstring promises
        # "before any work", and the dup fast-path (W73FVY) must sit in front
        # of verify: behind it, every duplicate candidate re-paid the full
        # verify choreography because the engine dedup only answers at
        # register_crawl_path (post-PRG-4 the crawl could not outrun its own
        # duplicates — verified-pool thrash of 945 lifecycles/122 pools live).
        try:
            # Resolve directions. A resolution failure is a fatal invariant
            # violation (subgraph vs constructed-pool disagree); the raised
            # DirectionResolutionError aborts the registration pipeline and
            # propagates to shut the bot down loudly.
            zfo_list = self._resolve_path_directions(pools, directions)
            if zfo_list is None:
                # Operator-pinned directions whose per-hop count disagrees with
                # the resolved path — the `zip(strict=True)` below would raise
                # a cryptic TypeError (None is not iterable): name it, skip.
                return RegistrationUnitOutcome(
                    kind="skip",
                    tag=RegistrationOutcome.DIRECTION_MISMATCH.value,
                )

            # Hop ids before the gate: pure reads of the built pools, needed
            # by BOTH the negative path memo below and the D7KMQO gate — the
            # gate still precedes ALL verify/RPC work (the constraint it has
            # always carried).
            engine_hops = [
                (self._pool_engine_id(pool), zfo) for pool, zfo in zip(pools, zfo_list, strict=True)
            ]
            hop_sig = tuple(engine_hops)

            # Rejected-path memo (cold-soak follow-up): the D7KMQO gate deny
            # and the engine path-predicate deny are DETERMINISTIC per hop
            # signature — a re-yieldedcandidate is answered without a second
            # gate evaluation.
            if self._ledger.path_rejected(hop_sig):
                return RegistrationUnitOutcome(
                    kind="reject",
                    tag=RegistrationOutcome.PATH_REJECTED.value,
                )

            # D7KMQO: enforce deployment policy before any verify work — the
            # same pre-check register_path runs for the operator surface (a
            # rejection is a typed PathRejectedError subtype, never engine);
            # the deny is deterministic, so it memoizes before propagating.
            try:
                reg.path_predicate.evaluate(list(zip(pools, zfo_list, strict=True)))
            except PathRejectedError:
                self._ledger.memoize_rejected_path(hop_sig)
                raise

            # Dup fast-path (W73FVY): answer hop signatures ALREADY registered
            # before the verify choreography, with the SAME outcome shape the
            # engine dedup produces (created=False → the driver folds the dup
            # counters identically, v4_hops parity included). The engine path
            # registry stays the source of truth: this memo is exact within
            # the pipeline (every crawl path and both operator surfaces funnel
            # through _registration_unit; no other production caller registers
            # crawl paths), and a memo miss merely re-pays the old cost — the
            # engine still dedups at register_crawl_path. Concurrency: the
            # signature is an immutable tuple and the only mutation is
            # set.add (GIL-atomic); a raced duplicate add is idempotent, and
            # a raced miss still resolves through engine dedup. Memory is
            # bounded by the registered-path cap (MAX_REGISTERED_PATHS).
            if self._ledger.path_registered(hop_sig):
                return RegistrationUnitOutcome(
                    kind="registered",
                    created=False,
                    v4_hops=v4_hops,
                )

            for pool, pt in zip(pools, pool_type_strs, strict=True):
                if pt == "V2":
                    # V2 needs no lifecycle; mirror the retired
                    # register_v2_pool diagnostic (asymmetric-fee warning) —
                    # the key cache itself is the OPERATOR surface's state.
                    v2_pool = cast("UniswapV2Pool", pool)
                    if v2_pool._fee_token0 != v2_pool._fee_token1:  # ruff:ignore[private-member-access]
                        bot_logger.warning(
                            f"Asymmetric V2 fees detected for {v2_pool.address} "
                            f"(fee_token0={v2_pool._fee_token0}, "  # ruff:ignore[private-member-access]
                            f"fee_token1={v2_pool._fee_token1}).",  # ruff:ignore[private-member-access]
                        )
                elif pt == "V3":
                    # DMZ3DD seat twin: at-most-once verify per pool address.
                    # Cold-soak verify-once memo: the claims table dedups
                    # CONCURRENT windows only; sequential sightings re-ran
                    # the lifecycle for the same live pool (~50x live). A
                    # completed lifecycle is a pool fact — never re-run it
                    # this pipeline lifetime; a failed one is not recorded
                    # (the add rides a successful return).
                    v3_key = f"v3:{pool.address}"
                    if not self._ledger.pool_verified(v3_key):
                        self._verify_claims.run_exclusive(
                            v3_key,
                            lambda pool=pool, reg=reg: reg.run_v3_verify_lifecycle_sync(
                                pool.address,
                            ),
                        )
                        self._ledger.memoize_verified_pool(v3_key)
                elif pt == "V4":
                    v4_pool = cast("UniswapV4Pool", pool)
                    v4_key = f"v4:{to_0x_hex(v4_pool.pool_id)}"
                    if not self._ledger.pool_verified(v4_key):
                        self._verify_claims.run_exclusive(
                            v4_key,
                            lambda v4_pool=v4_pool, reg=reg, policy=self.retry_policy_obj: (
                                retry_verification_call(
                                    policy,
                                    reg.run_v4_verify_lifecycle_sync,
                                    UNISWAP_V4_POOL_MANAGER_ADDRESS,
                                    to_0x_hex(v4_pool.pool_id),
                                )
                            ),
                        )
                        self._ledger.memoize_verified_pool(v4_key)

            # PRG-4: the engine path registry dedups by construction; `created`
            # is False exactly when the core signature dedup answered, and the
            # cap refusal surfaces as the typed PathRegistryFullError (PRG-4).
            try:
                _path_id, created = reg.register_crawl_path(engine_hops)
            except PathRegistryFullError:
                # PRG-4: the cap refusal came from the ENGINE path registry —
                # the benign stop (the driver stops discovery on this outcome).
                return RegistrationUnitOutcome(
                    kind="cap",
                    tag=RegistrationOutcome.PATH_CAP.value,
                    v4_hops=v4_hops,
                )
            except PathRejectedError:
                # The engine path-predicate deny is deterministic per hop
                # signature — stable-negative memo before propagating (the
                # outer driver folds the reject identically).
                self._ledger.memoize_rejected_path(hop_sig)
                raise
            except (
                VerificationMismatchError,
                VerificationRpcError,
                DirectionResolutionError,
            ):
                # The typed fatals are never downgraded to a register-fail —
                # a verification failure surfacing through the FFI refuses
                # registration and refuses to be counted as a benign miss.
                raise
            except Exception as exc:
                # A TRANSIENT register failure is deliberately NOT negatively
                # memoized (a raced build or blip must stay retryable).
                return RegistrationUnitOutcome(
                    kind="register-fail",
                    tag=RegistrationOutcome.REGISTER_FAILED.value,
                    detail=f"{type(exc).__name__}: {exc}",
                    v4_hops=v4_hops,
                )

            # A completed registration (created or engine-dedup'd dup) enters
            # the memo; stable-negative outcomes entered THEIR memos at their
            # sites above — a failed verify never registered a path and must
            # stay retriable, so it records nothing here.
            self._ledger.memoize_registered_path(hop_sig)
        except (
            VerificationMismatchError,
            VerificationRpcError,
            DirectionResolutionError,
        ):
            # Fatal invariants preserved from the retired body: on-chain
            # divergence (mismatch = the tripwire), transient-RPC exhaustion
            # after retries, and the subgraph/constructed-pool disagreement —
            # all propagate through the receipt and abort the crawl loudly.
            raise
        except Exception as exc:
            # Engine registration failed — the counted (non-fatal) refusal.
            tag = f"{type(exc).__name__}: {exc}"
            bot_logger.info(
                f"[build_paths] Engine registration failed ({type(exc).__name__}): {exc}",
            )
            return RegistrationUnitOutcome(kind="reject", tag=tag, v4_hops=v4_hops)

        return RegistrationUnitOutcome(
            kind="registered",
            created=created,
            v4_hops=v4_hops,
        )

    @staticmethod
    def _pool_engine_id(
        pool: UniswapV2Pool | UniswapV3Pool | UniswapV4Pool,
    ) -> int:
        """The engine hop key off a build handle (ADR-006 D3: one pool_id)."""
        return pool._py_pool.pool_id  # ruff:ignore[private-member-access]

    def _build_v3_fallback_chain(self, address: str) -> object:
        """The V3 build chain: Uniswap, Sushi, Pancake trackers, generic Bot.

        Runs inside ONE offloaded call (PRG-1: the Rust build path itself
        single-flights duplicate same-family-keyed builds, so pool-identity
        fallbacks cannot race the Rust registration across consumers); the
        whole chain occupies one bounded-executor slot instead of
        re-queueing per rung.
        """
        try:
            return self.uniswap_v3_tracker.get_pool(pool_address=address, silent=True)
        except Exception:
            try:
                return self.sushiswap_v3_tracker.get_pool(pool_address=address, silent=True)
            except Exception:
                try:
                    return self.pancakeswap_v3_tracker.get_pool(pool_address=address, silent=True)
                except Exception:
                    return self.constr_bot.build_pool(address, silent=True)

    def _record_skip(
        self,
        reason: str,
        detail: BaseException | str | None = None,
    ) -> None:
        """Record a reason-tagged skip so the periodic summary shows WHY.

        `reason` is a short stable tag (e.g. ``"build-v3:ConnectionError"``,
        ``"dup"``, ``"direction-fail"``) — never an interpolated address, so
        the aggregate stays compact and greppable.

        PRG-2: the skip ALSO lands in the Rust `degenbot.registration.skips`
        metric family (closed-set labels — the per-error-class detail stays
        here in logs, first few occurrences only so a skip-flood cannot
        resurrect the 2CBDPR motive). The former fatal-memo gate is retired:
        immutable V4 admission verdicts are refused pre-RPC by the core
        registration gate, and raced duplicates self-heal in the build path.
        """
        self._skip_reasons[reason] += 1
        if detail is not None and self._skip_reasons[reason] <= 3:
            bot_logger.debug(f"[build_paths] Pool-build skip ({reason}): {detail}")
        py_bot = getattr(self.constr_bot, "_py_bot", None)
        if py_bot is not None:
            py_bot.record_registration_skip(reason)

    def emit_registration_progress(self, *, force: bool = False) -> None:
        """Log the registration counters + top skip-reason breakdown.

        The legacy ``[build_paths] Progress`` line only fires when ``path_count``
        reaches a multiple of 1000. During a discovery-heavy crawl that registers
        few paths it never fires, so the skip/dup/reject counts (and their
        reasons) stay invisible. This is the same summary emitted on ``force``
        (a wall-clock cadence) so the cause is always observable mid-crawl.
        """
        if not force:
            now = time.monotonic()
            if now - self._last_progress_ts < self._PROGRESS_INTERVAL_S:
                return
            self._last_progress_ts = now
        top = self._skip_reasons.most_common(8)
        breakdown = ", ".join(f"{reason}={n}" for reason, n in top)
        bot_logger.info(
            f"[build_paths] Progress: {self.path_count} paths registered, "
            f"{self.skip_count} skipped, {self.token_filter_count} token-filtered, "
            f"{self.engine_reject_count} engine-rejected, "
            f"{self.register_fail_count} register-fail, "
            f"{self.cap_skip_count} cap-skipped, "
            f"{self.dup_count} duplicates "
            f"{{skip_reasons: {breakdown}}}",
        )

    async def run_registration(self, *, producer: AsyncIterable[object]) -> None:
        """Run the crawl: submit each discovered path as ONE fleet unit.

        PRG-5: the bounded producer/consumer queue retired with the crawl
        shell — discovery iterates directly and every path leaves as a single
        ``PoolStateUpdater`` intake unit (build + verify lifecycles + path
        registration inside the Rust core). The concurrency is the fleet's
        (duty-counted seats + its own bounded queue), and the driver-side
        backpressure is the submission window: at most
        :data:`REG_INTAKE_WINDOW` receipts are outstanding, so discovery can
        never outrun registration by more than the window.

        Units are resolved in FIFO submission order (the retired workers'
        ordering guarantee), so the Progress summary's counter drift and the
        1000-boundary log lines keep their retired shapes exactly.

        Returns with EVERY submitted receipt resolved (the completion clause
        that replaced the retired executor-drain: all cloned
        ``Arc<SnapshotDb>`` handles acquired inside units are dropped before
        ``build_paths`` returns, keeping the close_snapshot_tx()
        Arc::try_unwrap canary quiet — EZOKDR).

        Raises:
            The unit's fatal exception (VerificationMismatchError /
            VerificationRpcError / DirectionResolutionError) propagates
            through the receipt and aborts the crawl loudly — the
            "shut down" contract, unchanged. On a fatal the crawl stops
            submitting immediately (outstanding units still finish — fleet
            units are never cancelled, the Deferrable cordon class).
        """
        inflight: deque[Any] = deque()

        async def _resolve(receipt: Any) -> None:
            """Await one receipt and fold its outcome into the counters."""
            await receipt.wait_async()
            self._absorb_outcome(receipt.result())

        async for path in producer:
            if self.capped:
                break
            # Reap completed receipts first: a fast (unbounded) discovery
            # producer folds outcomes as they land instead of parking a full
            # window of unreaped receipts (and the counters stay fresh for
            # the progress cadence).
            if inflight:
                pending: deque[Any] = deque()
                for receipt in inflight:
                    if receipt.done():
                        await _resolve(receipt)
                    else:
                        pending.append(receipt)
                inflight = pending
                if self.capped:
                    break

            def _unit(path: Any = path) -> RegistrationUnitOutcome:
                return self._registration_unit(path)

            inflight.append(self.constr_bot.submit_registration_unit(_unit))
            if len(inflight) >= REG_INTAKE_WINDOW:
                await _resolve(inflight.popleft())

        # Drain the window: every submitted receipt resolves before the crawl
        # returns (the executor-drain replacement). Past a cap the remaining
        # units short-circuit in the engine (the path registry is full — no
        # build RPC is spent), so the drain is cheap.
        while inflight:
            await _resolve(inflight.popleft())

    async def enqueue_path(
        self,
        path_steps: Any,
        directions: list[bool] | None = None,
    ) -> None:
        """Add ONE specific path at any time (NWTUM3 / D1c operator surface)."""
        await self._consume(path_steps, directions=directions)

    def _graph_edition(self) -> tuple[int, int, int, int] | None:
        """Cheap structural fingerprint of the discovery subgraph.

        The candidate-cycle set is a pure function of the pool-row structure
        (never of pool state/prices), so the row count + max id of each pool
        family for this chain suffices to detect structural change. Returns
        None when no DB handle is attached or the probe fails for any reason
        (fail-open): the latch stays disabled and every sweep runs — the
        pre-latch behavior. A failed probe never blocks discovery.
        """
        db = self.constr_db
        if db is None:
            return None
        try:
            chain = self.constr_chain_id
            with cast("Session", db()) as session:
                v2v3 = session.execute(
                    text("SELECT count(*), COALESCE(max(id), 0) FROM pools WHERE chain = :chain"),
                    {"chain": chain},
                ).one()
                v4 = session.execute(
                    text(
                        "SELECT count(*), COALESCE(max(mp.id), 0) "
                        "FROM managed_pools mp "
                        "JOIN pool_managers pm ON pm.id = mp.manager_id "
                        "WHERE pm.chain = :chain"
                    ),
                    {"chain": chain},
                ).one()
        except Exception:
            return None
        return (
            int(v2v3[0]),
            int(v2v3[1]),
            int(v4[0]),
            int(v4[1]),
        )

    async def trigger_discovery(self, *, bound: int | None = None) -> int:
        """Trigger a bounded one-shot discovery sweep (NWTUM3 / D1c).

        A sweep that runs to NATURAL completion (not bound-truncated, not
        capped) latches the structural graph edition; a later trigger over
        the same edition stops immediately and returns 0 — the unchanged
        structure can only re-yield paths the pipeline already processed.
        The latch re-arms itself when the edition changes (pool added or
        removed), when the probe is unavailable, and after any truncated
        sweep.
        """
        edition = self._graph_edition()
        if edition is not None and edition == self._sweep_completed_edition:
            return 0

        count = 0
        truncated = False
        # 4IOEVT: close the sweep deterministically on the bound-truncation
        # break so the Rust batch iterator is dropped (releasing a mid-DFS
        # search via its cooperative cancel flag) with no zombie threads.
        sweep = self.discovery_sweep()
        try:
            async for item in sweep:
                if bound is not None and count >= bound:
                    truncated = True
                    break
                await self._consume(item)
                count += 1
        finally:
            aclose = getattr(sweep, "aclose", None)
            if aclose is not None:
                await aclose()

        if not truncated and not self.capped and edition is not None:
            self._sweep_completed_edition = edition
        return count

    def discovery_sweep(self) -> AsyncGenerator[object, None]:
        """A single discovery sweep over the DB subgraph (V2/V3/V4 DFS)."""
        return find_paths_async(
            chain_id=self.constr_chain_id,
            start_tokens=[
                WETH_ADDRESS,
                NATIVE_CURRENCY_ADDRESS,  # V4 allows Ether-paired pools
            ],
            end_tokens=[
                WETH_ADDRESS,
                NATIVE_CURRENCY_ADDRESS,  # V4 allows Ether-paired pools
            ],
            max_depth=3,
            pool_types=self.pool_types,
            db=self.constr_db,
            pool_type_per_depth=self.pool_type_per_depth,
            allowed_intermediate_tokens=ALLOWED_INTERMEDIATE_TOKENS,
            batch_size=_discovery_batch_size(),
        )

    def _resolve_path_directions(
        self,
        pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool],
        directions: list[bool] | None,
    ) -> list[bool] | None:
        """Return per-hop directions for `pools` (operator-pinned or resolved)."""
        if directions is not None:
            if len(directions) != len(pools):
                return None
            return list(directions)
        return resolve_directions(pools, self.weth.address)

    async def _consume(
        self,
        path_steps: Any,
        directions: list[bool] | None = None,
    ) -> None:
        """Process a single path: submit it as ONE fleet unit and absorb it.

        The same entry the discovery crawl and BOTH operator surfaces
        (``enqueue_path`` / ``trigger_discovery``) funnel through — the
        per-path body itself is `_registration_unit` (a seat-thread unit);
        this coroutine is the thin submission + counter-fold seam, which is
        also what keeps the operator surface a "thin Rust submission"
        (NWTUM3): the work happens in Rust-coordinated fleet seats, not on
        the event loop.

        Raises:
            The unit's fatal exceptions propagate (VerificationMismatchError
            / VerificationRpcError / DirectionResolutionError — the loud
            shutdown contract), as does any unit panic re-raised through the
            receipt.
        """
        await asyncio.sleep(0)
        # Time-throttled periodic progress summary — fire independently of the
        # path_count==1000 gate so a discovery-heavy skip-fest stays visible.
        self.emit_registration_progress()

        def _unit() -> RegistrationUnitOutcome:
            return self._registration_unit(path_steps, directions)

        receipt = self.constr_bot.submit_registration_unit(_unit)
        await receipt.wait_async()
        self._absorb_outcome(cast("RegistrationUnitOutcome", receipt.result()))

    def _absorb_outcome(self, outcome: RegistrationUnitOutcome) -> None:
        """Fold one unit outcome into the summary counters (driver-side).

        The single-loop mutation point: units on fleet seats never touch the
        counters (concurrency), so the Progress summary and the completion
        log keep their retired counter shapes by construction (PRG-3/5
        counter-parity bar). The tags mirror the retired inline branches.
        """
        if outcome.kind == "skip":
            if outcome.counts_as_skip:
                self.skip_count += 1
            if outcome.tag == "v4-hook-rejected":
                self.v4_hook_rejected += 1
            elif outcome.tag == "v4-dynamic-fee-rejected":
                self.v4_dynamic_fee_rejected += 1
            if outcome.tag is not None:
                self._record_skip(outcome.tag, detail=outcome.detail)
            return
        if outcome.kind == "reject":
            self.engine_reject_count += 1
            self.other_exc_count += 1
            return
        if outcome.kind == "register-fail":
            self.register_fail_count += 1
            self._record_skip(outcome.tag or "register-fail", detail=outcome.detail)
            if self.register_fail_count <= 5:
                bot_logger.warning(f"Path registration failed: {outcome.detail}")
            return
        if outcome.kind == "cap":
            self.skip_count += 1
            self.cap_skip_count += 1
            self.capped = True
            self._record_skip("path-cap")
            bot_logger.info("[build_paths] Path cap reached — stopping discovery crawl")
            return
        # "registered": the parity witness for v4_pool_count is counted on
        # created AND duplicate outcomes (the retired body incremented the
        # V4 counter inside the registration loop, before the dedup check).
        self.v4_pool_count += outcome.v4_hops
        if outcome.created:
            self.path_count += 1
        else:
            self.dup_count += 1
            self._record_skip("dup")
            return
        if self.path_count % 1000 == 0:
            bot_logger.info(
                f"[build_paths] Progress: {self.path_count} paths registered, "
                f"{self.skip_count} skipped, {self.token_filter_count} token-filtered, "
                f"{self.engine_reject_count} engine-rejected, {self.dup_count} duplicates",
            )


async def build_paths(
    *,
    bot: Bot,
    engine_registry: EngineRegistry,
    v3_snapshot: UniswapV3LiquiditySnapshot | None = None,
    v4_snapshot: UniswapV4LiquiditySnapshot | None = None,
    retry_policy: VerificationRetryPolicy | None = None,
    context: ConstructionContext | None = None,
    pipeline: PathRegistrationPipeline | None = None,
    permutation_filter: frozenset[str] | None = None,
) -> None:
    """Discover V2/V3/V4 arb paths, build Python pools, register with Rust engine.

    V4 pools are discovered via find_paths_async and built through
    ``bot.build_managed_pool()``. V4 pool admission (amount-modifying hooks /
    dynamic fees) is enforced by the Rust core at registration time, surfacing
    as typed HookedPoolRejectedError / DynamicFeePoolRejectedError. Each
    ``register_vN_pool`` call is wrapped in ``retry_verification_call`` with a
    bounded retry-with-backoff policy (transient ``VerificationRpcError`` is
    retried; ``VerificationMismatchError`` is never retried and crashes loudly).

    Discovery is a single pass over the DB subgraph driven through a reusable
    :class:`PathRegistrationPipeline`; after it completes the orphan sweep
    releases Tracked pools whose path was skipped before ``register_vN_pool``.
    """
    constr_ctx = context if context is not None else ConstructionContext.for_bot(bot, v3_snapshot)

    pipeline = pipeline or PathRegistrationPipeline(
        context=constr_ctx,
        engine_registry=engine_registry,
        retry_policy=retry_policy,
    )
    perms = set(permutation_filter) if permutation_filter else None
    pipeline.pool_type_per_depth = _parse_permutation_filter(perms)
    pipeline.pool_types = _pool_types_from_filter(perms)
    if pipeline.pool_type_per_depth is not None:
        bot_logger.info(
            "[build_paths] Permutation filter active: "
            f"{perms} → depths={pipeline.pool_type_per_depth}",
        )
    bot_logger.info(f"[build_paths] Pool types: {[t.__name__ for t in pipeline.pool_types]}")

    start = time.perf_counter()

    bot_logger.info("[build_paths] Calling find_paths_async...")
    bot_logger.info(
        f"[build_paths] Starting registration crawl: fleet PoolStateUpdater "
        f"intake, window {REG_INTAKE_WINDOW}"
    )

    discovery_producer: AsyncGenerator[object, None] = pipeline.discovery_sweep()
    bot_logger.info("[build_paths] Discovery: single pass over the DB subgraph")

    # PRG-5: the cap is no longer an unwind exception (the queue that carried
    # `DiscoveryCrawlComplete` retired) — run_registration returns normally
    # and the pipeline's `capped` flag carries the benign-stop witness.
    # 4IOEVT: close the async discovery generator deterministically when the
    # crawl breaks on the path cap (or aborts on a fatal receipt) so the Rust
    # batch iterator is dropped (releasing a mid-DFS search) instead of
    # leaving the search running.
    try:
        await pipeline.run_registration(producer=discovery_producer)
    finally:
        aclose = getattr(discovery_producer, "aclose", None)
        if aclose is not None:
            await aclose()
    if pipeline.capped:
        bot_logger.info(
            f"[build_paths] Registration stopped at the path cap "
            f"({pipeline.path_count} registered, {pipeline.cap_skip_count} "
            "candidates discarded post-cap)"
        )

    # INN6TK observability: always emit the skip-reason breakdown at completion,
    # even if the time-throttled cadence fell on a throttled tick.
    pipeline.emit_registration_progress(force=True)

    bot_logger.info(
        f"[build_paths] Path discovery complete: {pipeline.path_count} paths in "
        f"{time.perf_counter() - start:.1f}s — "
        f"{pipeline.skip_count} skipped, {pipeline.token_filter_count} token-filtered, "
        f"{pipeline.engine_reject_count} engine-rejected "
        f"(other_exc={pipeline.other_exc_count}), "
        f"{pipeline.v4_hook_rejected} V4 hook-rejected, "
        f"{pipeline.v4_dynamic_fee_rejected} V4 dynamic-fee-rejected, "
        f"{pipeline.dup_count} duplicates, "
        f"{pipeline.register_fail_count} register-failed",
    )
    bot_logger.info(
        f"[build_paths] Summary: {pipeline.path_count} paths in "
        f"{time.perf_counter() - start:.1f}s — "
        f"{engine_registry.engine.v2_pool_count()} V2, "
        f"{engine_registry.engine.v3_pool_count()} V3, "
        f"{pipeline.v4_pool_count} V4 pools, "
        f"{pipeline.v4_hook_rejected} V4 hook-rejected, "
        f"{pipeline.v4_dynamic_fee_rejected} V4 dynamic-fee-rejected, "
        f"{pipeline.other_exc_count} other-Exception, "
        f"{engine_registry.engine.path_count()} engine paths",
    )

    # DFQYM5 orphan sweep: release Tracked pools whose path was skipped.
    engine_registry.engine.release_all_v3_v4_quarantined()
