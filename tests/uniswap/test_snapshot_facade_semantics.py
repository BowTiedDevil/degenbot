"""Characterization tests for the shared V3/V4 liquidity-snapshot facade semantics.

Pins the behavior the snapshot->pool update flow depends on: lazy per-pool
liquidity-map loading, drain-on-read for tick_bitmap/tick_data/pending_updates,
and the update() merge. Fake in-memory sources keep these hermetic (no RPC/DB).
"""

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions.pool import UnknownPool, UnknownPoolId
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.v3_snapshot import LiquidityMap, UniswapV3LiquiditySnapshot
from degenbot.uniswap.v3_types import UniswapV3LiquidityEvent
from degenbot.uniswap.v4_snapshot import UniswapV4LiquiditySnapshot
from degenbot.uniswap.v4_types import UniswapV4LiquidityEvent
from degenbot.utils.bytes import to_0x_hex

V3_POOL = get_checksum_address("0x" + "11" * 20)
V4_POOL_MANAGER = get_checksum_address("0x" + "22" * 20)
V4_POOL_ID = to_0x_hex("0x" + "ab" * 32)
V4_POOL_KEY = (V4_POOL_MANAGER, V4_POOL_ID)


def _map(bitmap_word: int, tick: int) -> LiquidityMap:
    return LiquidityMap(
        tick_bitmap={bitmap_word: BitmapAtWord(bitmap=7, block=100)},
        tick_data={tick: LiquidityAtTick(liquidity_net=5, liquidity_gross=5, block=100)},
    )


class _FakeV3Source:
    storage_kind = "memory"

    def __init__(self) -> None:
        self.chain_id = 1
        self.newest_block = 100
        self.pools = {V3_POOL}
        self.maps = {V3_POOL: _map(1, 10)}
        self.load_calls = 0

    def get_newest_block(self) -> int:
        return self.newest_block

    def get_pools(self) -> set[str]:
        return set(self.pools)

    def get_liquidity_map(self, pool_address: str) -> LiquidityMap | None:
        self.load_calls += 1
        return self.maps.get(pool_address)


class _FakeV4Source:
    storage_kind = "memory"

    def __init__(self) -> None:
        self.chain_id = 1
        self.newest_block = 100
        self.pools = {V4_POOL_ID}
        self.maps = {V4_POOL_KEY: _map(2, 20)}
        self.load_calls = 0

    def get_newest_block(self) -> int:
        return self.newest_block

    def get_pools(self) -> set[str]:
        return set(self.pools)

    def get_liquidity_map(self, pool_manager: str, pool_id: bytes | str) -> LiquidityMap | None:
        self.load_calls += 1
        return self.maps.get((pool_manager, to_0x_hex(pool_id)))


class TestV3FacadeSemantics:
    def test_lazy_load_defers_source_read_until_first_access(self) -> None:
        source = _FakeV3Source()
        snapshot = UniswapV3LiquiditySnapshot(source=source)
        assert source.load_calls == 0
        assert snapshot.tick_bitmap(V3_POOL) == _map(1, 10)["tick_bitmap"]
        assert source.load_calls == 1
        # Second access on the same pool is served from the cached snapshot.
        assert snapshot.tick_data(V3_POOL) == _map(1, 10)["tick_data"]
        assert source.load_calls == 1

    def test_tick_bitmap_and_tick_data_drain_on_read(self) -> None:
        snapshot = UniswapV3LiquiditySnapshot(source=_FakeV3Source())
        assert snapshot.tick_bitmap(V3_POOL) == _map(1, 10)["tick_bitmap"]
        assert snapshot.tick_bitmap(V3_POOL) == {}
        assert snapshot.tick_data(V3_POOL) == _map(1, 10)["tick_data"]
        assert snapshot.tick_data(V3_POOL) == {}

    def test_update_merges_into_cached_snapshot_then_drains(self) -> None:
        snapshot = UniswapV3LiquiditySnapshot(source=_FakeV3Source())
        snapshot.update(
            pool=V3_POOL,
            tick_data={99: LiquidityAtTick(liquidity_net=1, liquidity_gross=1, block=101)},
            tick_bitmap={3: BitmapAtWord(bitmap=9, block=101)},
        )
        assert snapshot.tick_bitmap(V3_POOL) == {
            1: BitmapAtWord(bitmap=7, block=100),
            3: BitmapAtWord(bitmap=9, block=101),
        }
        assert snapshot.tick_bitmap(V3_POOL) == {}
        assert snapshot.tick_data(V3_POOL) == {
            10: LiquidityAtTick(liquidity_net=5, liquidity_gross=5, block=100),
            99: LiquidityAtTick(liquidity_net=1, liquidity_gross=1, block=101),
        }

    def test_update_unknown_pool_raises(self) -> None:
        snapshot = UniswapV3LiquiditySnapshot(source=_FakeV3Source())
        with pytest.raises(UnknownPool):
            snapshot.update(pool=ZERO_ADDRESS, tick_data={}, tick_bitmap={})

    def test_pending_updates_drains_and_converts_events(self) -> None:
        snapshot = UniswapV3LiquiditySnapshot(source=_FakeV3Source())
        snapshot._liquidity_events[V3_POOL].append(
            UniswapV3LiquidityEvent(
                block_number=101,
                liquidity=7,
                tick_lower=-10,
                tick_upper=10,
                tx_index=0,
                log_index=0,
            )
        )
        updates = snapshot.pending_updates(V3_POOL)
        assert len(updates) == 1
        assert updates[0].block_number == 101
        assert updates[0].liquidity == 7
        assert snapshot.pending_updates(V3_POOL) == ()

    def test_pools_delegates_to_source(self) -> None:
        snapshot = UniswapV3LiquiditySnapshot(source=_FakeV3Source())
        assert snapshot.pools == {V3_POOL}


class TestV4FacadeSemantics:
    def test_lazy_load_defers_source_read_until_first_access(self) -> None:
        source = _FakeV4Source()
        snapshot = UniswapV4LiquiditySnapshot(source=source)
        assert source.load_calls == 0
        assert snapshot.tick_bitmap(*V4_POOL_KEY) == _map(2, 20)["tick_bitmap"]
        assert source.load_calls == 1
        assert snapshot.tick_data(*V4_POOL_KEY) == _map(2, 20)["tick_data"]
        assert source.load_calls == 1

    def test_tick_bitmap_and_tick_data_drain_on_read(self) -> None:
        snapshot = UniswapV4LiquiditySnapshot(source=_FakeV4Source())
        assert snapshot.tick_bitmap(*V4_POOL_KEY) == _map(2, 20)["tick_bitmap"]
        assert snapshot.tick_bitmap(*V4_POOL_KEY) == {}
        assert snapshot.tick_data(*V4_POOL_KEY) == _map(2, 20)["tick_data"]
        assert snapshot.tick_data(*V4_POOL_KEY) == {}

    def test_update_merges_into_cached_snapshot_then_drains(self) -> None:
        snapshot = UniswapV4LiquiditySnapshot(source=_FakeV4Source())
        snapshot.update(
            *V4_POOL_KEY,
            tick_data={99: LiquidityAtTick(liquidity_net=1, liquidity_gross=1, block=101)},
            tick_bitmap={3: BitmapAtWord(bitmap=9, block=101)},
        )
        assert snapshot.tick_bitmap(*V4_POOL_KEY) == {
            2: BitmapAtWord(bitmap=7, block=100),
            3: BitmapAtWord(bitmap=9, block=101),
        }
        assert snapshot.tick_data(*V4_POOL_KEY) == {
            20: LiquidityAtTick(liquidity_net=5, liquidity_gross=5, block=100),
            99: LiquidityAtTick(liquidity_net=1, liquidity_gross=1, block=101),
        }

    def test_update_unknown_pool_raises(self) -> None:
        snapshot = UniswapV4LiquiditySnapshot(source=_FakeV4Source())
        unknown_key = (get_checksum_address("0x" + "33" * 20), to_0x_hex("0x" + "cd" * 32))
        with pytest.raises(UnknownPoolId):
            snapshot.update(*unknown_key, tick_data={}, tick_bitmap={})

    def test_pending_updates_drains_and_converts_events(self) -> None:
        snapshot = UniswapV4LiquiditySnapshot(source=_FakeV4Source())
        snapshot._liquidity_events[V4_POOL_KEY].append(
            UniswapV4LiquidityEvent(
                block_number=101,
                liquidity=-7,
                tick_lower=-10,
                tick_upper=10,
                tx_index=0,
                log_index=0,
            )
        )
        updates = snapshot.pending_updates(*V4_POOL_KEY)
        assert len(updates) == 1
        assert updates[0].block_number == 101
        assert updates[0].liquidity == -7
        assert snapshot.pending_updates(*V4_POOL_KEY) == ()

    def test_pools_reports_only_touched_keys(self) -> None:
        snapshot = UniswapV4LiquiditySnapshot(source=_FakeV4Source())
        assert snapshot.pools == set()
        snapshot.tick_bitmap(*V4_POOL_KEY)
        assert snapshot.pools == {V4_POOL_KEY}
