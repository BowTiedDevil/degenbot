"""Uniswap V3 pool builder (sync)."""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, Any, cast

from degenbot.abi import decode as abi_decode
from degenbot.builders.tick_data_fetcher import (
    FetchedTickData,
    TickDataTypes,
    make_tick_data_fetcher,
)
from degenbot.builders.v3_builder_base import V3BuilderBase, V3DbValues
from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.math import (
    get_tick_word_and_bit_position as cl_get_tick_word_and_bit_position,
)
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3PoolExternalUpdate,
)
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

if TYPE_CHECKING:
    from collections.abc import Callable

    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
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
        io: PoolIO,
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
        io: PoolIO,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from DB/RPC and construct an I/O-free V3-style pool.

        Returns:
            The computed value.

        Raises:
            ValueError: If the operation fails.
            DegenbotValueError: If the operation fails.
            LiquidityPoolError: If the operation fails.

        """
        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        state_block = (
            request.state_block if request.state_block is not None else io.get_block_number()
        )

        # Try DB first — route the construction-time read through the Rust
        # `PyBotIo` seam (QVMWQC). `fetch_pool_row` returns the scalar + FK-id
        # columns; the relationships (`exchange`, `token0/1`) + the V3 subclass
        # row (`tick_spacing`/fees) hydrate via per-FK fetches, mirroring the
        # prior SQLAlchemy lazy-load. Falls back to skipping when no `io` /
        # `database_path` is configured (mirrors `contextlib.suppress`).
        db_values = None
        pool_id_db: int | None = None
        pool_kind_db: str | None = None
        fetch_pool_row = getattr(io, "fetch_pool_row", None) if io is not None else None
        if fetch_pool_row is not None:
            # All PyBotIo DB-query methods are present together (gated on
            # `fetch_pool_row`); bind them via `getattr(..., None)` so the
            # static type checker doesn't flag the `PoolIO`-protocol access
            # (`PyBotIo` defines them; `PoolIO` does not).
            fetch_pool_kind = getattr(io, "fetch_pool_kind", None)
            fetch_exchange = getattr(io, "fetch_exchange", None)
            fetch_token_by_id = getattr(io, "fetch_token_by_id", None)
            if (
                fetch_pool_kind is not None
                and fetch_exchange is not None
                and fetch_token_by_id is not None
            ):
                with contextlib.suppress(Exception):
                    pool_row = fetch_pool_row(chain_id=chain_id, address=pool_address)
                    if pool_row is not None:
                        kind_row = fetch_pool_kind(kind=pool_row.kind, pool_id=pool_row.id)
                        exchange_row = fetch_exchange(exchange_id=pool_row.exchange_id)
                        token0_row = fetch_token_by_id(token_id=pool_row.token0_id)
                        token1_row = fetch_token_by_id(token_id=pool_row.token1_id)
                        if (
                            kind_row is not None
                            and exchange_row is not None
                            and token0_row is not None
                            and token1_row is not None
                        ):
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
                        pool_id_db = pool_row.id
                        pool_kind_db = pool_row.kind

        # Get immutable values
        if db_values is not None:
            factory = db_values.factory
            token0_address = db_values.token0_address
            token1_address = db_values.token1_address
            fee = db_values.fee
            tick_spacing_for_pool = db_values.tick_spacing
        else:
            # ADR-005 slice 14f: when io is a PyBotIo (Bot's build path),
            # delegate the 5-call immutable RPC choreography to Rust. SyncPoolIO
            # fallback keeps the Python implementation as a parity gate.
            fetch_v3_immutable_data = getattr(io, "fetch_v3_immutable_data", None)
            if fetch_v3_immutable_data is not None:
                try:
                    (
                        factory,
                        token0_address,
                        token1_address,
                        fee,
                        tick_spacing_for_pool,
                    ) = fetch_v3_immutable_data(pool_address)
                except Exception as exc:
                    raise LiquidityPoolError(message="Could not decode contract data") from exc
            else:
                try:
                    factory_result = io.call(
                        to=pool_address,
                        data=encode_function_calldata("factory()", None),
                    )
                    token0_result = io.call(
                        to=pool_address,
                        data=encode_function_calldata("token0()", None),
                    )
                    token1_result = io.call(
                        to=pool_address,
                        data=encode_function_calldata("token1()", None),
                    )
                    fee_result = io.call(
                        to=pool_address,
                        data=encode_function_calldata("fee()", None),
                    )
                    tick_spacing_result = io.call(
                        to=pool_address,
                        data=encode_function_calldata("tickSpacing()", None),
                    )
                except Exception as exc:
                    raise LiquidityPoolError(message="Could not decode contract data") from exc

                immutable = V3BuilderBase.decode_immutable_data(
                    factory_result=factory_result,
                    token0_result=token0_result,
                    token1_result=token1_result,
                    fee_result=fee_result,
                    tick_spacing_result=tick_spacing_result,
                )
                factory = immutable.factory
                token0_address = immutable.token0_address
                token1_address = immutable.token1_address
                fee = immutable.fee
                tick_spacing_for_pool = immutable.tick_spacing

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

        # Fetch slot0 + liquidity
        # ADR-005 slice 14f: same delegation seam as the immutable-data block.
        fetch_v3_slot0_liquidity = getattr(io, "fetch_v3_slot0_liquidity", None)
        if fetch_v3_slot0_liquidity is not None:
            try:
                sqrt_price_x96, tick, liquidity = fetch_v3_slot0_liquidity(
                    pool_address,
                    block=state_block,
                )
            except Exception as exc:
                raise LiquidityPoolError(message="Could not decode contract data") from exc
        else:
            try:
                slot0_result = io.call(
                    to=pool_address,
                    data=encode_function_calldata("slot0()", None),
                    block=state_block,
                )
                liquidity_result = io.call(
                    to=pool_address,
                    data=encode_function_calldata("liquidity()", None),
                    block=state_block,
                )
            except Exception as exc:
                raise LiquidityPoolError(message="Could not decode contract data") from exc

            slot0_data = V3BuilderBase.decode_slot0(slot0_result)
            sqrt_price_x96 = slot0_data.sqrt_price_x96
            tick = slot0_data.tick
            (liquidity,) = abi_decode(types=["uint128"], data=liquidity_result)

        # Fetch initial tick bitmap and tick data
        db_snapshot_loaded = False
        working_tick_bitmap: dict[int, Any] = {}
        working_tick_data: dict[int, Any] = {}

        # Use provided tick data if given (snapshot or test fixtures)
        if request.tick_bitmap is not None and request.tick_data is not None:
            working_tick_bitmap = dict(request.tick_bitmap)
            working_tick_data = dict(request.tick_data)
            db_snapshot_loaded = True
        elif request.tick_bitmap is not None or request.tick_data is not None:
            raise DegenbotValueError(message="Provide both tick_bitmap and tick_data, or neither.")
        else:
            # Try DB snapshot tables first — route the tick-snapshot read
            # through the Rust `PyBotIo` seam (QVMWQC). The per-FK fetch
            # methods mirror the prior `pool.initialization_maps` /
            # `pool.liquidity_positions` lazy-loads; `liquidity_update_block`
            # comes from the subclass row.
            if pool_id_db is not None:
                # Same `getattr(..., None)` binding as the immutable-data
                # read above (PyBotIo DB-query methods share presence).
                fetch_init_maps = getattr(io, "fetch_initialization_maps", None)
                fetch_liq_positions = getattr(io, "fetch_liquidity_positions", None)
                fetch_pool_kind = getattr(io, "fetch_pool_kind", None)
                if (
                    fetch_init_maps is not None
                    and fetch_liq_positions is not None
                    and fetch_pool_kind is not None
                ):
                    with contextlib.suppress(Exception):
                        init_maps = fetch_init_maps(pool_id_db)
                        liq_positions = fetch_liq_positions(pool_id_db)
                        update_block = None
                        kind_row = fetch_pool_kind(kind=pool_kind_db, pool_id=pool_id_db)
                        if kind_row is not None:
                            update_block = kind_row.liquidity_update_block
                        working_tick_bitmap, working_tick_data, db_snapshot_loaded = (
                            V3BuilderBase.load_tick_snapshot_from_seam_rows(
                                init_maps=init_maps,
                                liq_positions=liq_positions,
                                liquidity_update_block=update_block,
                            )
                        )

            if not db_snapshot_loaded:
                word, _ = cl_get_tick_word_and_bit_position(
                    tick=int(tick),
                    tick_spacing=tick_spacing_for_pool,
                )

                # ADR-005 slice 14t: delegate tick-bitmap + per-tick RPCs to Rust.
                fetch_tick_bitmap = getattr(io, "fetch_tick_bitmap", None)
                fetch_tick_data = getattr(io, "fetch_tick_data", None)
                if fetch_tick_bitmap is not None:
                    bitmap_at_word = fetch_tick_bitmap(pool_address, word, block=state_block)
                else:
                    (bitmap_at_word,) = abi_decode(
                        types=["uint256"],
                        data=io.call(
                            to=pool_address,
                            data=encode_function_calldata("tickBitmap(int16)", [word]),
                            block=state_block,
                        ),
                    )

                if bitmap_at_word != 0:
                    active_ticks = [
                        ((word << 8) + i) * tick_spacing_for_pool
                        for i in range(256)
                        if bitmap_at_word & (1 << i) > 0
                    ]

                    for active_tick in active_ticks:
                        if fetch_tick_data is not None:
                            liquidity_gross, liquidity_net = fetch_tick_data(
                                pool_address,
                                active_tick,
                                block=state_block,
                            )
                        else:
                            result = io.call(
                                to=pool_address,
                                data=encode_function_calldata("ticks(int24)", [active_tick]),
                                block=state_block,
                            )
                            liquidity_gross, liquidity_net, *_ = abi_decode(
                                types=[
                                    "uint128",
                                    "int128",
                                    "uint256",
                                    "uint256",
                                    "int56",
                                    "uint160",
                                    "uint32",
                                    "bool",
                                ],
                                data=result,
                            )
                        working_tick_data[active_tick] = LiquidityAtTick(
                            liquidity_net=int(liquidity_net),
                            liquidity_gross=int(liquidity_gross),
                            block=state_block,
                        )

                working_tick_bitmap[word] = BitmapAtWord(
                    bitmap=bitmap_at_word,
                    block=state_block,
                )

        # Deployer / init-hash are resolved off the Rust handle by _from_py_pool
        # (Fork A, P62DKO) — the builder no longer computes them.

        # Only pass tick data if we have a complete DB snapshot.
        # Map factory addresses to pool classes for V3 variants
        pool_class = pool_type_registry.get_v3_class(chain_id, factory)

        if pool_class is None:
            msg = f"No V3 pool class registered for chain {chain_id}, factory {factory}"
            raise ValueError(msg)

        # Register the pool in the shared Rust Bot and wrap the handle with
        # the Python companion (ADR-005 slice 8b). ADR-006 rolling-start race
        # closure: the snapshot tick_data is seeded INLINE in
        # ``register_v3_pool`` (one BotState write lock) so the pool is never
        # visible to the live pump (resumed before ``build_paths``) in an
        # unseeded state. Previously the builder registered with empty
        # tick_data then called ``update_tick_data`` to seed — a pump
        # Mint/Burn landing between register and seed was applied then
        # CLOBBERED by the seed overwrite (lost update → verify failure).
        # ``apply_swap`` still anchors the reorg genesis delta at state_block
        # (mirrors the V2 builder writing reserves with update_block).
        rust_rows: dict[int, tuple[int, int, int]] = {}
        for t, info in working_tick_data.items():
            if isinstance(info, LiquidityAtTick):
                rust_rows[int(t)] = (
                    int(info.liquidity_gross),
                    int(info.liquidity_net),
                    int(info.block),
                )
            else:
                rust_rows[int(t)] = (
                    int(info[0]),
                    int(info[1]),
                    int(info[2]) if len(info) > 2 else 0,  # noqa: PLR2004
                )
        state_block_int = int(state_block) if state_block is not None else 0
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
            update_block=state_block_int,
            coverage="tracked" if db_snapshot_loaded else "sparse",
            tick_data_fetcher=self._make_tick_data_fetcher(pool_address, chain_id, io=io),
        )
        py_pool_handle = self._py_bot.get_pool(pool_id)
        assert py_pool_handle is not None, "register_v3_pool returned a pool_id with no handle"
        if state_block is not None and state_block_int > 0:
            py_pool_handle.apply_swap(
                sqrt_price_x96=int(sqrt_price_x96),
                liquidity=int(liquidity),
                tick=int(tick),
                block_number=state_block_int,
            )
        # ADR-006: tick_data was seeded inline in ``register_v3_pool`` above —
        # no separate ``update_tick_data`` Rust call (that would overwrite the
        # tick_data map and clobber pump events applied after registration).
        # The companion's ``_bitmap_override`` is populated by the constructor
        # from ``tick_bitmap_override`` below; it does not depend on a Rust
        # ``update_tick_data`` call.
        # ADR-005 sealed seam: the companion reads ALL identity off the handle
        # via `_from_py_pool` (no identity kwargs). The tick fetcher was stored
        # Rust-side at registration above (task MLJT4V).
        pool = pool_class._from_py_pool(py_pool_handle)  # noqa: SLF001
        # Sparse-liquidity-map flag: the companion infers from tick_data_snapshot,
        # but the builder knows the true coverage from the DB snapshot. Override
        # if the builder has more info.
        pool._sparse_liquidity_map = not (db_snapshot_loaded and bool(working_tick_data))  # noqa: SLF001
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
        io: PoolIO | None = None,
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

        # ADR-005 slice 14p: when io is a PyBotIo, delegate slot0+liquidity to Rust.
        # Reuses the fetch_v3_slot0_liquidity seam from the build path (14f).
        fetch_v3_slot0_liquidity = getattr(io, "fetch_v3_slot0_liquidity", None)
        if fetch_v3_slot0_liquidity is not None:
            sqrt_price_x96, tick, liquidity = fetch_v3_slot0_liquidity(
                pool.address,
                block=block_number_,
            )
        else:
            slot0_result = io.call(
                to=pool.address,
                data=encode_function_calldata("slot0()", None),
                block=block_number_,
            )
            slot0_data = V3BuilderBase.decode_slot0(slot0_result)
            sqrt_price_x96 = slot0_data.sqrt_price_x96
            tick = slot0_data.tick

            (liquidity,) = cast(
                "tuple[int]",
                abi_decode(
                    types=["uint256"],
                    data=io.call(
                        to=pool.address,
                        data=encode_function_calldata("liquidity()", None),
                        block=block_number_,
                    ),
                ),
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
