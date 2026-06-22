"""Async builder for V4 singleton-architecture concentrated-liquidity pools."""

from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
from hexbytes import HexBytes
from sqlalchemy import select

from degenbot.builders.request import BuildManagedPoolRequest
from degenbot.builders.v4_builder_base import V4BuilderBase
from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.database.models.pools import PoolManagerTable, UniswapV4PoolTable
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.uniswap.v4_types import UniswapV4PoolExternalUpdate

if TYPE_CHECKING:
    from web3.types import BlockIdentifier

    from degenbot.builders.async_context import AsyncBuilderContext
    from degenbot.builders.pool_io import AsyncPoolIO
    from degenbot.builders.request import BuildRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


class AsyncV4PoolBuilder:
    """Async counterpart of V4PoolBuilder.

    Builds UniswapV4Pool instances using AsyncPoolIO for I/O.
    Shares pure decode/resolve logic with V4BuilderBase via static methods.
    """

    def __init__(self, ctx: AsyncBuilderContext) -> None:
        """Initialize the instance."""
        assert ctx.managed_pools is not None, (
            "AsyncV4PoolBuilder requires managed_pools in BuilderContext"
        )
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._managed_pools = ctx.managed_pools
        self._erc20_builder = ctx.erc20_builder
        self._py_bot = ctx.py_bot

    async def build(
        self,
        address: str,
        *,
        chain_id: ChainId | None = None,
        io: AsyncPoolIO,
        request: BuildRequest,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV4Pool.

        Returns:
            The computed value.

        Raises:
            DegenbotValueError: If the operation fails.
            LiquidityPoolError: If the operation fails.

        """
        # For V4, `address` is the pool manager address
        pool_manager_address = get_checksum_address(address)
        assert isinstance(request, BuildManagedPoolRequest)
        pool_id_bytes = HexBytes(request.pool_id)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        assert io is not None

        state_block = (
            request.state_block if request.state_block is not None else await io.get_block_number()
        )

        # Try DB first
        db_values = None
        pool_id_db: int | None = None
        with contextlib.suppress(Exception), self._db() as session:
            pool_manager_in_db = session.scalar(
                select(PoolManagerTable).where(
                    PoolManagerTable.address == pool_manager_address,
                    PoolManagerTable.chain == chain_id,
                )
            )
            if pool_manager_in_db is not None:
                pool_from_db = session.scalar(
                    select(UniswapV4PoolTable).where(
                        UniswapV4PoolTable.pool_hash == pool_id_bytes.to_0x_hex(),
                        UniswapV4PoolTable.manager.has(id=pool_manager_in_db.id),
                    )
                )
                if pool_from_db is not None:
                    db_values = V4BuilderBase.extract_db_values(pool_from_db)
                    pool_id_db = pool_from_db.id

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
                    message="A state view contract address must be provided for a pool not in the database."  # noqa: E501
                )
            if request.fee is None:
                raise DegenbotValueError(
                    message="A fee must be provided for a pool not in the database."
                )
            if request.tick_spacing is None:
                raise DegenbotValueError(
                    message="A tick spacing must be provided for a pool not in the database."
                )
            if request.tokens is None:
                raise DegenbotValueError(
                    message="Token addresses must be provided for a pool not in the database."
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

        # Build tokens (async)
        token0 = await self._erc20_builder.build(
            currency0_address, chain_id=chain_id, silent=request.silent, io=io
        )
        token1 = await self._erc20_builder.build(
            currency1_address, chain_id=chain_id, silent=request.silent, io=io
        )

        # Fetch slot0 + liquidity via state view contract
        try:
            slot0_calldata = encode_function_calldata(
                "getSlot0(bytes32)",
                [pool_id_bytes],
            )
            liquidity_calldata = encode_function_calldata(
                "getLiquidity(bytes32)",
                [pool_id_bytes],
            )

            assert state_view_address is not None
            slot0_result = await io.call(
                to=state_view_address,
                data=slot0_calldata,
                block=state_block,
            )
            liquidity_result = await io.call(
                to=state_view_address,
                data=liquidity_calldata,
                block=state_block,
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        slot0_data = V4BuilderBase.decode_slot0(slot0_result)
        (liquidity_val,) = eth_abi.abi.decode(
            types=["uint256"],
            data=liquidity_result,
        )

        # Fetch initial tick bitmap and tick data
        working_tick_bitmap: dict[int, Any] = {}
        working_tick_data: dict[int, Any] = {}

        # Use provided tick data if given (snapshot or test fixtures)
        if request.tick_bitmap is not None and request.tick_data is not None:
            working_tick_bitmap = dict(request.tick_bitmap)
            working_tick_data = dict(request.tick_data)
        elif request.tick_bitmap is not None or request.tick_data is not None:
            raise DegenbotValueError(message="Provide both tick_bitmap and tick_data, or neither.")
        else:
            # Try DB snapshot tables first
            db_snapshot_loaded = False
            if pool_id_db is not None:
                with contextlib.suppress(Exception), self._db() as session:
                    pool_with_data = session.scalar(
                        select(UniswapV4PoolTable).where(UniswapV4PoolTable.id == pool_id_db)
                    )
                    if pool_with_data is not None:
                        working_tick_bitmap, working_tick_data, db_snapshot_loaded = (
                            V4BuilderBase.load_tick_snapshot(pool_with_data)
                        )

            if not db_snapshot_loaded:
                word, _ = get_tick_word_and_bit_position(
                    tick=int(slot0_data.tick), tick_spacing=tick_spacing_for_pool
                )

                assert state_view_address is not None
                (bitmap_at_word,) = eth_abi.abi.decode(
                    types=["uint256"],
                    data=await io.call(
                        to=state_view_address,
                        data=encode_function_calldata(
                            "getTickBitmap(bytes32,int16)",
                            [pool_id_bytes, word],
                        ),
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
                        result = await io.call(
                            to=state_view_address,
                            data=encode_function_calldata(
                                "getTickLiquidity(bytes32,int24)",
                                [pool_id_bytes, active_tick],
                            ),
                            block=state_block,
                        )
                        liquidity_gross, liquidity_net = eth_abi.abi.decode(
                            types=["uint128", "int128"],
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

        # If tick data was populated, pass both. Otherwise pass None (sparse mode).
        assert state_view_address is not None
        # Register the V4 pool in Rust (BotState) and wrap the returned
        # PyLiquidityPool handle in the companion (mirrors the sync V4 builder;
        # ADR-005 slice 9b/9c).
        hook_flags = int(hook_address, 16) if hook_address else 0
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
        )
        py_pool_handle = self._py_bot.get_pool(pool_handle_pool_id)
        assert py_pool_handle is not None, "register_v4_pool returned a pool_id with no handle"
        if working_tick_data:
            rows: dict[int, tuple[int, int, int]] = {}
            for t, info in working_tick_data.items():
                if isinstance(info, LiquidityAtTick):
                    rows[int(t)] = (
                        int(info.liquidity_gross),
                        int(info.liquidity_net),
                        int(info.block),
                    )
                else:
                    rows[int(t)] = (
                        int(info[0]),
                        int(info[1]),
                        int(info[2]) if len(info) > 2 else 0,  # noqa: PLR2004
                    )
            py_pool_handle.update_tick_data(
                working_tick_bitmap,
                rows,
                int(state_block),
            )
        pool = UniswapV4Pool(
            py_pool_handle,
            pool_id=pool_id_bytes,
            pool_manager_address=pool_manager_address,
            token0=token0,
            token1=token1,
            fee=fee_for_pool,
            tick_spacing=tick_spacing_for_pool,
            hook_address=hook_address,
            state_view_address=state_view_address,
            chain_id=chain_id,
            protocol_fee_zero_for_one=slot0_data.protocol_fee_zero_to_one,
            protocol_fee_one_for_zero=slot0_data.protocol_fee_one_to_zero,
            lp_fee=slot0_data.lp_fee,
            tick_bitmap=working_tick_bitmap if working_tick_data else None,
            state_block=state_block,
            tick_data_fetcher=None,
            sparse_liquidity_map=not bool(working_tick_data),
        )

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
    async def update(
        pool: AbstractLiquidityPool,
        *,
        io: AsyncPoolIO | None = None,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool.

        Returns:
            The computed value.

        Raises:
            TypeError: If the operation fails.

        """
        if not isinstance(pool, UniswapV4Pool):
            msg = f"AsyncV4PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert pool.chain_id is not None
        assert io is not None

        raw_block = block_number if block_number is not None else await io.get_block_number()
        block_number_ = int(raw_block) if not isinstance(raw_block, int) else raw_block

        slot0_calldata = encode_function_calldata("getSlot0(bytes32)", [pool.pool_id])
        slot0_result = await io.call(
            to=pool._state_view_address,  # noqa: SLF001
            data=slot0_calldata,
            block=block_number_,
        )
        slot0_data = V4BuilderBase.decode_slot0(slot0_result)

        liquidity_calldata = encode_function_calldata("getLiquidity(bytes32)", [pool.pool_id])
        (liquidity_val,) = cast(
            "tuple[int]",
            eth_abi.abi.decode(
                types=["uint256"],
                data=await io.call(
                    to=pool._state_view_address,  # noqa: SLF001
                    data=liquidity_calldata,
                    block=block_number_,
                ),
            ),
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
