"""Tests for pool_type_per_depth filtering in pathfinding.

Regression test: 2-hop pool_type_per_depth (e.g. V2-V3) must not raise
IndexError when max_depth exceeds the filter length. The DFS must honour
the implicit max depth from pool_type_per_depth's length.

Hermetic: the ``db`` fixture seeds its own temp-file SQLite through the Rust
write seams (``tests/helpers/database.py``), so the module is deterministic
and runs in the DEFAULT pytest suite — no production snapshot, no ``slow``
marker. The seeded chain is Ethereum (``ChainId.ETH``); ``WETH_ADDRESS`` is
its wrapped-native token.
"""

import pathlib

import pytest

from degenbot.checksum_cache import get_checksum_address
from degenbot.constants import WRAPPED_NATIVE_TOKENS
from degenbot.pathfinding import (
    PathfindingRequest,
    PathStep,
    PoolKind,
    find_paths,
    find_paths_async,
)
from degenbot.types.chain import ChainId
from tests.helpers.database import seed_v2_topology, seed_v3_topology

CHAIN_ID = ChainId.ETH
WETH_ADDRESS = WRAPPED_NATIVE_TOKENS[CHAIN_ID]

TOKEN_A = get_checksum_address("0x" + "aa" * 20)
TOKEN_B = get_checksum_address("0x" + "bb" * 20)
TOKEN_C = get_checksum_address("0x" + "cc" * 20)

# Pools for the 2-hop V2-V3 cycle WETH -(V2)-> A -(V3)-> WETH.
POOL_V2_WETH_A = get_checksum_address("0x" + "21" * 20)
POOL_V3_A_WETH = get_checksum_address("0x" + "31" * 20)
# Pools for the 3-hop V2-V2-V3 cycle WETH -(V2)-> B -(V2)-> C -(V3)-> WETH.
POOL_V2_WETH_B = get_checksum_address("0x" + "22" * 20)
POOL_V2_B_C = get_checksum_address("0x" + "23" * 20)
POOL_V3_C_WETH = get_checksum_address("0x" + "33" * 20)


def _step_ids(path: list[PathStep]) -> tuple[str, ...]:
    return tuple((step.hash or step.address) for step in path)


def _build_file_db(db_path: pathlib.Path) -> pathlib.Path:
    """Seed a mixed V2+V3 graph carrying both a 2-hop and a 3-hop cycle.

    Graph (nodes = tokens, edges = pools):

    - 2-hop cycle WETH<->TOKEN_A: a V2 ``WETH/A`` + a V3 ``A/WETH``.
    - 3-hop cycle WETH->TOKEN_B->TOKEN_C->WETH: a V2 ``WETH/B`` + a V2
      ``B/C`` + a V3 ``C/WETH``.

    A ``pool_type_per_depth`` filter therefore has real pools to select at
    both the 2-hop (V2-then-V3) and 3-hop (V2-V2-V3) depths.
    """
    seed_v2_topology(
        db_path,
        [
            (POOL_V2_WETH_A, WETH_ADDRESS, TOKEN_A),
            (POOL_V2_WETH_B, WETH_ADDRESS, TOKEN_B),
            (POOL_V2_B_C, TOKEN_B, TOKEN_C),
        ],
        chain_id=CHAIN_ID,
    )
    seed_v3_topology(
        db_path,
        [
            (POOL_V3_A_WETH, TOKEN_A, WETH_ADDRESS, 500, 10),
            (POOL_V3_C_WETH, TOKEN_C, WETH_ADDRESS, 500, 10),
        ],
        chain_id=CHAIN_ID,
    )
    return db_path


@pytest.fixture
def db(tmp_path: pathlib.Path) -> pathlib.Path:
    return _build_file_db(tmp_path / "pool_type_per_depth.db")


def _request(
    db: pathlib.Path,
    *,
    pool_type_per_depth: list[set[PoolKind]],
    max_depth: int | None,
) -> PathfindingRequest:
    return PathfindingRequest(
        database_path=db,
        chain_id=CHAIN_ID,
        start_tokens=[WETH_ADDRESS],
        end_tokens=[WETH_ADDRESS],
        min_depth=2,
        max_depth=max_depth,
        pool_types=[PoolKind.V2, PoolKind.V3],
        pool_type_per_depth=pool_type_per_depth,
    )


class TestPoolTypePerDepthBounds:
    """Ensure pool_type_per_depth with fewer entries than max_depth doesn't crash."""

    def test_two_hop_filter_with_max_depth_3(self, db):
        """2-hop pool_type_per_depth must not IndexError when max_depth=3.

        The bot's --permutation flag produces pool_type_per_depth from the
        permutation string length, but max_depth is fixed at 3. Before the
        fix, the DFS would index pool_type_per_depth[2] when the filter
        only had 2 entries, raising IndexError.
        """
        pool_type_per_depth = [
            {PoolKind.V2},  # depth 0: V2
            {PoolKind.V3},  # depth 1: V3
        ]
        paths = list(
            find_paths(request=_request(db, pool_type_per_depth=pool_type_per_depth, max_depth=3))
        )
        assert paths, "the seeded V2-V3 2-hop cycle must be found"
        # All paths should be exactly 2 hops
        for path in paths:
            assert len(path) == 2, f"Expected 2-hop path, got {len(path)} hops"
        # The only matching cycle is the seeded WETH -(V2)-> A -(V3)-> WETH one.
        assert {_step_ids(path) for path in paths} <= {
            (POOL_V2_WETH_A, POOL_V3_A_WETH),
            (POOL_V3_A_WETH, POOL_V2_WETH_A),
        }

    def test_two_hop_filter_with_max_depth_none(self, db):
        """2-hop pool_type_per_depth with max_depth=None must not IndexError."""
        pool_type_per_depth = [
            {PoolKind.V2},
            {PoolKind.V3},
        ]
        # max_depth=None would normally explore infinitely, but
        # pool_type_per_depth should cap it at 2 hops
        paths = list(
            find_paths(
                request=_request(db, pool_type_per_depth=pool_type_per_depth, max_depth=None)
            )
        )
        assert paths
        for path in paths:
            assert len(path) == 2

    async def test_two_hop_filter_async(self, db):
        """Async version of the 2-hop filter test."""
        pool_type_per_depth = [
            {PoolKind.V2},
            {PoolKind.V3},
        ]
        paths = [
            path
            async for path in find_paths_async(
                request=_request(db, pool_type_per_depth=pool_type_per_depth, max_depth=3)
            )
        ]
        assert paths
        for path in paths:
            assert len(path) == 2

    def test_three_hop_filter_respects_depth(self, db):
        """3-hop pool_type_per_depth should produce only 3-hop paths."""
        pool_type_per_depth = [
            {PoolKind.V2},  # depth 0
            {PoolKind.V2},  # depth 1
            {PoolKind.V3},  # depth 2
        ]
        paths = list(
            find_paths(request=_request(db, pool_type_per_depth=pool_type_per_depth, max_depth=3))
        )
        assert paths, "the seeded V2-V2-V3 3-hop cycle must be found"
        for path in paths:
            assert len(path) == 3
        assert all(
            tuple(step.type for step in path) == (PoolKind.V2, PoolKind.V2, PoolKind.V3)
            for path in paths
        )

    def test_filter_shorter_than_max_depth_cuts_early(self, db):
        """pool_type_per_depth of length 2 should not produce 3-hop paths."""
        pool_type_per_depth = [
            {PoolKind.V2},
            {PoolKind.V3},
        ]
        paths = list(
            find_paths(request=_request(db, pool_type_per_depth=pool_type_per_depth, max_depth=3))
        )
        assert paths
        # All paths must be exactly 2 hops (pool_type_per_depth length)
        assert all(len(path) == 2 for path in paths), (
            "pool_type_per_depth of length 2 should not produce longer paths"
        )
