from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
from sqlalchemy import select

from degenbot.builders.tick_data_fetcher import TickDataTypes, make_tick_data_fetcher
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.pools import LiquidityPoolTable, UniswapV3PoolTableBase
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.pool import LiquidityPoolError
from degenbot.logging import logger
from degenbot.provider.call_helpers import encode_function_calldata, raw_call
from degenbot.registry.pool_type import pool_type_registry
from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
from degenbot.uniswap.v3_liquidity_pool import UniswapV3Pool
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_types import (
    UniswapV3PoolExternalUpdate,
)
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool

if TYPE_CHECKING:
    from collections.abc import Callable

    from web3.types import BlockIdentifier

    from degenbot.builders.erc20_builder import Erc20Builder
    from degenbot.connection.connection_manager import ConnectionManager
    from degenbot.database.session_manager import DatabaseSessionManager
    from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
    from degenbot.types.abstract.liquidity_pool import AbstractLiquidityPool
    from degenbot.types.aliases import ChainId


class V3PoolBuilder:
    """
    Builds and updates V3-style concentrated-liquidity pools.

    Owns the full I/O choreography: DB lookup → RPC fetch → decode →
    construct pool → register.
    """

    def __init__(
        self,
        *,
        connections: ConnectionManager,
        db: DatabaseSessionManager,
        pools: PoolRegistry,
        tokens: TokenRegistry,
        managed_pools: ManagedPoolRegistry,
        erc20_builder: Erc20Builder,
    ) -> None:
        self._connections = connections
        self._db = db
        self._pools = pools
        self._tokens = tokens
        self._managed_pools = managed_pools
        self._erc20_builder = erc20_builder

    def _make_tick_data_fetcher(
        self, pool_address: str, chain_id: int
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
            provider_lookup=lambda: self._connections.get_provider(chain_id),
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
        deployer_address: str | None = None,
        init_hash: str | None = None,
        state_block: int | None = None,
        tick_bitmap: dict[int, BitmapAtWord] | None = None,
        tick_data: dict[int, LiquidityAtTick] | None = None,
        silent: bool = False,
        state_cache_depth: int = 8,
        **kwargs: Any,
    ) -> AbstractLiquidityPool:
        """Fetch pool data from DB/RPC and construct an I/O-free V3-style pool."""

        pool_address = get_checksum_address(address)
        chain_id = chain_id or self._connections.default_chain_id
        provider = self._connections.get_provider(chain_id)

        state_block = state_block if state_block is not None else provider.get_block_number()

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
            factory = get_checksum_address(pool_from_db.exchange.factory)
            token0_address = get_checksum_address(pool_from_db.token0.address)
            token1_address = get_checksum_address(pool_from_db.token1.address)
            if isinstance(pool_from_db, UniswapV3PoolTableBase):
                fee = pool_from_db.fee_token0
                tick_spacing_for_pool = pool_from_db.tick_spacing
            else:
                msg = f"Expected UniswapV3PoolTableBase, got {type(pool_from_db).__name__}"
                raise DegenbotValueError(message=msg)

            if pool_from_db.exchange.deployer is not None:
                deployer_address = pool_from_db.exchange.deployer
        else:
            try:
                factory_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("factory()", None),
                )
                token0_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("token0()", None),
                )
                token1_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("token1()", None),
                )
                fee_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("fee()", None),
                )
                tick_spacing_result = provider.call(
                    to=pool_address,
                    data=encode_function_calldata("tickSpacing()", None),
                )
            except Exception as exc:
                raise LiquidityPoolError(message="Could not decode contract data") from exc

            (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
            (token0_raw,) = eth_abi.abi.decode(types=["address"], data=token0_result)
            (token1_raw,) = eth_abi.abi.decode(types=["address"], data=token1_result)
            (fee,) = eth_abi.abi.decode(types=["uint24"], data=fee_result)
            (tick_spacing_for_pool,) = eth_abi.abi.decode(types=["int24"], data=tick_spacing_result)

            factory = get_checksum_address(factory_raw)
            token0_address = get_checksum_address(token0_raw)
            token1_address = get_checksum_address(token1_raw)
            fee = int(fee)
            tick_spacing_for_pool = int(tick_spacing_for_pool)

        # Build tokens
        token0 = self._erc20_builder.build(token0_address, chain_id=chain_id, silent=silent)
        token1 = self._erc20_builder.build(token1_address, chain_id=chain_id, silent=silent)

        # Fetch slot0 + liquidity
        try:
            slot0_result = provider.call(
                to=pool_address,
                data=encode_function_calldata("slot0()", None),
                block=state_block,
            )
            liquidity_result = provider.call(
                to=pool_address,
                data=encode_function_calldata("liquidity()", None),
                block=state_block,
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        sqrt_price_x96, tick, *_ = eth_abi.abi.decode(
            types=["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
            data=slot0_result,
        )
        (liquidity,) = eth_abi.abi.decode(types=["uint128"], data=liquidity_result)

        # Fetch initial tick bitmap and tick data
        db_snapshot_loaded = False
        working_tick_bitmap: dict[int, Any] = {}
        working_tick_data: dict[int, Any] = {}

        # Use provided tick data if given (snapshot or test fixtures)
        if tick_bitmap is not None and tick_data is not None:  # noqa:PLR1702
            working_tick_bitmap = dict(tick_bitmap)
            working_tick_data = dict(tick_data)
            db_snapshot_loaded = True
        elif tick_bitmap is not None or tick_data is not None:
            raise DegenbotValueError(message="Provide both tick_bitmap and tick_data, or neither.")
        else:
            # Try DB snapshot tables first
            db_snapshot_loaded = False
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
                            init_maps = pool_with_data.initialization_maps
                            liq_positions = pool_with_data.liquidity_positions
                            if init_maps and liq_positions:
                                for init_map in init_maps:
                                    working_tick_bitmap[int(init_map.word)] = BitmapAtWord(
                                        bitmap=int(init_map.bitmap),
                                        block=pool_with_data.liquidity_update_block or 0,
                                    )
                                for pos in liq_positions:
                                    working_tick_data[int(pos.tick)] = LiquidityAtTick(
                                        liquidity_net=int(pos.liquidity_net),
                                        liquidity_gross=int(pos.liquidity_gross),
                                        block=pool_with_data.liquidity_update_block or 0,
                                    )
                                db_snapshot_loaded = True

            if not db_snapshot_loaded:
                word, _ = get_tick_word_and_bit_position(
                    tick=int(tick), tick_spacing=tick_spacing_for_pool
                )

                (bitmap_at_word,) = raw_call(
                    provider,
                    address=pool_address,
                    calldata=encode_function_calldata("tickBitmap(int16)", [word]),
                    return_types=["uint256"],
                    block_identifier=state_block,
                )

                if bitmap_at_word != 0:
                    active_ticks = [
                        ((word << 8) + i) * tick_spacing_for_pool
                        for i in range(256)
                        if bitmap_at_word & (1 << i) > 0
                    ]

                    for active_tick in active_ticks:
                        result = provider.call(
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

        # Determine deployer and init_hash
        deployer = factory
        init_hash = UniswapV3Pool.UNISWAP_V3_MAINNET_POOL_INIT_HASH
        registry_deployment = pool_type_registry.get_deployment(chain_id, factory)
        if registry_deployment is not None:
            if registry_deployment.pool_init_hash is not None:
                init_hash = registry_deployment.pool_init_hash
            if registry_deployment.deployer is not None:
                deployer = get_checksum_address(registry_deployment.deployer)

        deployer = get_checksum_address(deployer_address) if deployer_address else deployer
        init_hash = init_hash or init_hash

        # Only pass tick data if we have a complete DB snapshot.
        if db_snapshot_loaded and working_tick_data:
            tick_bitmap_arg = working_tick_bitmap
            tick_data_arg = working_tick_data
        else:
            tick_bitmap_arg = None
            tick_data_arg = None

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
            tick_data_fetcher=self._make_tick_data_fetcher(pool_address, chain_id),
            state_cache_depth=state_cache_depth,
        )

        # Register pool
        self._pools.add(
            pool=pool,
            chain_id=chain_id,
            pool_address=pool.address,
        )

        if not silent:
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

    def update(
        self,
        pool: AbstractLiquidityPool,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool."""

        if isinstance(pool, UniswapV4Pool):
            msg = f"V3PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)
        if not isinstance(pool, UniswapV3Pool):
            msg = f"V3PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        assert pool.chain_id is not None
        provider = self._connections.get_provider(pool.chain_id)
        raw_block = block_number if block_number is not None else provider.get_block_number()
        block_number_ = int(raw_block) if not isinstance(raw_block, int) else raw_block

        slot0_result = provider.call(
            to=pool.address,
            data=encode_function_calldata("slot0()", None),
            block=block_number_,
        )
        sqrt_price_x96, tick, *_ = cast(
            "tuple[int, ...]",
            eth_abi.abi.decode(
                types=["uint160", "int24", "uint16", "uint16", "uint16"], data=slot0_result
            ),
        )

        (liquidity,) = cast(
            "tuple[int]",
            eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call(
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
