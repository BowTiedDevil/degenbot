import pytest
from eth_typing import ChainId

from degenbot.config import _init_config
from degenbot.constants import WRAPPED_NATIVE_TOKENS, ZERO_ADDRESS
from degenbot.database.models.pools import (
    SwapbasedV2PoolTable,
    UniswapV2PoolTable,
    UniswapV3PoolTable,
    UniswapV4PoolTable,
)
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.pathfinding import PathStep, find_paths, find_paths_async

BASE_CHAIN_ID = ChainId.BASE
WETH_BASE_ADDRESS = WRAPPED_NATIVE_TOKENS[BASE_CHAIN_ID]


pytestmark = pytest.mark.slow(reason="Use -m 'slow' to run slow pathfinding tests")


def path_step_identifiers(path: list[PathStep]) -> tuple[str, ...]:
    return [(step.hash or step.address) for step in path]


@pytest.fixture
def db():
    """Provide the module-level database session manager."""
    cfg = _init_config()
    return DatabaseSessionManager(get_scoped_sqlite_session(database_path=cfg.database.path))


def test_two_pool_pathfinding_cycling_weth(db):
    paths = list(
        find_paths(
            db=db,
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=2,
            pool_types=[
                UniswapV2PoolTable,
                UniswapV3PoolTable,
            ],
        )
    )
    assert paths
    print(f"Found {len(paths)} 2-pool paths")


async def test_two_pool_pathfinding_cycling_weth_async(db):
    paths = [
        path
        async for path in find_paths_async(
            db=db,
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=2,
            pool_types=[
                UniswapV2PoolTable,
                UniswapV3PoolTable,
            ],
        )
    ]
    assert paths
    print(f"Found {len(paths)} 2-pool paths")


def test_generic_algo_multiple_tokens(db):
    depth = 2

    # UniswapV4 pools hold both native and WETH pairs, so paths to and from both can be found using
    # it only
    pool_types: list[type] = [UniswapV4PoolTable]

    generic_paths_weth_to_weth = list(
        find_paths(
            db=db,
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
            db=db,
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
            db=db,
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
            db=db,
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
            db=db,
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
            db=db,
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
            db=db,
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


def test_three_pool_pathfinding_cycling_weth_generic_with_limited_types(db):
    depth = 3

    paths_found = 0
    for i, _ in enumerate(
        find_paths(
            db=db,
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


def test_three_pool_pathfinding_cycling_weth_native_with_limited_types(db):
    depth = 3

    paths_found = 0
    for i, _ in enumerate(
        find_paths(
            db=db,
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


def test_three_pool_pathfinding_cycling_weth(db):
    paths = list(
        find_paths(
            db=db,
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=3,
        )
    )
    assert paths
    print(f"Found {len(paths)} 3-pool paths (WETH-X -> X-Y -> WETH-Y)")


def test_four_pool_pathfinding_cycling_weth_with_limited_types(db):
    paths = list(
        find_paths(
            db=db,
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


def test_whitelist_restricts_intermediate_tokens(db):
    """Token whitelist should restrict graph to only whitelisted intermediates."""
    # Without whitelist: should find paths
    all_paths = list(
        find_paths(
            db=db,
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=2,
            pool_types=[
                UniswapV2PoolTable,
                UniswapV3PoolTable,
            ],
        )
    )
    assert all_paths, "Should find paths without whitelist"

    # With whitelist containing only WETH: only WETH→WETH paths survive
    # (i.e. pools that are WETH/WETH, which shouldn't exist)
    weth_only_paths = list(
        find_paths(
            db=db,
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=2,
            pool_types=[
                UniswapV2PoolTable,
                UniswapV3PoolTable,
            ],
            allowed_intermediate_tokens=[WETH_BASE_ADDRESS],
        )
    )
    # WETH-only intermediate means only pools of form WETH/WETH exist,
    # which are degenerate and shouldn't appear. So result should be empty
    # or very small.
    assert len(weth_only_paths) < len(all_paths), (
        f"Whitelist should reduce path count: got {len(weth_only_paths)} vs {len(all_paths)}"
    )


def test_whitelist_none_is_same_as_no_whitelist(db):
    """Passing None for allowed_intermediate_tokens should behave identically to omitting it."""
    paths_no_arg = list(
        find_paths(
            db=db,
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=2,
            pool_types=[
                UniswapV2PoolTable,
                UniswapV3PoolTable,
            ],
        )
    )
    paths_none_arg = list(
        find_paths(
            db=db,
            chain_id=BASE_CHAIN_ID,
            start_tokens=[WETH_BASE_ADDRESS],
            end_tokens=[WETH_BASE_ADDRESS],
            max_depth=2,
            pool_types=[
                UniswapV2PoolTable,
                UniswapV3PoolTable,
            ],
            allowed_intermediate_tokens=None,
        )
    )
    assert len(paths_no_arg) == len(paths_none_arg), (
        f"None whitelist should match no-arg: {len(paths_none_arg)} vs {len(paths_no_arg)}"
    )
