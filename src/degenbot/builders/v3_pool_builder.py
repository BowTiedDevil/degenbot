from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
from sqlalchemy import select

from degenbot.builders.tick_data_fetcher import TickDataTypes, make_tick_data_fetcher
from degenbot.builders.v3_builder_base import V3BuilderBase
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.pools import LiquidityPoolTable, UniswapV3PoolTableBase
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.v3_types import (
    UniswapV3PoolExternalUpdate,
)
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

if TYPE_CHECKING:
    from collections.abc import Callable

    from web3.types import BlockIdentifier

    from degenbot.builders.context import BuilderContext
    from degenbot.builders.pool_io import PoolIO
    from degenbot.builders.request import BuildRequest
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


class V3PoolBuilder(V3BuilderBase):
    """
    Builds and updates V3-style concentrated-liquidity pools.

    Owns the full I/O choreography: DB lookup → RPC fetch → decode →
    construct pool → register.
    """

    def __init__(self, ctx: BuilderContext) -> None:
        assert ctx.managed_pools is not None, (
            "V3PoolBuilder requires managed_pools in BuilderContext"
        )
        self._default_chain_id = ctx.default_chain_id
        self._db = ctx.db
        self._pools = ctx.pools
        self._tokens = ctx.tokens
        self._managed_pools = ctx.managed_pools
        self._erc20_builder = ctx.erc20_builder

    def _make_tick_data_fetcher(
        self, pool_address: str, chain_id: int, io: PoolIO
    ) -> Callable[[int, int], None]:
        """Create a tick data fetcher callback for a V3 pool."""
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
        """Fetch pool data from DB/RPC and construct an I/O-free V3-style pool."""

        pool_address = get_checksum_address(address)
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
            pool_from_db = session.scalar(
                select(LiquidityPoolTable).where(
                    LiquidityPoolTable.address == pool_address,
                    LiquidityPoolTable.chain == chain_id,
                )
            )

        # Get immutable values
        if pool_from_db is not None:
            if not isinstance(pool_from_db, UniswapV3PoolTableBase):
                msg = f"Expected UniswapV3PoolTableBase, got {type(pool_from_db).__name__}"
                raise DegenbotValueError(message=msg)

            db_values = V3BuilderBase.extract_db_values(pool_from_db)
            factory = db_values.factory
            token0_address = db_values.token0_address
            token1_address = db_values.token1_address
            fee = db_values.fee
            tick_spacing_for_pool = db_values.tick_spacing
            db_deployer = db_values.deployer_address
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
            token0_address, chain_id=chain_id, silent=request.silent, io=io
        )
        token1 = self._erc20_builder.build(
            token1_address, chain_id=chain_id, silent=request.silent, io=io
        )

        # Fetch slot0 + liquidity
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
        (liquidity,) = eth_abi.abi.decode(types=["uint128"], data=liquidity_result)

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
            # Try DB snapshot tables first
            if pool_from_db is not None and hasattr(pool_from_db, "liquidity_positions"):
                with contextlib.suppress(Exception), self._db() as session:
                    if hasattr(pool_from_db, "pool_id"):
                        pool_with_data = session.scalar(
                            select(type(pool_from_db)).where(
                                LiquidityPoolTable.id == pool_from_db.id
                            )
                        )
                        if pool_with_data is not None and isinstance(
                            pool_with_data, UniswapV3PoolTableBase
                        ):
                            working_tick_bitmap, working_tick_data, db_snapshot_loaded = (
                                V3BuilderBase.load_tick_snapshot(pool_with_data)
                            )

            if not db_snapshot_loaded:
                word, _ = get_tick_word_and_bit_position(
                    tick=int(tick), tick_spacing=tick_spacing_for_pool
                )

                (bitmap_at_word,) = eth_abi.abi.decode(
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
                        result = io.call(
                            to=pool_address,
                            data=encode_function_calldata("ticks(int24)", [active_tick]),
                            block=state_block,
                        )
                        liquidity_gross, liquidity_net, *_ = eth_abi.abi.decode(
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

        # Determine deployer and init_hash from DB (if available) or pool type registry
        deployer = factory
        init_hash = UniswapV3Pool.UNISWAP_V3_MAINNET_POOL_INIT_HASH
        db_deployer = locals().get("db_deployer")  # Set if pool was found in DB
        if db_deployer is not None:
            deployer = get_checksum_address(db_deployer)
        else:
            registry_deployment = pool_type_registry.get_deployment(chain_id, factory)
            if registry_deployment is not None:
                if registry_deployment.pool_init_hash is not None:
                    init_hash = registry_deployment.pool_init_hash
                if registry_deployment.deployer is not None:
                    deployer = get_checksum_address(registry_deployment.deployer)

        # Only pass tick data if we have a complete DB snapshot.
        tick_bitmap_arg, tick_data_arg = V3BuilderBase.resolve_tick_data_args(
            db_snapshot_loaded=db_snapshot_loaded,
            working_tick_bitmap=working_tick_bitmap,
            working_tick_data=working_tick_data,
        )

        # Map factory addresses to pool classes for V3 variants
        pool_class = pool_type_registry.get_v3_class(chain_id, factory)

        if pool_class is None:
            msg = f"No V3 pool class registered for chain {chain_id}, factory {factory}"
            raise ValueError(msg)

        pool = pool_class(
            address=pool_address,
            chain_id=chain_id,
            token0=token0,
            token1=token1,
            factory=factory,
            fee=fee,
            tick_spacing=tick_spacing_for_pool,
            sqrt_price_x96=int(sqrt_price_x96),
            tick=int(tick),
            liquidity=int(liquidity),
            state_block=state_block,
            tick_bitmap=tick_bitmap_arg,
            tick_data=tick_data_arg,
            deployer_address=deployer,
            init_hash=init_hash,
            tick_data_fetcher=self._make_tick_data_fetcher(pool_address, chain_id, io=io),
            state_cache_depth=request.state_cache_depth,
        )

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
        """Fetch current state from chain and push update to the pool."""

        if isinstance(pool, UniswapV4Pool):
            msg = f"V3PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)
        if not isinstance(pool, UniswapV3Pool):
            msg = f"V3PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert io is not None
        assert pool.chain_id is not None
        raw_block = block_number if block_number is not None else io.get_block_number()
        block_number_ = int(raw_block) if not isinstance(raw_block, int) else raw_block

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
            eth_abi.abi.decode(
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
