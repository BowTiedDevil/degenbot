"""Path discovery + registration for the settlement-arbitrage ``BotRunner``.

Extracted from ``examples/eth_backrun_v2_v3_v4_rust.py``.
Owns ``build_paths`` and its registration machinery:
:class:`ConstructionContext` (registration-owned construction resources kept
out of the main-loop trim), :class:`PathRegistrationPipeline` (the reusable,
pump-concurrent per-path registration / verify / dedup), and the bounded
producer/consumer helper that drives it.

The driver is Python-companion orchestration (``stays-python``): it registers
paths with the Rust-owned engine (``EngineRegistry``) but owns no pool state.
"""

from __future__ import annotations

import asyncio
import time
from collections import Counter, deque
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, cast

from degenbot.arbitrage import RetryPolicy
from degenbot.builders.request import BuildManagedPoolRequest, ConstructionRoute
from degenbot.db import db_fetch_graph_edition
from degenbot.exceptions import (
    DegenbotValueError,
    DirectionResolutionError,
    PathRegistryFullError,
    PathRejectedError,
    UnsupportedPoolFamilyError,
    VerificationMismatchError,
    VerificationRpcError,
)
from degenbot.logging import logger as bot_logger
from degenbot.pathfinding import (
    PathfindingRequest,
    PoolKind,
    find_paths_async,
)
from degenbot.pathfinding import (
    resolve_directions as core_resolve_directions,
)
from degenbot.runner._registration_ledger import (
    RegistrationLedger,
    RegistrationOutcome,
    RegistrationUnitKind,
    fold_registration_unit,
)
from degenbot.runner.identity import (
    PANCAKESWAP_V3_MAINNET_FACTORY,
    SUSHISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V4_POOL_MANAGER_ADDRESS,
    WETH_ADDRESS,
)
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS
from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    import pathlib
    from collections.abc import AsyncGenerator, AsyncIterable, Callable

    from degenbot import Bot, UniswapV2Pool, UniswapV3Pool, UniswapV4Pool
    from degenbot.arbitrage.engine_registry import EngineRegistry


# ──────────────────────────────────────────────────────────────────
# Permutation filter helpers
# ──────────────────────────────────────────────────────────────────


# The V2/V3/V4 labels are the public permutation API surface and map directly
# to the typed Rust-backed pathfinding families.
_POOL_VERSION_MAP: dict[str, PoolKind] = {
    "V2": PoolKind.V2,
    "V3": PoolKind.V3,
    "V4": PoolKind.V4,
}


def _parse_permutation_filter(
    perms: set[str] | None,
) -> list[set[PoolKind] | None] | None:
    """Convert permutation strings like {'V3-V4-V3'} into a pool_type list.

    The result is a pool_type_per_depth list suitable for ``find_paths_async``.

    Returns:
        The per-depth allowed-family list, or ``None`` when ``perms`` is
        None/empty (no filter). An entry is ``None`` when every permutation
        allows any type at that depth.

    Raises:
        ValueError: On an unknown version tag, or permutations of unequal
            depth.

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
    result: list[set[PoolKind] | None] = []
    for depth in range(max_depth):
        allowed_this_depth: set[PoolKind] = {
            _POOL_VERSION_MAP[perm_parts[depth]] for perm_parts in parsed
        }
        result.append(allowed_this_depth or None)
    return result


def _pool_types_from_filter(perms: set[str] | None) -> list[PoolKind]:
    """Derive the typed pool families needed by the permutation filter.

    Returns:
        One entry per pool family the filter's permutations reference (all
        families when there is no filter).

    """
    if not perms:
        return list(_POOL_VERSION_MAP.values())

    versions_needed = {version for perm in perms for version in perm.split("-")}
    return [_POOL_VERSION_MAP[version] for version in sorted(versions_needed)]


# ──────────────────────────────────────────────────────────────────
# Direction resolver
# ──────────────────────────────────────────────────────────────────


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

    The typed outcome vocabulary and the counter fold are the CORE's
    (`degenbot_bot::bot_core::registration_ledger` — the pure-Rust driver
    constructs and folds the same definition): ``kind`` is one of the
    core's ``RegistrationUnitOutcome`` kinds (minted into
    :class:`RegistrationUnitKind` from the core's exported list), and the
    single-loop driver folds each outcome through the core's arithmetic
    (``fold_registration_unit``), never inline. The units run on fleet
    seats (plain threads, possibly concurrent), so they NEVER touch the
    pipeline counters — they return one of these and the driver folds it.
    """

    #: The unit kind — the core's closed set (skip / reject / cap /
    #: register-fail / registered).
    kind: RegistrationUnitKind
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


def resolve_directions(
    pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool],
    input_token_address: str,
) -> list[bool]:
    """Determine zero_for_one for each hop so the cycle closes.

    The cycle: input_token → hop_0 → intermediate → hop_1 → ... → input_token.
    Returns a list of zfo values (one per hop).

    The mechanic lives in the core
    (`degenbot_pathfinding::directions::resolve_directions`); this adapter
    flattens the constructed pools into the typed seam's hop tuples and
    re-raises the core's refusal as :class:`DirectionResolutionError`.
    Resolution is kind-blind. V4
    pools use NATIVE_CURRENCY_ADDRESS (address(0)) for ETH, which the core
    treats as equivalent to WETH — the profit token is always WETH. The
    core's refusal surfaces as :class:`DirectionResolutionError` — a hop
    carries neither tracked token, or the cycle does not close: an invariant
    violation (wrong pool built, stale subgraph, or a builder bug), never a
    skip.

    Returns:
        One zero-for-one value per hop, in hop order.

    """
    hops = [(pool.token0.address, pool.token1.address, str(pool)) for pool in pools]
    return core_resolve_directions(hops, input_token_address, WETH_ADDRESS)


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

    The context holds RESOLVED POLICY VALUES only: the construction route
    (GLOSSARY.md, Construction route — the ordered factory rungs + the generic
    builder rung) and the WETH token, built once here. The core route entry
    (``pool_builder::route``) owns the walk — route order, the DB two-step
    identity, get-or-register into session state — and classifies every
    failure on the build-refusal taxonomy, so no tracker or snapshot object
    lives here (the retired three-tracker fallback chain was the bare
    except-and-continue bug this replaces).
    """

    bot: Bot
    chain_id: int
    database_path: pathlib.Path
    construction_route: ConstructionRoute
    weth: Any  # Erc20Token (WETH)

    @classmethod
    def for_bot(
        cls,
        bot: Bot,
    ) -> ConstructionContext:
        """Build the construction context for a bot.

        Resolves the route policy (the mainnet V3 fork factories in policy
        order, generic builder rung armed) and builds WETH once. The core
        route entry owns everything else about construction.

        Returns:
            The construction context.

        """
        construction_route = ConstructionRoute(
            factories=(
                UNISWAP_V3_MAINNET_FACTORY,
                SUSHISWAP_V3_MAINNET_FACTORY,
                PANCAKESWAP_V3_MAINNET_FACTORY,
            ),
            generic=True,
        )
        weth = bot.build_erc20token(WETH_ADDRESS)
        return cls(
            bot=bot,
            chain_id=bot.chain_id,
            database_path=bot.database_path,
            construction_route=construction_route,
            weth=weth,
        )


class PathRegistrationPipeline:
    """Reusable, pump-concurrent registration pipeline (D1c).

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

    ``max_paths`` is the registered-path cap (``0`` = uncapped) and is
    REQUIRED, because it is a configuration value: the caller that resolved it
    (:attr:`~degenbot.runner.config.ArbitrageConfig.max_registered_paths` in
    production) states it, and a default here would be a second authority that
    no config layer can reach. Registration stops accepting new paths once the
    engine path registry reaches the cap; the engine then reaches steady state
    with a bounded path universe, so solve performance is observable without
    ongoing registration load. The pipeline announces the cap it was given at
    startup, since the code default and the running environment may differ.

    The fail-fast tripwire is preserved: a fatal ``VerificationMismatchError``
    / ``VerificationRpcError`` is NOT swallowed here — it propagates out of the
    worker and must abort the pipeline loudly.
    """

    def __init__(  # ruff: ignore[too-many-arguments] - two resolved budgets plus three wiring seams
        self,
        *,
        context: ConstructionContext,
        engine_registry: EngineRegistry,
        retry_policy: RetryPolicy | None = None,
        max_paths: int,
        discovery_batch_size: int,
        progress_interval_secs: float | None = None,
    ) -> None:
        """Bind the construction context, engine registry, and resolved budgets.

        Raises:
            RuntimeError: If a resolved budget is inconsistent at bind time.

        """
        self.constr_ctx = context
        self.constr_bot = context.bot
        self.constr_chain_id = context.chain_id
        self.constr_database_path = context.database_path
        # The resolved construction-route policy (GLOSSARY.md, Construction
        # route) — the core route entry walks it; the driver supplies values,
        # never construction code.
        self.construction_route = context.construction_route
        self.weth = context.weth
        self.engine_registry = engine_registry
        self.retry_policy_obj = retry_policy or RetryPolicy()
        # The resolved discovery delivery batch size, positive-clamped at the
        # construction boundary; the sweep forwards it to the Rust batched
        # async iterator without reading the process verdict.
        self.discovery_batch_size = max(1, discovery_batch_size)

        # PRG-5 hard cutover: the crawl shell (the bounded
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
                "registration is fleet-hosted only (PRG-5 hard cutover): the "
                "legacy crawl shell (bounded queue + offload executor) is "
                "retired and the worker fleet is the only behavior after the "
                "stance cutover — the fleet intake boot "
                "descriptor is missing, so the engine was not constructed "
                "or its fleet boot failed; check the worker-census boot "
                "table for fleet_pool_state_updater_slots."
            )
            raise RuntimeError(msg)
        # The at-most-once verify-claims table for units running concurrently
        # on seats (the seat-side twin of EngineRegistry's loop-bound asyncio
        # claims, which serve the operator surface only).

        # Configured discovery inputs (set by the driver before discovery runs).
        self.pool_types: list[PoolKind] = []
        self.pool_type_per_depth: list[set[PoolKind] | None] | None = None

        # PRG-4: the registered-path budget transfers to the engine path
        # registry. Counters keep the Progress summary; the dedup set and the
        # pre-count cap gate retire. (Pipeline tests run with
        # engine_registry=None.)
        py_engine = getattr(self.engine_registry, "engine", None)
        if py_engine is not None and hasattr(py_engine, "set_path_cap"):
            py_engine.set_path_cap(max_paths or None)
        self._progress_interval_s = (
            progress_interval_secs
            if progress_interval_secs is not None
            else PathRegistrationPipeline._PROGRESS_INTERVAL_S
        )
        bot_logger.info(
            f"[build_paths] registered-path cap: {max_paths or 'uncapped'} "
            "(from the configured path cap); progress cadence "
            f"{self._progress_interval_s:.0f}s"
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
        # The fold's own witnesses (core-owned arithmetic): units folded and
        # skips that carry only their family counters. The core's fold
        # identities relate these to the buckets above; a driver that folds
        # one outcome per unit ends with `units_folded` equal to its unit
        # count.
        self.units_folded = 0
        self.uncounted_skip_count = 0
        # The registration outcome ledger owns the four memos that used to
        # live here ad-hoc — the registered-path dup fast-path, the verify-once
        # pool fact, the unregistrable-pool stable refusals, and the
        # deterministic rejected-path deny — plus the typed build-refusal
        # classification and the bounded metric tag vocabulary. A transient
        # failure is never memoized (a raced build or blip stays retryable).
        # See _registration_ledger.
        self._ledger = RegistrationLedger()
        # Observability: reason-tagged skip breakdown + time-throttled
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
    _PROGRESS_INTERVAL_S = 30.0

    def _registration_unit(
        self,
        path_steps: Any,
        directions: list[bool] | None = None,
    ) -> RegistrationUnitOutcome:
        """Run the per-path registration unit — SYNC, on a fleet seat.

        One unit = one path = one receipt. Hop builds ride the Rust
        single-flighted build path, the V3/V4 verify lifecycles run BLOCKING
        on the shared tokio runtime under the seat-claims at-most-once table,
        and the engine FFI owns the path dedup + cap. Counters are NEVER
        mutated here (concurrent seats): the returned outcome travels back to
        the single-loop driver, which folds it into the summary.

        Returns:
            The unit outcome (registered / skip / reject / cap).

        Raises:
            DirectionResolutionError: On a direction invariant violation —
                the loud-abort contract.
            VerificationMismatchError: On a verify truth mismatch — the
                loud-abort contract.
            VerificationRpcError: On a verify RPC failure — the loud-abort
                contract.

        """
        steps = list(path_steps)
        pool_kinds = self._hop_pool_kinds(steps)

        memoized = self._memoized_unregistrable_outcome(steps, pool_kinds)
        if memoized is not None:
            return memoized

        built = self._build_hop_pools(steps, pool_kinds)
        if isinstance(built, RegistrationUnitOutcome):
            return built
        pools = built

        # Pools that reached the registration stage are the v4_pool_count
        # parity witness (counted on registered AND rejected outcomes).
        v4_hops = pool_kinds.count(PoolKind.V4)
        reg = self.engine_registry

        # Fatal contract: VerificationMismatchError / VerificationRpcError /
        # DirectionResolutionError propagate out of the seat (through the
        # receipt) and abort the pipeline loudly. Every OTHER
        # registration-stage exception is the counted engine-reject path.
        try:
            plan = self._evaluate_registration_eligibility(pools, directions, v4_hops)
            if isinstance(plan, RegistrationUnitOutcome):
                return plan
            engine_hops, hop_sig = plan

            self._run_verify_lifecycles(pools, pool_kinds, reg)

            outcome = self._register_path_outcome(reg, engine_hops, hop_sig, v4_hops)
            if outcome.kind == "registered":
                # Only a completed registration (created or engine-dedup'd
                # dup) enters the memo; stable negatives entered theirs at
                # their own sites, and a failed verify stays retriable.
                self._ledger.memoize_registered_path(hop_sig)
        except (
            VerificationMismatchError,
            VerificationRpcError,
            DirectionResolutionError,
        ):
            raise
        except Exception as exc:
            tag = f"{type(exc).__name__}: {exc}"
            bot_logger.info(
                f"[build_paths] Engine registration failed ({type(exc).__name__}): {exc}",
            )
            return RegistrationUnitOutcome(
                kind=RegistrationUnitKind.REJECT, tag=tag, v4_hops=v4_hops
            )
        else:
            return outcome

    @staticmethod
    def _hop_pool_kinds(steps: list[Any]) -> list[PoolKind]:
        """Resolve each hop's typed pool family.

        The hop's `PoolKind` is the discovery edge's discriminant, carried by
        `PathStep.type`.

        Returns:
            The `PoolKind` each step carries.

        """
        return [step.type for step in steps]

    def _memoized_unregistrable_outcome(
        self,
        steps: list[Any],
        pool_kinds: list[PoolKind],
    ) -> RegistrationUnitOutcome | None:
        """Answer a hop whose stable build refusal is already memoized.

        The pathological DFS region re-yielded one refused pool alongside
        thousands of candidate paths; the ledger owns the record. An
        unrecognized hop family never reaches a memo ask: the ledger's
        closed-set gate raises `DegenbotValueError` (wire drift).

        Returns:
            The memoized refusal outcome for the path, or ``None`` when no
            hop carries a stable memoized refusal.

        """
        for step, pool_kind in zip(steps, pool_kinds, strict=True):
            memo = self._ledger.unregistrable_record(self._ledger.pool_memo_key(step, pool_kind))
            if memo is not None:
                return RegistrationUnitOutcome(
                    kind=RegistrationUnitKind.SKIP,
                    tag=memo.outcome,
                    counts_as_skip=memo.counts_as_skip,
                )
        return None

    def _refusal_skip(
        self,
        step: Any,
        exc: Exception,
        pool_kind: PoolKind,
    ) -> RegistrationUnitOutcome:
        """Classify one hop-build failure and shape its skip outcome.

        A stable refusal also memoizes the hop's unregistrable-pool record;
        a transient failure is never memoized (a retriable blip must stay
        retryable). An unrecognized hop family never reaches the classifier:
        the ledger's closed-set gate raises `DegenbotValueError` (wire drift).

        Returns:
            The classified skip outcome.

        """
        refusal = self._ledger.classify_build_refusal(exc, pool_kind=pool_kind)
        if refusal.stable:
            self._ledger.memoize_unregistrable(
                self._ledger.pool_memo_key(step, pool_kind),
                refusal.outcome,
                counts_as_skip=refusal.counts_as_skip,
            )
        return RegistrationUnitOutcome(
            kind=RegistrationUnitKind.SKIP,
            tag=refusal.outcome,
            counts_as_skip=refusal.counts_as_skip,
            detail=refusal.detail,
        )

    def _build_hop_pools(
        self,
        steps: list[Any],
        pool_kinds: list[PoolKind],
    ) -> list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool] | RegistrationUnitOutcome:
        """Build (or registry-answer) every hop, or return the refusal outcome.

        V4 admission (dynamic-fee / fee-encoder-limit / hooked) is answered
        pre-RPC by the FFI build call as a typed refusal; a stable refusal is a
        pool fact and memoizes its hop identity, while a transient failure
        stays retryable.

        Returns:
            The built pools, or a skip outcome for a malformed hop or an
            already-classified build refusal.

        Raises:
            UnsupportedPoolFamilyError: On a family-level stable refusal —
                the route's loud arm; never swallowed or counted as a skip.
            DegenbotValueError: On a hop family outside the closed `PoolKind`
                set — wire drift is a loud abort, never a classified refusal,
                so the unmatched arm raises OUTSIDE the classified try.

        """
        pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool] = []
        for step, pool_kind in zip(steps, pool_kinds, strict=True):
            match pool_kind:
                case PoolKind.V4:
                    if not step.hash:
                        return RegistrationUnitOutcome(
                            kind=RegistrationUnitKind.SKIP,
                            tag=RegistrationOutcome.V4_NO_HASH.value,
                        )
                    try:
                        pool = self.constr_bot.build_managed_pool(
                            UNISWAP_V4_POOL_MANAGER_ADDRESS,
                            BuildManagedPoolRequest(pool_id=step.hash, silent=True),
                        )
                    except UnsupportedPoolFamilyError:
                        # The route's loud arm (ADR-055 D4): a family-level
                        # stable refusal — no rung serves this pool's factory,
                        # no DEX preset, no identity selector, CREATE2
                        # contradiction — aborts the unit LOUDLY. Never
                        # swallowed into the next rung (the retired bare
                        # except-and-continue), never a benign skip.
                        raise
                    except Exception as exc:
                        return self._refusal_skip(step, exc, pool_kind)
                case PoolKind.V2:
                    try:
                        pool = self.constr_bot.build_pool(step.address, silent=True)
                    except UnsupportedPoolFamilyError:
                        raise
                    except Exception as exc:
                        return self._refusal_skip(step, exc, pool_kind)
                case PoolKind.V3:
                    try:
                        # ONE core entry: the construction route (route order +
                        # DB two-step identity + get-or-register) lives in
                        # ``pool_builder::route`` — the cockpit supplies the
                        # resolved policy value and receives the constructed,
                        # registered pool or a typed refusal.
                        pool = self.constr_bot.build_pool(
                            step.address,
                            silent=True,
                            construction_route=self.construction_route,
                        )
                    except UnsupportedPoolFamilyError:
                        raise
                    except Exception as exc:
                        return self._refusal_skip(step, exc, pool_kind)
                case _:
                    msg = (
                        f"Unrecognized pool kind {pool_kind!r}: the registration "
                        "pipeline builds only PoolKind.V2, PoolKind.V3, "
                        "PoolKind.V4 hops — Rust/Python wire drift."
                    )
                    raise DegenbotValueError(message=msg)
            pools.append(cast("UniswapV2Pool | UniswapV3Pool | UniswapV4Pool", pool))
        return pools

    def _evaluate_registration_eligibility(
        self,
        pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool],
        directions: list[bool] | None,
        v4_hops: int,
    ) -> tuple[list[tuple[int, bool]], tuple[tuple[int, bool], ...]] | RegistrationUnitOutcome:
        """Resolve directions and run the pre-verify gates for one path.

        Returns the (engine_hops, hop_sig) plan to verify + register, or an
        early outcome: a direction mismatch (skip), a memoized deny
        (reject), or a hop signature already registered (registered dup).
        The deployment-policy gate and the dup fast-path both sit in front of
        ALL verify/RPC work.

        Returns:
            The ``(engine_hops, hop_sig)`` plan, or an early
            ``RegistrationUnitOutcome``.

        Raises:
            PathRejectedError: When the deployment-policy gate denies the
                path (the deny memoizes, then re-raises).

        """
        zfo_list = self._resolve_path_directions(pools, directions)
        if zfo_list is None:
            # Operator-pinned directions whose per-hop count disagrees with
            # the resolved path — name it rather than letting the later
            # zip(strict=True) raise a cryptic TypeError.
            return RegistrationUnitOutcome(
                kind=RegistrationUnitKind.SKIP,
                tag=RegistrationOutcome.DIRECTION_MISMATCH.value,
            )

        engine_hops = [
            (self._pool_engine_id(pool), zfo) for pool, zfo in zip(pools, zfo_list, strict=True)
        ]
        hop_sig = tuple(engine_hops)

        # The policy deny is deterministic per hop signature, so a re-yielded
        # candidate is answered without a second gate evaluation.
        if self._ledger.path_rejected(hop_sig):
            return RegistrationUnitOutcome(
                kind=RegistrationUnitKind.REJECT,
                tag=RegistrationOutcome.PATH_REJECTED_MEMO.value,
            )

        try:
            self.engine_registry.path_predicate.evaluate(list(zip(pools, zfo_list, strict=True)))
        except PathRejectedError:
            self._ledger.memoize_rejected_path(hop_sig)
            raise

        # Dup fast-path: answer hop signatures already registered with the
        # same outcome shape the engine dedup produces (created=False). The
        # engine path registry stays the source of truth; a memo miss merely
        # re-pays the old cost, and the only mutation is a GIL-atomic set.add.
        if self._ledger.path_registered(hop_sig):
            return RegistrationUnitOutcome(
                kind=RegistrationUnitKind.REGISTERED,
                created=False,
                v4_hops=v4_hops,
            )
        return engine_hops, hop_sig

    def _run_verify_lifecycles(
        self,
        pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool],
        pool_kinds: list[PoolKind],
        reg: EngineRegistry,
    ) -> None:
        """Run the per-pool verify choreography before path registration.

        The sync lifecycle twins run parked on the shared tokio runtime; the
        seat-claims table keeps them at-most-once per live window, and the
        verify-once memo makes a completed lifecycle a pool fact for this
        pipeline lifetime.

        Raises:
            DegenbotValueError: On a hop family outside the closed `PoolKind`
                set — wire drift, never a silently skipped verify.

        """
        for pool, pool_kind in zip(pools, pool_kinds, strict=True):
            match pool_kind:
                case PoolKind.V2:
                    self._warn_asymmetric_v2_fees(cast("UniswapV2Pool", pool))
                case PoolKind.V3:
                    self._verify_v3_pool(cast("UniswapV3Pool", pool), reg)
                case PoolKind.V4:
                    self._verify_v4_pool(cast("UniswapV4Pool", pool), reg)
                case _:
                    msg = (
                        f"Unrecognized pool kind {pool_kind!r}: the verify "
                        "choreography runs only PoolKind.V2, PoolKind.V3, "
                        "PoolKind.V4 hops — Rust/Python wire drift."
                    )
                    raise DegenbotValueError(message=msg)

    @staticmethod
    def _warn_asymmetric_v2_fees(v2_pool: UniswapV2Pool) -> None:
        """Warn when a V2 pool's two fee tiers differ."""
        if v2_pool._fee_token0 != v2_pool._fee_token1:  # ruff:ignore[private-member-access]
            bot_logger.warning(
                f"Asymmetric V2 fees detected for {v2_pool.address} "
                f"(fee_token0={v2_pool._fee_token0}, "  # ruff:ignore[private-member-access]
                f"fee_token1={v2_pool._fee_token1}).",  # ruff:ignore[private-member-access]
            )

    def _verify_v3_pool(self, pool: UniswapV3Pool, reg: EngineRegistry) -> None:
        """Verify a V3 pool once per pipeline lifetime, at most once concurrently.

        Two layers, neither of them a claim table: the CORE owns the
        at-most-once window (the driver's session claim table, keyed by pool
        identity — a concurrent seat parks inside
        `run_v3_verify_lifecycle_sync` and receives the leader's outcome),
        and this ledger's verify-once memo turns a COMPLETED lifecycle into a
        pool fact so later units skip the call entirely.
        """
        v3_key = f"v3:{pool.address}"
        if self._ledger.pool_verified(v3_key):
            return
        reg.run_v3_verify_lifecycle_sync_with_retry(pool.address, self.retry_policy_obj)
        self._ledger.memoize_verified_pool(v3_key)

    def _verify_v4_pool(self, pool: UniswapV4Pool, reg: EngineRegistry) -> None:
        """V4 twin of :meth:`_verify_v3_pool`, with the retry policy applied here.

        The retry wraps the core-owned window from OUTSIDE: a transient
        `VerificationRpcError` releases the claim, so the next attempt
        re-claims and re-runs the lifecycle — a failed verify stays
        retryable, and a fatal mismatch propagates immediately.
        """
        v4_key = f"v4:{to_0x_hex(pool.pool_id)}"
        if self._ledger.pool_verified(v4_key):
            return
        reg.run_v4_verify_lifecycle_sync_with_retry(
            UNISWAP_V4_POOL_MANAGER_ADDRESS,
            to_0x_hex(pool.pool_id),
            self.retry_policy_obj,
        )
        self._ledger.memoize_verified_pool(v4_key)

    def _register_path_outcome(
        self,
        reg: EngineRegistry,
        engine_hops: list[tuple[int, bool]],
        hop_sig: tuple[tuple[int, bool], ...],
        v4_hops: int,
    ) -> RegistrationUnitOutcome:
        """Register the path with the engine, classifying the refusal.

        The engine path registry dedups by construction; `created` is False
        exactly when the core signature dedup answered, and the cap refusal
        surfaces as the typed PathRegistryFullError. The typed fatals are
        never downgraded to a register-fail, and a transient register failure
        is deliberately NOT negatively memoized (a raced build or blip must
        stay retryable).

        Returns:
            The registration outcome (registered / cap / register-fail).

        Raises:
            PathRejectedError: Re-raised after the deny memoizes.
            DirectionResolutionError: The loud-abort contract.
            VerificationMismatchError: The loud-abort contract.
            VerificationRpcError: The loud-abort contract.

        """
        try:
            _path_id, created = reg.register_crawl_path(engine_hops)
        except PathRegistryFullError:
            return RegistrationUnitOutcome(
                kind=RegistrationUnitKind.CAP,
                tag=RegistrationOutcome.PATH_CAP.value,
                v4_hops=v4_hops,
            )
        except PathRejectedError:
            self._ledger.memoize_rejected_path(hop_sig)
            raise
        except (
            VerificationMismatchError,
            VerificationRpcError,
            DirectionResolutionError,
        ):
            raise
        except Exception as exc:
            return RegistrationUnitOutcome(
                kind=RegistrationUnitKind.REGISTER_FAIL,
                tag=RegistrationOutcome.REGISTER_FAIL.value,
                detail=f"{type(exc).__name__}: {exc}",
                v4_hops=v4_hops,
            )
        return RegistrationUnitOutcome(
            kind=RegistrationUnitKind.REGISTERED,
            created=created,
            v4_hops=v4_hops,
        )

    @staticmethod
    def _pool_engine_id(
        pool: UniswapV2Pool | UniswapV3Pool | UniswapV4Pool,
    ) -> int:
        """Return the engine hop key off a build handle (ADR-006 D3: one pool_id).

        Returns:
            The core pool id serving as the hop key.

        """
        return pool._py_pool.pool_id  # ruff:ignore[private-member-access]

    def _record_skip(
        self,
        reason: str,
        detail: BaseException | str | None = None,
    ) -> None:
        """Record a reason-tagged skip so the periodic summary shows WHY.

        The direct-seed path (tests, future callers) — one breakdown entry
        plus the observation policy. The FOLD path never routes through
        here: the core's fold delta owns the breakdown entries, and
        ``_observe_skip`` carries only the metric/log policy.
        """
        self._skip_reasons[reason] += 1
        self._observe_skip(reason, detail)

    def _observe_skip(
        self,
        reason: str,
        detail: BaseException | str | None = None,
    ) -> None:
        """Apply the driver-side observation policy for one skip reason.

        `reason` is a short stable tag (e.g. ``"build-v3-refused"``,
        ``"dup"``, ``"path-cap"``) — never an interpolated address, so the
        aggregate stays compact and greppable.

        PRG-2: the skip ALSO lands in the Rust `degenbot.registration.skips`
        metric family (closed-set labels — the per-error-class detail stays
        here in logs, first few occurrences only so a skip-flood cannot
        resurrect the log-flood hazard). The former fatal-memo gate is retired:
        immutable V4 admission verdicts are refused pre-RPC by the core
        registration gate, and raced duplicates self-heal in the build path.
        """
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
            if now - self._last_progress_ts < self._progress_interval_s:
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
        Arc::try_unwrap canary quiet). The unit's fatal exception
        (VerificationMismatchError / VerificationRpcError /
        DirectionResolutionError) propagates through the receipt and aborts
        the crawl loudly — the "shut down" contract, unchanged. On a fatal
        the crawl stops submitting immediately (outstanding units still
        finish — fleet units are never cancelled, the Deferrable cordon
        class).

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
        """Add ONE specific path at any time (D1c operator surface)."""
        await self._consume(path_steps, directions=directions)

    def _graph_edition(self) -> tuple[int, int, int, int] | None:
        """Cheap structural fingerprint of the discovery subgraph.

        The candidate-cycle set is a pure function of the pool-row structure
        (never of pool state/prices), so the row count + max id of each pool
        family for this chain suffices to detect structural change.

        Returns:
            The structural edition, or ``None`` when no DB handle is attached
            or the probe fails (fail-open): the latch stays disabled and
            every sweep runs — the pre-latch behavior. A failed probe never
            blocks discovery.

        """
        try:
            return db_fetch_graph_edition(str(self.constr_database_path), self.constr_chain_id)
        except Exception:
            return None

    async def trigger_discovery(self, *, bound: int | None = None) -> int:
        """Trigger a bounded one-shot discovery sweep (D1c).

        A sweep that runs to NATURAL completion (not bound-truncated, not
        capped) latches the structural graph edition; a later trigger over
        the same edition stops immediately and returns 0 — the unchanged
        structure can only re-yield paths the pipeline already processed.
        The latch re-arms itself when the edition changes (pool added or
        removed), when the probe is unavailable, and after any truncated
        sweep.

        Returns:
            The number of paths processed by the sweep.

        """
        edition = self._graph_edition()
        if edition is not None and edition == self._sweep_completed_edition:
            return 0

        count = 0
        truncated = False
        # Close the sweep deterministically on the bound-truncation
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

    def discovery_sweep(
        self,
        *,
        find_paths_async: Callable[..., AsyncGenerator[object, None]] = find_paths_async,
    ) -> AsyncGenerator[object, None]:
        """Run a single discovery sweep over the DB subgraph (V2/V3/V4 DFS).

        ``find_paths_async`` is the discovery producer seam (tests inject a
        recording producer to observe the forwarded batch size); the default
        is the production adapter.

        Returns:
            The async generator of discovered candidate paths.

        """
        return find_paths_async(
            request=PathfindingRequest(
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
                database_path=self.constr_database_path,
                pool_type_per_depth=self.pool_type_per_depth,
            ),
            batch_size=self.discovery_batch_size,
        )

    def _resolve_path_directions(
        self,
        pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool],
        directions: list[bool] | None,
    ) -> list[bool] | None:
        """Return per-hop directions for `pools` (operator-pinned or resolved).

        Returns:
            The per-hop zero-for-one values, or ``None`` when operator-pinned
            directions disagree with the hop count.

        """
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
        also what keeps the operator surface a "thin Rust submission" —
        the work happens in Rust-coordinated fleet seats, not on the event
        loop. The unit's fatal exceptions propagate through the receipt
        (VerificationMismatchError / VerificationRpcError /
        DirectionResolutionError — the loud shutdown contract), as does any
        unit panic re-raised through the receipt.

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
        counters (concurrency). The counter ARITHMETIC is the core's — the
        outcome maps onto the core's typed unit vocabulary and the fold delta
        applies here, one fold per unit (the core's fold identities relate
        the buckets; a repeated or dropped fold shows in ``units_folded``).
        What stays driver-side is the observation policy the retired inline
        branches carried: the skip metric family, the first-few-occurrence
        logs, the cap announcement, and the 1000-boundary progress line.
        """
        delta = fold_registration_unit(
            kind=outcome.kind,
            tag=outcome.tag,
            counts_as_skip=outcome.counts_as_skip,
            created=outcome.created,
            v4_hops=outcome.v4_hops,
            detail=outcome.detail,
        )
        self.path_count += delta.path_count
        self.skip_count += delta.skip_count
        self.cap_skip_count += delta.cap_skip_count
        self.engine_reject_count += delta.engine_reject_count
        self.dup_count += delta.dup_count
        self.register_fail_count += delta.register_fail_count
        self.v4_pool_count += delta.v4_pool_count
        self.v4_hook_rejected += delta.v4_hook_rejected
        self.v4_dynamic_fee_rejected += delta.v4_dynamic_fee_rejected
        self.other_exc_count += delta.other_exc_count
        self.units_folded += delta.units_folded
        self.uncounted_skip_count += delta.uncounted_skip_count
        if delta.capped:
            self.capped = True
        for reason, count in delta.skip_reasons:
            self._skip_reasons[reason] += count

        # Driver-side observation policy (the counters above are the fold's).
        if outcome.kind == RegistrationUnitKind.SKIP:
            if outcome.tag is not None:
                self._observe_skip(outcome.tag, detail=outcome.detail)
            return
        if outcome.kind == RegistrationUnitKind.REJECT:
            # A rejection is not a skip: the engine_reject/other_exc pair
            # carries it; the exception text was logged at the unit site.
            return
        if outcome.kind == RegistrationUnitKind.REGISTER_FAIL:
            self._observe_skip(
                outcome.tag or RegistrationOutcome.REGISTER_FAIL.value,
                detail=outcome.detail,
            )
            if self.register_fail_count <= 5:
                bot_logger.warning(f"Path registration failed: {outcome.detail}")
            return
        if outcome.kind == RegistrationUnitKind.CAP:
            self._observe_skip(RegistrationOutcome.PATH_CAP.value)
            bot_logger.info("[build_paths] Path cap reached — stopping discovery crawl")
            return
        # "registered": the parity witness for v4_pool_count is counted on
        # created AND duplicate outcomes (the retired body incremented the
        # V4 counter inside the registration loop, before the dedup check).
        if not outcome.created:
            self._observe_skip(RegistrationOutcome.DUP.value)
            return
        if self.path_count % 1000 == 0:
            bot_logger.info(
                f"[build_paths] Progress: {self.path_count} paths registered, "
                f"{self.skip_count} skipped, {self.token_filter_count} token-filtered, "
                f"{self.engine_reject_count} engine-rejected, {self.dup_count} duplicates",
            )


@dataclass
class BuildPathsOptions:
    """The knobs :func:`build_paths` takes, one options object.

    Bundling the construction/registration inputs keeps the ``build_paths``
    call site to the two required resources plus one options object; each field
    mirrors the former keyword parameter.

    ``max_registered_paths`` and ``discovery_batch_size`` are the
    non-defaulted fields, and they are required for the same reason the
    pipeline's ``max_paths`` and ``discovery_batch_size`` are: both are
    configuration values, so the caller states what it resolved and no code
    path invents one. A caller that supplies ``pipeline`` already carries them
    on the pipeline it passes.

    The snapshot fields are retired: per-pool tick data resolves core-side at
    construction (the DB arm of the Rust builder, or the Chain arm), so a
    driver-side snapshot object has no consumer in the construction route.
    """

    max_registered_paths: int
    discovery_batch_size: int
    retry_policy: RetryPolicy | None = None
    context: ConstructionContext | None = None
    pipeline: PathRegistrationPipeline | None = None
    permutation_filter: frozenset[str] | None = None


async def build_paths(
    *,
    bot: Bot,
    engine_registry: EngineRegistry,
    options: BuildPathsOptions,
) -> None:
    """Discover V2/V3/V4 arb paths, build Python pools, register with Rust engine.

    V4 pools are discovered via find_paths_async and built through
    ``bot.build_managed_pool()``. V4 pool admission (amount-modifying hooks /
    dynamic fees) is enforced by the Rust core at registration time, surfacing
    as typed HookedPoolRejectedError / DynamicFeePoolRejectedError. Each
    per-pool verify lifecycle runs through the core-owned bounded retry dance
    with the resolved policy injected (transient ``VerificationRpcError`` is
    retried; ``VerificationMismatchError`` is never retried and crashes loudly).

    Discovery is a single pass over the DB subgraph driven through a reusable
    :class:`PathRegistrationPipeline`; after it completes the orphan sweep
    releases Tracked pools whose path was skipped before ``register_vN_pool``.

    ``options`` is required because it carries the registered-path cap: the
    caller that resolved the cap states it, and this function never invents
    one for a pipeline it builds itself.
    """
    constr_ctx = (
        options.context if options.context is not None else ConstructionContext.for_bot(bot)
    )

    pipeline = options.pipeline or PathRegistrationPipeline(
        context=constr_ctx,
        engine_registry=engine_registry,
        retry_policy=options.retry_policy,
        max_paths=options.max_registered_paths,
        discovery_batch_size=options.discovery_batch_size,
    )
    perms = set(options.permutation_filter) if options.permutation_filter else None
    pipeline.pool_type_per_depth = _parse_permutation_filter(perms)
    pipeline.pool_types = _pool_types_from_filter(perms)
    if pipeline.pool_type_per_depth is not None:
        bot_logger.info(
            "[build_paths] Permutation filter active: "
            f"{perms} → depths={pipeline.pool_type_per_depth}",
        )
    bot_logger.info(f"[build_paths] Pool types: {pipeline.pool_types}")

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
    # Close the async discovery generator deterministically when the
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

    # Observability: always emit the skip-reason breakdown at completion,
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

    # Orphan sweep: release Tracked pools whose path was skipped.
    engine_registry.engine.release_all_v3_v4_quarantined()
