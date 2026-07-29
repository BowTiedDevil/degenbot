"""Uniswap V4 pool builder (sync)."""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, cast

from hexbytes import HexBytes

from degenbot.builders.request import BuildManagedPoolRequest
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

        # Try DB first — route the construction-time read through the Rust
        # `PyBotIo` seam (QVMWQC). The V4 lookup is two-step: resolve the pool
        # manager by `(address, chain)` (`state_view` + `id`), then the V4 pool
        # row by `pool_hash`; the currency relationships hydrate via per-FK
        # token fetches. Falls back to skipping when no `io` /
        # `database_path` is configured (mirrors `contextlib.suppress`).
        db_values = None
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
                        db_values = V4DbValues(
                            currency0_address=get_checksum_address(currency0_row.address),
                            currency1_address=get_checksum_address(currency1_row.address),
                            hook_address=get_checksum_address(v4_row.hooks),
                            tick_spacing=v4_row.tick_spacing,
                            fee=v4_row.fee_token0,
                            state_view_address=manager_row.state_view,
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
                    message="A state view contract address must be provided for a pool not in the database.",  # noqa: E501
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

        # Fetch slot0 + liquidity via state view contract
        # ADR-005 slice 14o: delegate both RPCs to Rust (PyBotIo is the only
        # executor; the Python parity-gate fallback is retired).
        try:
            assert state_view_address is not None
            sqrt_price_x96, tick_raw, protocol_fee_raw, lp_fee, liquidity_val = (
                io.fetch_v4_slot0_liquidity(state_view_address, pool_id_bytes, block=state_block)
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
        register_rows: dict[int, tuple[int, int, int]] | None = None
        coverage = "sparse"
        tick_map_is_tracked = False

        assembled = self._py_bot.assemble_v4_tick_map(
            pool_manager_address,
            pool_id_bytes,
            state_view_address,
            tick=int(slot0_data.tick),
            tick_spacing=tick_spacing_for_pool,
            block=int(state_block),
            io=io,
        )
        if assembled is not None:
            rows, coverage = assembled
            tick_map_is_tracked = coverage == "tracked"
            register_rows = rows
        # Cold-start fallback: when `io=None` the Chain arm is off →
        # register_rows stays None + coverage stays "sparse" (matches the
        # pre-cutover path).

        # If tick data was populated, pass both. Otherwise pass None (sparse mode).
        assert state_view_address is not None
        # Register the V4 pool in Rust (BotState) and wrap the returned
        # PyLiquidityPool handle in the companion. Mirrors the V3 builder cut-
        # over (ADR-005 slice 9a/9c): the companion owns NO mutable state;
        # Rust is the source of truth. Hook + dynamic-fee admission is enforced
        # in BotState::register_v4_pool (surface exceptions propagate).
        hook_flags = int(hook_address, 16) if hook_address else 0
        # ADR-006 rolling-start race closure: seed tick_data INLINE in
        # ``register_v4_pool`` (one BotState write lock) so the pool is never
        # visible to the live pump (resumed before ``build_paths``) in an
        # unseeded state — mirrors the V3 builder + the async V4 builder.
        # Previously the builder registered empty then called
        # ``update_tick_data`` (a `state.tick_data = …` REPLACE that clobbered
        # any live ModifyLiquidity in the register→seed window → V4 desync).
        # ``coverage`` is the completeness contract: ``tracked`` ONLY when the
        # tick map is complete (full snapshot) — a windowed single-word seed
        # stays ``sparse`` so the Rust miss-detection backfills neighbouring
        # words on demand.
        pool_handle_pool_id = self._py_bot.register_v4_pool(
            pool_manager=pool_manager_address,
            pool_id_hex=pool_id_bytes.to_0x_hex(),
            currency0=token0.address,
            currency1=token1.address,
            fee=fee_for_pool,
            tick_spacing=tick_spacing_for_pool,
            hook_flags=hook_flags,
            sqrt_price_x96=slot0_data.sqrt_price_x96,
            liquidity=int(liquidity_val),
            tick=slot0_data.tick,
            block=state_block,
            tick_data=register_rows,
            coverage=coverage,
            tick_data_fetcher=self._make_tick_data_fetcher(
                pool_id_bytes,
                pool_manager_address,
                state_view_address,
                chain_id,
                io=io,
            ),
            protocol_fee=int(protocol_fee_raw),
        )
        py_pool_handle = self._py_bot.get_pool(pool_handle_pool_id)
        assert py_pool_handle is not None, "register_v4_pool returned a pool_id with no handle"
        # No separate ``update_tick_data`` — the inline seed is complete (tick
        # map + known bitmap words, atomically with registration). A separate
        # REPLACE would clobber live pump events in the now-closed window.
        pool = UniswapV4Pool._from_py_pool(py_pool_handle)  # noqa: SLF001
        # Builder-supplied values the seam defaults; override from RPC.
        pool._state_view_address = (  # noqa: SLF001
            get_checksum_address(state_view_address) if state_view_address else _ZERO_ADDRESS
        )
        pool.protocol_fee = ProtocolFee(
            zero_for_one=slot0_data.protocol_fee_zero_to_one,
            one_for_zero=slot0_data.protocol_fee_one_to_zero,
        )
        pool.lp_fee = slot0_data.lp_fee
        pool._sparse_liquidity_map = not tick_map_is_tracked  # noqa: SLF001

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
                pool._state_view_address,  # noqa: SLF001
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
