"""V3 liquidity-snapshot source fixtures: facade reads over recorded data.

The two tracker-seeding tests that used to live here
(``test_apply_update_to_snapshot`` and
``test_pool_manager_applies_snapshot_from_dir``) asserted the pre-builder-
retirement behavior where drained snapshot tick maps seeded a tracker-built
pool. That behavior died with the Python V3 builder (48f6b55a5) — the tick
maps were silently discarded — and the surviving tracker contract (pending
Mint/Burn events applying to a live-built pool, against chain truth) is
pinned by the golden test
``tests/uniswap/v3/test_v3_tracker_snapshot_events_golden.py``.

What remains here: the snapshot facade's own read surface over the file,
dir, and database sources (drain-on-read, missing-pool misses, the pools
property), which do not route through pool construction.
"""

from collections.abc import Iterator

import pytest

from degenbot.config import resolve_database_path
from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions.pool import UnknownPool
from degenbot.fork import AnvilFork
from degenbot.uniswap.v3_snapshot import (
    DatabaseSnapshot,
    IndividualJsonFileSnapshot,
    MonolithicJsonFileSnapshot,
    UniswapV3LiquiditySnapshot,
)

EMPTY_SNAPSHOT_FILENAME = "tests/uniswap/v3/empty_v3_liquidity_snapshot.json"
SNAPSHOT_AT_BLOCK_12_369_870_FILENAME = (
    "tests/uniswap/v3/mainnet_v3_liquidity_snapshot_block_21_369_870.json"
)
SNAPSHOT_AT_BLOCK_12_369_870_DIR = "tests/uniswap/v3/snapshot"


@pytest.fixture
def empty_mainnet_snapshot_from_file() -> UniswapV3LiquiditySnapshot:
    return UniswapV3LiquiditySnapshot(
        source=MonolithicJsonFileSnapshot(EMPTY_SNAPSHOT_FILENAME),
    )


@pytest.fixture
def mainnet_snapshot_at_block_12_369_870_from_file() -> UniswapV3LiquiditySnapshot:
    return UniswapV3LiquiditySnapshot(
        source=MonolithicJsonFileSnapshot(SNAPSHOT_AT_BLOCK_12_369_870_FILENAME),
    )


@pytest.fixture
def mainnet_snapshot_at_block_12_369_870_from_dir() -> UniswapV3LiquiditySnapshot:
    return UniswapV3LiquiditySnapshot(
        source=IndividualJsonFileSnapshot(SNAPSHOT_AT_BLOCK_12_369_870_DIR),
    )


@pytest.fixture
def base_snapshot_from_database(
    fork_base_full: AnvilFork,
) -> Iterator[UniswapV3LiquiditySnapshot]:
    source = DatabaseSnapshot(
        chain_id=8453,
        database_path=resolve_database_path(),
    )
    try:
        yield UniswapV3LiquiditySnapshot(source=source)
    finally:
        source.close()


@pytest.mark.base
def test_snapshot_fixtures(
    empty_mainnet_snapshot_from_file: UniswapV3LiquiditySnapshot,
    mainnet_snapshot_at_block_12_369_870_from_file: UniswapV3LiquiditySnapshot,
    mainnet_snapshot_at_block_12_369_870_from_dir: UniswapV3LiquiditySnapshot,
    base_snapshot_from_database: UniswapV3LiquiditySnapshot,
): ...


@pytest.mark.base
def test_fetch_pool_from_database_snapshot(
    base_snapshot_from_database: UniswapV3LiquiditySnapshot,
    fork_base_full: AnvilFork,
):

    # TODO: improve test by constructing standalone database and testing against it
    # TODO: make sure that test database is upgraded with alembic

    for pool in [
        "0xe13514AaCc27a3dFd2ae0db6aDA4eF7658c1E435",
    ]:
        assert base_snapshot_from_database.tick_bitmap(pool) is not None
        assert base_snapshot_from_database.tick_data(pool) is not None


@pytest.mark.online_rpc
def test_apply_update_to_unknown_pool(
    empty_mainnet_snapshot_from_file: UniswapV3LiquiditySnapshot,
    fork_mainnet_full: AnvilFork,
):

    with pytest.raises(UnknownPool):
        empty_mainnet_snapshot_from_file.update(
            pool=ZERO_ADDRESS,
            tick_data={},
            tick_bitmap={},
        )


@pytest.mark.online_rpc
def test_liquidity_map_is_none_for_missing_pools(
    mainnet_snapshot_at_block_12_369_870_from_file: UniswapV3LiquiditySnapshot,
    mainnet_snapshot_at_block_12_369_870_from_dir: UniswapV3LiquiditySnapshot,
):
    assert mainnet_snapshot_at_block_12_369_870_from_file.tick_bitmap(ZERO_ADDRESS) is None
    assert mainnet_snapshot_at_block_12_369_870_from_file.tick_data(ZERO_ADDRESS) is None
    assert mainnet_snapshot_at_block_12_369_870_from_dir.tick_bitmap(ZERO_ADDRESS) is None
    assert mainnet_snapshot_at_block_12_369_870_from_dir.tick_data(ZERO_ADDRESS) is None


@pytest.mark.online_rpc
def test_snapshot_finds_known_pool(
    mainnet_snapshot_at_block_12_369_870_from_file: UniswapV3LiquiditySnapshot,
    mainnet_snapshot_at_block_12_369_870_from_dir: UniswapV3LiquiditySnapshot,
):
    wbtc_weth_pool = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"

    mainnet_snapshot_at_block_12_369_870_from_file.tick_bitmap(wbtc_weth_pool)
    mainnet_snapshot_at_block_12_369_870_from_file.tick_data(wbtc_weth_pool)
    mainnet_snapshot_at_block_12_369_870_from_dir.tick_bitmap(wbtc_weth_pool)
    mainnet_snapshot_at_block_12_369_870_from_dir.tick_data(wbtc_weth_pool)


def test_pools_property(
    mainnet_snapshot_at_block_12_369_870_from_file: UniswapV3LiquiditySnapshot,
    mainnet_snapshot_at_block_12_369_870_from_dir: UniswapV3LiquiditySnapshot,
):
    assert len(list(mainnet_snapshot_at_block_12_369_870_from_file.pools)) == 6
    assert len(list(mainnet_snapshot_at_block_12_369_870_from_dir.pools)) == 6
