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

from degenbot import Bot, LiquidityPool
from degenbot.degenbot_rs import UniswapArbEngine  # type: ignore[attr-defined]
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
        verify_on_register: bool = True,
    ) -> int:
        """Run the pre-pump startup ritual and stop BEFORE resume().

        Sequences: subscribe(ws) → stream snapshots → backfill → verify config.
        Stops at EnginePhase::Backfilled so the caller can attach its result
        consumer before batches begin to flow (`resume()` is the single gate
        after which the pump emits one ResultBatch per block into the
        fire-and-forget channel — attaching the consumer after resume risks
        unbounded backlog and stale-batch dispatch).

        `snapshot_block` is derived internally from min(snap.newest_block)
        across the supplied snapshots — the caller never passes a block.
        Snapshots are a first-class degenbot feature, so the registry owns this
        coupling rather than exposing it.

        Returns:
            The backfill target (first observed WS block) from ``subscribe``.

        """
        backfill_target = self.engine.subscribe(node_ws)

        # Stream snapshots and backfill the snapshot→WS gap. Zero batches are
        # emitted here — the pump isn't running until resume().
        snapshots = [s for s in (v3_snapshot, v4_snapshot) if s is not None]
        if snapshots:
            if v3_snapshot is not None:
                stream_v3_snapshot_to_engine(v3_snapshot, self.engine)
            if v4_snapshot is not None:
                stream_v4_snapshot_to_engine(v4_snapshot, self.engine)
            snapshot_block = min(s.newest_block for s in snapshots)
            self.engine.backfill_from_snapshot(node_http, snapshot_block)

        # Verify config (consumer-safe: nothing emits yet).
        self.engine.set_verify_rpc_url(node_http)
        if verify_state_view is not None:
            self.engine.set_verify_state_view(verify_state_view)
        self.engine.set_verify_on_register(verify_on_register)

        # Intentionally NOT calling resume() — the caller attaches its
        # consumer next, then calls resume() as the single batch-flow gate.
        return backfill_target

    def register_v2_pool(self, pool: LiquidityPool) -> int:  # noqa: D102
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
                f"(fee_token0={pool._fee_token0}, fee_token1={pool._fee_token1})."  # noqa: SLF001
            )
        self._v2_keys[pool.address] = key
        return key

    def register_v3_pool(
        self,
        pool: UniswapV3Pool,
        block: int = 0,
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

        key = self.engine.register_v3_pool(
            address=pool.address,
            token0=pool.token0.address,
            token1=pool.token1.address,
            fee=pool.fee,
            tick_spacing=pool.tick_spacing,
            factory=pool.factory,
            sqrt_price_x96=pool.sqrt_price_x96,
            liquidity=pool.liquidity,
            tick=pool.tick,
            block=block,
        )
        self._v3_keys[pool.address] = key
        return key

    def register_v4_pool(
        self,
        pool: UniswapV4Pool,
        block: int = 0,
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

        # V4 hook flags live in the bottom 12 bits of the hook address
        # (passed to the engine, which enforces the admission floor itself).
        hook_flags = int(pool.hook_address, 16) & 0xFFF

        key = self.engine.register_v4_pool(
            pool_manager=pool.address,
            pool_id_hex=pool_id_hex,
            currency0=pool.token0.address,
            currency1=pool.token1.address,
            fee=pool.fee,
            tick_spacing=pool.tick_spacing,
            hook_flags=hook_flags,
            sqrt_price_x96=pool.sqrt_price_x96,
            liquidity=pool.liquidity,
            tick=pool.tick,
            block=block,
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
        pools_and_zfos: Sequence[tuple[LiquidityPool | UniswapV3Pool | UniswapV4Pool, bool]],
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
            elif isinstance(pool, LiquidityPool):
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
