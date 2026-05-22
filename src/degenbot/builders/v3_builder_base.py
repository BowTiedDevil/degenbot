"""V3 builder base class and shared data types for pure-logic helpers."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

import eth_abi.abi

from degenbot.checksum_cache import get_checksum_address
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress
    from hexbytes import HexBytes

    from degenbot.database.models.pools import UniswapV3PoolTableBase


@dataclass(frozen=True)
class V3ImmutableData:
    """
    Decoded immutable V3 pool data from RPC calls.

    Produced by V3BuilderBase.decode_immutable_data().
    Consumed by V3 builder build() methods.
    """

    factory: ChecksumAddress
    token0_address: ChecksumAddress
    token1_address: ChecksumAddress
    fee: int
    tick_spacing: int


@dataclass(frozen=True)
class V3Slot0Data:
    """
    Decoded V3 slot0 data.

    Produced by V3BuilderBase.decode_slot0().
    Shared by both build() and update().
    """

    sqrt_price_x96: int
    tick: int


@dataclass(frozen=True)
class V3DbValues:
    """
    Immutable values extracted from a V3 DB row.

    Produced by V3BuilderBase.extract_db_values().
    """

    factory: ChecksumAddress
    token0_address: ChecksumAddress
    token1_address: ChecksumAddress
    fee: int
    tick_spacing: int
    deployer_address: str | None


class V3BuilderBase:
    """
    Shared pure-logic helpers for V3 pool builders.

    Both V3PoolBuilder (sync) and AsyncV3PoolBuilder (async) delegate
    decode/extract/snapshot steps to these helpers. Only the I/O
    steps differ between sync and async.
    """

    @staticmethod
    def decode_immutable_data(
        *,
        factory_result: HexBytes,
        token0_result: HexBytes,
        token1_result: HexBytes,
        fee_result: HexBytes,
        tick_spacing_result: HexBytes,
    ) -> V3ImmutableData:
        """Decode raw call results into typed immutable V3 pool data."""
        (factory_raw,) = eth_abi.abi.decode(types=["address"], data=factory_result)
        (token0_raw,) = eth_abi.abi.decode(types=["address"], data=token0_result)
        (token1_raw,) = eth_abi.abi.decode(types=["address"], data=token1_result)
        (fee,) = eth_abi.abi.decode(types=["uint24"], data=fee_result)
        (tick_spacing,) = eth_abi.abi.decode(types=["int24"], data=tick_spacing_result)

        return V3ImmutableData(
            factory=get_checksum_address(factory_raw),
            token0_address=get_checksum_address(token0_raw),
            token1_address=get_checksum_address(token1_raw),
            fee=int(fee),
            tick_spacing=int(tick_spacing),
        )

    @staticmethod
    def decode_slot0(slot0_result: HexBytes) -> V3Slot0Data:
        """
        Decode sqrt_price_x96 and tick from a V3 slot0() call result.

        V3 slot0() returns [uint160 sqrtPriceX96, int24 tick, uint16, uint16,
        uint16, uint8, bool]. Only the first two fields are needed.
        """
        sqrt_price_x96, tick, *_ = eth_abi.abi.decode(
            types=["uint160", "int24", "uint16", "uint16", "uint16", "uint8", "bool"],
            data=slot0_result,
        )
        return V3Slot0Data(
            sqrt_price_x96=int(sqrt_price_x96),
            tick=int(tick),
        )

    @staticmethod
    def extract_db_values(pool_from_db: UniswapV3PoolTableBase) -> V3DbValues:
        """Extract factory, token addresses, fee, tick_spacing, deployer from a DB row."""
        return V3DbValues(
            factory=get_checksum_address(pool_from_db.exchange.factory),
            token0_address=get_checksum_address(pool_from_db.token0.address),
            token1_address=get_checksum_address(pool_from_db.token1.address),
            fee=pool_from_db.fee_token0,
            tick_spacing=pool_from_db.tick_spacing,
            deployer_address=pool_from_db.exchange.deployer
            if pool_from_db.exchange.deployer is not None
            else None,
        )

    @staticmethod
    def load_tick_snapshot(
        pool_with_data: UniswapV3PoolTableBase,
    ) -> tuple[dict[int, BitmapAtWord], dict[int, LiquidityAtTick], bool]:
        """
        Load tick bitmap and tick data from a re-queried DB row with active relationships.

        The caller is responsible for re-querying the pool row within an
        active SQLAlchemy session so that lazy-loaded relationships
        (initialization_maps, liquidity_positions) are accessible.

        Returns (working_tick_bitmap, working_tick_data, db_snapshot_loaded).
        If the snapshot cannot be loaded, returns ({}, {}, False).
        """
        init_maps = pool_with_data.initialization_maps
        liq_positions = pool_with_data.liquidity_positions

        if not init_maps or not liq_positions:
            return {}, {}, False

        update_block = pool_with_data.liquidity_update_block or 0

        working_tick_bitmap: dict[int, BitmapAtWord] = {}
        working_tick_data: dict[int, LiquidityAtTick] = {}

        for init_map in init_maps:
            working_tick_bitmap[int(init_map.word)] = BitmapAtWord(
                bitmap=int(init_map.bitmap),
                block=update_block,
            )
        for pos in liq_positions:
            working_tick_data[int(pos.tick)] = LiquidityAtTick(
                liquidity_net=int(pos.liquidity_net),
                liquidity_gross=int(pos.liquidity_gross),
                block=update_block,
            )

        return working_tick_bitmap, working_tick_data, True

    @staticmethod
    def resolve_tick_data_args(
        *,
        db_snapshot_loaded: bool,
        working_tick_bitmap: dict[int, BitmapAtWord],
        working_tick_data: dict[int, LiquidityAtTick],
    ) -> tuple[dict[int, BitmapAtWord] | None, dict[int, LiquidityAtTick] | None]:
        """
        Decide whether to pass tick data to the pool constructor.

        Returns (tick_bitmap_arg, tick_data_arg). Only passes data when
        we have a complete DB snapshot with actual tick data.
        """
        if db_snapshot_loaded and working_tick_data:
            return working_tick_bitmap, working_tick_data
        return None, None
