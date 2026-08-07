"""Path discovery + registration for the backrun ``BotRunner``.

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
from collections import Counter
from collections.abc import AsyncIterable, AsyncIterator, Awaitable, Callable
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from typing import Any, cast

from degenbot import Bot, UniswapV2Pool, UniswapV3Pool, UniswapV4Pool, get_checksum_address
from degenbot.arbitrage.engine_registry import EngineRegistry
from degenbot.arbitrage.verification_retry import (
    VerificationRetryPolicy,
    retry_verification_call,
    retry_verification_call_async,
)
from degenbot.database.models.pools import (
    UniswapV2PoolTableBase,
    UniswapV3PoolTableBase,
    UniswapV4PoolTable,
    UniswapV4PoolTableBase,
)
from degenbot.exceptions import (
    DynamicFeePoolRejectedError,
    HookedPoolRejectedError,
    VerificationMismatchError,
    VerificationRpcError,
)
from degenbot.logging import logger as bot_logger
from degenbot.pathfinding import find_paths_async
from degenbot.runner.driver_constants import (
    ALLOWED_INTERMEDIATE_TOKENS,
    PANCAKESWAP_V3_MAINNET_FACTORY,
    PATH_PERMUTATION_FILTER,
    REG_QUEUE_BOUND,
    REG_WORKERS,
    SUSHISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V3_MAINNET_FACTORY,
    UNISWAP_V4_POOL_MANAGER_ADDRESS,
    WETH_ADDRESS,
)
from degenbot.uniswap.trackers import UniswapV3PoolTracker
from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
from degenbot.uniswap.v4_liquidity_pool import NATIVE_CURRENCY_ADDRESS
from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot

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
    zfo_list: list[bool] = []

    for pool in pools:
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
            return None

        addr = token1_addr if zfo else token0_addr
        zfo_list.append(zfo)

    if addr != get_checksum_address(input_token_address):
        return None

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


_REG_PIPELINE_SENTINEL = object()


async def run_registration_pipeline(
    *,
    producer: AsyncIterable[object],
    consume: Callable[[object], Awaitable[None]],
    queue_size: int,
    worker_count: int,
) -> None:
    """Run a bounded producer/consumer pipeline with backpressure.

    ``build_paths`` registers discovered paths through this helper. The
    producer (path discovery) yields items (paths) into a bounded
    ``asyncio.Queue``; the ``await queue.put()`` in the producer is the
    backpressure — discovery blocks the instant the queue is full, so it can
    never run more than ``queue_size`` paths ahead of activation, and a flood
    of new registrations is held at the queue boundary instead of stalling
    pools already enqueued for verification/activation. ``worker_count``
    concurrent workers drain the queue FIFO (preserving registration order)
    and call ``consume(item)`` each.

    When the producer exhausts, ``consume`` has processed every item and the
    call returns. Any exception escaping ``consume`` aborts the whole pipeline:
    the sibling workers and producer are cancelled and the exception is
    re-raised (preserving the fatal-verification "shut down loudly" contract).
    """
    if queue_size < 1:
        msg = f"run_registration_pipeline: queue_size must be >= 1, got {queue_size}"
        raise ValueError(msg)
    if worker_count < 1:
        msg = f"run_registration_pipeline: worker_count must be >= 1, got {worker_count}"
        raise ValueError(msg)

    queue: asyncio.Queue[object] = asyncio.Queue(maxsize=queue_size)

    async def _produce() -> None:
        try:
            async for item in producer:
                await queue.put(item)  # backpressure: blocks discovery when full
        except Exception:
            for _ in range(worker_count):
                await queue.put(_REG_PIPELINE_SENTINEL)
            raise
        else:
            for _ in range(worker_count):
                await queue.put(_REG_PIPELINE_SENTINEL)

    async def _work() -> None:
        while True:
            item = await queue.get()
            if item is _REG_PIPELINE_SENTINEL:
                queue.task_done()
                return
            try:
                await consume(item)
            finally:
                queue.task_done()

    producer_task = asyncio.create_task(_produce())
    worker_tasks = [asyncio.create_task(_work()) for _ in range(worker_count)]

    done, _pending = await asyncio.wait(worker_tasks, return_when=asyncio.FIRST_EXCEPTION)
    for task in done:
        if task.cancelled():
            continue
        exc = task.exception()
        if exc is not None:
            for t in [producer_task, *worker_tasks]:
                t.cancel()
            await asyncio.gather(*[producer_task, *worker_tasks], return_exceptions=True)
            raise exc

    await producer_task


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

        # Bounded thread pool for the blocking pool-build RPC (35NMBX).
        self._build_pool_executor: ThreadPoolExecutor | None = None

        # Configured discovery inputs (set by the driver before discovery runs).
        self.pool_types: list[type] = []
        self.pool_type_per_depth: list[set[type] | None] | None = None

        # Summary counters + registered-path dedup set.
        self.path_count = 0
        self.skip_count = 0
        self.token_filter_count = 0
        self.engine_reject_count = 0
        self.dup_count = 0
        self.direction_fail_count = 0
        self.register_fail_count = 0
        self.v4_pool_count = 0
        self.v4_hook_rejected = 0
        self.v4_dynamic_fee_rejected = 0
        self.other_exc_count = 0
        self.registered_path_sigs: set[tuple[str | bool, ...]] = set()
        # INN6TK observability: reason-tagged skip breakdown + time-throttled
        # progress emission. The legacy `[build_paths] Progress` line only fires
        # when `path_count` crosses each 1000-boundary; a discovery-heavy crawl
        # that registers few paths never prints it, hiding the skip/dup/reject
        # reasons. We record a reason tag per skip and emit the same summary on
        # a wall-clock cadence so the cause stays visible mid-crawl.
        self._skip_reasons: Counter[str] = Counter()
        self._last_progress_ts = 0.0

    #: Seconds between periodic registration-progress summaries (time-based, so
    #: they fire even when ``path_count`` never reaches the 1000 print gate).
    _PROGRESS_INTERVAL_S = float(os.environ.get("DEGENBOT_REG_PROGRESS_SECS", "30"))

    def _bounded_build_executor(self) -> ThreadPoolExecutor:
        """Lazily create the bounded pool-build thread pool (35NMBX Guard 2)."""
        if self._build_pool_executor is None:
            self._build_pool_executor = ThreadPoolExecutor(max_workers=REG_WORKERS)
        return self._build_pool_executor

    async def _run_build_offloaded(self, fn: Callable[[], object]) -> object:
        """Run a blocking pool-build callable on the bounded worker pool."""
        loop = asyncio.get_running_loop()
        return await loop.run_in_executor(self._bounded_build_executor(), fn)

    def _record_skip(self, reason: str) -> None:
        """Record a reason-tagged skip so the periodic summary shows WHY.

        `reason` is a short stable tag (e.g. ``"build-v3:ConnectionError"``,
        ``"dup"``, ``"direction-fail"``) — never an interpolated address, so
        the aggregate stays compact and greppable.
        """
        self._skip_reasons[reason] += 1

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
            f"{self.direction_fail_count} direction-fail, "
            f"{self.register_fail_count} register-fail, "
            f"{self.dup_count} duplicates "
            f"{{skip_reasons: {breakdown}}}",
        )

    async def run_registration(self, *, producer: AsyncIterable[object]) -> None:
        """Run the bounded producer/consumer pipeline against ``producer``."""
        await run_registration_pipeline(
            producer=producer,
            consume=self._consume,
            queue_size=REG_QUEUE_BOUND,
            worker_count=REG_WORKERS,
        )

    async def enqueue_path(
        self,
        path_steps: Any,
        directions: list[bool] | None = None,
    ) -> None:
        """Add ONE specific path at any time (NWTUM3 / D1c operator surface)."""
        await self._consume(path_steps, directions=directions)

    async def trigger_discovery(self, *, bound: int | None = None) -> int:
        """Trigger a bounded one-shot discovery sweep (NWTUM3 / D1c)."""
        count = 0
        async for item in self.discovery_sweep():
            if bound is not None and count >= bound:
                break
            await self._consume(item)
            count += 1
        return count

    def discovery_sweep(self) -> AsyncIterator[object]:
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
        """Process a single discovered/operator path: build, register, verify."""
        await asyncio.sleep(0)
        # Time-throttled periodic progress summary — fire independently of the
        # path_count==1000 gate so a discovery-heavy skip-fest stays visible.
        self.emit_registration_progress()

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

        pools: list[UniswapV2Pool | UniswapV3Pool | UniswapV4Pool] = []
        skip = False
        v4_admission_rejected = False
        for step, pt in zip(steps, pool_type_strs, strict=True):  # ruff:ignore[too-many-nested-blocks]
            if pt == "V2":
                try:
                    pool = await self._run_build_offloaded(
                        lambda: self.constr_bot.build_pool(step.address, silent=True)
                    )
                except Exception as exc:
                    bot_logger.debug(f"Skip V2 {step.address}: {exc}")
                    self._record_skip(f"build-v2:{type(exc).__name__}")
                    skip = True
                    break
            elif pt == "V3":
                try:
                    try:
                        pool = await self._run_build_offloaded(
                            lambda: self.uniswap_v3_tracker.get_pool(
                                pool_address=step.address, silent=True
                            )
                        )
                    except Exception:
                        try:
                            pool = await self._run_build_offloaded(
                                lambda: self.sushiswap_v3_tracker.get_pool(
                                    pool_address=step.address, silent=True
                                )
                            )
                        except Exception:
                            try:
                                pool = await self._run_build_offloaded(
                                    lambda: self.pancakeswap_v3_tracker.get_pool(
                                        pool_address=step.address, silent=True
                                    )
                                )
                            except Exception:
                                pool = await self._run_build_offloaded(
                                    lambda: self.constr_bot.build_pool(step.address, silent=True)
                                )
                except Exception as exc:
                    bot_logger.debug(f"Skip V3 {step.address}: {exc}")
                    self._record_skip(f"build-v3:{type(exc).__name__}")
                    skip = True
                    break
            elif pt == "V4":
                if not step.hash:
                    self._record_skip("v4-no-hash")
                    skip = True
                    break
                try:
                    pool = await self._run_build_offloaded(
                        lambda: self.constr_bot.build_managed_pool(
                            address=UNISWAP_V4_POOL_MANAGER_ADDRESS,
                            pool_id=step.hash,
                            silent=True,
                        )
                    )
                except HookedPoolRejectedError:
                    self.v4_hook_rejected += 1
                    self._record_skip("v4-hook-rejected")
                    skip = True
                    v4_admission_rejected = True
                    break
                except DynamicFeePoolRejectedError:
                    self.v4_dynamic_fee_rejected += 1
                    self._record_skip("v4-dynamic-fee-rejected")
                    skip = True
                    v4_admission_rejected = True
                    break
                except Exception as exc:
                    bot_logger.debug(f"Skip V4 {step.hash}: {exc}")
                    self._record_skip(f"build-v4:{type(exc).__name__}")
                    skip = True
                    break
            else:
                self._record_skip("unknown-pool-type")
                skip = True
                break
            pools.append(cast("UniswapV2Pool | UniswapV3Pool | UniswapV4Pool", pool))

        if skip:
            if not v4_admission_rejected:
                self.skip_count += 1
            return

        # Register with Rust engine
        try:
            for pool, pt in zip(pools, pool_type_strs, strict=True):
                if pt == "V2":
                    retry_verification_call(
                        self.retry_policy_obj, self.engine_registry.register_v2_pool, pool
                    )
                elif pt == "V3":
                    await retry_verification_call_async(
                        self.retry_policy_obj,
                        self.engine_registry.register_v3_pool,
                        pool,
                    )
                elif pt == "V4":
                    self.v4_pool_count += 1
                    await retry_verification_call_async(
                        self.retry_policy_obj,
                        self.engine_registry.register_v4_pool,
                        pool,
                    )
        except VerificationMismatchError as exc:
            bot_logger.critical(f"[build_paths] VERIFICATION FAILURE — shutting down: {exc}")
            raise
        except VerificationRpcError as exc:
            bot_logger.critical(f"[build_paths] VERIFICATION RPC FAILURE — shutting down: {exc}")
            raise
        except RuntimeError as exc:
            self.engine_reject_count += 1
            self.other_exc_count += 1
            bot_logger.info(
                f"[build_paths] Engine registration failed ({type(exc).__name__}): {exc}",
            )
            return
        except Exception as exc:
            self.engine_reject_count += 1
            self.other_exc_count += 1
            bot_logger.info(
                f"[build_paths] Engine registration failed ({type(exc).__name__}): {exc}",
            )
            return

        # Resolve directions and register path
        zfo_list = self._resolve_path_directions(pools, directions)
        if zfo_list is None:
            self.direction_fail_count += 1
            self._record_skip("direction-fail")
            return

        pool_sigs: list[str] = []
        for p in pools:
            if isinstance(p, UniswapV4Pool):
                pool_sigs.append(p.pool_id.to_0x_hex())
            else:
                pool_sigs.append(p.address)
        path_sig = tuple(v for pair in zip(pool_sigs, zfo_list, strict=True) for v in pair)
        if path_sig in self.registered_path_sigs:
            self.dup_count += 1
            self._record_skip("dup")
            return
        self.registered_path_sigs.add(path_sig)

        try:
            self.engine_registry.register_path(list(zip(pools, zfo_list, strict=True)))
        except Exception as exc:
            self.register_fail_count += 1
            self._record_skip(f"register-fail:{type(exc).__name__}")
            if self.register_fail_count <= 5:
                bot_logger.warning(f"Path registration failed: {type(exc).__name__}: {exc}")
            return

        self.path_count += 1
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
    pipeline.pool_type_per_depth = _parse_permutation_filter(PATH_PERMUTATION_FILTER)
    pipeline.pool_types = _pool_types_from_filter(PATH_PERMUTATION_FILTER)
    if pipeline.pool_type_per_depth is not None:
        bot_logger.info(
            "[build_paths] Permutation filter active: "
            f"{PATH_PERMUTATION_FILTER} → depths={pipeline.pool_type_per_depth}",
        )
    bot_logger.info(f"[build_paths] Pool types: {[t.__name__ for t in pipeline.pool_types]}")

    start = time.perf_counter()

    bot_logger.info("[build_paths] Calling find_paths_async...")
    bot_logger.info(
        f"[build_paths] Starting registration pipeline: {REG_WORKERS} workers, "
        f"queue bound {REG_QUEUE_BOUND}"
    )

    discovery_producer: AsyncIterable[object] = pipeline.discovery_sweep()
    bot_logger.info("[build_paths] Discovery: single pass over the DB subgraph")

    await pipeline.run_registration(producer=discovery_producer)

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
        f"{pipeline.direction_fail_count} direction-failed, "
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
