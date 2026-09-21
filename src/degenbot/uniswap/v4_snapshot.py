"""Uniswap V4 pool snapshot and subscription handler."""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, cast, override

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.pool import UnknownPoolId
from degenbot.types.chain import ChecksummedAddress
from degenbot.uniswap.concentrated.snapshot_readers import (
    DatabaseSnapshotBase,
    LiquidityMap,
    LiquiditySnapshotBase,
    LiquiditySnapshotSource,
    MonolithicJsonFileSnapshotBase,
)
from degenbot.uniswap.v4_types import (
    UniswapV4LiquidityEvent,
    UniswapV4PoolLiquidityMappingUpdate,
)
from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    from typing import Any

    from degenbot.types.aliases import BlockNumber
    from degenbot.types.chain import HexAddress, HexStr
    from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick

type PoolId = str
type PoolManagerAddress = ChecksummedAddress
type ManagedPoolIdentifier = tuple[PoolManagerAddress, PoolId]


class UniswapV4LiquiditySnapshotSource(LiquiditySnapshotSource, Protocol):
    """A minimal protocol allowing the UniswapV4LiquiditySnapshot class to retrieve pool data.

    from a generic source.
    """

    def get_liquidity_map(
        self,
        pool_manager: ChecksummedAddress,
        pool_id: bytes | str,
    ) -> LiquidityMap | None:
        """Return liquidity map."""
        ...

    def get_pools(self) -> set[PoolId]:
        """Return pools."""
        ...


class MonolithicJsonFileSnapshot(MonolithicJsonFileSnapshotBase):
    """A pool liquidity source backed by a single JSON file."""

    def get_liquidity_map(
        self,
        pool_manager: ChecksummedAddress,  # ruff:ignore[unused-method-argument]
        pool_id: bytes | str,
    ) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if the pool is not found.

        """
        return self._liquidity_map_for(to_0x_hex(pool_id))


class DatabaseSnapshot(DatabaseSnapshotBase[ManagedPoolIdentifier]):
    """Snapshot source backed by the built-in SQLite database."""

    def _seam_liquidity_map(self, pool_key: ManagedPoolIdentifier) -> dict[str, Any] | None:
        return self._rust().get_liquidity_map_v4(pool_key[0], pool_key[1])

    def _seam_all_liquidity_maps(self) -> dict[tuple[str, str], dict[int, tuple[int, int]]]:
        return self._rust().get_all_liquidity_maps_v4()

    def _seam_newest_block(self) -> BlockNumber | None:
        return self._rust().get_newest_block_v4()

    def _seam_pools(self) -> set[str]:
        return self._rust().get_pools_v4()

    @override
    def _all_maps_key(self, raw_key: object) -> ManagedPoolIdentifier:
        raw_pool_key = cast("tuple[str, str]", raw_key)
        return get_checksum_address(raw_pool_key[0]), raw_pool_key[1]

    @override
    def _db_pool_key(self, pool: str) -> str:
        return pool

    def get_liquidity_map(
        self,
        pool_manager: ChecksummedAddress,
        pool_id: bytes | str,
    ) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found in the database.

        """
        return self._liquidity_map_for_key((
            get_checksum_address(pool_manager),
            cast("str", pool_id),
        ))


class UniswapV4LiquiditySnapshot(
    LiquiditySnapshotBase[
        ManagedPoolIdentifier,
        UniswapV4PoolLiquidityMappingUpdate,
        UniswapV4LiquidityEvent,
        UniswapV4LiquiditySnapshotSource,
    ]
):
    """Retrieve and maintain liquidity positions for Uniswap V4 pools."""

    _family_label = "V4"
    _update_type = UniswapV4PoolLiquidityMappingUpdate

    def _source_liquidity_map(self, pool_key: ManagedPoolIdentifier) -> LiquidityMap | None:
        return self._source.get_liquidity_map(pool_key[0], pool_key[1])

    @property
    def pools(self) -> set[ManagedPoolIdentifier]:
        """Pools."""
        return set(self._liquidity_snapshot)

    def pending_updates(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> tuple[UniswapV4PoolLiquidityMappingUpdate, ...]:
        """Consume and return all pending liquidity events for this pool.

        Returns:
            Tuple of pending liquidity mapping updates for the pool.

        """
        return self._pending_updates((get_checksum_address(pool_manager), to_0x_hex(pool_id)))

    def tick_bitmap(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> dict[int, BitmapAtWord] | None:
        """Consume the tick initialization bitmaps for the pool.

        Returns:
            The tick bitmap dict, or None if the pool snapshot is unavailable.

        """
        return self._drain_tick_bitmap((get_checksum_address(pool_manager), to_0x_hex(pool_id)))

    def tick_data(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> dict[int, LiquidityAtTick] | None:
        """Consume the liquidity mapping for the pool.

        Returns:
            The tick data dict, or None if the pool snapshot is unavailable.

        """
        return self._drain_tick_data((get_checksum_address(pool_manager), to_0x_hex(pool_id)))

    def update(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
        tick_data: dict[int, LiquidityAtTick],
        tick_bitmap: dict[int, BitmapAtWord],
    ) -> None:
        """Update the liquidity mapping for the pool.

        Raises:
            UnknownPoolId: If the pool is not found in the snapshot.

        """
        pool_key = (get_checksum_address(pool_manager), to_0x_hex(pool_id))
        pool_snapshot = self._liquidity_snapshot[pool_key]
        if pool_snapshot is None:
            raise UnknownPoolId(pool_id)
        pool_snapshot["tick_bitmap"].update(tick_bitmap)
        pool_snapshot["tick_data"].update(tick_data)
