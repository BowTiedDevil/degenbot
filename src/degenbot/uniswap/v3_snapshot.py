"""Uniswap V3 pool snapshot and subscription handler."""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, cast, override

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.pool import UnknownPool
from degenbot.types.chain import ChecksummedAddress
from degenbot.uniswap.concentrated.snapshot_readers import (
    DatabaseSnapshotBase,
    IndividualJsonFileSnapshotBase,
    LiquidityMap,
    LiquiditySnapshotBase,
    LiquiditySnapshotSource,
    MonolithicJsonFileSnapshotBase,
)
from degenbot.uniswap.v3_types import (
    UniswapV3LiquidityEvent,
    UniswapV3PoolLiquidityMappingUpdate,
)

if TYPE_CHECKING:
    from typing import Any

    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import HexAddress
    from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick


class UniswapV3LiquiditySnapshotSource(LiquiditySnapshotSource, Protocol):
    """A minimal protocol for retrieving pool data from a generic source.

    Any class implementing the protocol must implement these methods,
    transforming data as necessary to return the specified types.
    """

    def get_liquidity_map(self, pool_address: ChecksummedAddress) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found.

        """
        ...

    def get_pools(self) -> set[ChecksummedAddress]:
        """Return pools.

        Returns:
            The set of pool addresses.

        """
        ...


class MonolithicJsonFileSnapshot(MonolithicJsonFileSnapshotBase):
    """A pool liquidity source backed by a single JSON file."""

    def get_liquidity_map(self, pool_address: ChecksummedAddress) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found.

        """
        return self._liquidity_map_for(pool_address)


class IndividualJsonFileSnapshot(IndividualJsonFileSnapshotBase):
    """Snapshot source backed by a directory of JSON files."""


class DatabaseSnapshot(DatabaseSnapshotBase[ChecksummedAddress]):
    """Snapshot source backed by the built-in SQLite database."""

    def _seam_liquidity_map(self, pool_key: ChecksummedAddress) -> dict[str, Any] | None:
        return self._rust().get_liquidity_map_v3(pool_key)

    def _seam_all_liquidity_maps(self) -> dict[str, dict[int, tuple[int, int]]]:
        return self._rust().get_all_liquidity_maps_v3()

    def _seam_newest_block(self) -> BlockNumber | None:
        return self._rust().get_newest_block_v3()

    def _seam_pools(self) -> set[str]:
        return self._rust().get_pools_v3()

    @override
    def _all_maps_key(self, raw_key: object) -> ChecksummedAddress:
        return get_checksum_address(cast("str", raw_key))

    @override
    def _db_pool_key(self, pool: str) -> str:
        return get_checksum_address(pool)

    def get_liquidity_map(self, pool_address: ChecksummedAddress) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found.

        """
        return self._liquidity_map_for_key(get_checksum_address(pool_address))


class UniswapV3LiquiditySnapshot(
    LiquiditySnapshotBase[
        ChecksummedAddress,
        UniswapV3PoolLiquidityMappingUpdate,
        UniswapV3LiquidityEvent,
        UniswapV3LiquiditySnapshotSource,
    ]
):
    """Retrieve and maintain liquidity positions for Uniswap V3 pools."""

    _family_label = "V3"
    _update_type = UniswapV3PoolLiquidityMappingUpdate

    def _source_liquidity_map(self, pool_key: ChecksummedAddress) -> LiquidityMap | None:
        return self._source.get_liquidity_map(get_checksum_address(pool_key))

    def pending_updates(
        self,
        pool_address: str,
    ) -> tuple[UniswapV3PoolLiquidityMappingUpdate, ...]:
        """Consume pending liquidity updates for the pool.

        Returns:
            A tuple of liquidity mapping updates for the pool.

        """
        return self._pending_updates(get_checksum_address(pool_address))

    def tick_bitmap(self, pool_address: str | bytes) -> dict[int, BitmapAtWord] | None:
        """Consume the tick initialization bitmaps for the pool.

        Returns:
            The tick bitmap dict, or None if no snapshot exists.

        """
        return self._drain_tick_bitmap(get_checksum_address(pool_address))

    def tick_data(self, pool_address: str | bytes) -> dict[int, LiquidityAtTick] | None:
        """Consume the liquidity mapping for the pool.

        Returns:
            The tick data dict, or None if no snapshot exists.

        """
        return self._drain_tick_data(get_checksum_address(pool_address))

    def update(
        self,
        pool: HexAddress,
        tick_data: dict[int, LiquidityAtTick],
        tick_bitmap: dict[int, BitmapAtWord],
    ) -> None:
        """Update the liquidity mapping for the pool.

        Raises:
            UnknownPool: If the pool has no snapshot.

        """
        pool_key = get_checksum_address(pool)
        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            raise UnknownPool(pool_key)
        pool_snapshot["tick_bitmap"].update(tick_bitmap)
        pool_snapshot["tick_data"].update(tick_data)
