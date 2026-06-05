"""Tests for binary serialization of V3/V4 liquidity snapshots.

Plan 098: Python serializes snapshots into binary buffers, passes them to Rust
via a single memcpy. These tests verify the serialization format is correct
and can be round-tripped.
"""

import struct

from degenbot.checksum_cache import get_checksum_address
from degenbot.uniswap.concentrated.types import LiquidityAtTick
from degenbot.uniswap.snapshot_binary import (
    SNAPSHOT_VERSION,
    serialize_v3_snapshot,
    serialize_v4_snapshot,
    v3_snapshot_binary_size,
    v4_snapshot_binary_size,
)
from degenbot.uniswap.v3_snapshot import LiquidityMap, UniswapV3LiquiditySnapshot
from degenbot.uniswap.v4_snapshot import LiquidityMap as V4LiquidityMap
from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot


class FakeV3SnapshotSource:
    """Fake V3 snapshot source for testing."""

    storage_kind = "fake"
    chain_id = 1

    def __init__(
        self,
        pools: dict[str, LiquidityMap | None],
        newest_block: int = 100,
    ) -> None:
        # Normalize all keys to checksum addresses
        self._pools = {get_checksum_address(k): v for k, v in pools.items()}
        self._newest_block = newest_block

    def get_liquidity_map(self, pool_address: str) -> LiquidityMap | None:
        return self._pools.get(get_checksum_address(pool_address))

    def get_newest_block(self) -> int | None:
        return self._newest_block

    def get_pools(self) -> set[str]:
        return {addr for addr, mapping in self._pools.items() if mapping is not None}


class FakeV4SnapshotSource:
    """Fake V4 snapshot source for testing."""

    storage_kind = "fake"
    chain_id = 1

    def __init__(
        self,
        pools: dict[tuple[str, str], V4LiquidityMap | None],
        newest_block: int = 100,
    ) -> None:
        # Normalize pool_manager keys to checksum addresses
        self._pools = {(get_checksum_address(pm), pid): v for (pm, pid), v in pools.items()}
        self._newest_block = newest_block

    def get_liquidity_map(self, pool_manager: str, pool_id: str) -> V4LiquidityMap | None:
        return self._pools.get((get_checksum_address(pool_manager), pool_id))

    def get_newest_block(self) -> int | None:
        return self._newest_block

    def get_pools(self) -> set[str]:
        return {pool_id for (_, pool_id), mapping in self._pools.items() if mapping is not None}


# --- V3 serialization tests ---


class TestV3SerializeEmptySnapshot:
    """Test serializing a snapshot with no pools."""

    def test_empty_snapshot_produces_valid_header(self) -> None:
        source = FakeV3SnapshotSource(pools={})
        snapshot = UniswapV3LiquiditySnapshot(source=source)
        data = serialize_v3_snapshot(snapshot)

        # Version byte + 4 bytes pool_count
        assert len(data) == 5
        assert data[0] == SNAPSHOT_VERSION
        (pool_count,) = struct.unpack_from("<I", data, 1)
        assert pool_count == 0

    def test_empty_snapshot_binary_size_matches(self) -> None:
        source = FakeV3SnapshotSource(pools={})
        snapshot = UniswapV3LiquiditySnapshot(source=source)
        assert v3_snapshot_binary_size(snapshot) == len(serialize_v3_snapshot(snapshot))


class TestV3SerializePoolWithNoTicks:
    """Test serializing a pool that exists in the snapshot but has no initialized ticks."""

    def test_pool_with_empty_tick_data(self) -> None:
        pool_addr = "0x" + "11" * 20
        source = FakeV3SnapshotSource(
            pools={
                pool_addr: LiquidityMap(
                    tick_bitmap={},
                    tick_data={},
                ),
            }
        )
        snapshot = UniswapV3LiquiditySnapshot(source=source)
        data = serialize_v3_snapshot(snapshot)

        # header(5) + pool_record(20+4) = 29 bytes
        assert len(data) == 5 + 20 + 4
        assert data[0] == SNAPSHOT_VERSION

        (pool_count,) = struct.unpack_from("<I", data, 1)
        assert pool_count == 1

        # Pool address at offset 5
        addr_bytes = data[5:25]
        assert addr_bytes == bytes.fromhex("11" * 20)

        # Tick count at offset 25
        (tick_count,) = struct.unpack_from("<I", data, 25)
        assert tick_count == 0


class TestV3SerializePoolWithTicks:
    """Test serializing a pool with initialized ticks."""

    def test_single_tick(self) -> None:
        pool_addr = "0x" + "aa" * 20
        tick_index = -100
        liquidity_gross = 2**64  # fits in u128
        liquidity_net = -(2**60)  # fits in i128

        source = FakeV3SnapshotSource(
            pools={
                pool_addr: LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        tick_index: LiquidityAtTick(
                            liquidity_gross=liquidity_gross,
                            liquidity_net=liquidity_net,
                        ),
                    },
                ),
            }
        )
        snapshot = UniswapV3LiquiditySnapshot(source=source)
        data = serialize_v3_snapshot(snapshot)

        # header(5) + pool_header(20+4) + 1 tick(4+16+16) = 5+24+36 = 65
        expected_size = 5 + 20 + 4 + (4 + 16 + 16)
        assert len(data) == expected_size

        # Verify tick_index
        (decoded_tick_index,) = struct.unpack_from("<i", data, 29)
        assert decoded_tick_index == tick_index

        # Verify liquidity_gross (u128 as two u64 LE)
        lo, hi = struct.unpack_from("<QQ", data, 33)
        decoded_gross = lo | (hi << 64)
        assert decoded_gross == liquidity_gross

        # Verify liquidity_net (i128 as two u64 LE)
        lo, hi = struct.unpack_from("<QQ", data, 49)
        decoded_net = lo | (hi << 64)
        if decoded_net >= (1 << 127):
            decoded_net -= 1 << 128
        assert decoded_net == liquidity_net

    def test_two_pools_with_ticks(self) -> None:
        pool_a = "0x" + "11" * 20
        pool_b = "0x" + "22" * 20

        source = FakeV3SnapshotSource(
            pools={
                pool_a: LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        -100: LiquidityAtTick(liquidity_gross=100, liquidity_net=-100),
                        0: LiquidityAtTick(liquidity_gross=200, liquidity_net=200),
                    },
                ),
                pool_b: LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        50: LiquidityAtTick(liquidity_gross=300, liquidity_net=300),
                    },
                ),
            }
        )
        snapshot = UniswapV3LiquiditySnapshot(source=source)
        data = serialize_v3_snapshot(snapshot)

        (pool_count,) = struct.unpack_from("<I", data, 1)
        assert pool_count == 2

        # Binary size should match
        assert len(data) == v3_snapshot_binary_size(snapshot)

    def test_max_u128_liquidity_gross(self) -> None:
        """Test that u128 max value (2^128 - 1) serializes correctly."""
        pool_addr = "0x" + "ff" * 20
        max_u128 = (1 << 128) - 1

        source = FakeV3SnapshotSource(
            pools={
                pool_addr: LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        0: LiquidityAtTick(
                            liquidity_gross=max_u128,
                            liquidity_net=0,
                        ),
                    },
                ),
            }
        )
        snapshot = UniswapV3LiquiditySnapshot(source=source)
        data = serialize_v3_snapshot(snapshot)

        # Read back the u128 value (after header=5 + pool_addr=20 + tick_count=4 + tick_index=4)
        lo, hi = struct.unpack_from("<QQ", data, 33)
        reconstructed = lo | (hi << 64)
        assert reconstructed == max_u128

    def test_min_int128_liquidity_net(self) -> None:
        """Test that i128 min value (-2^127) serializes correctly."""
        pool_addr = "0x" + "ff" * 20
        min_i128 = -(1 << 127)

        source = FakeV3SnapshotSource(
            pools={
                pool_addr: LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        0: LiquidityAtTick(
                            liquidity_gross=0,
                            liquidity_net=min_i128,
                        ),
                    },
                ),
            }
        )
        snapshot = UniswapV3LiquiditySnapshot(source=source)
        data = serialize_v3_snapshot(snapshot)

        # Read back the i128 value
        # (after header=5 + pool_addr=20 + tick_count=4 + tick_index=4 + liquidity_gross=16)
        lo, hi = struct.unpack_from("<QQ", data, 33 + 16)
        reconstructed = lo | (hi << 64)
        # Sign-extend from 128-bit
        if reconstructed >= (1 << 127):
            reconstructed -= 1 << 128
        assert reconstructed == min_i128


class TestV3SerializeNonDestructive:
    """Verify that serialization does NOT consume the snapshot's internal data."""

    def test_tick_data_remains_after_serialization(self) -> None:
        pool_addr = "0x" + "11" * 20
        source = FakeV3SnapshotSource(
            pools={
                pool_addr: LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        -100: LiquidityAtTick(liquidity_gross=100, liquidity_net=-100),
                    },
                ),
            }
        )
        snapshot = UniswapV3LiquiditySnapshot(source=source)

        # Serialize
        _ = serialize_v3_snapshot(snapshot)

        # tick_data should still be accessible
        tick_data = snapshot.tick_data(pool_addr)
        assert tick_data is not None
        assert -100 in tick_data
        assert tick_data[-100].liquidity_gross == 100

    def test_pools_set_remains_after_serialization(self) -> None:
        pool_addr = "0x" + "11" * 20
        source = FakeV3SnapshotSource(
            pools={
                pool_addr: LiquidityMap(
                    tick_bitmap={},
                    tick_data={},
                ),
            }
        )
        snapshot = UniswapV3LiquiditySnapshot(source=source)

        _ = serialize_v3_snapshot(snapshot)
        assert pool_addr in snapshot.pools


# --- V4 serialization tests ---


class TestV4SerializeEmptySnapshot:
    """Test serializing a V4 snapshot with no pools."""

    def test_empty_v4_snapshot_produces_valid_header(self) -> None:
        source = FakeV4SnapshotSource(pools={})
        snapshot = UniswapV4LiquiditySnapshot(source=source)
        data = serialize_v4_snapshot(snapshot, managed_pools=set())

        assert len(data) == 5
        assert data[0] == SNAPSHOT_VERSION
        (pm_count,) = struct.unpack_from("<I", data, 1)
        assert pm_count == 0


class TestV4SerializePoolWithTicks:
    """Test serializing V4 pools with ticks, organized by pool_manager."""

    def test_single_pool_manager_single_pool(self) -> None:
        pm_addr = "0x" + "aa" * 20
        pool_id = "0x" + "bb" * 32
        tick_index = -200
        liquidity_gross = 500
        liquidity_net = 500

        source = FakeV4SnapshotSource(
            pools={
                (pm_addr, pool_id): V4LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        tick_index: LiquidityAtTick(
                            liquidity_gross=liquidity_gross,
                            liquidity_net=liquidity_net,
                        ),
                    },
                ),
            }
        )
        snapshot = UniswapV4LiquiditySnapshot(source=source)
        managed_pools = {(pm_addr, pool_id)}
        data = serialize_v4_snapshot(snapshot, managed_pools=managed_pools)

        # header(5) + pm_header(20+4) + pool_id(32+4) + 1 tick(4+16+16) = 5+24+36+36 = 101
        expected_size = 5 + 20 + 4 + 32 + 4 + (4 + 16 + 16)
        assert len(data) == expected_size
        assert len(data) == v4_snapshot_binary_size(snapshot, managed_pools=managed_pools)

        # Verify pool_manager address
        pm_bytes = data[5:25]
        assert pm_bytes == bytes.fromhex("aa" * 20)

        # Verify pool_id count
        (pool_id_count,) = struct.unpack_from("<I", data, 25)
        assert pool_id_count == 1

        # Verify pool_id bytes
        pool_id_bytes = data[29:61]
        assert pool_id_bytes == bytes.fromhex("bb" * 32)

        # Verify tick count
        (tick_count,) = struct.unpack_from("<I", data, 61)
        assert tick_count == 1

        # Verify tick index
        (decoded_tick,) = struct.unpack_from("<i", data, 65)
        assert decoded_tick == tick_index

    def test_two_pool_managers(self) -> None:
        pm_a = "0x" + "11" * 20
        pm_b = "0x" + "22" * 20
        pool_id_a = "0x" + "33" * 32
        pool_id_b = "0x" + "44" * 32

        source = FakeV4SnapshotSource(
            pools={
                (pm_a, pool_id_a): V4LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        -10: LiquidityAtTick(liquidity_gross=100, liquidity_net=-100),
                    },
                ),
                (pm_b, pool_id_b): V4LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        20: LiquidityAtTick(liquidity_gross=200, liquidity_net=200),
                    },
                ),
            }
        )
        snapshot = UniswapV4LiquiditySnapshot(source=source)
        managed_pools = {(pm_a, pool_id_a), (pm_b, pool_id_b)}
        data = serialize_v4_snapshot(snapshot, managed_pools=managed_pools)

        (pm_count,) = struct.unpack_from("<I", data, 1)
        assert pm_count == 2

    def test_pool_with_no_ticks(self) -> None:
        """V4 pool in snapshot but with zero initialized ticks."""
        pm_addr = "0x" + "aa" * 20
        pool_id = "0x" + "bb" * 32

        source = FakeV4SnapshotSource(
            pools={
                (pm_addr, pool_id): V4LiquidityMap(
                    tick_bitmap={},
                    tick_data={},
                ),
            }
        )
        snapshot = UniswapV4LiquiditySnapshot(source=source)
        managed_pools = {(pm_addr, pool_id)}
        data = serialize_v4_snapshot(snapshot, managed_pools=managed_pools)

        # header(5) + pm(20+4) + pool_id(32+4) = 65
        assert len(data) == 5 + 20 + 4 + 32 + 4

        (pm_count,) = struct.unpack_from("<I", data, 1)
        assert pm_count == 1

        (pool_id_count,) = struct.unpack_from("<I", data, 25)
        assert pool_id_count == 1

        (tick_count,) = struct.unpack_from("<I", data, 61)
        assert tick_count == 0

    def test_same_pool_manager_multiple_pools(self) -> None:
        pm_addr = "0x" + "aa" * 20
        pool_id_1 = "0x" + "11" * 32
        pool_id_2 = "0x" + "22" * 32

        source = FakeV4SnapshotSource(
            pools={
                (pm_addr, pool_id_1): V4LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        -10: LiquidityAtTick(liquidity_gross=100, liquidity_net=-100),
                    },
                ),
                (pm_addr, pool_id_2): V4LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        20: LiquidityAtTick(liquidity_gross=200, liquidity_net=200),
                    },
                ),
            }
        )
        snapshot = UniswapV4LiquiditySnapshot(source=source)
        managed_pools = {(pm_addr, pool_id_1), (pm_addr, pool_id_2)}
        data = serialize_v4_snapshot(snapshot, managed_pools=managed_pools)

        (pm_count,) = struct.unpack_from("<I", data, 1)
        assert pm_count == 1  # Both pools share the same pool manager

        (pool_id_count,) = struct.unpack_from("<I", data, 25)
        assert pool_id_count == 2


class TestV4SerializeNonDestructive:
    """Verify that V4 serialization does NOT consume the snapshot's internal data."""

    def test_tick_data_remains_after_v4_serialization(self) -> None:
        pm_addr = "0x" + "aa" * 20
        pool_id = "0x" + "bb" * 32

        source = FakeV4SnapshotSource(
            pools={
                (pm_addr, pool_id): V4LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        -100: LiquidityAtTick(liquidity_gross=100, liquidity_net=-100),
                    },
                ),
            }
        )
        snapshot = UniswapV4LiquiditySnapshot(source=source)
        managed_pools = {(pm_addr, pool_id)}

        _ = serialize_v4_snapshot(snapshot, managed_pools=managed_pools)

        tick_data = snapshot.tick_data(pm_addr, pool_id)
        assert tick_data is not None
        assert -100 in tick_data


class TestV3BinarySize:
    """Test the v3_snapshot_binary_size helper."""

    def test_size_matches_serialized_output(self) -> None:
        pool_addr = "0x" + "11" * 20
        source = FakeV3SnapshotSource(
            pools={
                pool_addr: LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        -100: LiquidityAtTick(liquidity_gross=100, liquidity_net=-100),
                        0: LiquidityAtTick(liquidity_gross=200, liquidity_net=200),
                        100: LiquidityAtTick(liquidity_gross=300, liquidity_net=-300),
                    },
                ),
            }
        )
        snapshot = UniswapV3LiquiditySnapshot(source=source)

        assert v3_snapshot_binary_size(snapshot) == len(serialize_v3_snapshot(snapshot))


class TestV4BinarySize:
    """Test the v4_snapshot_binary_size helper."""

    def test_size_matches_serialized_output(self) -> None:
        pm_addr = "0x" + "aa" * 20
        pool_id = "0x" + "bb" * 32

        source = FakeV4SnapshotSource(
            pools={
                (pm_addr, pool_id): V4LiquidityMap(
                    tick_bitmap={},
                    tick_data={
                        -10: LiquidityAtTick(liquidity_gross=100, liquidity_net=-100),
                        10: LiquidityAtTick(liquidity_gross=200, liquidity_net=200),
                    },
                ),
            }
        )
        snapshot = UniswapV4LiquiditySnapshot(source=source)
        managed_pools = {(pm_addr, pool_id)}

        assert v4_snapshot_binary_size(snapshot, managed_pools=managed_pools) == len(
            serialize_v4_snapshot(snapshot, managed_pools=managed_pools)
        )
