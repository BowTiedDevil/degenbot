from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
from hexbytes import HexBytes
from sqlalchemy import select

from degenbot.builders.tick_data_fetcher import TickDataTypes, make_tick_data_fetcher
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
from degenbot.uniswap.v4_types import (
    UniswapV4PoolExternalUpdate,
)

if TYPE_CHECKING:
    from collections.abc import Callable

    from web3.types import BlockIdentifier

    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
    from degenbot.builders.request import BuildPoolRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId
    from degenbot.uniswap.v3_types import BitmapWord, Tick


class V4PoolBuilder(V4BuilderBase):
    """
    Builds and updates V4 singleton-architecture concentrated-liquidity pools.

    Owns the full I/O choreography: DB lookup → RPC fetch → decode →
    construct pool → register.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        assert ctx.managed_pools is not None, (
            "V4PoolBuilder requires managed_pools in BuilderContext"
        )
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._managed_pools = ctx.managed_pools
        self._erc20_builder = ctx.erc20_builder

    def _make_tick_data_fetcher(
        self,
        pool_id: HexBytes,
        pool_manager_address: str,
        state_view_address: str,
        chain_id: int,
        io: PoolIO,
    ) -> Callable[[int, int], None]:
        """Create a tick data fetcher callback for a V4 pool."""
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
        io: PoolIO,
        request: BuildPoolRequest,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV4Pool."""

        assert request.pool_id is not None
        pool_manager_address = get_checksum_address(address)
        pool_id = request.pool_id
        pool_id_bytes = HexBytes(pool_id)
        chain_id = chain_id or self._default_chain_id
        assert chain_id is not None, "chain_id must be provided or set as default_chain_id"

        state_block = (
            request.state_block
            if request.state_block is not None
            else io.get_block_number()
        )

        # Try DB first
        pool_from_db = None
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

        # Get immutable values
        if pool_from_db is not None:
            db_values = V4BuilderBase.extract_db_values(pool_from_db)
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

        # Build tokens
        token0 = self._erc20_builder.build(
            currency0_address, chain_id=chain_id, silent=request.silent, io=io
        )
        token1 = self._erc20_builder.build(
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
            slot0_result = io.call(
                to=state_view_address,
                data=slot0_calldata,
                block=state_block,
            )
            liquidity_result = io.call(
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
            if pool_from_db is not None and hasattr(pool_from_db, "liquidity_positions"):
                with contextlib.suppress(Exception), self._db() as session:
                    if hasattr(pool_from_db, "managed_pool_id"):
                        pool_with_data = session.scalar(
                            select(type(pool_from_db)).where(
                                UniswapV4PoolTable.id == pool_from_db.id
                            )
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
                    data=io.call(
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
                        result = io.call(
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
        tick_bitmap_arg, tick_data_arg = V4BuilderBase.resolve_tick_data_args(
            working_tick_data=working_tick_data,
            working_tick_bitmap=working_tick_bitmap,
        )

        assert state_view_address is not None
        pool = UniswapV4Pool(
            pool_id=pool_id_bytes,
            pool_manager_address=pool_manager_address,
            token0=token0,
            token1=token1,
            fee=fee_for_pool,
            tick_spacing=tick_spacing_for_pool,
            hook_address=hook_address,
            state_view_address=state_view_address,
            sqrt_price_x96=slot0_data.sqrt_price_x96,
            tick=slot0_data.tick,
            liquidity=int(liquidity_val),
            protocol_fee_zero_for_one=slot0_data.protocol_fee_zero_to_one,
            protocol_fee_one_for_zero=slot0_data.protocol_fee_one_to_zero,
            lp_fee=slot0_data.lp_fee,
            state_block=state_block,
            tick_bitmap=cast(
                "dict[BitmapWord, dict[str, Any] | BitmapAtWord] | None",
                tick_bitmap_arg,
            ),
            tick_data=cast(
                "dict[Tick, dict[str, Any] | LiquidityAtTick] | None",
                tick_data_arg,
            ),
            tick_data_fetcher=self._make_tick_data_fetcher(
                pool_id_bytes, pool_manager_address, state_view_address, chain_id, io=io
            ),
            state_cache_depth=request.state_cache_depth,
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

    def update(  # noqa: PLR6301
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
        io: PoolIO | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool."""
        if not isinstance(pool, UniswapV4Pool):
            msg = f"V4PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert io is not None
        assert pool.chain_id is not None
        raw_block = block_number if block_number is not None else io.get_block_number()
        block_number_ = int(raw_block) if not isinstance(raw_block, int) else raw_block

        slot0_calldata = encode_function_calldata("getSlot0(bytes32)", [pool.pool_id])
        slot0_result = io.call(
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
                data=io.call(
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
