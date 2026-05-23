"""V4 builder base class and shared data types for pure-logic helpers."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

import eth_abi.abi

from degenbot.checksum_cache import get_checksum_address
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress
    from hexbytes import HexBytes

    from degenbot.database.models.pools import UniswapV4PoolTableBase
    from degenbot.uniswap.v3_types import BitmapWord, Tick


@dataclass(frozen=True)
class V4Slot0Data:
    """Decoded V4 slot0 data.

    Produced by V4BuilderBase.decode_slot0().
    Shared by both build() and update().
    """

    sqrt_price_x96: int
    tick: int
    protocol_fee_one_to_zero: int
    protocol_fee_zero_to_one: int
    lp_fee: int


@dataclass(frozen=True)
class V4DbValues:
    """Immutable values extracted from a V4 DB row.

    Produced by V4BuilderBase.extract_db_values().
    """

    currency0_address: ChecksumAddress
    currency1_address: ChecksumAddress
    hook_address: ChecksumAddress
    tick_spacing: int
    fee: int
    state_view_address: str | None


class V4BuilderBase:
    """Shared pure-logic helpers for V4 pool builders.

    Both V4PoolBuilder (sync) and AsyncV4PoolBuilder (async) delegate
    decode/extract/snapshot steps to these helpers. Only the I/O
    steps differ between sync and async.
    """

    @staticmethod
    def decode_slot0(slot0_result: HexBytes) -> V4Slot0Data:
        """Decode sqrt_price, tick, protocol fees, and lp_fee from V4 getSlot0(bytes32).

        V4 getSlot0 returns [uint160 sqrtPriceX96, int24 tick, uint24 protocolFee,
        uint24 lpFee]. The protocol fee uint24 packs two uint12 values:
        bits 12-23 = fee_one_to_zero, bits 0-11 = fee_zero_to_one.

        Returns:
            The computed value.

        """
        price, tick, protocol_fee, lp_fee = eth_abi.abi.decode(
            types=["uint160", "int24", "uint24", "uint24"],
            data=slot0_result,
        )
        return V4Slot0Data(
            sqrt_price_x96=int(price),
            tick=int(tick),
            protocol_fee_one_to_zero=protocol_fee >> 12,
            protocol_fee_zero_to_one=protocol_fee & 0xFFF,
            lp_fee=int(lp_fee),
        )

    @staticmethod
    def extract_db_values(pool_from_db: UniswapV4PoolTableBase) -> V4DbValues:
        """Extract currency addresses, hook, tick spacing, fee, state_view from a V4 DB row.

        Returns:
            The computed value.

        """
        return V4DbValues(
            currency0_address=get_checksum_address(pool_from_db.currency0.address),
            currency1_address=get_checksum_address(pool_from_db.currency1.address),
            hook_address=get_checksum_address(pool_from_db.hooks),
            tick_spacing=pool_from_db.tick_spacing,
            fee=pool_from_db.fee_currency0,
            state_view_address=pool_from_db.manager.state_view,
        )

    @staticmethod
    def load_tick_snapshot(
        pool_with_data: UniswapV4PoolTableBase,
    ) -> tuple[dict[BitmapWord, BitmapAtWord], dict[Tick, LiquidityAtTick], bool]:
        """Load tick bitmap and tick data from a re-queried V4 DB row with active relationships.

        The caller is responsible for re-querying the pool row within an
        active SQLAlchemy session so that lazy-loaded relationships
        (initialization_maps, liquidity_positions) are accessible.

        Returns (working_tick_bitmap, working_tick_data, db_snapshot_loaded).
        If the snapshot cannot be loaded, returns ({}, {}, False).

        Returns:
            The computed value.

        """
        init_maps = pool_with_data.initialization_maps
        liq_positions = pool_with_data.liquidity_positions

        if not init_maps or not liq_positions:
            return {}, {}, False

        update_block = pool_with_data.liquidity_update_block or 0

        working_tick_bitmap: dict[BitmapWord, BitmapAtWord] = {}
        working_tick_data: dict[Tick, LiquidityAtTick] = {}

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
        working_tick_data: dict[Tick, LiquidityAtTick],
        working_tick_bitmap: dict[BitmapWord, BitmapAtWord],
    ) -> tuple[dict[BitmapWord, BitmapAtWord] | None, dict[Tick, LiquidityAtTick] | None]:
        """Decide whether to pass tick data to the pool constructor.

        V4 passes tick data when any tick data was populated.
        Otherwise passes None (sparse mode).

        Returns:
            The computed value.

        """
        if working_tick_data:
            return working_tick_bitmap, working_tick_data
        return None, None
