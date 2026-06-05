"""Binary serialization for V3/V4 liquidity snapshots.

Plan 098: Python serializes the complete V3/V4 snapshots into binary buffers
and passes each to Rust via a single memcpy. Rust deserializes directly into
typed HashMaps with zero per-tick extract() calls.

The format is non-destructive — it reads the snapshot's internal
_liquidity_snapshot dict without calling the destructive tick_data() method.

## V3 format

    [1 byte: version]
    [4 bytes LE: pool_count]
    Per pool:
      [20 bytes: pool address]
      [4 bytes LE: tick_count]
      Per tick:
        [4 bytes LE: tick_index (i32)]
        [16 bytes LE: liquidity_gross (u128)]
        [16 bytes LE: liquidity_net (i128)]

## V4 format

    [1 byte: version]
    [4 bytes LE: pool_manager_count]
    Per pool_manager:
      [20 bytes: pool_manager address]
      [4 bytes LE: pool_id_count]
      Per pool_id:
        [32 bytes: pool_id]
        [4 bytes LE: tick_count]
        Per tick:
          [4 bytes LE: tick_index (i32)]
          [16 bytes LE: liquidity_gross (u128)]
          [16 bytes LE: liquidity_net (i128)]

A tick_count of zero means the pool has no initialized ticks (genuinely illiquid).
The PoolTickCoverage enum distinguishes this from a pool absent from the snapshot.
"""

from __future__ import annotations

import struct
from typing import TYPE_CHECKING

from hexbytes import HexBytes

if TYPE_CHECKING:
    from eth_typing import ChecksumAddress

    from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot
    from degenbot.uniswap.v4_snapshot import ManagedPoolIdentifier, UniswapV4LiquiditySnapshot

SNAPSHOT_VERSION: int = 1

# Record sizes (bytes)
_TICK_ENTRY_SIZE = 4 + 16 + 16  # tick_index(i32) + liquidity_gross(u128) + liquidity_net(i128)
_V3_POOL_HEADER_SIZE = 20 + 4  # address(20) + tick_count(u32)
_V4_POOL_ID_HEADER_SIZE = 32 + 4  # pool_id(32) + tick_count(u32)
_V4_PM_HEADER_SIZE = 20 + 4  # pool_manager(20) + pool_id_count(u32)
_HEADER_SIZE = 1 + 4  # version(1) + count(u32)


def _write_u128(buf: bytearray, offset: int, value: int) -> int:
    """Write a u128 value as two little-endian u64s. Returns new offset."""
    struct.pack_into("<QQ", buf, offset, value & 0xFFFFFFFFFFFFFFFF, value >> 64)
    return offset + 16


def _write_i128(buf: bytearray, offset: int, value: int) -> int:
    """Write an i128 value as two little-endian u64s. Returns new offset."""
    # Convert signed i128 to unsigned 128-bit representation
    if value < 0:
        value = (1 << 128) + value
    struct.pack_into("<QQ", buf, offset, value & 0xFFFFFFFFFFFFFFFF, value >> 64)
    return offset + 16


def _write_address(buf: bytearray, offset: int, address_hex: str) -> int:
    """Write a 20-byte address from hex string (with 0x prefix). Returns new offset."""
    buf[offset : offset + 20] = bytes.fromhex(address_hex[2:])
    return offset + 20


def _prime_v3_snapshot(snapshot: UniswapV3LiquiditySnapshot) -> None:
    """Eagerly populate the lazy KeyedDefaultDict by accessing each pool.

    The snapshot's _liquidity_snapshot is a KeyedDefaultDict that lazily
    populates entries on __getitem__. We must access each pool address
    before iterating to ensure all entries are present.
    """
    for pool_address in snapshot.pools:
        _ = snapshot._liquidity_snapshot[pool_address]  # noqa: SLF001


def _prime_v4_snapshot(
    snapshot: UniswapV4LiquiditySnapshot,
    managed_pools: set[ManagedPoolIdentifier],
) -> None:
    """Eagerly populate the lazy KeyedDefaultDict by accessing each pool.

    The V4 snapshot's _liquidity_snapshot is keyed by (pool_manager, pool_id).
    Unlike V3, we can't enumerate pools from snapshot.pools (it's empty until
    accessed). The caller must provide the set of (pool_manager, pool_id) pairs.
    """
    for pool_manager, pool_id in managed_pools:
        _ = snapshot._liquidity_snapshot[(pool_manager, pool_id)]  # noqa: SLF001


def _normalize_v4_pool_id(pool_id: str | bytes) -> str:
    """Normalize a V4 pool_id to a 0x-prefixed hex string."""
    if isinstance(pool_id, bytes):
        return HexBytes(pool_id).to_0x_hex()
    if pool_id.startswith("0x"):
        return pool_id
    return HexBytes(pool_id).to_0x_hex()


def v3_snapshot_binary_size(snapshot: UniswapV3LiquiditySnapshot) -> int:
    """Calculate the exact binary size of the serialized V3 snapshot without building it.

    This is useful for pre-allocating the output buffer.
    """
    _prime_v3_snapshot(snapshot)
    size = _HEADER_SIZE
    liquidity_snapshot = snapshot._liquidity_snapshot  # noqa: SLF001
    for pool_address, pool_mapping in liquidity_snapshot.items():
        size += _V3_POOL_HEADER_SIZE
        if pool_mapping is not None:
            size += len(pool_mapping["tick_data"]) * _TICK_ENTRY_SIZE
    return size


def v4_snapshot_binary_size(
    snapshot: UniswapV4LiquiditySnapshot,
    managed_pools: set[ManagedPoolIdentifier],
) -> int:
    """Calculate the exact binary size of the serialized V4 snapshot without building it.

    Args:
        snapshot: The V4 liquidity snapshot.
        managed_pools: Set of (pool_manager, pool_id) tuples identifying
            which pools to include. The caller knows these from the bot's
            pool registry.

    """
    _prime_v4_snapshot(snapshot, managed_pools)
    liquidity_snapshot = snapshot._liquidity_snapshot  # noqa: SLF001

    # Group by pool_manager to count pool_managers and pool_ids
    pool_managers: dict[str, list[tuple[str, object]]] = {}
    for (pm_addr, pool_id), pool_mapping in liquidity_snapshot.items():
        if pm_addr not in pool_managers:
            pool_managers[pm_addr] = []
        pool_managers[pm_addr].append((pool_id, pool_mapping))

    size = _HEADER_SIZE
    for pm_addr, pool_entries in pool_managers.items():
        size += _V4_PM_HEADER_SIZE
        for pool_id, pool_mapping in pool_entries:
            size += _V4_POOL_ID_HEADER_SIZE
            if pool_mapping is not None:
                size += len(pool_mapping["tick_data"]) * _TICK_ENTRY_SIZE
    return size


def serialize_v3_snapshot(snapshot: UniswapV3LiquiditySnapshot) -> bytes:
    """Serialize a UniswapV3LiquiditySnapshot into a binary buffer.

    Non-destructive: reads _liquidity_snapshot directly without calling
    the destructive tick_data() method.

    Args:
        snapshot: The V3 liquidity snapshot.

    Returns:
        Binary buffer in the V3 snapshot format.

    """
    _prime_v3_snapshot(snapshot)
    liquidity_snapshot = snapshot._liquidity_snapshot  # noqa: SLF001

    # Pre-calculate size and allocate buffer
    size = v3_snapshot_binary_size(snapshot)
    buf = bytearray(size)

    # Header
    buf[0] = SNAPSHOT_VERSION
    struct.pack_into("<I", buf, 1, len(liquidity_snapshot))

    offset = _HEADER_SIZE

    for pool_address, pool_mapping in liquidity_snapshot.items():
        # Pool address
        offset = _write_address(buf, offset, pool_address)

        if pool_mapping is None:
            # Pool not found in source — zero ticks
            struct.pack_into("<I", buf, offset, 0)
            offset += 4
            continue

        tick_data = pool_mapping["tick_data"]
        struct.pack_into("<I", buf, offset, len(tick_data))
        offset += 4

        for tick_index, liquidity_at_tick in tick_data.items():
            struct.pack_into("<i", buf, offset, tick_index)
            offset += 4
            offset = _write_u128(buf, offset, liquidity_at_tick.liquidity_gross)
            offset = _write_i128(buf, offset, liquidity_at_tick.liquidity_net)

    assert offset == size, f"Serialized {offset} bytes but expected {size}"
    return bytes(buf)


def serialize_v4_snapshot(
    snapshot: UniswapV4LiquiditySnapshot,
    managed_pools: set[ManagedPoolIdentifier],
) -> bytes:
    """Serialize a UniswapV4LiquiditySnapshot into a binary buffer.

    Non-destructive: reads _liquidity_snapshot directly without calling
    the destructive tick_data() method.

    Unlike V3, V4 requires the caller to provide the set of
    (pool_manager, pool_id) tuples. The V4 snapshot's internal dict is
    lazily populated and cannot self-enumerate without external knowledge
    of which pool_managers and pool_ids exist.

    Args:
        snapshot: The V4 liquidity snapshot.
        managed_pools: Set of (pool_manager, pool_id) tuples identifying
            which pools to include. The caller knows these from the bot's
            pool registry.

    Returns:
        Binary buffer in the V4 snapshot format.

    """
    _prime_v4_snapshot(snapshot, managed_pools)
    liquidity_snapshot = snapshot._liquidity_snapshot  # noqa: SLF001

    # Group by pool_manager
    pool_managers: dict[ChecksumAddress, list[tuple[str, object]]] = {}
    for (pm_addr, pool_id), pool_mapping in liquidity_snapshot.items():
        if pm_addr not in pool_managers:
            pool_managers[pm_addr] = []
        pool_managers[pm_addr].append((pool_id, pool_mapping))

    # Pre-calculate size and allocate buffer
    size = v4_snapshot_binary_size(snapshot, managed_pools)
    buf = bytearray(size)

    # Header
    buf[0] = SNAPSHOT_VERSION
    struct.pack_into("<I", buf, 1, len(pool_managers))

    offset = _HEADER_SIZE

    for pm_addr, pool_entries in pool_managers.items():
        # Pool manager address
        offset = _write_address(buf, offset, pm_addr)

        # Pool ID count
        struct.pack_into("<I", buf, offset, len(pool_entries))
        offset += 4

        for pool_id, pool_mapping in pool_entries:
            # Pool ID (32 bytes)
            pool_id_hex = _normalize_v4_pool_id(pool_id)
            buf[offset : offset + 32] = bytes.fromhex(pool_id_hex[2:])
            offset += 32

            if pool_mapping is None:
                struct.pack_into("<I", buf, offset, 0)
                offset += 4
                continue

            tick_data = pool_mapping["tick_data"]
            struct.pack_into("<I", buf, offset, len(tick_data))
            offset += 4

            for tick_index, liquidity_at_tick in tick_data.items():
                struct.pack_into("<i", buf, offset, tick_index)
                offset += 4
                offset = _write_u128(buf, offset, liquidity_at_tick.liquidity_gross)
                offset = _write_i128(buf, offset, liquidity_at_tick.liquidity_net)

    assert offset == size, f"Serialized {offset} bytes but expected {size}"
    return bytes(buf)
