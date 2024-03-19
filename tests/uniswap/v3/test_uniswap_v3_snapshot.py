import degenbot
import pytest
from degenbot import (
    UniswapV3BitmapAtWord,
    UniswapV3LiquidityAtTick,
    UniswapV3LiquidityEvent,
    UniswapV3LiquiditySnapshot,
)
from degenbot.uniswap.managers import UniswapV3LiquidityPoolManager

EMPTY_SNAPSHOT_FILENAME = "tests/uniswap/v3/empty_v3_liquidity_snapshot.json"
EMPTY_SNAPSHOT_BLOCK = 12_369_620  # Uniswap V3 factory was deployed on the next block, so use this as the initial zero state


@pytest.fixture
def zero_snapshot() -> UniswapV3LiquiditySnapshot:
    return UniswapV3LiquiditySnapshot(file=EMPTY_SNAPSHOT_FILENAME)


@pytest.fixture
def first_250_blocks_snapshot(
    fork_mainnet_archive: degenbot.AnvilFork,
) -> UniswapV3LiquiditySnapshot:
    degenbot.set_web3(fork_mainnet_archive.w3)
    snapshot = UniswapV3LiquiditySnapshot(file=EMPTY_SNAPSHOT_FILENAME)
    snapshot.fetch_new_liquidity_events(to_block=EMPTY_SNAPSHOT_BLOCK + 250, span=50)
    return snapshot


def test_create_snapshot_from_file_path():
    UniswapV3LiquiditySnapshot(file=EMPTY_SNAPSHOT_FILENAME)


def test_create_snapshot_from_file_handle():
    with open(EMPTY_SNAPSHOT_FILENAME) as file:
        UniswapV3LiquiditySnapshot(file)


def test_fetch_liquidity_events_first_250_blocks(
    first_250_blocks_snapshot: UniswapV3LiquiditySnapshot,
    fork_mainnet_archive: degenbot.AnvilFork,
):
    degenbot.set_web3(fork_mainnet_archive.w3)

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
    fork_mainnet_archive: degenbot.AnvilFork,
):
    degenbot.set_web3(fork_mainnet_archive.w3)

    for pool_address in [
        "0x1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801",
        "0x6c6Bc977E13Df9b0de53b251522280BB72383700",
        "0x7BeA39867e4169DBe237d55C8242a8f2fcDcc387",
        "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD",
        "0xC2e9F25Be6257c210d7Adf0D4Cd6E3E881ba25f8",
        "0x7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf",
    ]:
        first_250_blocks_snapshot.get_new_liquidity_updates(pool_address)
        assert first_250_blocks_snapshot._liquidity_events[pool_address] == []


def test_apply_update_to_snapshot(
    zero_snapshot: UniswapV3LiquiditySnapshot,
    fork_mainnet_archive: degenbot.AnvilFork,
):
    POOL_ADDRESS = "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD"

    degenbot.set_web3(fork_mainnet_archive.w3)

    tick_data = {
        253320: UniswapV3LiquidityAtTick(
            liquidityNet=34399999543676, liquidityGross=34399999543676, block=12369821
        ),
        264600: UniswapV3LiquidityAtTick(
            liquidityNet=-34399999543676, liquidityGross=34399999543676, block=12369821
        ),
        255540: UniswapV3LiquidityAtTick(
            liquidityNet=2154941425, liquidityGross=2154941425, block=12369846
        ),
        262440: UniswapV3LiquidityAtTick(
            liquidityNet=-2154941425, liquidityGross=2154941425, block=12369846
        ),
    }
    tick_bitmap = {
        16: UniswapV3BitmapAtWord(
            bitmap=11692013098732293937359713277596107809105402396672, block=12369846
        ),
        17: UniswapV3BitmapAtWord(bitmap=288230376155906048, block=12369846),
    }
    zero_snapshot.update_snapshot(
        pool=POOL_ADDRESS,
        tick_data=tick_data,
        tick_bitmap=tick_bitmap,
    )
    zero_snapshot.update_snapshot(
        pool=POOL_ADDRESS,
        tick_data=tick_data,
        tick_bitmap=tick_bitmap,
    )

    assert zero_snapshot.get_tick_data(POOL_ADDRESS) is tick_data
    assert zero_snapshot.get_tick_bitmap(POOL_ADDRESS) is tick_bitmap

    pool_manager = UniswapV3LiquidityPoolManager(
        factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        chain_id=1,
        snapshot=zero_snapshot,
    )
    pool = pool_manager.get_pool(POOL_ADDRESS)
    assert pool.tick_bitmap == tick_bitmap
    assert pool.tick_data == tick_data


def test_pool_manager_applies_snapshots(
    first_250_blocks_snapshot: UniswapV3LiquiditySnapshot,
    fork_mainnet_archive: degenbot.AnvilFork,
):
    degenbot.set_web3(fork_mainnet_archive.w3)

    # Build a pool manager to inject the liquidity events into the new pools as they are created
    pool_manager = UniswapV3LiquidityPoolManager(
        factory_address="0x1F98431c8aD98523631AE4a59f267346ea31F984",
        chain_id=1,
        snapshot=first_250_blocks_snapshot,
    )

    print(f"{first_250_blocks_snapshot._liquidity_events=}")

    # Check that the pending events were applied
    for pool_address in first_250_blocks_snapshot._liquidity_snapshot:
        pool = pool_manager.get_pool(pool_address)

        match pool.address:
            case "0x1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801":
                assert pool.tick_data == {
                    -50580: UniswapV3LiquidityAtTick(
                        liquidityNet=383995753785830744,
                        liquidityGross=383995753785830744,
                        block=12369739,
                    ),
                    -36720: UniswapV3LiquidityAtTick(
                        liquidityNet=-383995753785830744,
                        liquidityGross=383995753785830744,
                        block=12369739,
                    ),
                }
                assert pool.tick_bitmap == {
                    -4: UniswapV3BitmapAtWord(
                        bitmap=3064991081731777716716694054300618367237478244367204352,
                        block=12369739,
                    ),
                    -3: UniswapV3BitmapAtWord(
                        bitmap=91343852333181432387730302044767688728495783936, block=12369739
                    ),
                }
            case "0x6c6Bc977E13Df9b0de53b251522280BB72383700":
                assert pool.tick_data == {
                    -276330: UniswapV3LiquidityAtTick(
                        liquidityNet=3964498619038659,
                        liquidityGross=3964498619038659,
                        block=12369760,
                    ),
                    -276320: UniswapV3LiquidityAtTick(
                        liquidityNet=-3964498619038659,
                        liquidityGross=3964498619038659,
                        block=12369760,
                    ),
                    -276400: UniswapV3LiquidityAtTick(
                        liquidityNet=2698389804940873511,
                        liquidityGross=2698389804940873511,
                        block=12369823,
                    ),
                    -276250: UniswapV3LiquidityAtTick(
                        liquidityNet=-2698389804940873511,
                        liquidityGross=2698389804940873511,
                        block=12369823,
                    ),
                }
                assert pool.tick_bitmap == {
                    -108: UniswapV3BitmapAtWord(bitmap=8487168, block=12369823)
                }

            case "0x7BeA39867e4169DBe237d55C8242a8f2fcDcc387":
                assert pool.tick_data == {
                    192200: UniswapV3LiquidityAtTick(
                        liquidityNet=123809464957093, liquidityGross=123809464957093, block=12369811
                    ),
                    198000: UniswapV3LiquidityAtTick(
                        liquidityNet=-123809464957093,
                        liquidityGross=123809464957093,
                        block=12369811,
                    ),
                }
                assert pool.tick_bitmap == {
                    3: UniswapV3BitmapAtWord(
                        bitmap=6739986679341863419440115299426486514824618937839854009203971588096,
                        block=12369811,
                    )
                }
            case "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD":
                assert pool.tick_data == {
                    253320: UniswapV3LiquidityAtTick(
                        liquidityNet=34399999543676, liquidityGross=34399999543676, block=12369821
                    ),
                    264600: UniswapV3LiquidityAtTick(
                        liquidityNet=-34399999543676, liquidityGross=34399999543676, block=12369821
                    ),
                    255540: UniswapV3LiquidityAtTick(
                        liquidityNet=2154941425, liquidityGross=2154941425, block=12369846
                    ),
                    262440: UniswapV3LiquidityAtTick(
                        liquidityNet=-2154941425, liquidityGross=2154941425, block=12369846
                    ),
                }
                assert pool.tick_bitmap == {
                    16: UniswapV3BitmapAtWord(
                        bitmap=11692013098732293937359713277596107809105402396672, block=12369846
                    ),
                    17: UniswapV3BitmapAtWord(bitmap=288230376155906048, block=12369846),
                }
            case "0xC2e9F25Be6257c210d7Adf0D4Cd6E3E881ba25f8":
                assert pool.tick_data == {
                    -84120: UniswapV3LiquidityAtTick(
                        liquidityNet=80059851033970806503,
                        liquidityGross=80059851033970806503,
                        block=12369854,
                    ),
                    -78240: UniswapV3LiquidityAtTick(
                        liquidityNet=-80059851033970806503,
                        liquidityGross=80059851033970806503,
                        block=12369854,
                    ),
                }
                assert pool.tick_bitmap == {
                    -6: UniswapV3BitmapAtWord(
                        bitmap=6901746346790563787434755862298803523934049033832042530038157389332480,
                        block=12369854,
                    )
                }
            case "0x7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf":
                assert pool.tick_data == {
                    -10: UniswapV3LiquidityAtTick(
                        liquidityNet=21206360421978, liquidityGross=21206360421978, block=12369863
                    ),
                    10: UniswapV3LiquidityAtTick(
                        liquidityNet=-21206360421978, liquidityGross=21206360421978, block=12369863
                    ),
                }
                assert pool.tick_bitmap == {
                    -1: UniswapV3BitmapAtWord(
                        bitmap=57896044618658097711785492504343953926634992332820282019728792003956564819968,
                        block=12369863,
                    ),
                    0: UniswapV3BitmapAtWord(bitmap=2, block=12369863),
                }

    # Check that the injected events were removed from the queue
    for pool in first_250_blocks_snapshot._liquidity_events:
        assert first_250_blocks_snapshot._liquidity_events[pool] == []
