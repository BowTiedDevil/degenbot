from degenbot.constants import WRAPPED_NATIVE_TOKENS, ZERO_ADDRESS
from degenbot.database.models.pools import UniswapV2PoolTable, UniswapV3PoolTable
from degenbot.pathfinding import find_paths

BASE_CHAIN_ID = 8453
WETH_BASE_ADDRESS = WRAPPED_NATIVE_TOKENS[BASE_CHAIN_ID]


def test_two_pool_pathfinding_cycling_weth():
    paths = tuple(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_token=WETH_BASE_ADDRESS,
            end_token=WETH_BASE_ADDRESS,
            max_depth=2,
        )
    )
    assert paths
    print(f"Found {len(paths)} paths (WETH)")


def test_two_pool_pathfinding_cycling_weth_with_limited_types():
    paths = tuple(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_token=WETH_BASE_ADDRESS,
            end_token=WETH_BASE_ADDRESS,
            pool_types=[UniswapV2PoolTable, UniswapV3PoolTable],
            max_depth=2,
        )
    )
    assert paths
    print(f"Found {len(paths)} paths (WETH, Uniswap V2 pools only)")


def test_two_pool_pathfinding_cycling_native():
    paths = tuple(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_token=ZERO_ADDRESS,
            end_token=ZERO_ADDRESS,
            max_depth=2,
        )
    )
    assert paths
    print(f"Found {len(paths)} paths (Native Ether)")


def test_two_pool_pathfinding_cycling_weth_native_equivalent():
    paths = tuple(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_token=WETH_BASE_ADDRESS,
            end_token=ZERO_ADDRESS,
            max_depth=2,
            equivalent_tokens=[
                (
                    WETH_BASE_ADDRESS,
                    ZERO_ADDRESS,
                )
            ],
        )
    )
    assert paths
    print(f"Found {len(paths)} paths (WETH -> Ether) with equivalents")

    paths = tuple(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_token=ZERO_ADDRESS,
            end_token=WETH_BASE_ADDRESS,
            max_depth=2,
            equivalent_tokens=[
                (
                    WETH_BASE_ADDRESS,
                    ZERO_ADDRESS,
                )
            ],
        )
    )
    assert paths
    print(f"Found {len(paths)} paths (Ether -> WETH) with equivalents")
