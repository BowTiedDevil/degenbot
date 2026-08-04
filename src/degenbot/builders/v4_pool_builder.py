"""Uniswap V4 pool builder (sync)."""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, cast

from hexbytes import HexBytes

from degenbot.builders.request import BuildManagedPoolRequest
from degenbot.builders.seed_block_resolver import resolve_seed_block
from degenbot.builders.tick_data_fetcher import (
    FetchedTickData,
    TickDataTypes,
    make_tick_data_fetcher,
)
from degenbot.builders.v4_builder_base import V4BuilderBase, V4DbValues, V4Slot0Data
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v4_liquidity_pool import ProtocolFee, UniswapV4Pool
from degenbot.uniswap.v4_types import (
    UniswapV4PoolExternalUpdate,
)

if TYPE_CHECKING:
    from collections.abc import Callable

    from degenbot.bot import PyBotIo
    from degenbot.builders.context import BuilderContext
    from degenbot.builders.request import BuildRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId
    from degenbot.types.rpc_types import BlockIdentifier


class V4PoolBuilder(V4BuilderBase):
    """Builds and updates V4 singleton-architecture concentrated-liquidity pools.

    Owns the full I/O choreography: DB lookup → RPC fetch → decode →
    construct pool → register.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        """Initialize the instance."""
        assert ctx.managed_pools is not None, (
            "V4PoolBuilder requires managed_pools in BuilderContext"
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
        pool_id: HexBytes,
        pool_manager_address: str,
        state_view_address: str,
        chain_id: int,
        io: PyBotIo,
    ) -> Callable[[int, int], FetchedTickData | None]:
        """Create a tick data fetcher callback for a V4 pool.

        Returns:
            The computed value.

        """
        pool_manager_address_ = get_checksum_address(pool_manager_address)
        return make_tick_data_fetcher(
            pool_lookup=lambda _: cast(
                "UniswapV4Pool | None",
                self._managed_pools.get(
                    chain_id=chain_id,
                    pool_manager_address=pool_manager_address_,
                    pool_id=pool_id,
                ),
            ),
            io=io,
            types=TickDataTypes(
                bitmap_at_word=BitmapAtWord,
                liquidity_at_tick=LiquidityAtTick,
                tick_struct_types=("uint128", "int128"),
            ),
            state_view_address=state_view_address,
            pool_id=bytes(pool_id),
        )

    def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: PyBotIo,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV4Pool.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: If the operation fails.
            LiquidityPoolError: If the operation fails.

        """
        assert isinstance(request, BuildManagedPoolRequest)
        pool_id_bytes = HexBytes(request.pool_id)
        pool_manager_address = get_checksum_address(address)
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
        # `PyBotIo` seam (QVMWQC). The V4 lookup is two-step: resolve the pool
        # manager by `(address, chain)` (`state_view` + `id`), then the V4 pool
        # row by `pool_hash`; the currency relationships hydrate via per-FK
        # token fetches. Falls back to skipping when no `io` /
        # `database_path` is configured (mirrors `contextlib.suppress`).
        db_values = None
        db_liquidity_update_block: int | None = None
        # Route the DB read through the Rust `PyBotIo` seam (QVMWQC). The
        # `contextlib.suppress` makes a missing/empty DB a skip, not an error.
        with contextlib.suppress(Exception):
            manager_row = io.fetch_pool_manager(chain_id=chain_id, address=pool_manager_address)
            if manager_row is not None:
                v4_row = io.fetch_v4_pool_by_pool_hash(pool_hash_hex=pool_id_bytes.to_0x_hex())
                if (
                    v4_row is not None
                    and v4_row.currency0_id is not None
                    and v4_row.currency1_id is not None
                ):
                    currency0_row = io.fetch_token_by_id(token_id=v4_row.currency0_id)
                    currency1_row = io.fetch_token_by_id(token_id=v4_row.currency1_id)
                    if currency0_row is not None and currency1_row is not None:
                        # The block at which the DB's liquidity snapshot is
                        # exact. If the pool's tick/liquidity data is seeded
                        # from the DB, the seed must be anchored to THIS block,
                        # not the live head — the V4 twin of the V3 H1 fix
                        # (without it, post-drain verify reads on-chain at head
                        # against stale DB tick data and false-positives on
                        # every tick that moved in the gap).
                        db_liquidity_update_block = v4_row.liquidity_update_block
                        db_values = V4DbValues(
                            currency0_address=get_checksum_address(currency0_row.address),
                            currency1_address=get_checksum_address(currency1_row.address),
                            hook_address=get_checksum_address(v4_row.hooks),
                            tick_spacing=v4_row.tick_spacing,
                            fee=v4_row.fee_token0,
                            state_view_address=manager_row.state_view,
                        )

        # Seed-anchor policy (V4 twin of the V3 split, two-stamp OB7UNY; the
        # 0x5653 staged-clock fix): `state_block` is the block the DB liquidity
        # snapshot's assembled tick map is exact at and becomes
        # `tick_data_block`. The SLOT0/price is fetched FRESH at `head_block`
        # and stamps `update_block` — a cheap slot0 read refreshes the price
        # clock past the snapshot block without draining event logs. The
        # post-drain verify compares the seeded tick data against on-chain at
        # `tick_data_block` (the block it actually reflects) while the solver
        # gets a fresh price for ADR-021. Only the tick anchor is auto-applied
        # when the caller did not pin an explicit `state_block`. Conservative:
        # an old snapshot defers the pool's paths via the freshness gate until
        # the pump backfill catches up — never a false positive.
        state_block = resolve_seed_block(
            request_state_block=request.state_block,
            db_liquidity_update_block=db_liquidity_update_block,
            head_block=state_block,
        )

        # Get immutable values
        if db_values is not None:
            currency0_address = db_values.currency0_address
            currency1_address = db_values.currency1_address
            hook_address = db_values.hook_address
            tick_spacing_for_pool = db_values.tick_spacing
            fee_for_pool = db_values.fee
            state_view_address = db_values.state_view_address
        else:
            if request.state_view_address is None:
                raise DegenbotValueError(
                    message="A state view contract address must be provided for a pool not in the database.",  # ruff:ignore[line-too-long]
                )
            if request.fee is None:
                raise DegenbotValueError(
                    message="A fee must be provided for a pool not in the database.",
                )
            if request.tick_spacing is None:
                raise DegenbotValueError(
                    message="A tick spacing must be provided for a pool not in the database.",
                )
            if request.tokens is None:
                raise DegenbotValueError(
                    message="Token addresses must be provided for a pool not in the database.",
                )

            state_view_address = get_checksum_address(request.state_view_address)
            currency0_address, currency1_address = sorted(
                [get_checksum_address(t) for t in request.tokens],
                key=lambda t: t.lower(),
            )
            hook_address = (
                get_checksum_address(request.hook_address)
                if request.hook_address is not None
                else _ZERO_ADDRESS
            )
            fee_for_pool = request.fee
            tick_spacing_for_pool = request.tick_spacing

        # Build tokens
        token0 = self._erc20_builder.build(
            currency0_address,
            chain_id=chain_id,
            silent=request.silent,
            io=io,
        )
        token1 = self._erc20_builder.build(
            currency1_address,
            chain_id=chain_id,
            silent=request.silent,
            io=io,
        )

        # Fetch slot0 + liquidity FRESH at the price seed block (`head_block`)
        # — the cheap slot0 read that stamps `update_block` at head while the
        # tick map stays anchored at `state_block` (two-stamp OB7UNY).
        # ADR-005 slice 14o: delegate both RPCs to Rust (PyBotIo is the only
        # executor; the Python parity-gate fallback is retired).
        try:
            assert state_view_address is not None
            sqrt_price_x96, tick_raw, protocol_fee_raw, lp_fee, _liquidity_val = (
                io.fetch_v4_slot0_liquidity(state_view_address, pool_id_bytes, block=head_block)
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc
        slot0_data = V4Slot0Data(
            sqrt_price_x96=int(sqrt_price_x96),
            tick=int(tick_raw),
            protocol_fee_one_to_zero=int(protocol_fee_raw) >> 12,
            protocol_fee_zero_to_one=int(protocol_fee_raw) & 0xFFF,
            lp_fee=int(lp_fee),
        )

        # Fetch initial tick bitmap and tick data via the Rust `assemble_*`
        # helper (UHPXSD cutover / epic Candidate 1 / Decision 6 (B)) — V4 twin
        # of the V3 builder cutover. One call — Store take → Db precedence,
        # both in Rust; on a hit, the returned `tick_rows` is already in
        # `register_v4_pool`'s `tick_data` arg shape
        # (`{tick: (liquidity_gross, liquidity_net, block)}`). On a miss, fall
        # through to Branch 3 (sparse RPC) as before.
        #
        # ``tick_map_is_tracked`` is True ONLY when the tick map is complete
        # (a full snapshot) — so the Rust dense swap's ``gen_ticks(tick_data)``
        # can trust it to propose every crossing tick. The live single-word RPC
        # fetch path below seeds ONLY the current tick-bitmap word; swaps that
        # cross into a neighbouring word would silently walk uninitialized
        # boundary ticks (applying no liquidity_net) and produce wrong amounts.
        # Such pools MUST register ``coverage=sparse`` + a fetcher so the Rust
        # miss-detection backfills missing words.
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

        # Delegate the build to the Rust PoolBuilder (T4 / 4GQWZ4): core
        # `build_v4` fetches slot0/liquidity FRESH + assembles the tick map
        # (Db → Chain precedence) + registers into BotState atomically. V4
        # identity is caller-supplied (mirrors `register_v4_pool`); the
        # StateView mapping is registered Rust-side (idempotent) inside the
        # adapter. Returns `(pool_id, coverage)` — coverage drives the
        # companion's `_sparse_liquidity_map`. The `slot0_data` decoded above
        # is kept for the Python-side companion overrides (`protocol_fee` /
        # `lp_fee`, which the Rust handle does not expose).
        assert state_view_address is not None
        hook_flags = int(hook_address, 16) if hook_address else 0
        pool_handle_pool_id, coverage = self._py_bot.build_v4_pool(
            pool_manager=pool_manager_address,
            pool_id_hex=pool_id_bytes.to_0x_hex(),
            currency0=token0.address,
            currency1=token1.address,
            fee=fee_for_pool,
            tick_spacing=tick_spacing_for_pool,
            hook_flags=hook_flags,
            state_view_address=state_view_address,
            block=int(head_block) if head_block is not None else None,
            db=True,
            tick_data_fetcher=self._make_tick_data_fetcher(
                pool_id_bytes,
                pool_manager_address,
                state_view_address,
                chain_id,
                io=io,
            ),
        )
        tick_map_is_tracked = coverage == "tracked"
        py_pool_handle = self._py_bot.get_pool(pool_handle_pool_id)
        assert py_pool_handle is not None, "build_v4_pool returned a pool_id with no handle"
        # No separate ``update_tick_data`` — the inline seed is complete (tick
        # map + known bitmap words, atomically with registration). A separate
        # REPLACE would clobber live pump events in the now-closed window.
        pool = UniswapV4Pool._from_py_pool(py_pool_handle)  # ruff:ignore[private-member-access]
        # Builder-supplied values the seam defaults; override from RPC.
        pool._state_view_address = (  # ruff:ignore[private-member-access]
            get_checksum_address(state_view_address) if state_view_address else _ZERO_ADDRESS
        )
        pool.protocol_fee = ProtocolFee(
            zero_for_one=slot0_data.protocol_fee_zero_to_one,
            one_for_zero=slot0_data.protocol_fee_one_to_zero,
        )
        pool.lp_fee = slot0_data.lp_fee
        pool._sparse_liquidity_map = not tick_map_is_tracked  # ruff:ignore[private-member-access]

        # Register pool in managed pool registry
        self._managed_pools.add(
            pool=pool,
            chain_id=chain_id,
            pool_manager_address=pool.address,
            pool_id=pool.pool_id,
        )

        if not request.silent:
            logger.info(pool.name)
            logger.info(f"• ID: {pool.pool_id.to_0x_hex()}")
            logger.info(f"• Token 0: {token0}")
            logger.info(f"• Token 1: {token1}")
            logger.info(f"• Liquidity: {pool.liquidity}")
            logger.info(f"• SqrtPrice: {pool.sqrt_price_x96}")
            logger.info(f"• Tick: {pool.tick}")

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
        if not isinstance(pool, UniswapV4Pool):
            msg = f"V4PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert io is not None
        raw_block = block_number if block_number is not None else io.get_block_number()
        block_number_ = int(raw_block) if not isinstance(raw_block, int) else raw_block

        # ADR-005 slice 14o: delegate both RPCs to Rust (PyBotIo is the only
        # executor; the Python parity-gate fallback is retired).
        sqrt_price_x96, tick_raw, protocol_fee_raw, lp_fee, liquidity_val = (
            io.fetch_v4_slot0_liquidity(
                pool._state_view_address,  # ruff:ignore[private-member-access]
                pool.pool_id,
                block=block_number_,
            )
        )
        slot0_data = V4Slot0Data(
            sqrt_price_x96=int(sqrt_price_x96),
            tick=int(tick_raw),
            protocol_fee_one_to_zero=int(protocol_fee_raw) >> 12,
            protocol_fee_zero_to_one=int(protocol_fee_raw) & 0xFFF,
            lp_fee=int(lp_fee),
        )

        if (
            pool.sqrt_price_x96 == slot0_data.sqrt_price_x96
            and pool.liquidity == liquidity_val
            and pool.tick == slot0_data.tick
        ):
            return False

        update = UniswapV4PoolExternalUpdate(
            block_number=block_number_,
            sqrt_price_x96=slot0_data.sqrt_price_x96,
            tick=slot0_data.tick,
            liquidity=liquidity_val,
        )
        pool.external_update(update)
        return True
