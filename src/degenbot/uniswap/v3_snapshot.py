"""Uniswap V3 pool snapshot and subscription handler.

V3 identifies a pool by one checksummed address. The shared source and facade
implementations — and the Rust db seams both read through — live in
`degenbot.uniswap.concentrated.snapshot_readers`; this module supplies the V3
pool-key spelling.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, override

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.pool import UnknownPool
from degenbot.types.chain import ChecksummedAddress
from degenbot.uniswap.concentrated.snapshot_readers import (
    DatabaseSnapshotBase,
    DatabaseSnapshotSeams,
    LiquidityMap,
    LiquiditySnapshotBase,
    LiquiditySnapshotSource,
    _merge_tick_maps,
)
from degenbot.uniswap.v3_types import (
    UniswapV3LiquidityEvent,
    UniswapV3PoolLiquidityMappingUpdate,
)

if TYPE_CHECKING:
    from degenbot.types.chain import HexAddress
    from degenbot.uniswap.concentrated.types import LiquidityAtTick


def _pool_key(pool_address: HexAddress | bytes) -> ChecksummedAddress:
    return get_checksum_address(pool_address)


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


class DatabaseSnapshot(DatabaseSnapshotBase[ChecksummedAddress]):
    """Snapshot source backed by the built-in SQLite database."""

    _seams = DatabaseSnapshotSeams[ChecksummedAddress](
        liquidity_map=lambda rust, key: rust.get_liquidity_map_v3(key),
        all_liquidity_maps=lambda rust: rust.get_all_liquidity_maps_v3(),
        newest_block=lambda rust: rust.get_newest_block_v3(),
        pools=lambda rust: rust.get_pools_v3(),
        all_maps_key=_pool_key,
        db_pool_key=_pool_key,
    )

    def get_liquidity_map(self, pool_address: ChecksummedAddress) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found.

        """
        return self._liquidity_map_for_key(_pool_key(pool_address))


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

    @override
    def _source_liquidity_map(self, pool_key: ChecksummedAddress) -> LiquidityMap | None:
        return self._source.get_liquidity_map(pool_key)

    def pending_updates(
        self,
        pool_address: str,
    ) -> tuple[UniswapV3PoolLiquidityMappingUpdate, ...]:
        """Consume pending liquidity updates for the pool.

        Returns:
            A tuple of liquidity mapping updates for the pool.

        """
        return self._pending_updates(_pool_key(pool_address))

    def tick_bitmap(self, pool_address: str | bytes) -> set[int] | None:
        """Consume the tick bitmap WORD positions known for the pool.

        Returns:
            The set of known bitmap word indices, or None if no snapshot exists.

        """
        return self._drain_tick_bitmap(_pool_key(pool_address))

    def tick_data(self, pool_address: str | bytes) -> dict[int, LiquidityAtTick] | None:
        """Consume the liquidity mapping for the pool.

        Returns:
            The tick data dict, or None if no snapshot exists.

        """
        return self._drain_tick_data(_pool_key(pool_address))

    def update(
        self,
        pool: HexAddress,
        tick_data: dict[int, LiquidityAtTick],
        tick_bitmap: set[int],
    ) -> None:
        """Update the liquidity mapping for the pool.

        Raises:
            UnknownPool: If the pool has no snapshot.

        """
        pool_key = _pool_key(pool)
        if (pool_snapshot := self._liquidity_snapshot[pool_key]) is None:
            raise UnknownPool(pool_key)
        _merge_tick_maps(pool_snapshot, tick_data=tick_data, tick_bitmap=tick_bitmap)
