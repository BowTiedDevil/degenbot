import pytest
from eth_typing import ChainId

from degenbot.constants import WRAPPED_NATIVE_TOKENS, ZERO_ADDRESS
from degenbot.database.models.pools import (
    SwapbasedV2PoolTable,
    UniswapV3PoolTable,
    UniswapV4PoolTable,
)
from degenbot.pathfinding import PathStep, find_paths

BASE_CHAIN_ID = ChainId.BASE
WETH_BASE_ADDRESS = WRAPPED_NATIVE_TOKENS[BASE_CHAIN_ID]


pytestmark = pytest.mark.skip(reason="Skipping this entire file because it is very slow.")


def path_step_identifiers(path: list[PathStep]) -> tuple[str, ...]:
    return [(step.hash or step.address) for step in path]


def test_generic_algo_multiple_tokens():
    depth = 2

    # UniswapV4 pools hold both native and WETH pairs, so paths to and from both can be found using
    # it only
    pool_types: list[type] = [UniswapV4PoolTable]

    generic_paths_weth_to_weth = list(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=depth,
            pool_types=pool_types,
        )
    )
    assert generic_paths_weth_to_weth
    generic_paths_weth_to_native = list(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[ZERO_ADDRESS],
            max_depth=depth,
            pool_types=pool_types,
        )
    )
    assert generic_paths_weth_to_native
    generic_paths_weth_to_weth_or_native = list(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS, ZERO_ADDRESS],
            max_depth=depth,
            pool_types=pool_types,
        )
    )
    assert generic_paths_weth_to_weth_or_native

    assert len(generic_paths_weth_to_weth_or_native) == len(generic_paths_weth_to_weth) + len(
        generic_paths_weth_to_native
    )
    assert sorted(
        generic_paths_weth_to_weth_or_native,
        key=path_step_identifiers,
    ) == sorted(
        generic_paths_weth_to_weth + generic_paths_weth_to_native,
        key=path_step_identifiers,
    )

    generic_paths_native_to_weth = list(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[ZERO_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=depth,
            pool_types=pool_types,
        )
    )
    assert generic_paths_native_to_weth
    generic_paths_native_to_native = list(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[ZERO_ADDRESS],
            end_tokens=[ZERO_ADDRESS],
            max_depth=depth,
            pool_types=pool_types,
        )
    )
    assert generic_paths_native_to_native
    generic_paths_native_to_weth_or_native = list(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[ZERO_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS, ZERO_ADDRESS],
            max_depth=depth,
            pool_types=pool_types,
        )
    )
    assert generic_paths_native_to_weth_or_native

    assert len(generic_paths_native_to_weth_or_native) == len(generic_paths_native_to_weth) + len(
        generic_paths_native_to_native
    )
    assert sorted(
        generic_paths_native_to_weth_or_native,
        key=path_step_identifiers,
    ) == sorted(
        generic_paths_native_to_weth + generic_paths_native_to_native, key=path_step_identifiers
    )

    generic_paths_weth_or_native_to_weth_or_native = list(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS, ZERO_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS, ZERO_ADDRESS],
            max_depth=depth,
            pool_types=pool_types,
        )
    )
    assert generic_paths_weth_or_native_to_weth_or_native

    assert len(generic_paths_weth_or_native_to_weth_or_native) == len(
        generic_paths_weth_to_weth_or_native
    ) + len(generic_paths_native_to_weth_or_native)
    assert sorted(
        generic_paths_weth_or_native_to_weth_or_native,
        key=path_step_identifiers,
    ) == sorted(
        generic_paths_weth_to_weth_or_native + generic_paths_native_to_weth_or_native,
        key=path_step_identifiers,
    )


def test_three_pool_pathfinding_cycling_weth_generic_with_limited_types():
    depth = 3

    paths_found = 0
    for i, _ in enumerate(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            pool_types=[UniswapV3PoolTable],
            min_depth=depth,
            max_depth=depth,
        ),
        start=1,
    ):
        paths_found = i
        if paths_found % 10_000 == 0:
            print(f"Marker: {paths_found} paths found")

    print(f"Found {paths_found} {depth}-pool paths (WETH-X -> X-Y -> WETH-Y)")


def test_three_pool_pathfinding_cycling_weth_native_with_limited_types():
    depth = 3

    paths_found = 0
    for i, _ in enumerate(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS, ZERO_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS, ZERO_ADDRESS],
            pool_types=[UniswapV4PoolTable],
            min_depth=depth,
            max_depth=depth,
        ),
        start=1,
    ):
        paths_found = i
        if paths_found % 10_000 == 0:
            print(f"Marker: {paths_found} paths found")


def test_three_pool_pathfinding_cycling_weth():
    paths = list(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=3,
        )
    )
    assert paths
    print(f"Found {len(paths)} 3-pool paths (WETH-X -> X-Y -> WETH-Y)")


def test_four_pool_pathfinding_cycling_weth_with_limited_types():
    paths = list(
        find_paths(
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            pool_types=[
                SwapbasedV2PoolTable,
                # SushiswapV2PoolTable,
                # UniswapV4PoolTable,
            ],
            max_depth=4,
        )
    )
    assert paths
    print(f"Found {len(paths)} 4-pool paths (WETH)")
