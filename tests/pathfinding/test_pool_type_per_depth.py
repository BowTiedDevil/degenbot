"""Tests for pool_type_per_depth filtering in pathfinding.

Regression test: 2-hop pool_type_per_depth (e.g. V2-V3) must not raise
IndexError when max_depth exceeds the filter length. The DFS must honour
the implicit max depth from pool_type_per_depth's length.
"""

import pytest
from eth_typing import ChainId

from degenbot.config import _init_config
from degenbot.constants import WRAPPED_NATIVE_TOKENS
from degenbot.database.models.pools import UniswapV2PoolTable, UniswapV3PoolTable
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.pathfinding import find_paths, find_paths_async

BASE_CHAIN_ID = ChainId.BASE
WETH_BASE_ADDRESS = WRAPPED_NATIVE_TOKENS[BASE_CHAIN_ID]

pytestmark = pytest.mark.slow(reason="Use -m 'slow' to run slow pathfinding tests")


@pytest.fixture
def db():
    cfg = _init_config()
    return DatabaseSessionManager(get_scoped_sqlite_session(database_path=cfg.database.path))


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
            {UniswapV2PoolTable},  # depth 0: V2
            {UniswapV3PoolTable},  # depth 1: V3
        ]
        # This must not raise IndexError
        paths = list(
            find_paths(
                db=db,
                chain_id=BASE_CHAIN_ID,
                start_tokens=[WETH_BASE_ADDRESS],
                end_tokens=[WETH_BASE_ADDRESS],
                max_depth=3,  # exceeds pool_type_per_depth length
                pool_types=[UniswapV2PoolTable, UniswapV3PoolTable],
                pool_type_per_depth=pool_type_per_depth,
            )
        )
        # All paths should be exactly 2 hops
        for path in paths:
            assert len(path) == 2, f"Expected 2-hop path, got {len(path)} hops"

    def test_two_hop_filter_with_max_depth_none(self, db):
        """2-hop pool_type_per_depth with max_depth=None must not IndexError."""
        pool_type_per_depth = [
            {UniswapV2PoolTable},
            {UniswapV3PoolTable},
        ]
        # max_depth=None would normally explore infinitely, but
        # pool_type_per_depth should cap it at 2 hops
        paths = list(
            find_paths(
                db=db,
                chain_id=BASE_CHAIN_ID,
                start_tokens=[WETH_BASE_ADDRESS],
                end_tokens=[WETH_BASE_ADDRESS],
                max_depth=None,
                pool_types=[UniswapV2PoolTable, UniswapV3PoolTable],
                pool_type_per_depth=pool_type_per_depth,
            )
        )
        for path in paths:
            assert len(path) == 2

    async def test_two_hop_filter_async(self, db):
        """Async version of the 2-hop filter test."""
        pool_type_per_depth = [
            {UniswapV2PoolTable},
            {UniswapV3PoolTable},
        ]
        paths = [
            path
            async for path in find_paths_async(
                db=db,
                chain_id=BASE_CHAIN_ID,
                start_tokens=[WETH_BASE_ADDRESS],
                end_tokens=[WETH_BASE_ADDRESS],
                max_depth=3,
                pool_types=[UniswapV2PoolTable, UniswapV3PoolTable],
                pool_type_per_depth=pool_type_per_depth,
            )
        ]
        for path in paths:
            assert len(path) == 2

    def test_three_hop_filter_respects_depth(self, db):
        """3-hop pool_type_per_depth should produce only 3-hop paths."""
        pool_type_per_depth = [
            {UniswapV2PoolTable},  # depth 0
            {UniswapV2PoolTable},  # depth 1
            {UniswapV3PoolTable},  # depth 2
        ]
        paths = list(
            find_paths(
                db=db,
                chain_id=BASE_CHAIN_ID,
                start_tokens=[WETH_BASE_ADDRESS],
                end_tokens=[WETH_BASE_ADDRESS],
                max_depth=3,
                pool_types=[UniswapV2PoolTable, UniswapV3PoolTable],
                pool_type_per_depth=pool_type_per_depth,
            )
        )
        for path in paths:
            assert len(path) == 3

    def test_filter_shorter_than_max_depth_cuts_early(self, db):
        """pool_type_per_depth of length 2 should not produce 3-hop paths."""
        pool_type_per_depth = [
            {UniswapV2PoolTable},
            {UniswapV3PoolTable},
        ]
        paths = list(
            find_paths(
                db=db,
                chain_id=BASE_CHAIN_ID,
                start_tokens=[WETH_BASE_ADDRESS],
                end_tokens=[WETH_BASE_ADDRESS],
                max_depth=3,
                pool_types=[UniswapV2PoolTable, UniswapV3PoolTable],
                pool_type_per_depth=pool_type_per_depth,
            )
        )
        # All paths must be exactly 2 hops (pool_type_per_depth length)
        for path in paths:
            assert len(path) == 2, (
                f"pool_type_per_depth of length 2 should not produce {len(path)}-hop paths"
            )
