"""Synthetic V2 round-trip: find_paths_async → build V2 pools against a shared
py_bot → EngineRegistry.register_path → real engine eager-solve.

Offline integration test of the registration surface. In-memory SQLite seeded
with synthetic V2 pools (topology only — reserves live in a side dict so we
can hand-craft a profitable cycle deterministically), fed to find_paths_async.
The returned pools are built against a single shared PyBot (ADR-006 D1 shared
core), registered with a real UniswapArbEngine, and a 2-hop WETH-A-WETH cycle
is registered via register_path. The engine eager-solves it and surfaces the
profitable result via latest_results.

Scope: V2 only (V3/V4 need SnapshotStore tick data the pathfinder DB doesn't
carry), registration + solve only (subscribe/stream/backfill/resume need RPC).
"""

from __future__ import annotations

import pathlib
from fractions import Fraction
from typing import TYPE_CHECKING

import pytest
from eth_typing import ChainId

import examples.eth_backrun_v2_v3_v4_rust as runner
from degenbot.constants import ZERO_ADDRESS
from degenbot.database.models import Erc20TokenTable, UniswapV2PoolTable
from degenbot.database.models.base import Base, ExchangeTable
from degenbot.database.operations import get_scoped_sqlite_session
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.degenbot_rs import PyBot, UniswapArbEngine
from degenbot.pathfinding import find_paths_async
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    from degenbot.erc20 import Erc20Token

CHAIN = ChainId.ETH

WETH_ADDR = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
TOKEN_A_ADDR = "0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"  # noqa: S105

# Two V2 pools pairing WETH<->A at different prices -> a profitable 2-hop
# cycle WETH->A->WETH exists. Reserves hand-crafted (mirroring the proven
# pattern in test_uniswap_arb_engine.py::TestRegisterAndSolvePath at 0.3%):
#   pool A (WETH=A token0/token1): sells 1 WETH for 2000 A (cheap WETH)
#   pool B (A=WETH token0/token1): buys  1 WETH for 1875 A (dear WETH)
# Arb: sell WETH in A (get 2000 A), buy WETH in B (cost 1875 A) -> profit.
POOL_A_ADDR = "0x1100000000000000000000000000000000000000"
POOL_B_ADDR = "0x1200000000000000000000000000000000000000"

_RESERVES: dict[str, tuple[int, int]] = {
    POOL_A_ADDR: (800 * 10**18, 1_600_000 * 10**18),  # (WETH, A)
    POOL_B_ADDR: (1_500_000 * 10**18, 800 * 10**18),  # (A, WETH)
}


def _build_in_memory_db() -> DatabaseSessionManager:
    """Seed an in-memory SQLite with two V2 pools forming a WETH-A-WETH cycle.

    Same pattern as tests/pathfinding/test_permutation_filter_min_depth.py.
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
        session.flush()

        weth = Erc20TokenTable(chain=CHAIN, address=WETH_ADDR, symbol="WETH")
        token_a = Erc20TokenTable(chain=CHAIN, address=TOKEN_A_ADDR, symbol="A")
        session.add_all([weth, token_a])
        session.flush()

        session.add(
            UniswapV2PoolTable(
                address=POOL_A_ADDR,
                chain=CHAIN,
                token0_id=weth.id,
                token1_id=token_a.id,
                exchange_id=exchange.id,
                fee_token0=3,
                fee_token1=3,
                fee_denominator=1000,
            )
        )
        session.add(
            UniswapV2PoolTable(
                address=POOL_B_ADDR,
                chain=CHAIN,
                token0_id=token_a.id,
                token1_id=weth.id,
                exchange_id=exchange.id,
                fee_token0=3,
                fee_token1=3,
                fee_denominator=1000,
            )
        )
        session.commit()
    finally:
        session.close()

    return DatabaseSessionManager(scoped)


@pytest.fixture
def db():
    return _build_in_memory_db()


async def test_synthetic_v2_round_trip_registers_and_eager_solves(db) -> None:
    """find_paths_async → build V2 pools against a shared py_bot → register_path
    over the resolved pools → real engine eager-solves the profitable cycle.

    Proves the full registration wiring end-to-end, offline: pathfinder
    discovers the cycle from the in-memory DB, pools are built against the
    shared bot, directions resolved, register_path + eager solve via the real
    engine adopting the shared BotState (ADR-006 D1).
    """
    shared_py_bot = PyBot()
    weth = make_erc20(
        shared_py_bot,
        WETH_ADDR,
        chain_id=1,
        name="Wrapped Ether",
        symbol="WETH",
        decimals=18,
    )
    token_a = make_erc20(
        shared_py_bot,
        TOKEN_A_ADDR,
        chain_id=1,
        name="A",
        symbol="A",
        decimals=18,
    )

    # Address → (token0, token1) ordering, so we can build the right pool from
    # a discovered PathStep address (the DB carries topology only).
    token_order: dict[str, tuple[Erc20Token, Erc20Token]] = {
        POOL_A_ADDR: (weth, token_a),
        POOL_B_ADDR: (token_a, weth),
    }

    # Discover the WETH→A→WETH 2-hop cycle from the in-memory DB.
    discovered = [
        path
        async for path in find_paths_async(
            db=db,
            chain_id=CHAIN,
            start_tokens=[WETH_ADDR],
            end_tokens=[WETH_ADDR],
            max_depth=2,
            pool_types=[UniswapV2PoolTable],
        )
    ]
    assert discovered, "pathfinder found no WETH-A-WETH cycle in the seeded DB"
    # Take the 2-hop WETH→A→WETH cycle.
    cycle = next(p for p in discovered if len(p) == 2)
    assert len(cycle) == 2

    # Build the discovered pools against the shared bot (reserves from the side
    # dict, token ordering from the table).
    pools = []
    for step in cycle:
        t0, t1 = token_order[step.address]
        pools.append(
            make_v2_pool(
                address=step.address,
                token0=t0,
                token1=t1,
                factory=ZERO_ADDRESS,
                fee_token0=Fraction(3, 1000),
                fee_token1=Fraction(3, 1000),
                reserves_token0=_RESERVES[step.address][0],
                reserves_token1=_RESERVES[step.address][1],
                state_block=18_000_000,
                py_bot=shared_py_bot,
            )
        )

    # The engine adopts the shared core (ADR-006 D1). The synthetic test's job
    # is registration/solve, not the bot= production path (covered by VQURUB's
    # FakeBot test) — use the engine seam directly with a bare shared PyBot.
    registry = runner.EngineRegistry(
        bot=None,
        engine=UniswapArbEngine(py_bot=shared_py_bot),
    )

    # Register the discovered pools (V2 path: just caches the shared pool_id).
    for pool in pools:
        registry.register_v2_pool(pool)

    # Resolve per-hop directions so the cycle closes (WETH→A→WETH).
    zfo_list = runner.resolve_directions(pools, WETH_ADDR)
    assert zfo_list is not None, "cycle does not close on WETH"

    path_id = registry.register_path(list(zip(pools, zfo_list, strict=True)))

    assert registry.engine.path_count() == 1
    # Eager solve should have surfaced a profitable result. The result tuple is
    # (path_id, optimal_input, profit, hop_outputs, consumed_inputs).
    results, _block = registry.engine.latest_results()
    path_ids = {entry[0] for entry in results}
    assert path_id in path_ids, f"eager solve did not surface path {path_id} in results: {results}"
