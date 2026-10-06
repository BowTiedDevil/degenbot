from collections.abc import Iterator

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.config import resolve_database_path
from degenbot.constants import ZERO_ADDRESS
from degenbot.exceptions.pool import UnknownPool
from degenbot.fork import AnvilFork
from degenbot.uniswap.concentrated.types import BitmapAtWord, LiquidityAtTick
from degenbot.uniswap.trackers import UniswapV3PoolTracker
from degenbot.uniswap.v3_snapshot import (
    DatabaseSnapshot,
    IndividualJsonFileSnapshot,
    MonolithicJsonFileSnapshot,
    UniswapV3LiquiditySnapshot,
)
from tests.helpers.bot_factory import make_bot_with_provider

EMPTY_SNAPSHOT_FILENAME = "tests/uniswap/v3/empty_v3_liquidity_snapshot.json"
SNAPSHOT_AT_BLOCK_12_369_870_FILENAME = (
    "tests/uniswap/v3/mainnet_v3_liquidity_snapshot_block_21_369_870.json"
)
SNAPSHOT_AT_BLOCK_12_369_870_DIR = "tests/uniswap/v3/snapshot"


# Expected tracker state for each pool in the dir-format snapshot fixture,
# keyed by pool address: the tick map and bitmap words the recorded pending
# updates must produce.
_EXPECTED_DIR_SNAPSHOT_POOL_STATE = {
    "0x1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801": {
        "tick_data": {
            -50580: LiquidityAtTick(
                liquidity_net=383995753785830744,
                liquidity_gross=383995753785830744,
                block=12369739,
            ),
            -36720: LiquidityAtTick(
                liquidity_net=-383995753785830744,
                liquidity_gross=383995753785830744,
                block=12369739,
            ),
        },
        "tick_bitmap": {
            -4: BitmapAtWord(
                bitmap=3064991081731777716716694054300618367237478244367204352,
                block=12369739,
            ),
            -3: BitmapAtWord(
                bitmap=91343852333181432387730302044767688728495783936,
                block=12369739,
            ),
        },
    },
    "0x6c6Bc977E13Df9b0de53b251522280BB72383700": {
        "tick_data": {
            -276330: LiquidityAtTick(
                liquidity_net=3964498619038659,
                liquidity_gross=3964498619038659,
                block=12369760,
            ),
            -276320: LiquidityAtTick(
                liquidity_net=-3964498619038659,
                liquidity_gross=3964498619038659,
                block=12369760,
            ),
            -276400: LiquidityAtTick(
                liquidity_net=2698389804940873511,
                liquidity_gross=2698389804940873511,
                block=12369823,
            ),
            -276250: LiquidityAtTick(
                liquidity_net=-2698389804940873511,
                liquidity_gross=2698389804940873511,
                block=12369823,
            ),
        },
        "tick_bitmap": {-108: BitmapAtWord(bitmap=8487168, block=12369823)},
    },
    "0x7BeA39867e4169DBe237d55C8242a8f2fcDcc387": {
        "tick_data": {
            192200: LiquidityAtTick(
                liquidity_net=123809464957093,
                liquidity_gross=123809464957093,
                block=12369811,
            ),
            198000: LiquidityAtTick(
                liquidity_net=-123809464957093,
                liquidity_gross=123809464957093,
                block=12369811,
            ),
        },
        "tick_bitmap": {
            3: BitmapAtWord(
                bitmap=6739986679341863419440115299426486514824618937839854009203971588096,
                block=12369811,
            ),
        },
    },
    "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD": {
        "tick_data": {
            253320: LiquidityAtTick(
                liquidity_net=34399999543676,
                liquidity_gross=34399999543676,
                block=12369821,
            ),
            264600: LiquidityAtTick(
                liquidity_net=-34399999543676,
                liquidity_gross=34399999543676,
                block=12369821,
            ),
            255540: LiquidityAtTick(
                liquidity_net=2154941425,
                liquidity_gross=2154941425,
                block=12369846,
            ),
            262440: LiquidityAtTick(
                liquidity_net=-2154941425,
                liquidity_gross=2154941425,
                block=12369846,
            ),
        },
        "tick_bitmap": {
            16: BitmapAtWord(
                bitmap=11692013098732293937359713277596107809105402396672,
                block=12369846,
            ),
            17: BitmapAtWord(bitmap=288230376155906048, block=12369846),
        },
    },
    "0xC2e9F25Be6257c210d7Adf0D4Cd6E3E881ba25f8": {
        "tick_data": {
            -84120: LiquidityAtTick(
                liquidity_net=80059851033970806503,
                liquidity_gross=80059851033970806503,
                block=12369854,
            ),
            -78240: LiquidityAtTick(
                liquidity_net=-80059851033970806503,
                liquidity_gross=80059851033970806503,
                block=12369854,
            ),
        },
        "tick_bitmap": {
            -6: BitmapAtWord(
                bitmap=6901746346790563787434755862298803523934049033832042530038157389332480,
                block=12369854,
            ),
        },
    },
    "0x7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf": {
        "tick_data": {
            -10: LiquidityAtTick(
                liquidity_net=21206360421978,
                liquidity_gross=21206360421978,
                block=12369863,
            ),
            10: LiquidityAtTick(
                liquidity_net=-21206360421978,
                liquidity_gross=21206360421978,
                block=12369863,
            ),
        },
        "tick_bitmap": {
            -1: BitmapAtWord(
                bitmap=57896044618658097711785492504343953926634992332820282019728792003956564819968,
                block=12369863,
            ),
            0: BitmapAtWord(bitmap=2, block=12369863),
        },
    },
}


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
def test_apply_update_to_snapshot(
    empty_mainnet_snapshot_from_file: UniswapV3LiquiditySnapshot,
    fork_mainnet_full: AnvilFork,
):
    pool_address = get_checksum_address("0xCBCdF9626bC03E24f779434178A73a0B4bad62eD")

    bot = make_bot_with_provider(fork_mainnet_full.provider)

    tick_data = {
        253320: LiquidityAtTick(
            liquidity_net=34399999543676,
            liquidity_gross=34399999543676,
            block=12369821,
        ),
        264600: LiquidityAtTick(
            liquidity_net=-34399999543676,
            liquidity_gross=34399999543676,
            block=12369821,
        ),
        255540: LiquidityAtTick(
            liquidity_net=2154941425,
            liquidity_gross=2154941425,
            block=12369846,
        ),
        262440: LiquidityAtTick(
            liquidity_net=-2154941425,
            liquidity_gross=2154941425,
            block=12369846,
        ),
    }
    tick_bitmap = {
        16: BitmapAtWord(bitmap=11692013098732293937359713277596107809105402396672, block=12369846),
        17: BitmapAtWord(bitmap=288230376155906048, block=12369846),
    }
    empty_mainnet_snapshot_from_file.update(
        pool=pool_address,
        tick_data=tick_data,
        tick_bitmap=tick_bitmap,
    )

    pool_manager = UniswapV3PoolTracker(
        factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        bot=bot,
        snapshot=empty_mainnet_snapshot_from_file,
    )
    pool = pool_manager.get_pool(pool_address)
    assert pool.tick_data == tick_data
    assert pool.tick_bitmap == tick_bitmap


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


@pytest.mark.online_rpc
def test_pool_manager_applies_snapshot_from_dir(
    mainnet_snapshot_at_block_12_369_870_from_dir: UniswapV3LiquiditySnapshot,
    fork_mainnet_full: AnvilFork,
):
    bot = make_bot_with_provider(fork_mainnet_full.provider)

    # Build a pool manager to inject the liquidity events into the new pools as they are created
    pool_manager = UniswapV3PoolTracker(
        factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        bot=bot,
        snapshot=mainnet_snapshot_at_block_12_369_870_from_dir,
    )

    # Check that the pending events were applied
    for pool_address in mainnet_snapshot_at_block_12_369_870_from_dir.pools:
        pool = pool_manager.get_pool(pool_address)
        expected = _EXPECTED_DIR_SNAPSHOT_POOL_STATE.get(pool.address)
        if expected is None:
            err_msg = f"Unknown pool! {pool.address}"
            raise ValueError(err_msg)
        assert pool.tick_data == expected["tick_data"]
        for word, bitmap in expected["tick_bitmap"].items():
            assert pool.tick_bitmap[word] == bitmap

    # Check that the injected events were removed from the queue
    for pool_address in mainnet_snapshot_at_block_12_369_870_from_dir.pools:
        assert not mainnet_snapshot_at_block_12_369_870_from_dir.pending_updates(pool_address)


def test_pools_property(
    mainnet_snapshot_at_block_12_369_870_from_file: UniswapV3LiquiditySnapshot,
    mainnet_snapshot_at_block_12_369_870_from_dir: UniswapV3LiquiditySnapshot,
):
    assert len(list(mainnet_snapshot_at_block_12_369_870_from_file.pools)) == 6
    assert len(list(mainnet_snapshot_at_block_12_369_870_from_dir.pools)) == 6
