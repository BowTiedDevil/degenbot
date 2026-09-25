"""Synthetic V2 round-trip: find_paths_async → build V2 pools against a shared
py_bot → EngineRegistry.register_path → real engine eager-solve.

Offline integration test of the registration surface. In-memory SQLite seeded
with synthetic V2 pools (topology only — reserves live in a side dict so we
can hand-craft a profitable cycle deterministically), fed to find_paths_async.
The returned pools are built against a single shared Bot (ADR-006 D1 shared
core), registered with a real ArbitrageEngine, and a 2-hop WETH-A-WETH cycle
is registered via register_path. The engine eager-solves it and surfaces the
profitable result via latest_results.

Scope: V2 only (V3/V4 need SnapshotStore tick data the pathfinder DB doesn't
carry), registration + solve only (subscribe/stream/backfill/resume need RPC).
"""

from __future__ import annotations

from fractions import Fraction
from typing import TYPE_CHECKING

import pytest

from degenbot._ffi import Bot
from degenbot.arbitrage.engine_registry import ArbitrageEngine, EngineRegistry
from degenbot.constants import ZERO_ADDRESS
from degenbot.pathfinding import PathfindingRequest, PoolKind, find_paths_async
from degenbot.runner.build_paths import resolve_directions
from degenbot.types.chain import ChainId
from tests.helpers.database import seed_v2_topology
from tests.helpers.erc20_factory import make_erc20
from tests.helpers.v2_pool_factory import make_v2_pool

if TYPE_CHECKING:
    import pathlib

    from degenbot.erc20 import Erc20Token

CHAIN = ChainId.ETH

WETH_ADDR = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
TOKEN_A_ADDR = "0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"  # ruff:ignore[hardcoded-password-string]

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


def _build_file_db(db_path: pathlib.Path) -> pathlib.Path:
    """Seed a file-backed temp SQLite with two V2 pools forming a WETH-A-WETH cycle.

    Same pattern as tests/pathfinding/test_permutation_filter_min_depth.py.
    """
    seed_v2_topology(
        db_path,
        [
            (POOL_A_ADDR, WETH_ADDR, TOKEN_A_ADDR),
            (POOL_B_ADDR, TOKEN_A_ADDR, WETH_ADDR),
        ],
        chain_id=CHAIN,
    )
    return db_path


@pytest.fixture
def db(tmp_path):
    return _build_file_db(tmp_path / "synthetic_v2_test.db")


async def test_synthetic_v2_round_trip_registers_and_eager_solves(db) -> None:
    """find_paths_async → build V2 pools against a shared py_bot → register_path
    over the resolved pools → real engine eager-solves the profitable cycle.

    Proves the full registration wiring end-to-end, offline: pathfinder
    discovers the cycle from the in-memory DB, pools are built against the
    shared bot, directions resolved, register_path + eager solve via the real
    engine adopting the shared BotState (ADR-006 D1).
    """
    shared_py_bot = Bot()
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

    # Discover the WETH→A→WETH 2-hop cycle from the file-backed DB.
    discovered = [
        path
        async for path in find_paths_async(
            request=PathfindingRequest(
                database_path=db,
                chain_id=CHAIN,
                start_tokens=[WETH_ADDR],
                end_tokens=[WETH_ADDR],
                max_depth=2,
                pool_types=[PoolKind.V2],
            )
        )
    ]
    assert discovered, "pathfinder found no WETH-A-WETH cycle in the seeded DB"
    # The pathfinder returns BOTH directions of the WETH→A→WETH cycle
    # (e.g. [pool A, pool B] and [pool B, pool A]); only one is profitable
    # given the asymmetric reserves. Discovery order is not stable across
    # pathfinder refactors (DFS iteration order, pruning, batch FFI), so
    # register every 2-hop cycle and assert the profitable direction surfaces
    # a result — never the lone first-discovered cycle.
    cycles = [p for p in discovered if len(p) == 2]
    assert cycles, "no 2-hop WETH-A-WETH cycle in the seeded DB"
    assert len(cycles) == 2, f"expected both cycle directions, got {len(cycles)}"

    # Build each discovered pool once against the shared bot (reserves from the
    # side dict, token ordering from the table). Both cycle directions share
    # the same pools, and the shared BotState panics on duplicate registration,
    # so dedup by address.
    pools_by_address: dict[str, object] = {}
    for cycle in cycles:
        for step in cycle:
            if step.address in pools_by_address:
                continue
            t0, t1 = token_order[step.address]
            pools_by_address[step.address] = make_v2_pool(
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

    # The engine adopts the shared core (ADR-006 D1). The synthetic test's job
    # is registration/solve, not the bot= production path (covered by VQURUB's
    # FakeBot test) — use the engine seam directly with a bare shared Bot.
    registry = EngineRegistry(
        bot=None,
        engine=ArbitrageEngine(py_bot=shared_py_bot),
    )

    # Register the discovered pools once (V2 path: just caches the shared
    # pool_id).
    for pool in pools_by_address.values():
        registry.register_v2_pool(pool)

    # Register every discovered 2-hop direction; collect each path_id. Only the
    # profitable direction should eager-solve.
    path_ids: list[int] = []
    for cycle in cycles:
        pools = [pools_by_address[step.address] for step in cycle]
        zfo_list = resolve_directions(pools, WETH_ADDR)
        assert zfo_list is not None, "cycle does not close on WETH"
        path_id, _created = registry.register_path(list(zip(pools, zfo_list, strict=True)))
        path_ids.append(path_id)

    assert registry.engine.path_count() == len(cycles)
    # Eager solve should have surfaced a profitable result for the profitable
    # cycle direction. The result tuple is
    # (path_id, optimal_input, profit, hop_outputs, consumed_inputs).
    results, _block = registry.engine.latest_results()
    result_path_ids = {entry[0] for entry in results}
    profitable = result_path_ids & set(path_ids)
    assert profitable, (
        f"eager solve did not surface a profitable path among {path_ids} in results: {results}"
    )
