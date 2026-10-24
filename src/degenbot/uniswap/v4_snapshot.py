"""Uniswap V4 pool snapshot and subscription handler.

V4 identifies a pool by its ``(pool manager, pool id)`` pair. The shared source
and facade implementations — and the Rust db seams both read through — live in
`degenbot.uniswap.concentrated.snapshot_readers`; this module supplies the V4
pool-key spelling.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, override

from degenbot.checksum_cache import get_checksum_address
from degenbot.exceptions.pool import UnknownPoolId
from degenbot.types.chain import ChecksummedAddress
from degenbot.uniswap.concentrated.snapshot_readers import (
    DatabaseSnapshotBase,
    DatabaseSnapshotSeams,
    LiquidityMap,
    LiquiditySnapshotBase,
    LiquiditySnapshotSource,
    _merge_tick_maps,
)
from degenbot.uniswap.v4_types import (
    UniswapV4LiquidityEvent,
    UniswapV4PoolLiquidityMappingUpdate,
)
from degenbot.utils.bytes import to_0x_hex

if TYPE_CHECKING:
    from degenbot.types.chain import HexAddress, HexStr
    from degenbot.uniswap.concentrated.types import LiquidityAtTick

type PoolId = str
type PoolManagerAddress = ChecksummedAddress
type ManagedPoolIdentifier = tuple[PoolManagerAddress, PoolId]


def _pool_key(
    pool_manager: HexAddress | bytes,
    pool_id: HexStr | bytes,
) -> ManagedPoolIdentifier:
    return get_checksum_address(pool_manager), to_0x_hex(pool_id)


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


class DatabaseSnapshot(DatabaseSnapshotBase[ManagedPoolIdentifier]):
    """Snapshot source backed by the built-in SQLite database."""

    _seams = DatabaseSnapshotSeams[ManagedPoolIdentifier](
        liquidity_map=lambda rust, key: rust.get_liquidity_map_v4(key[0], key[1]),
        all_liquidity_maps=lambda rust: rust.get_all_liquidity_maps_v4(),
        newest_block=lambda rust: rust.get_newest_block_v4(),
        pools=lambda rust: rust.get_pools_v4(),
        all_maps_key=lambda raw_key: (get_checksum_address(raw_key[0]), raw_key[1]),
    )

    def get_liquidity_map(
        self,
        pool_manager: ChecksummedAddress,
        pool_id: bytes | str,
    ) -> LiquidityMap | None:
        """Return liquidity map.

        Returns:
            The liquidity map for the pool, or None if not found in the database.

        """
        return self._liquidity_map_for_key(_pool_key(pool_manager, pool_id))


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

    @override
    def _source_liquidity_map(self, pool_key: ManagedPoolIdentifier) -> LiquidityMap | None:
        return self._source.get_liquidity_map(*pool_key)

    @property
    @override
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
        return self._pending_updates(_pool_key(pool_manager, pool_id))

    def tick_bitmap(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> set[int] | None:
        """Consume the tick bitmap WORD positions known for the pool.

        Returns:
            The set of known bitmap word indices, or None if the pool snapshot
            is unavailable.

        """
        return self._drain_tick_bitmap(_pool_key(pool_manager, pool_id))

    def tick_data(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
    ) -> dict[int, LiquidityAtTick] | None:
        """Consume the liquidity mapping for the pool.

        Returns:
            The tick data dict, or None if the pool snapshot is unavailable.

        """
        return self._drain_tick_data(_pool_key(pool_manager, pool_id))

    def update(
        self,
        pool_manager: HexAddress | bytes,
        pool_id: HexStr | bytes,
        tick_data: dict[int, LiquidityAtTick],
        tick_bitmap: set[int],
    ) -> None:
        """Update the liquidity mapping for the pool.

        Raises:
            UnknownPoolId: If the pool is not found in the snapshot.

        """
        pool_key = _pool_key(pool_manager, pool_id)
        if (pool_snapshot := self._liquidity_snapshot[pool_key]) is None:
            raise UnknownPoolId(pool_id)
        _merge_tick_maps(pool_snapshot, tick_data=tick_data, tick_bitmap=tick_bitmap)
