"""Uniswap V3 pool builder (sync)."""

from __future__ import annotations

import contextlib
import os
from typing import TYPE_CHECKING, cast

from degenbot.builders.seed_block_resolver import resolve_seed_block
from degenbot.builders.tick_data_fetcher import (
    FetchedTickData,
    TickDataTypes,
    make_tick_data_fetcher,
)
from degenbot.builders.v3_builder_base import V3BuilderBase, V3DbValues
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3PoolExternalUpdate,
)
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

if TYPE_CHECKING:
    from collections.abc import Callable

    from degenbot.bot import PyBotIo
    from degenbot.builders.context import BuilderContext
    from degenbot.builders.request import BuildRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId
    from degenbot.types.rpc_types import BlockIdentifier


class V3PoolBuilder(V3BuilderBase):
    """Builds and updates V3-style concentrated-liquidity pools.

    Owns the full I/O choreography: DB lookup → RPC fetch → decode →
    construct pool → register.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        """Initialize the instance."""
        assert ctx.managed_pools is not None, (
            "V3PoolBuilder requires managed_pools in BuilderContext"
        )
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._managed_pools = ctx.managed_pools
        self._erc20_builder = ctx.erc20_builder
        self._py_bot = ctx.py_bot

    def _make_tick_data_fetcher(
        self,
        pool_address: str,
        chain_id: int,
        io: PyBotIo,
    ) -> Callable[[int, int], FetchedTickData | None]:
        """Create a tick data fetcher callback for a V3 pool.

        Returns:
            The computed value.

        """
        return make_tick_data_fetcher(
            pool_lookup=lambda _block: cast(
                "UniswapV3Pool | None",
                self._pools.get(
                    chain_id=chain_id,
                    pool_address=get_checksum_address(pool_address),
                ),
            ),
            io=io,
            types=TickDataTypes(
                bitmap_at_word=BitmapAtWord,
                liquidity_at_tick=LiquidityAtTick,
                tick_struct_types=UniswapV3Pool.TICK_STRUCT_TYPES,
            ),
        )

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: PyBotIo,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from DB/RPC and construct an I/O-free V3-style pool.

        Returns:
            The computed value.

        Raises:
            ValueError: If the operation fails.
            LiquidityPoolError: If the operation fails.

        """
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        state_block = (
            request.state_block if request.state_block is not None else io.get_block_number()
        )
        # The FRESH PRICE read block (two-stamp OB7UNY): the cheap slot0/price
        # read stamps `update_block` at the live head, while the liquidity
        # clock + assembled tick map anchor at `state_block` (the DB snapshot
        # block). When a caller pins `request.state_block`, price = that same
        # block (no split).
        head_block = state_block

        # Try DB first — route the construction-time read through the Rust
        # `PyBotIo` seam (QVMWQC). `fetch_pool_row` returns the scalar + FK-id
        # columns; the relationships (`exchange`, `token0/1`) + the V3 subclass
        # row (`tick_spacing`/fees) hydrate via per-FK fetches, mirroring the
        # prior SQLAlchemy lazy-load. Falls back to skipping when no `io` /
        # `database_path` is configured (mirrors `contextlib.suppress`).
        db_values = None
        db_liquidity_update_block: int | None = None
        # Route the DB read through the Rust `PyBotIo` seam (QVMWQC). The
        # `contextlib.suppress` makes a missing/empty DB a skip, not an error.
        with contextlib.suppress(Exception):
            pool_row = io.fetch_pool_row(chain_id=chain_id, address=pool_address)
            if pool_row is not None:
                kind_row = io.fetch_pool_kind(kind=pool_row.kind, pool_id=pool_row.id)
                exchange_row = io.fetch_exchange(exchange_id=pool_row.exchange_id)
                token0_row = io.fetch_token_by_id(token_id=pool_row.token0_id)
                token1_row = io.fetch_token_by_id(token_id=pool_row.token1_id)
                if (
                    kind_row is not None
                    and exchange_row is not None
                    and token0_row is not None
                    and token1_row is not None
                ):
                    # The block at which the DB's liquidity snapshot is exact. If
                    # the pool's tick/liquidity data is seeded from the DB (it is
                    # a DB-hit `coverage=tracked` below), `update_block` must be
                    # anchored to THIS block, not the live head — otherwise the
                    # registration seeds head while the tick data reflects
                    # `liquidity_update_block`, and step-2
                    # `verify_v3_post_drain_snapshot` reads on-chain at head and
                    # false-positives on every tick that moved in the
                    # `(liquidity_update_block, head]` window (H1 confirmed in
                    # run 2026-08-03: pool 0x5653 tick 63970 Mint @ 25676145).
                    db_liquidity_update_block = kind_row.liquidity_update_block
                    db_values = V3DbValues(
                        factory=get_checksum_address(exchange_row.factory),
                        token0_address=get_checksum_address(token0_row.address),
                        token1_address=get_checksum_address(token1_row.address),
                        fee=kind_row.fee_token0,
                        tick_spacing=kind_row.tick_spacing,
                        deployer_address=exchange_row.deployer
                        if exchange_row.deployer is not None
                        else None,
                    )

        # Seed-anchor policy (two-stamp OB7UNY): `state_block` is the block the
        # DB liquidity snapshot's assembled tick map is exact at, and becomes
        # `tick_data_block`. The SLOT0/price is fetched FRESH at `head_block`
        # and stamps `update_block` — a cheap slot0 read refreshes the price
        # clock past the snapshot block without draining event logs. This is
        # the H1 fix preserved: the post-drain verify compares the seeded tick
        # data against on-chain at `tick_data_block` (the block it actually
        # reflects), while the solver gets a fresh price for ADR-021. Row-valid
        # prior (line above): if the snapshot is old, the AV42C7 freshness gate
        # defers the pool's paths until the pump backfill catches it up — it
        # never false-positives.
        state_block = resolve_seed_block(
            request_state_block=request.state_block,
            db_liquidity_update_block=db_liquidity_update_block,
            head_block=state_block,
        )

        # Get immutable values
        if db_values is not None:
            factory = db_values.factory
            token0_address = db_values.token0_address
            token1_address = db_values.token1_address
            fee = db_values.fee
            tick_spacing_for_pool = db_values.tick_spacing
        else:
            # ADR-005 slice 14f: delegate the 5-call immutable RPC choreography
            # to Rust (PyBotIo is the only executor; the Python parity-gate
            # fallback is retired).
            try:
                (
                    factory,
                    token0_address,
                    token1_address,
                    fee,
                    tick_spacing_for_pool,
                ) = io.fetch_v3_immutable_data(pool_address)
            except Exception as exc:
                raise LiquidityPoolError(message="Could not decode contract data") from exc

        # Build tokens
        token0 = self._erc20_builder.build(
            token0_address,
            chain_id=chain_id,
            silent=request.silent,
            io=io,
        )
        token1 = self._erc20_builder.build(
            token1_address,
            chain_id=chain_id,
            silent=request.silent,
            io=io,
        )

        # Fetch slot0 + liquidity FRESH at the price seed block (`head_block`)
        # — the cheap slot0 read that stamps `update_block` at head while the
        # tick map stays anchored at `state_block` (two-stamp OB7UNY).
        # ADR-005 slice 14f: same delegation seam as the immutable-data block.
        # PyBotIo is the only executor; the Python parity-gate fallback is retired.
        try:
            sqrt_price_x96, tick, liquidity = io.fetch_v3_slot0_liquidity(
                pool_address,
                block=head_block,
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        # Fetch initial tick bitmap and tick data via the Rust `assemble_*`
        # helper (epic 5NT2OC: `Db → Chain` precedence; the former `Store` arm
        # was retired by epic XEANMB). One call: the helper reads the Db, then
        # (on a miss) the Chain arm (`AlloyTickBootstrapRpc` — pure-Rust
        # sparse-RPC word read). The returned `tick_rows` is already in
        # `register_v3_pool`'s `tick_data` arg shape (`{tick:
        # (liquidity_gross, liquidity_net, block)}`); `coverage` is `"tracked"`
        # on a Db hit, `"sparse"` on a Chain hit.
        #
        # QVMWQC: the tick-snapshot read stays routed through the Rust seam.
        # Db + Chain errors propagate as `RuntimeError` from the Rust helper
        # (Decision 8 (A): loud failure over silent degrade, deliberate
        # behavior change — a `database is locked` or RPC failure now aborts
        # pool registration where Python previously swallowed it and fell to
        # sparse RPC).
        #
        # Task XH5ID5: the Python Branch 3 inline sparse-RPC choreography is
        # GONE — the Rust Chain arm owns it. `io=io` threads the
        # `AlloyTickBootstrapRpc` through; `io=None` (cold-start, no `Bot`-bound
        # provider) leaves the Chain arm off → `(tick_data=None,
        # coverage="sparse")` registration (the defensive fallback).
        state_block_int = int(state_block) if state_block is not None else 0
        head_block_int = int(head_block) if head_block is not None else 0
        rust_rows: dict[int, tuple[int, int, int]] = {}
        coverage = "sparse"

        assembled = self._py_bot.assemble_v3_tick_map(
            pool_address,
            tick=int(tick),
            tick_spacing=tick_spacing_for_pool,
            block=state_block_int,
            io=io,
        )
        if assembled is not None:
            rust_rows, coverage = assembled
        # Cold-start fallback: when `io=None` the Chain arm is off → rust_rows
        # stays empty + coverage stays "sparse" (matches the pre-cutover path).

        # Deployer / init-hash are resolved off the Rust handle by _from_py_pool
        # (Fork A, P62DKO) — the builder no longer computes them.

        # Only pass tick data if we have a complete DB snapshot.
        # Map factory addresses to pool classes for V3 variants
        pool_class = pool_type_registry.get_v3_class(chain_id, factory)

        if pool_class is None:
            msg = f"No V3 pool class registered for chain {chain_id}, factory {factory}"
            raise ValueError(msg)
        # The registry types the class as the structural
        # ConcentratedLiquidityPool protocol, but the concrete class carries
        # the `_from_py_pool` construction seam (returns a concrete instance
        # via `Self`).
        pool_class = cast("type[UniswapV3Pool]", pool_class)

        # Register the pool in the shared Rust Bot and wrap the handle with
        # the Python companion (ADR-005 slice 8b). ADR-006 rolling-start race
        # closure: the snapshot tick_data is seeded INLINE in
        # ``register_v3_pool`` (one BotState write lock) so the pool is never
        # visible to the live pump (resumed before ``build_paths``) in an
        # unseeded state. Previously the builder registered with empty
        # tick_data then called ``update_tick_data`` to seed — a pump
        # Mint/Burn landing between register and seed was applied then
        # CLOBBERED by the seed overwrite (lost update → verify failure).
        # ``seed_genesis`` anchors the reorg genesis delta at state_block
        # without advancing either clock (two-stamp OB7UNY) — mirrors the V2
        # builder writing reserves with update_block.
        # [diag] seed-provenance probe (kept per JSFO5L; the seed-side half of
        # the `[verify-dbg]` pin). Logs the registration seed anchor: price at
        # `head_block` (fresh slot0) and tick map at `state_block` (the DB
        # liquidity snapshot block). A `tick_data_block` diverging from
        # `data_block` means the seed is inconsistent with the block its tick
        # data reflects — how the step-2
        # `verify_v3_post_drain_snapshot` false-positive was born
        # (data@DB_block vs on-chain@head). Gate: pre-existing
        # `DEGENBOT_TRACE_REGISTER_SEED`.
        if os.environ.get("DEGENBOT_TRACE_REGISTER_SEED"):
            max_tick_block = max([blk for _g, _n, blk in rust_rows.values()] + [state_block_int])
            logger.info(
                "[diag] register-v3-seed-provenance "
                f"pool_addr={pool_address} update_block={head_block_int} "
                f"tick_data_block={state_block_int} data_block={max_tick_block} "
                f"tick_count={len(rust_rows)} coverage={coverage}"
            )
        pool_id = self._py_bot.register_v3_pool(
            address=pool_address,
            token0=token0_address,
            token1=token1_address,
            fee=fee,
            tick_spacing=tick_spacing_for_pool,
            factory=factory,
            sqrt_price_x96=int(sqrt_price_x96),
            liquidity=int(liquidity),
            tick=int(tick),
            tick_data=rust_rows or None,
            update_block=head_block_int,
            tick_data_block=state_block_int,
            coverage=coverage,
            tick_data_fetcher=self._make_tick_data_fetcher(pool_address, chain_id, io=io),
        )
        py_pool_handle = self._py_bot.get_pool(pool_id)
        assert py_pool_handle is not None, "register_v3_pool returned a pool_id with no handle"
        if state_block is not None and state_block_int > 0:
            # Split-seed genesis: the reorg anchor (non-empty journal) is
            # established WITHOUT advancing the tick clock — the old
            # `apply_swap` genesis would have falsely claimed the tick map
            # reaches the price seed block (and would backward-panic
            # `update_block` when price is seeded at head past the DB map
            # block).
            py_pool_handle.seed_genesis(block_number=state_block_int)
        # ADR-006: tick_data was seeded inline in ``register_v3_pool`` above —
        # no separate ``update_tick_data`` Rust call (that would overwrite the
        # tick_data map and clobber pump events applied after registration).
        # The companion's ``_bitmap_override`` is populated by the constructor
        # from ``tick_bitmap_override`` below; it does not depend on a Rust
        # ``update_tick_data`` call.
        # ADR-005 sealed seam: the companion reads ALL identity off the handle
        # via `_from_py_pool` (no identity kwargs). The tick fetcher was stored
        # Rust-side at registration above (task MLJT4V).
        pool = pool_class._from_py_pool(py_pool_handle)  # ruff:ignore[private-member-access]
        # Sparse-liquidity-map flag: the companion infers from tick_data_snapshot,
        # but the builder knows the true coverage from the DB snapshot. Override
        # if the builder has more info.
        pool._sparse_liquidity_map = not (coverage == "tracked" and bool(rust_rows))  # ruff:ignore[private-member-access]
        # Deployer / init-hash: read off the Rust handle (Fork A, P62DKO).
        # The builder resolved the JSON-sourced deployer (effective deployer,
        # covering PancakeSwap V3's separate deployer) + init_hash at
        # registration; the companion already carries the verified values, so
        # no DB/registry override is needed here.
        # pool.deployer_address / pool.init_hash are set by _from_py_pool from
        # the handle.

        # Register pool
        self._pools.add(
            pool=pool,
            chain_id=chain_id,
            pool_address=pool.address,
        )

        if not request.silent:
            logger.info(pool.name)
            logger.info(f"• Address: {pool.address}")
            logger.info(f"• Token 0: {token0}")
            logger.info(f"• Token 1: {token1}")
            logger.info(f"• Fee: {fee}")
            logger.info(f"• Liquidity: {pool.liquidity}")
            logger.info(f"• SqrtPrice: {pool.sqrt_price_x96}")
            logger.info(f"• Tick: {pool.tick}")
            logger.info(f"• State Block (Initial): {state_block}")

        return pool

    @staticmethod
    def update(
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
        io: PyBotIo | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool.

        Returns:
            The computed value.

        Raises:
            TypeError: If the operation fails.

        """
        if isinstance(pool, UniswapV4Pool):
            msg = f"V3PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)
        if not isinstance(pool, UniswapV3Pool):
            msg = f"V3PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert io is not None
        raw_block = block_number if block_number is not None else io.get_block_number()
        block_number_ = int(raw_block) if not isinstance(raw_block, int) else raw_block

        # ADR-005 slice 14p: delegate slot0+liquidity to Rust (PyBotIo is the
        # only executor; the Python parity-gate fallback is retired). Reuses
        # the fetch_v3_slot0_liquidity seam from the build path (14f).
        sqrt_price_x96, tick, liquidity = io.fetch_v3_slot0_liquidity(
            pool.address,
            block=block_number_,
        )

        if (
            pool.sqrt_price_x96 == sqrt_price_x96
            and pool.liquidity == liquidity
            and pool.tick == tick
        ):
            return False

        update = UniswapV3PoolExternalUpdate(
            block_number=block_number_,
            sqrt_price_x96=sqrt_price_x96,
            tick=tick,
            liquidity=liquidity,
        )
        pool.external_update(update)
        return True

    # (The `[diag] register-v3-seed-provenance` probe lives inline in `build()`
    # under the pre-existing `DEGENBOT_TRACE_REGISTER_SEED` gate — the seed-side
    # half of the `[verify-dbg]` pin, kept per task JSFO5L.)
