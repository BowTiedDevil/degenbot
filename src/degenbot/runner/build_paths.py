"""Path discovery + registration for the settlement-arbitrage ``BotRunner``.

The ENGINE owns the crawl: discovery, the windowed submission, the per-unit
pipeline (memo → build → eligibility → verify → register), the fold, and the
stage profile all live in ``degenbot_bot::arb_engine::crawl``. This module is the
driver shell around it:

- :class:`ConstructionContext` is the boot assembly: it resolves the construction
  route and builds WETH once, then the engine holds the ``Arc<Bot>``;
- :func:`build_paths` resolves the crawl configuration (route, cap, permutation
  filter, retry policy, policy data), begins the engine crawl, and consumes the
  receipt stream for the progress cadence and the observation policy;
- the RENDER is the engine's: ``CrawlSession.progress_line()`` and
  ``CrawlSession.stage_profile_lines()`` are the core's own fold, byte for byte
  the block this module used to fold and print.

The session is kept on :class:`ConstructionContext` for the process's life, so
the operator surface (``enqueue_path`` / ``trigger_discovery``) keeps working
after ``run()`` trims the main-loop Python state — the trim-proof identity the
retired ``PathRegistrationPipeline`` used to be.

The driver is Python-companion orchestration (``stays-python``): it registers
paths with the Rust-owned engine but owns no pool state.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from degenbot.arbitrage import RetryPolicy
from degenbot.arbitrage.policy import NoOpPathPredicate, PathPolicy
from degenbot.builders.request import ConstructionRoute
from degenbot.logging import logger as bot_logger
from degenbot.pathfinding import FAMILY_TAG_TO_POOL_KIND, PoolKind
from degenbot.runner.identity import (
    PANCAKESWAP_V3_MAINNET_FACTORY,
    SUSHISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V3_MAINNET_FACTORY,
    WETH_ADDRESS,
)
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS

if TYPE_CHECKING:
    import pathlib

    from degenbot import Bot
    from degenbot._ffi import CrawlSession
    from degenbot.arbitrage.engine_registry import EngineRegistry


#: Paths legally in flight on the fleet intake at once (PRG-5). The engine's
#: submission window is the driver-side backpressure: a candidate is submitted,
#: and only when the OLDEST receipt resolves does the next submission leave the
#: window — discovery can never outrun registration by more than the window.
REG_INTAKE_WINDOW = 32

#: The minimum hop count every discovered cycle must have.
MIN_DEPTH = 2

#: The maximum hop count a discovered cycle may have.
MAX_DEPTH = 3


# ──────────────────────────────────────────────────────────────────
# Permutation filter helpers (the configuration resolution)
# ──────────────────────────────────────────────────────────────────


def _parse_permutation_filter(
    perms: set[str] | None,
) -> list[set[PoolKind] | None] | None:
    """Convert permutation strings like {'V3-V4-V3'} into a pool_type list.

    The result is a pool_type_per_depth list suitable for the engine's discovery
    configuration.

    Returns:
        The per-depth allowed-family list, or ``None`` when ``perms`` is
        None/empty (no filter). An entry is ``None`` when every permutation
        allows any type at that depth.

    Raises:
        ValueError: On an unknown version tag, or permutations of unequal depth.

    """
    if not perms:
        return None
    parsed: list[list[str]] = []
    for perm in perms:
        parts = perm.split("-")
        if not all(p in FAMILY_TAG_TO_POOL_KIND for p in parts):
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
            FAMILY_TAG_TO_POOL_KIND[perm_parts[depth]] for perm_parts in parsed
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
        return list(FAMILY_TAG_TO_POOL_KIND.values())

    versions_needed = {version for perm in perms for version in perm.split("-")}
    return [FAMILY_TAG_TO_POOL_KIND[version] for version in sorted(versions_needed)]


# ──────────────────────────────────────────────────────────────────
# The boot assembly
# ──────────────────────────────────────────────────────────────────


@dataclass
class ConstructionContext:
    """Registration-owned construction resources, kept out of run()'s trim.

    The boot assembly: it resolves the construction-route policy and builds WETH
    once, then the engine holds the ``Arc<Bot>`` — the context itself carries no
    per-unit machinery. ``BotRunner.run()`` trims *main-loop* state
    (``release_python_state()`` + ``self.bot = None``); this context is a
    *separate* identity that the trim never severs, and it keeps the crawl
    session the engine booted, which is the operator surface's only door.

    The core route entry (``pool_builder::route``) owns the walk — route order,
    the DB two-step identity, get-or-register into session state — and classifies
    every failure on the build-refusal taxonomy, so no tracker or snapshot object
    lives here (the retired three-tracker fallback chain was the bare
    except-and-continue bug this replaces).
    """

    bot: Bot
    chain_id: int
    database_path: pathlib.Path
    construction_route: ConstructionRoute
    weth: Any  # Erc20Token (WETH)
    #: The engine's booted crawl session, set by :func:`build_paths`. Held for
    #: the process's life: the engine driver holds the run's join handle, and
    #: the controller behind this session is the operator surface.
    session: CrawlSession | None = field(default=None, repr=False)

    @classmethod
    def for_bot(
        cls,
        bot: Bot,
    ) -> ConstructionContext:
        """Build the construction context for a bot.

        Resolves the route policy (the mainnet V3 fork factories in policy order,
        generic builder rung armed) and builds WETH once. The core route entry
        owns everything else about construction.

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


# ──────────────────────────────────────────────────────────────────
# The configuration resolution (values, never code)
# ──────────────────────────────────────────────────────────────────


def _retry_policy_value(retry_policy: RetryPolicy | None) -> dict[str, Any]:
    """Resolve the retry policy as the engine's value shape.

    Returns:
        The four bounds the core's retry dance reads.

    """
    policy = retry_policy if retry_policy is not None else RetryPolicy()
    return {
        "max_attempts": policy.max_attempts,
        "base_delay": policy.base_delay,
        "max_delay": policy.max_delay,
        "jitter": policy.jitter,
    }


def _policy_value(path_predicate: Any) -> tuple[dict[str, Any] | None, Any | None]:
    """Resolve the strategy's path-composition policy as engine data.

    A ``PathPolicy`` is DATA: every rule crosses as a value the core evaluates
    itself, including the named liquidity proxy (a callable ``liquidity_of``
    cannot cross, so the policy names the proxy it resolved). The default no-op
    predicate is the all-default policy. Anything else is the bulk-verdict
    escape: the object crosses as the engine's foreign policy oracle and answers
    ``verdicts(candidates)`` once per consult.

    Returns:
        The ``(policy data, oracle object)`` pair — exactly one is non-None when
        a policy is enforced.

    Raises:
        ValueError: A ``PathPolicy`` whose liquidity floor is a callable: the
            callable cannot cross the seam, and a dropped rule would silently
            admit paths the strategy refuses.

    """
    if isinstance(path_predicate, NoOpPathPredicate):
        return None, None
    if isinstance(path_predicate, PathPolicy):
        if path_predicate.liquidity_of is not None and path_predicate.min_liquidity > 0:
            msg = (
                "PathPolicy.liquidity_of cannot cross the FFI seam: name the "
                "proxy the policy compares (reserve_sum / active_liquidity)"
            )
            raise ValueError(msg)
        return (
            {
                "min_hops": path_predicate.min_hops,
                "max_hops": path_predicate.max_hops,
                "disallowed_tokens": sorted(path_predicate.disallowed_tokens),
                "allowed_tokens": (
                    None
                    if path_predicate.allowed_tokens is None
                    else sorted(path_predicate.allowed_tokens)
                ),
                "min_liquidity": path_predicate.min_liquidity,
                "reject_duplicate_pools": path_predicate.reject_duplicate_pools,
            },
            None,
        )
    return None, path_predicate


def _depth_filter_value(
    pool_type_per_depth: list[set[PoolKind] | None] | None,
) -> list[list[PoolKind] | None] | None:
    """Resolve the per-depth permutation filter as the engine's value shape.

    Returns:
        One entry per depth: the allowed families, or ``None`` for unconstrained.

    """
    if pool_type_per_depth is None:
        return None
    return [
        None if allowed is None else sorted(allowed, key=int) for allowed in pool_type_per_depth
    ]


def crawl_config(  # ruff: ignore[too-many-arguments] - the resolved values of one configuration, stated once
    *,
    context: ConstructionContext,
    engine_registry: EngineRegistry,
    max_paths: int,
    discovery_batch_size: int,
    retry_policy: RetryPolicy | None,
    pool_types: list[PoolKind],
    pool_type_per_depth: list[set[PoolKind] | None] | None,
) -> dict[str, Any]:
    """Resolve the engine crawl's configuration — values only.

    Returns:
        The configuration dict the engine's ``begin_crawl`` translates.

    """
    policy_data, oracle = _policy_value(engine_registry.path_predicate)
    config: dict[str, Any] = {
        "chain_id": context.chain_id,
        "database_path": str(context.database_path),
        "route": {
            "factories": list(context.construction_route.factories),
            "generic": context.construction_route.generic,
        },
        "pool_kinds": pool_types,
        "pool_type_per_depth": _depth_filter_value(pool_type_per_depth),
        "start_tokens": [
            WETH_ADDRESS,
            NATIVE_CURRENCY_ADDRESS,  # V4 allows Ether-paired pools
        ],
        "end_tokens": [
            WETH_ADDRESS,
            NATIVE_CURRENCY_ADDRESS,  # V4 allows Ether-paired pools
        ],
        "min_depth": MIN_DEPTH,
        "max_depth": MAX_DEPTH,
        "discovery_batch_size": max(1, discovery_batch_size),
        "intake_window": REG_INTAKE_WINDOW,
        "path_cap": max_paths or None,
        "retry": _retry_policy_value(retry_policy),
        "policy": policy_data,
        "oracle": oracle,
        "policy_batch_size": 1,
        "profit_token": WETH_ADDRESS,
        "snapshot_block": engine_registry.verify_snapshot_block,
        "state_block": None,
    }
    return config


# ──────────────────────────────────────────────────────────────────
# The observation policy (the counters are the engine's fold)
# ──────────────────────────────────────────────────────────────────


def _observe_skip(bot: Any, reason: str, detail: str | None = None) -> None:
    """Apply the driver-side observation policy for one skip reason.

    ``reason`` is a short stable tag (e.g. ``"build-v3-refused"``), never an
    interpolated address, so the aggregate stays compact and greppable. PRG-2:
    the skip ALSO lands in the Rust ``degenbot.registration.skips`` metric family
    (closed-set labels); the per-error-class detail stays here in logs, first few
    occurrences only so a skip flood cannot resurrect the log-flood hazard.
    """
    if detail is not None:
        bot_logger.debug(f"[build_paths] Pool-build skip ({reason}): {detail}")
    py_bot = getattr(bot, "_py_bot", None)
    if py_bot is not None:
        py_bot.record_registration_skip(reason)


async def _consume(
    session: CrawlSession,
    *,
    bot: Any,
    progress_interval_s: float,
) -> None:
    """Consume the engine's receipt stream: cadence, observation, anchors.

    The counters, the stage profile and the cap latch are the ENGINE's fold; this
    coroutine only decides WHEN a line is printed and which skip reasons land in
    the metric family. The render itself is ``session.progress_line()`` /
    ``session.stage_profile_lines()``.

    Raises:
        RuntimeError: the crawl's run aborted or failed loudly; the run's own
            recorded outcome carries the detail.

    """
    last_progress = 0.0
    last_anchor = 0
    async for receipt in session.receipts():
        now = time.monotonic()
        if now - last_progress >= progress_interval_s:
            last_progress = now
            bot_logger.info(session.progress_line())

        kind = receipt["kind"]
        tag = receipt["tag"]
        if kind == "skip":
            if tag is not None:
                _observe_skip(bot, tag)
        elif kind == "register-fail":
            _observe_skip(bot, tag or "register-fail")
            if session.summary()["register_fail_count"] <= 5:
                bot_logger.warning(f"Path registration failed: {tag}")
        elif kind == "cap":
            # The cap is the crawl's terminal latch: after it the stream quiets
            # but does not close until the cap drain finishes, so the
            # time-throttled cadence would repeat the terminal line every
            # interval. Announce it once, print the terminal line once, then
            # leave the loop for the completion render.
            _observe_skip(bot, "path-cap")
            bot_logger.info("[build_paths] Path cap reached — stopping discovery crawl")
            bot_logger.info(session.progress_line())
            break
        elif kind == "registered":
            if receipt["path_id"] is None:
                # The engine's own signature dedup answered: no counter moved, so
                # the anchor must not fire — the dup is a skip metric, as the
                # retired pipeline recorded it.
                _observe_skip(bot, "dup")
                continue
            # The anchor fires ONCE per 1000-boundary the engine's fold reaches,
            # never once per receipt: the fold has already absorbed every receipt
            # it sent, so a burst this consumer is still draining reads a single
            # (unchanged) count for the whole burst — the same line, over and over.
            path_count = session.summary()["path_count"]
            if path_count % 1000 == 0 and path_count != last_anchor:
                last_anchor = path_count
                summary = session.summary()
                bot_logger.info(
                    f"[build_paths] Progress: {summary['path_count']} paths registered, "
                    f"{summary['skip_count']} skipped, "
                    f"{summary['token_filter_count']} token-filtered, "
                    f"{summary['engine_reject_count']} engine-rejected, "
                    f"{summary['dup_count']} duplicates",
                )

    outcome = session.run_outcome()
    if outcome is None:
        return
    kind, detail = outcome
    if kind == "completed":
        return
    detail = detail or kind
    msg = f"build_paths: the engine crawl failed loudly: {detail}"
    raise RuntimeError(msg)


# ──────────────────────────────────────────────────────────────────
# The entry point
# ──────────────────────────────────────────────────────────────────


@dataclass
class BuildPathsOptions:
    """The knobs :func:`build_paths` takes, one options object.

    ``max_registered_paths`` and ``discovery_batch_size`` are the non-defaulted
    fields, and they are required for the same reason the engine's configuration
    requires them: both are configuration values, so the caller states what it
    resolved and no code path invents one.
    """

    max_registered_paths: int
    discovery_batch_size: int
    retry_policy: RetryPolicy | None = None
    context: ConstructionContext | None = None
    permutation_filter: frozenset[str] | None = None


async def build_paths(
    *,
    bot: Bot,
    engine_registry: EngineRegistry,
    options: BuildPathsOptions,
) -> None:
    """Configure and begin the engine's crawl, then render its measurement.

    The engine discovers V2/V3/V4 cycles, builds and verifies each candidate,
    registers the admitted paths, and folds every outcome. This function resolves
    the configuration, begins the crawl, consumes the receipt stream for the
    progress cadence and the skip-reason metric policy, and prints the engine's
    own renders (the counters line and the unit-stage profile) — the text the
    Python pipeline printed before the cutover, byte for byte.

    The booted session is kept on the construction context, so the runner's
    operator surface (``enqueue_path`` / ``trigger_discovery``) survives the
    main-loop trim.

    Args:
        bot: The Python-companion bot handle (the engine holds its own ``Arc``).
        engine_registry: The engine registry whose driver boots the crawl.
        options: The resolved configuration values.


    """
    constr_ctx = (
        options.context if options.context is not None else ConstructionContext.for_bot(bot)
    )
    perms = set(options.permutation_filter) if options.permutation_filter else None
    pool_type_per_depth = _parse_permutation_filter(perms)
    pool_types = _pool_types_from_filter(perms)
    if pool_type_per_depth is not None:
        bot_logger.info(
            f"[build_paths] Permutation filter active: {perms} → depths={pool_type_per_depth}",
        )
    bot_logger.info(f"[build_paths] Pool types: {pool_types}")
    bot_logger.info(
        f"[build_paths] registered-path cap: {options.max_registered_paths or 'uncapped'} "
        "(from the configured path cap)"
    )

    config = crawl_config(
        context=constr_ctx,
        engine_registry=engine_registry,
        max_paths=options.max_registered_paths,
        discovery_batch_size=options.discovery_batch_size,
        retry_policy=options.retry_policy,
        pool_types=pool_types,
        pool_type_per_depth=pool_type_per_depth,
    )

    start = time.perf_counter()
    bot_logger.info("[build_paths] Calling find_paths_async...")
    bot_logger.info(
        f"[build_paths] Starting registration crawl: the engine's crawl loop, "
        f"window {REG_INTAKE_WINDOW}"
    )
    bot_logger.info("[build_paths] Discovery: single pass over the DB subgraph")

    # The engine takes the core bot handle (the Python companion's ``_py_bot``):
    # its cached snapshot tick-map handle is what a head-stamped V3/V4
    # construction reads.
    engine_bot = getattr(bot, "_py_bot", bot)
    session = engine_registry.engine.begin_crawl(config, bot=engine_bot)
    constr_ctx.session = session
    progress_interval_s = getattr(options, "progress_interval_secs", None) or 30.0

    completed = False
    try:
        await _consume(
            session,
            bot=bot,
            progress_interval_s=progress_interval_s,
        )
        completed = True
    finally:
        # An aborted crawl (SIGINT, a loud abort) still renders the profile: the
        # abort case is exactly when the stage profile is worth reading.
        if not completed:
            for line in session.stage_profile_lines():
                bot_logger.info(line)

    summary = session.summary()
    if summary["capped"]:
        bot_logger.info(
            f"[build_paths] Registration stopped at the path cap "
            f"({summary['path_count']} registered, {summary['cap_skip_count']} "
            "candidates discarded post-cap)"
        )

    # Always emit the skip-reason breakdown at completion, even if the
    # time-throttled cadence fell on a throttled tick.
    bot_logger.info(session.progress_line())

    elapsed = time.perf_counter() - start
    bot_logger.info(
        f"[build_paths] Path discovery complete: {summary['path_count']} paths in "
        f"{elapsed:.1f}s — "
        f"{summary['skip_count']} skipped, {summary['token_filter_count']} token-filtered, "
        f"{summary['engine_reject_count']} engine-rejected "
        f"(other_exc={summary['other_exc_count']}), "
        f"{summary['v4_hook_rejected']} V4 hook-rejected, "
        f"{summary['v4_dynamic_fee_rejected']} V4 dynamic-fee-rejected, "
        f"{summary['dup_count']} duplicates, "
        f"{summary['register_fail_count']} register-failed",
    )
    bot_logger.info(
        f"[build_paths] Summary: {summary['path_count']} paths in "
        f"{elapsed:.1f}s — "
        f"{engine_registry.engine.reserve_pair_pool_count()} reserve-pair, "
        f"{engine_registry.engine.v3_pool_count()} V3, "
        f"{summary['v4_pool_count']} V4 pools, "
        f"{summary['v4_hook_rejected']} V4 hook-rejected, "
        f"{summary['v4_dynamic_fee_rejected']} V4 dynamic-fee-rejected, "
        f"{summary['other_exc_count']} other-Exception, "
        f"{engine_registry.engine.path_count()} engine paths",
    )
    for line in session.stage_profile_lines():
        bot_logger.info(line)

    # Orphan sweep: release Tracked pools whose path was skipped.
    engine_registry.engine.release_all_v3_v4_quarantined()
