import pathlib

import pytest
from eth_utils.address import to_checksum_address
from web3 import Web3

from degenbot.anvil_fork import AnvilFork
from degenbot.config import set_web3
from degenbot.uniswap.managers import UniswapV3PoolManager
from degenbot.uniswap.types import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3LiquidityEvent,
)
from degenbot.uniswap.v3_snapshot import UniswapV3LiquiditySnapshot

EMPTY_SNAPSHOT_FILENAME = "tests/uniswap/v3/empty_v3_liquidity_snapshot.json"
EMPTY_SNAPSHOT_BLOCK = (
    # Uniswap V3 factory was deployed on the next block, so use this as the initial zero state
    12_369_620
)


@pytest.fixture
def empty_snapshot(ethereum_archive_node_web3) -> UniswapV3LiquiditySnapshot:
    set_web3(ethereum_archive_node_web3)
    return UniswapV3LiquiditySnapshot(file=EMPTY_SNAPSHOT_FILENAME)


@pytest.fixture
def first_250_blocks_snapshot(
    fork_mainnet: AnvilFork,
) -> UniswapV3LiquiditySnapshot:
    set_web3(fork_mainnet.w3)
    snapshot = UniswapV3LiquiditySnapshot(file=EMPTY_SNAPSHOT_FILENAME)
    snapshot.fetch_new_liquidity_events(to_block=EMPTY_SNAPSHOT_BLOCK + 250, span=50)
    return snapshot


def test_create_snapshot_from_file_path(ethereum_archive_node_web3: Web3):
    set_web3(ethereum_archive_node_web3)
    UniswapV3LiquiditySnapshot(file=EMPTY_SNAPSHOT_FILENAME)


def test_create_snapshot_from_file_handle(ethereum_archive_node_web3: Web3):
    set_web3(ethereum_archive_node_web3)
    with pathlib.Path(EMPTY_SNAPSHOT_FILENAME).open() as file:
        UniswapV3LiquiditySnapshot(file)


def test_fetch_liquidity_events_first_250_blocks(
    first_250_blocks_snapshot: UniswapV3LiquiditySnapshot,
    fork_mainnet: AnvilFork,
):
    set_web3(fork_mainnet.w3)

    # Liquidity snapshots for each pool will be empty, since they only reflect the starting
    # liquidity at the initial snapshot block
    assert first_250_blocks_snapshot._liquidity_snapshot == {
        "0x1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801": {},
        "0x6c6Bc977E13Df9b0de53b251522280BB72383700": {},
        "0x7BeA39867e4169DBe237d55C8242a8f2fcDcc387": {},
        "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD": {},
        "0xC2e9F25Be6257c210d7Adf0D4Cd6E3E881ba25f8": {},
        "0x7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf": {},
    }

    # Unprocessed events should be found for these pools
    assert first_250_blocks_snapshot._liquidity_events == {
        "0x1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801": [
            UniswapV3LiquidityEvent(
                block_number=12369739,
                liquidity=383995753785830744,
                tick_lower=-50580,
                tick_upper=-36720,
                tx_index=33,
            )
        ],
        "0x6c6Bc977E13Df9b0de53b251522280BB72383700": [
            UniswapV3LiquidityEvent(
                block_number=12369760,
                liquidity=3964498619038659,
                tick_lower=-276330,
                tick_upper=-276320,
                tx_index=82,
            ),
            UniswapV3LiquidityEvent(
                block_number=12369823,
                liquidity=2698389804940873511,
                tick_lower=-276400,
                tick_upper=-276250,
                tx_index=19,
            ),
        ],
        "0x7BeA39867e4169DBe237d55C8242a8f2fcDcc387": [
            UniswapV3LiquidityEvent(
                block_number=12369811,
                liquidity=123809464957093,
                tick_lower=192200,
                tick_upper=198000,
                tx_index=255,
            )
        ],
        "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD": [
            UniswapV3LiquidityEvent(
                block_number=12369821,
                liquidity=34399999543676,
                tick_lower=253320,
                tick_upper=264600,
                tx_index=17,
            ),
            UniswapV3LiquidityEvent(
                block_number=12369846,
                liquidity=2154941425,
                tick_lower=255540,
                tick_upper=262440,
                tx_index=119,
            ),
        ],
        "0xC2e9F25Be6257c210d7Adf0D4Cd6E3E881ba25f8": [
            UniswapV3LiquidityEvent(
                block_number=12369854,
                liquidity=80059851033970806503,
                tick_lower=-84120,
                tick_upper=-78240,
                tx_index=85,
            )
        ],
        "0x7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf": [
            UniswapV3LiquidityEvent(
                block_number=12369863,
                liquidity=21206360421978,
                tick_lower=-10,
                tick_upper=10,
                tx_index=43,
            )
        ],
    }


def test_get_new_liquidity_updates(
    first_250_blocks_snapshot: UniswapV3LiquiditySnapshot,
    fork_mainnet: AnvilFork,
):
    set_web3(fork_mainnet.w3)

    for pool_address in [
        "0x1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801",
        "0x6c6Bc977E13Df9b0de53b251522280BB72383700",
        "0x7BeA39867e4169DBe237d55C8242a8f2fcDcc387",
        "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD",
        "0xC2e9F25Be6257c210d7Adf0D4Cd6E3E881ba25f8",
        "0x7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf",
    ]:
        first_250_blocks_snapshot.get_new_liquidity_updates(pool_address)
        assert first_250_blocks_snapshot._liquidity_events[to_checksum_address(pool_address)] == []


def test_apply_update_to_snapshot(
    empty_snapshot: UniswapV3LiquiditySnapshot,
    fork_mainnet: AnvilFork,
):
    pool_address = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"

    set_web3(fork_mainnet.w3)

    tick_data = {
        253320: UniswapV3LiquidityAtTick(
            liquidity_net=34399999543676, liquidity_gross=34399999543676, block=12369821
        ),
        264600: UniswapV3LiquidityAtTick(
            liquidity_net=-34399999543676, liquidity_gross=34399999543676, block=12369821
        ),
        255540: UniswapV3LiquidityAtTick(
            liquidity_net=2154941425, liquidity_gross=2154941425, block=12369846
        ),
        262440: UniswapV3LiquidityAtTick(
            liquidity_net=-2154941425, liquidity_gross=2154941425, block=12369846
        ),
    }
    tick_bitmap = {
        16: UniswapV3BitmapAtWord(
            bitmap=11692013098732293937359713277596107809105402396672, block=12369846
        ),
        17: UniswapV3BitmapAtWord(bitmap=288230376155906048, block=12369846),
    }
    empty_snapshot.update_snapshot(
        pool=pool_address,
        tick_data=tick_data,
        tick_bitmap=tick_bitmap,
    )
    empty_snapshot.update_snapshot(
        pool=pool_address,
        tick_data=tick_data,
        tick_bitmap=tick_bitmap,
    )

    assert empty_snapshot.get_tick_data(pool_address) is tick_data
    assert empty_snapshot.get_tick_bitmap(pool_address) is tick_bitmap

    pool_manager = UniswapV3PoolManager(
        factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        chain_id=1,
        snapshot=empty_snapshot,
    )
    pool = pool_manager.get_pool(pool_address)
    for word in tick_bitmap:
        assert pool.tick_bitmap[word] == tick_bitmap[word]
    assert pool.tick_data == tick_data


def test_pool_manager_applies_snapshots(
    first_250_blocks_snapshot: UniswapV3LiquiditySnapshot,
    fork_mainnet: AnvilFork,
):
    set_web3(fork_mainnet.w3)

    # Build a pool manager to inject the liquidity events into the new pools as they are created
    pool_manager = UniswapV3PoolManager(
        factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        chain_id=1,
        snapshot=first_250_blocks_snapshot,
    )

    # Check that the pending events were applied
    for pool_address in first_250_blocks_snapshot._liquidity_snapshot:
        pool = pool_manager.get_pool(pool_address)

        match pool.address:
            case "0x1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801":
                assert pool.tick_data == {
                    -50580: UniswapV3LiquidityAtTick(
                        liquidity_net=383995753785830744,
                        liquidity_gross=383995753785830744,
                        block=12369739,
                    ),
                    -36720: UniswapV3LiquidityAtTick(
                        liquidity_net=-383995753785830744,
                        liquidity_gross=383995753785830744,
                        block=12369739,
                    ),
                }
                for word, bitmap in {
                    -4: UniswapV3BitmapAtWord(
                        bitmap=3064991081731777716716694054300618367237478244367204352,
                        block=12369739,
                    ),
                    -3: UniswapV3BitmapAtWord(
                        bitmap=91343852333181432387730302044767688728495783936, block=12369739
                    ),
                }.items():
                    assert pool.tick_bitmap[word] == bitmap
            case "0x6c6Bc977E13Df9b0de53b251522280BB72383700":
                assert pool.tick_data == {
                    -276330: UniswapV3LiquidityAtTick(
                        liquidity_net=3964498619038659,
                        liquidity_gross=3964498619038659,
                        block=12369760,
                    ),
                    -276320: UniswapV3LiquidityAtTick(
                        liquidity_net=-3964498619038659,
                        liquidity_gross=3964498619038659,
                        block=12369760,
                    ),
                    -276400: UniswapV3LiquidityAtTick(
                        liquidity_net=2698389804940873511,
                        liquidity_gross=2698389804940873511,
                        block=12369823,
                    ),
                    -276250: UniswapV3LiquidityAtTick(
                        liquidity_net=-2698389804940873511,
                        liquidity_gross=2698389804940873511,
                        block=12369823,
                    ),
                }
                for word, bitmap in {
                    -108: UniswapV3BitmapAtWord(bitmap=8487168, block=12369823)
                }.items():
                    assert pool.tick_bitmap[word] == bitmap

            case "0x7BeA39867e4169DBe237d55C8242a8f2fcDcc387":
                assert pool.tick_data == {
                    192200: UniswapV3LiquidityAtTick(
                        liquidity_net=123809464957093,
                        liquidity_gross=123809464957093,
                        block=12369811,
                    ),
                    198000: UniswapV3LiquidityAtTick(
                        liquidity_net=-123809464957093,
                        liquidity_gross=123809464957093,
                        block=12369811,
                    ),
                }
                for word, bitmap in {
                    3: UniswapV3BitmapAtWord(
                        bitmap=6739986679341863419440115299426486514824618937839854009203971588096,
                        block=12369811,
                    )
                }.items():
                    assert pool.tick_bitmap[word] == bitmap
            case "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD":
                assert pool.tick_data == {
                    253320: UniswapV3LiquidityAtTick(
                        liquidity_net=34399999543676, liquidity_gross=34399999543676, block=12369821
                    ),
                    264600: UniswapV3LiquidityAtTick(
                        liquidity_net=-34399999543676,
                        liquidity_gross=34399999543676,
                        block=12369821,
                    ),
                    255540: UniswapV3LiquidityAtTick(
                        liquidity_net=2154941425, liquidity_gross=2154941425, block=12369846
                    ),
                    262440: UniswapV3LiquidityAtTick(
                        liquidity_net=-2154941425, liquidity_gross=2154941425, block=12369846
                    ),
                }
                for word, bitmap in {
                    16: UniswapV3BitmapAtWord(
                        bitmap=11692013098732293937359713277596107809105402396672, block=12369846
                    ),
                    17: UniswapV3BitmapAtWord(bitmap=288230376155906048, block=12369846),
                }.items():
                    assert pool.tick_bitmap[word] == bitmap
            case "0xC2e9F25Be6257c210d7Adf0D4Cd6E3E881ba25f8":
                assert pool.tick_data == {
                    -84120: UniswapV3LiquidityAtTick(
                        liquidity_net=80059851033970806503,
                        liquidity_gross=80059851033970806503,
                        block=12369854,
                    ),
                    -78240: UniswapV3LiquidityAtTick(
                        liquidity_net=-80059851033970806503,
                        liquidity_gross=80059851033970806503,
                        block=12369854,
                    ),
                }
                for word, bitmap in {
                    -6: UniswapV3BitmapAtWord(
                        bitmap=6901746346790563787434755862298803523934049033832042530038157389332480,
                        block=12369854,
                    )
                }.items():
                    assert pool.tick_bitmap[word] == bitmap
            case "0x7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf":
                assert pool.tick_data == {
                    -10: UniswapV3LiquidityAtTick(
                        liquidity_net=21206360421978, liquidity_gross=21206360421978, block=12369863
                    ),
                    10: UniswapV3LiquidityAtTick(
                        liquidity_net=-21206360421978,
                        liquidity_gross=21206360421978,
                        block=12369863,
                    ),
                }
                for word, bitmap in {
                    -1: UniswapV3BitmapAtWord(
                        bitmap=57896044618658097711785492504343953926634992332820282019728792003956564819968,
                        block=12369863,
                    ),
                    0: UniswapV3BitmapAtWord(bitmap=2, block=12369863),
                }.items():
                    assert pool.tick_bitmap[word] == bitmap

    # Check that the injected events were removed from the queue
    for pool_address in first_250_blocks_snapshot._liquidity_events:
        assert first_250_blocks_snapshot._liquidity_events[pool_address] == []
