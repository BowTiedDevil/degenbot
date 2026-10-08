"""Hermetic seeded pathfinding graph tests.

Every test builds its own temp-file SQLite through the Rust write seams
(``tests/helpers/database.py``) and searches it, so the module is fully
deterministic and runs in the DEFAULT pytest suite — no production snapshot,
no ``slow`` marker.

The seeded chain is Ethereum (``ChainId.ETH``); ``WETH_ADDRESS`` is its
wrapped-native token and ``NATIVE_ADDRESS`` is the V4 native-currency
sentinel (``ZERO_ADDRESS``). V4 pools that hold the native/WETH pair let a
search reach both tokens from either boundary.
"""

import pathlib

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import WRAPPED_NATIVE_TOKENS, ZERO_ADDRESS
from degenbot.pathfinding import (
    PathfindingRequest,
    PathStep,
    PoolKind,
    find_paths,
    find_paths_async,
)
from degenbot.types.chain import ChainId
from tests.helpers.database import seed_v2_topology, seed_v3_topology, seed_v4_topology

CHAIN_ID = ChainId.ETH
WETH_ADDRESS = WRAPPED_NATIVE_TOKENS[CHAIN_ID]
NATIVE_ADDRESS = ZERO_ADDRESS

# Pool addresses as module constants so tests can name the known seeded paths.
POOL_V2_WETH_A = get_checksum_address("0x" + "21" * 20)
POOL_V2_WETH_E = get_checksum_address("0x" + "22" * 20)
POOL_V2_E_F = get_checksum_address("0x" + "23" * 20)
POOL_V2_F_G = get_checksum_address("0x" + "24" * 20)
POOL_V2_G_WETH = get_checksum_address("0x" + "25" * 20)
POOL_V3_A_WETH = get_checksum_address("0x" + "31" * 20)

# Distinct synthetic tokens, one hex byte repeated to 20 bytes.
TOKEN_A = get_checksum_address("0x" + "aa" * 20)
TOKEN_B = get_checksum_address("0x" + "bb" * 20)
TOKEN_C = get_checksum_address("0x" + "cc" * 20)
TOKEN_D = get_checksum_address("0x" + "dd" * 20)
TOKEN_E = get_checksum_address("0x" + "ee" * 20)
TOKEN_F = get_checksum_address("0x" + "ff" * 20)
TOKEN_G = get_checksum_address("0x" + "11" * 20)
POOL_MANAGER = get_checksum_address("0x" + "99" * 20)


def _addr(byte: str) -> str:
    return get_checksum_address("0x" + byte * 20)


def _step_ids(path: list[PathStep]) -> tuple[str, ...]:
    return tuple((step.hash or step.address) for step in path)


def _seed_three_family_db(db_path: pathlib.Path) -> pathlib.Path:
    """Seed V2 + V3 + V4 pools giving each search request a reachable cycle.

    Graph (nodes = tokens, edges = pools):

    - V2 2-hop cycle WETH<->TOKEN_A (``WETH/A`` + ``A/WETH``).
    - V3 3-hop cycle WETH->TOKEN_B->TOKEN_C->WETH.
    - V2 4-hop cycle WETH->TOKEN_E->TOKEN_F->TOKEN_G->WETH.
    - V4 native cycle ZERO<->WETH (``ZERO/WETH`` + ``WETH/ZERO``) plus a
      second V4 2-hop cycle through TOKEN_D, and the ZERO<->TOKEN_D pair, so
      a V4-only search reaches WETH and ZERO from both boundaries.
    """
    seed_v2_topology(
        db_path,
        [
            (POOL_V2_WETH_A, WETH_ADDRESS, TOKEN_A),
            (POOL_V2_WETH_E, WETH_ADDRESS, TOKEN_E),
            (POOL_V2_E_F, TOKEN_E, TOKEN_F),
            (POOL_V2_F_G, TOKEN_F, TOKEN_G),
            (POOL_V2_G_WETH, TOKEN_G, WETH_ADDRESS),
        ],
        chain_id=CHAIN_ID,
    )
    seed_v3_topology(
        db_path,
        [
            (_addr("31"), TOKEN_A, WETH_ADDRESS, 500, 10),
            (_addr("32"), WETH_ADDRESS, TOKEN_B, 500, 10),
            (_addr("33"), TOKEN_B, TOKEN_C, 500, 10),
            (_addr("34"), TOKEN_C, WETH_ADDRESS, 500, 10),
        ],
        chain_id=CHAIN_ID,
    )
    seed_v4_topology(
        db_path,
        [
            ("0x" + "0" * 63 + "1", NATIVE_ADDRESS, WETH_ADDRESS, ZERO_ADDRESS, 3000),
            ("0x" + "0" * 63 + "2", WETH_ADDRESS, TOKEN_D, ZERO_ADDRESS, 3000),
            ("0x" + "0" * 63 + "3", TOKEN_D, WETH_ADDRESS, ZERO_ADDRESS, 3000),
            ("0x" + "0" * 63 + "4", NATIVE_ADDRESS, TOKEN_D, ZERO_ADDRESS, 3000),
            ("0x" + "0" * 63 + "5", TOKEN_D, NATIVE_ADDRESS, ZERO_ADDRESS, 3000),
        ],
        pool_manager_address=POOL_MANAGER,
        chain_id=CHAIN_ID,
    )
    return db_path


@pytest.fixture
def db(tmp_path: pathlib.Path) -> pathlib.Path:
    """A hermetic seeded DB (see ``_seed_three_family_db`` for the topology)."""
    return _seed_three_family_db(tmp_path / "pathfinding.db")


def _request(
    db: pathlib.Path,
    *,
    start_tokens: list[str],
    end_tokens: list[str],
    pool_types: list[PoolKind],
    min_depth: int = 2,
    max_depth: int | None = None,
    allowed_intermediate_tokens: list[str] | None = None,
) -> PathfindingRequest:
    return PathfindingRequest(
        database_path=db,
        chain_id=CHAIN_ID,
        start_tokens=start_tokens,
        end_tokens=end_tokens,
        min_depth=min_depth,
        max_depth=max_depth,
        pool_types=pool_types,
        allowed_intermediate_tokens=allowed_intermediate_tokens,
    )


def test_two_pool_pathfinding_cycling_weth(db: pathlib.Path) -> None:
    paths = list(
        find_paths(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V2, PoolKind.V3],
                max_depth=2,
            )
        )
    )
    assert paths, "the seeded V2/V3 2-hop cycle must be found"
    assert all(len(path) == 2 for path in paths)
    # The known seeded 2-hop cycle uses the WETH<->TOKEN_A pools (one V2, one V3).
    assert {_step_ids(path) for path in paths} == {
        (POOL_V2_WETH_A, POOL_V3_A_WETH),
        (POOL_V3_A_WETH, POOL_V2_WETH_A),
    }


async def test_two_pool_pathfinding_cycling_weth_async(db: pathlib.Path) -> None:
    paths = [
        path
        async for path in find_paths_async(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V2, PoolKind.V3],
                max_depth=2,
            )
        )
    ]
    assert paths, "the seeded V2/V3 2-hop cycle must be found"
    assert all(len(path) == 2 for path in paths)
    assert {_step_ids(path) for path in paths} == {
        (POOL_V2_WETH_A, POOL_V3_A_WETH),
        (POOL_V3_A_WETH, POOL_V2_WETH_A),
    }


def test_generic_algo_multiple_tokens(db: pathlib.Path) -> None:
    """V4 pools carry both the native sentinel and WETH, so set identities hold.

    Every assertion is an exact set identity between searches whose boundary
    token sets are unions: ``f(WETH->{WETH,NATIVE}) == f(WETH->WETH) +
    f(WETH->NATIVE)`` and so on. The seeded V4 pairs ZERO<->WETH and
    WETH/ZERO<->TOKEN_D reach both tokens from either boundary.
    """
    depth = 2
    pool_types: list[PoolKind] = [PoolKind.V4]

    def search(start: list[str], end: list[str]) -> list[list[PathStep]]:
        return list(
            find_paths(
                request=_request(
                    db,
                    start_tokens=start,
                    end_tokens=end,
                    pool_types=pool_types,
                    max_depth=depth,
                )
            )
        )

    weth_to_weth = search([WETH_ADDRESS], [WETH_ADDRESS])
    weth_to_native = search([WETH_ADDRESS], [NATIVE_ADDRESS])
    weth_to_weth_or_native = search([WETH_ADDRESS], [WETH_ADDRESS, NATIVE_ADDRESS])
    assert weth_to_weth
    assert weth_to_native
    assert weth_to_weth_or_native

    assert len(weth_to_weth_or_native) == len(weth_to_weth) + len(weth_to_native)
    assert sorted(weth_to_weth_or_native, key=_step_ids) == sorted(
        weth_to_weth + weth_to_native, key=_step_ids
    )

    native_to_weth = search([NATIVE_ADDRESS], [WETH_ADDRESS])
    native_to_native = search([NATIVE_ADDRESS], [NATIVE_ADDRESS])
    native_to_weth_or_native = search([NATIVE_ADDRESS], [WETH_ADDRESS, NATIVE_ADDRESS])
    assert native_to_weth
    assert native_to_native
    assert native_to_weth_or_native

    assert len(native_to_weth_or_native) == len(native_to_weth) + len(native_to_native)
    assert sorted(native_to_weth_or_native, key=_step_ids) == sorted(
        native_to_weth + native_to_native, key=_step_ids
    )

    weth_or_native_to_weth_or_native = search(
        [WETH_ADDRESS, NATIVE_ADDRESS], [WETH_ADDRESS, NATIVE_ADDRESS]
    )
    assert weth_or_native_to_weth_or_native

    assert len(weth_or_native_to_weth_or_native) == len(weth_to_weth_or_native) + len(
        native_to_weth_or_native
    )
    assert sorted(weth_or_native_to_weth_or_native, key=_step_ids) == sorted(
        weth_to_weth_or_native + native_to_weth_or_native, key=_step_ids
    )


def test_three_pool_pathfinding_cycling_weth_generic_with_limited_types(
    db: pathlib.Path,
) -> None:
    depth = 3
    paths = list(
        find_paths(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V3],
                min_depth=depth,
                max_depth=depth,
            )
        )
    )
    assert paths, "the seeded 3-hop V3 cycle must be found"
    assert all(len(path) == 3 for path in paths)


def test_three_pool_pathfinding_cycling_weth_native_with_limited_types(
    db: pathlib.Path,
) -> None:
    depth = 3
    paths = list(
        find_paths(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS, NATIVE_ADDRESS],
                end_tokens=[WETH_ADDRESS, NATIVE_ADDRESS],
                pool_types=[PoolKind.V4],
                min_depth=depth,
                max_depth=depth,
            )
        )
    )
    assert paths, "the seeded 3-hop V4 native cycle must be found"
    assert all(len(path) == 3 for path in paths)


def test_three_pool_pathfinding_cycling_weth(db: pathlib.Path) -> None:
    paths = list(
        find_paths(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V2, PoolKind.V3, PoolKind.V4],
                max_depth=3,
            )
        )
    )
    assert paths
    assert any(len(path) == 3 for path in paths), "a seeded 3-hop cycle must be found"


def test_four_pool_pathfinding_cycling_weth_with_limited_types(db: pathlib.Path) -> None:
    paths = list(
        find_paths(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V2],
                max_depth=4,
            )
        )
    )
    assert paths, "the seeded 4-hop V2 cycle must be found"
    assert all(len(path) == 4 for path in paths)
    assert {_step_ids(path) for path in paths} == {
        (POOL_V2_WETH_E, POOL_V2_E_F, POOL_V2_F_G, POOL_V2_G_WETH),
        (POOL_V2_G_WETH, POOL_V2_F_G, POOL_V2_E_F, POOL_V2_WETH_E),
    }


def test_whitelist_restricts_intermediate_tokens(db: pathlib.Path) -> None:
    """Token whitelist should restrict graph to only whitelisted intermediates."""
    all_paths = list(
        find_paths(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V2, PoolKind.V3],
                max_depth=2,
            )
        )
    )
    assert all_paths, "Should find paths without whitelist"

    # With the whitelist holding only WETH, no intermediate token qualifies,
    # so the seeded 2-hop cycles (which both route through TOKEN_A) vanish.
    weth_only_paths = list(
        find_paths(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V2, PoolKind.V3],
                max_depth=2,
                allowed_intermediate_tokens=[WETH_ADDRESS],
            )
        )
    )
    assert len(weth_only_paths) < len(all_paths), (
        f"Whitelist should reduce path count: got {len(weth_only_paths)} vs {len(all_paths)}"
    )
    assert not weth_only_paths


def test_whitelist_none_is_same_as_no_whitelist(db: pathlib.Path) -> None:
    """Passing None for allowed_intermediate_tokens should behave identically to omitting it."""
    paths_no_arg = list(
        find_paths(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V2, PoolKind.V3],
                max_depth=2,
            )
        )
    )
    paths_none_arg = list(
        find_paths(
            request=_request(
                db,
                start_tokens=[WETH_ADDRESS],
                end_tokens=[WETH_ADDRESS],
                pool_types=[PoolKind.V2, PoolKind.V3],
                max_depth=2,
                allowed_intermediate_tokens=None,
            )
        )
    )
    assert sorted(paths_no_arg, key=_step_ids) == sorted(paths_none_arg, key=_step_ids)
