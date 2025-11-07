from cchecksum import to_checksum_address

from degenbot.pathfinding import find_paths

BASE_CHAIN_ID = 8453
WETH_BASE_ADDRESS = to_checksum_address("0x4200000000000000000000000000000000000006")


def test_two_pool_pathfinding():
    paths = tuple(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_token=WETH_BASE_ADDRESS,
            end_token=WETH_BASE_ADDRESS,
            max_depth=2,
        )
    )
    assert paths
    print(f"Found {len(paths)} paths")
