from __future__ import annotations

import contextlib
from typing import TYPE_CHECKING, Any, cast

import eth_abi.abi
from hexbytes import HexBytes
from sqlalchemy import select

from degenbot.builders.erc20_builder import Erc20Builder
from degenbot.builders.tick_data_fetcher import TickDataTypes, make_tick_data_fetcher
from degenbot.checksum_cache import get_checksum_address
from degenbot.connection.connection_manager import ConnectionManager
from degenbot.constants import ZERO_ADDRESS as _ZERO_ADDRESS
from degenbot.database.models.pools import PoolManagerTable, UniswapV4PoolTable
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.exceptions.base import DegenbotValueError
from degenbot.exceptions.liquidity_pool import LiquidityPoolError
from degenbot.functions import encode_function_calldata, raw_call
from degenbot.logging import logger
from degenbot.registry import ManagedPoolRegistry, PoolRegistry, TokenRegistry
from degenbot.types.aliases import ChainId
from degenbot.uniswap.v3_functions import get_tick_word_and_bit_position
from degenbot.uniswap.v4_liquidity_pool import UniswapV4Pool
from degenbot.uniswap.v4_types import (
    UniswapV4BitmapAtWord,
    UniswapV4LiquidityAtTick,
    UniswapV4PoolExternalUpdate,
)

if TYPE_CHECKING:
    from collections.abc import Sequence

    from web3.types import BlockIdentifier


class V4PoolBuilder:
    """
    Builds and updates V4 singleton-architecture concentrated-liquidity pools.

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
        self, pool_id: HexBytes, pool_manager_address: str, state_view_address: str, chain_id: int
    ) -> Any:
        """Create a tick data fetcher callback for a V4 pool."""
        return make_tick_data_fetcher(
            pool_lookup=lambda _: self._managed_pools.get(
                chain_id=chain_id,
                pool_manager_address=pool_manager_address,
                pool_id=pool_id,
            ),
            provider_lookup=lambda: self._connections.get_provider(chain_id),
            types=TickDataTypes(
                bitmap_at_word=UniswapV4BitmapAtWord,
                liquidity_at_tick=UniswapV4LiquidityAtTick,
                tick_struct_types=("uint128", "int128"),
            ),
            state_view_address=state_view_address,
            pool_id=bytes(pool_id),
        )

    def build(
        self,
        *,
        pool_id: str | bytes,
        pool_manager_address: str,
        state_view_address: str | None = None,
        tokens: Sequence[str] | None = None,
        fee: int | None = None,
        tick_spacing: int | None = None,
        hook_address: str | None = None,
        chain_id: ChainId | None = None,
        state_block: int | None = None,
        tick_bitmap: dict[int, UniswapV4BitmapAtWord] | None = None,
        tick_data: dict[int, UniswapV4LiquidityAtTick] | None = None,
        silent: bool = False,
    ) -> UniswapV4Pool:
        """Fetch pool data from DB/RPC and construct an I/O-free UniswapV4Pool."""

        pool_manager_address = get_checksum_address(pool_manager_address)
        pool_id_bytes = HexBytes(pool_id)
        chain_id = chain_id or self._connections.default_chain_id
        provider = self._connections.get_provider(chain_id)

        state_block = state_block if state_block is not None else provider.get_block_number()

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
            currency0_address = pool_from_db.currency0.address
            currency1_address = pool_from_db.currency1.address
            hook_address = get_checksum_address(pool_from_db.hooks)
            tick_spacing_for_pool = pool_from_db.tick_spacing
            fee_for_pool = pool_from_db.fee_currency0
            state_view_address = pool_from_db.manager.state_view
        else:
            if state_view_address is None:
                raise DegenbotValueError(
                    message="A state view contract address must be provided for a pool not in the database."  # noqa: E501
                )
            if fee is None:
                raise DegenbotValueError(
                    message="A fee must be provided for a pool not in the database."
                )
            if tick_spacing is None:
                raise DegenbotValueError(
                    message="A tick spacing must be provided for a pool not in the database."
                )
            if tokens is None:
                raise DegenbotValueError(
                    message="Token addresses must be provided for a pool not in the database."
                )

            state_view_address = get_checksum_address(state_view_address)
            currency0_address, currency1_address = sorted(
                [get_checksum_address(t) for t in tokens],
                key=lambda t: t.lower(),
            )
            hook_address = (
                get_checksum_address(hook_address) if hook_address is not None else _ZERO_ADDRESS
            )
            fee_for_pool = fee
            tick_spacing_for_pool = tick_spacing

        # Build tokens
        token0 = self._erc20_builder.build(currency0_address, chain_id=chain_id, silent=silent)
        token1 = self._erc20_builder.build(currency1_address, chain_id=chain_id, silent=silent)

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

            slot0_result = provider.call(
                to=state_view_address,
                data=slot0_calldata,
                block=state_block,
            )
            liquidity_result = provider.call(
                to=state_view_address,
                data=liquidity_calldata,
                block=state_block,
            )
        except Exception as exc:
            raise LiquidityPoolError(message="Could not decode contract data") from exc

        price, tick_val, protocol_fee_val, lp_fee_val = eth_abi.abi.decode(
            types=["uint160", "int24", "uint24", "uint24"],
            data=slot0_result,
        )
        (liquidity_val,) = eth_abi.abi.decode(
            types=["uint256"],
            data=liquidity_result,
        )

        # Extract two fees (uint12) from packed uint24
        protocol_fee_one_to_zero = protocol_fee_val >> 12
        protocol_fee_zero_to_one = protocol_fee_val & 0xFFF

        # Fetch initial tick bitmap and tick data
        working_tick_bitmap: dict[int, Any] = {}
        working_tick_data: dict[int, Any] = {}

        # Use provided tick data if given (snapshot or test fixtures)
        if tick_bitmap is not None and tick_data is not None:  # noqa:PLR1702
            working_tick_bitmap = dict(tick_bitmap)
            working_tick_data = dict(tick_data)
        elif tick_bitmap is not None or tick_data is not None:
            raise DegenbotValueError(message="Provide both tick_bitmap and tick_data, or neither.")
        else:
            # Try DB snapshot tables first
            db_snapshot_loaded = False
            if pool_from_db is not None and hasattr(pool_from_db, "liquidity_positions"):
                with contextlib.suppress(Exception), self._db() as session:
                    if hasattr(pool_from_db, "managed_pool_id"):
                        pool_with_data = session.scalar(
                            select(type(pool_from_db)).where(  # type: ignore[arg-type]
                                UniswapV4PoolTable.id == pool_from_db.id
                            )
                        )
                        if pool_with_data is not None:
                            init_maps = pool_with_data.initialization_maps
                            liq_positions = pool_with_data.liquidity_positions
                            if init_maps and liq_positions:
                                for init_map in init_maps:
                                    working_tick_bitmap[int(init_map.word)] = UniswapV4BitmapAtWord(
                                        bitmap=int(init_map.bitmap),
                                        block=pool_with_data.liquidity_update_block or 0,
                                    )
                                for pos in liq_positions:
                                    working_tick_data[int(pos.tick)] = UniswapV4LiquidityAtTick(
                                        liquidity_net=int(pos.liquidity_net),
                                        liquidity_gross=int(pos.liquidity_gross),
                                        block=pool_with_data.liquidity_update_block or 0,
                                    )
                                db_snapshot_loaded = True

            if not db_snapshot_loaded:
                word, _ = get_tick_word_and_bit_position(
                    tick=int(tick_val), tick_spacing=tick_spacing_for_pool
                )

                (bitmap_at_word,) = raw_call(
                    provider,
                    address=state_view_address,
                    calldata=encode_function_calldata(
                        "getTickBitmap(bytes32,int16)",
                        [pool_id_bytes, word],
                    ),
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
                        working_tick_data[active_tick] = UniswapV4LiquidityAtTick(
                            liquidity_net=int(liquidity_net),
                            liquidity_gross=int(liquidity_gross),
                            block=state_block,
                        )

                working_tick_bitmap[word] = UniswapV4BitmapAtWord(
                    bitmap=bitmap_at_word,
                    block=state_block,
                )

        # If tick data was populated, pass both. Otherwise pass None (sparse mode).
        tick_bitmap_arg = working_tick_bitmap if working_tick_data else None
        tick_data_arg = working_tick_data or None

        pool = UniswapV4Pool(
            pool_id=pool_id_bytes,
            pool_manager_address=pool_manager_address,
            token0=token0,
            token1=token1,
            fee=fee_for_pool,
            tick_spacing=tick_spacing_for_pool,
            hook_address=hook_address,
            state_view_address=state_view_address,
            sqrt_price_x96=int(price),
            tick=int(tick_val),
            liquidity=int(liquidity_val),
            protocol_fee_zero_for_one=protocol_fee_zero_to_one,
            protocol_fee_one_for_zero=protocol_fee_one_to_zero,
            lp_fee=int(lp_fee_val),
            state_block=state_block,
            tick_bitmap=tick_bitmap_arg,
            tick_data=tick_data_arg,
            tick_data_fetcher=self._make_tick_data_fetcher(
                pool_id_bytes, pool_manager_address, state_view_address, chain_id
            ),
        )

        # Register pool in managed pool registry
        self._managed_pools.add(
            pool=pool,
            chain_id=chain_id,
            pool_manager_address=pool.address,
            pool_id=pool.pool_id,
        )

        if not silent:
            logger.info(pool.name)
            logger.info(f"• ID: {pool.pool_id.to_0x_hex()}")
            logger.info(f"• Token 0: {token0}")
            logger.info(f"• Token 1: {token1}")
            logger.info(f"• Liquidity: {pool.liquidity}")
            logger.info(f"• SqrtPrice: {pool.sqrt_price_x96}")
            logger.info(f"• Tick: {pool.tick}")

        return pool

    def update(
        self,
        pool: Any,
        *,
        block_number: BlockIdentifier | None = None,
    ) -> bool:
        """Fetch current state from chain and push update to the pool."""
        if not isinstance(pool, UniswapV4Pool):
            msg = f"V4PoolBuilder cannot update {type(pool).__name__}"
            raise TypeError(msg)

        provider = self._connections.get_provider(pool.chain_id)
        _block_number = block_number if block_number is not None else provider.get_block_number()

        slot0_calldata = encode_function_calldata("getSlot0(bytes32)", [pool.pool_id])
        slot0_result = provider.call(
            to=pool._state_view_address,  # noqa: SLF001
            data=slot0_calldata,
            block=_block_number,
        )
        price, tick, protocol_fee, lp_fee = cast(
            "tuple[int, ...]",
            eth_abi.abi.decode(types=["uint160", "int24", "uint24", "uint24"], data=slot0_result),
        )

        liquidity_calldata = encode_function_calldata("getLiquidity(bytes32)", [pool.pool_id])
        (liquidity_val,) = cast(
            "tuple[int]",
            eth_abi.abi.decode(
                types=["uint256"],
                data=provider.call(
                    to=pool._state_view_address,  # noqa: SLF001
                    data=liquidity_calldata,
                    block=_block_number,
                ),
            ),
        )

        if pool.sqrt_price_x96 == price and pool.liquidity == liquidity_val and pool.tick == tick:
            return False

        update = UniswapV4PoolExternalUpdate(
            block_number=_block_number,
            sqrt_price_x96=price,
            tick=tick,
            liquidity=liquidity_val,
        )
        pool.external_update(update)
        return True
