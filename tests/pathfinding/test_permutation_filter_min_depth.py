"""Regression tests for the lower-bound (min-depth) behavior of
``pool_type_per_depth``.

Background: a ``pool_type_per_depth`` filter of length N constrains an
N-hop permutation (e.g. ``V2-V2-V3``). It must enforce BOTH bounds:

- Upper bound (already tested in ``test_pool_type_per_depth.py``): no path
  LONGER than N is yielded (the filter caps ``max_depth``).
- Lower bound (regressed here): no path SHORTER than N is yielded. Before
  the fix, ``find_paths``/``find_paths_async`` left ``min_depth`` at its
  caller-supplied default (2), so any 2-hop cycle whose two hops matched
  depths 0 and 1 of an N>=3 filter leaked through as a spurious path.

The existing ``test_three_hop_filter_respects_depth`` looked like coverage
for the lower bound but was a false positive: it ran against the BASE
production snapshot, whose graph happened to contain no 2-hop cycle
matching depths 0+1 of the chosen filter, so the leak path was unreachable
and the assertion ``len(path) == 3`` never tripped over a leaked path.

These tests build a small in-memory graph that deterministically contains
BOTH a 2-hop cycle (matching depths 0+1) and a 3-hop cycle, so the leak is
guaranteed to manifest if the lower bound is not enforced. They are
independent of any production snapshot.
"""

import pathlib

import pytest
from eth_typing import ChainId

from degenbot.constants import ZERO_ADDRESS
from degenbot.database.models import Erc20TokenTable, UniswapV2PoolTable
from degenbot.database.models.base import Base, ExchangeTable
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.pathfinding import find_paths, find_paths_async

CHAIN = ChainId.ETH  # value 1; arbitrary but conventional

WETH_ADDR = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
TOKEN_A_ADDR = "0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"  # noqa: S105
TOKEN_B_ADDR = "0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"  # noqa: S105

# Two DISTINCT V2 pools pairing WETH<->A, so a 2-hop cycle WETH->A->WETH
# exists using parallel edges (a multi-graph). On the buggy code with the
# default min_depth=2, this cycle matched depths 0 and 1 of a V2-V2-V2
# filter and leaked as a spurious 2-hop path.
POOL_WETH_A_1_ADDR = "0x1100000000000000000000000000000000000000"
POOL_WETH_A_2_ADDR = "0x1200000000000000000000000000000000000000"
# Pools completing the 3-hop cycle WETH->A->B->WETH.
POOL_A_B_ADDR = "0x2100000000000000000000000000000000000000"
POOL_B_WETH_ADDR = "0x3100000000000000000000000000000000000000"


def _build_in_memory_db() -> DatabaseSessionManager:
    """Build an in-memory SQLite DB seeded with a synthetic 4-pool V2 graph.

    Graph (nodes = tokens, edges = V2 pools):
        WETH ===pool1=== A
        WETH ===pool2=== A      (parallel edge -> 2-hop cycle WETH-A-WETH)
        A     ===pool3=== B
        B     ===pool4=== WETH   (completes 3-hop cycle WETH-A-B-WETH)
    """
    scoped = get_scoped_sqlite_session(database_path=pathlib.Path(":memory:"))
    Base.metadata.create_all(bind=scoped.bind)

    session = scoped()
    try:
        exchange = ExchangeTable(
            chain_id=CHAIN,
            name="test",
            active=True,
            factory=ZERO_ADDRESS,
        )
        session.add(exchange)
        session.flush()  # assigns exchange.id

        weth = Erc20TokenTable(chain=CHAIN, address=WETH_ADDR, symbol="WETH")
        token_a = Erc20TokenTable(chain=CHAIN, address=TOKEN_A_ADDR, symbol="A")
        token_b = Erc20TokenTable(chain=CHAIN, address=TOKEN_B_ADDR, symbol="B")
        session.add_all([weth, token_a, token_b])
        session.flush()  # assigns token ids

        def _pool(addr: str, t0: Erc20TokenTable, t1: Erc20TokenTable) -> UniswapV2PoolTable:
            # Joined-table inheritance: constructing the subclass inserts
            # both the ``pools`` base row (with the correct polymorphic
            # identity 'uniswap_v2') and the ``uniswap_v2_pools`` sub-row.
            pool = UniswapV2PoolTable(
                address=addr,
                chain=CHAIN,
                token0_id=t0.id,
                token1_id=t1.id,
                exchange_id=exchange.id,
                fee_token0=3,
                fee_token1=3,
                fee_denominator=1000,
            )
            session.add(pool)
            return pool

        _pool(POOL_WETH_A_1_ADDR, weth, token_a)
        _pool(POOL_WETH_A_2_ADDR, weth, token_a)
        _pool(POOL_A_B_ADDR, token_a, token_b)
        _pool(POOL_B_WETH_ADDR, token_b, weth)
        session.commit()
    finally:
        session.close()

    return DatabaseSessionManager(scoped)


@pytest.fixture
def db():
    return _build_in_memory_db()


def _v2v2v2_filter():
    return [{UniswapV2PoolTable}, {UniswapV2PoolTable}, {UniswapV2PoolTable}]


class TestPermutationFilterMinDepth:
    """Regress Bug B: a 3-depth filter must not leak 2-hop prefix cycles.

    Before the fix, ``pool_type_per_depth`` floored ``max_depth`` at its
    length but left ``min_depth`` at the caller default (2). A 2-hop cycle
    whose edges matched depths 0+1 was yielded as a spurious path. This
    deterministically reproduces that shape and asserts the leak is gone.
    """

    def test_three_hop_filter_yields_no_two_hop_cycles(self, db):
        """3-depth V2-V2-V2 filter must yield only 3-hop paths.

        The synthetic graph contains both a 2-hop cycle (WETH-A-WETH via
        the two parallel V2 pools) and a 3-hop cycle (WETH-A-B-WETH). The
        2-hop cycle matches the filter's depths 0 and 1, so the buggy code
        yielded it as a path.
        """
        paths = list(
            find_paths(
                db=db,
                chain_id=CHAIN,
                start_tokens=[WETH_ADDR],
                end_tokens=[WETH_ADDR],
                max_depth=3,
                pool_types=[UniswapV2PoolTable],
                pool_type_per_depth=_v2v2v2_filter(),
            )
        )

        # The core assertion: no path shorter than the filter length.
        for path in paths:
            assert len(path) == 3, (
                f"3-depth filter leaked a {len(path)}-hop path: {[s.address for s in path]}"
            )

        # And the expected 3-hop cycle is actually found (guards against a
        # future regression that floors min_depth but silently drops the
        # legitimate paths too).
        assert any(len(p) == 3 for p in paths), (
            f"3-depth filter yielded {len(paths)} paths, none 3-hop"
        )

    async def test_three_hop_filter_yields_no_two_hop_cycles_async(self, db):
        """Async variant — same regression against ``find_paths_async``.

        ``find_paths_async`` is the entry point used by the example backrun
        bot, where Bug B was originally observed.
        """
        paths = [
            p
            async for p in find_paths_async(
                db=db,
                chain_id=CHAIN,
                start_tokens=[WETH_ADDR],
                end_tokens=[WETH_ADDR],
                max_depth=3,
                pool_types=[UniswapV2PoolTable],
                pool_type_per_depth=_v2v2v2_filter(),
            )
        ]

        for path in paths:
            assert len(path) == 3, (
                f"3-depth filter leaked a {len(path)}-hop path: {[s.address for s in path]}"
            )
        assert any(len(p) == 3 for p in paths), (
            f"3-depth filter yielded {len(paths)} paths, none 3-hop"
        )
