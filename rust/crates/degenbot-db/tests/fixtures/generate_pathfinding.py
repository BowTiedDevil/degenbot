#!/usr/bin/env python3
"""Generate the pathfinding parity fixture for degenbot-db.

Builds ``pathfinding.db`` — a REAL Alembic-stamped SQLite DB seeded with V2 +
V3 + V4 pools connecting overlapping tokens (some with degree >= 2, some with
degree 1 so the candidate-token filter is exercised), plus a V4 manager +
parallel edges.

Then runs the Python ``_prepare_graph`` + ``_get_tokens_with_min_degree``
oracle against the same DB and dumps the expected results to
``pathfinding_expected.json``. The Rust integration test
``tests/pathfinding_parity.rs`` opens the frozen ``pathfinding.db`` and asserts
the Rust read fns produce identical results.

Run once (or whenever the seed data should change)::

    uv run python rust/crates/degenbot-db/tests/fixtures/generate_pathfinding.py

Outputs (committed-ish — the .db is gitignored, regenerate locally):
    rust/crates/degenbot-db/tests/fixtures/pathfinding.db
    rust/crates/degenbot-db/tests/fixtures/pathfinding_expected.json
"""

from __future__ import annotations

import json
import pathlib

from sqlalchemy import select

from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    PoolManagerTable,
    UniswapV2PoolTable,
    UniswapV3PoolTable,
    UniswapV4PoolTable,
)
from degenbot.database.operations import create_new_sqlite_database, get_scoped_sqlite_session
from degenbot.pathfinding import build_path_graph

FIXTURE_DIR = pathlib.Path(__file__).resolve().parent
DB_PATH = FIXTURE_DIR / "pathfinding.db"
EXPECTED_PATH = FIXTURE_DIR / "pathfinding_expected.json"

CHAIN = 8453


def _addr(hex_str: str) -> str:
    return get_checksum_address(hex_str)


# Token addresses (deterministic, distinct).
WETH_ADDR = _addr("0x4200000000000000000000000000000000000006")
USDC_ADDR = _addr("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")
USDT_ADDR = _addr("0xcC30Df1a5BeDe10eD3f9B2f23B0fFd0bD2f2eC3F")
DAI_ADDR = _addr("0x50c7fCD0a6809b1c5f0c1c0b1c0c1c0c1c0c1c0c")
DEAD_ADDR = _addr("0x000000000000000000000000000000000000dEaD")

V2_FACTORY = _addr("0x0000000000000000000000000000000000000002")
V3_FACTORY = _addr("0x33128a8fC17869897dcE68Ed026d6946217E4f5F")
V4_FACTORY = _addr("0x000000000004444c5dc75cB358380D2e3dE08A90")


def _build_db() -> None:
    if DB_PATH.exists():
        DB_PATH.unlink()
    for side in (DB_PATH.with_suffix(".db-wal"), DB_PATH.with_suffix(".db-shm")):
        if side.exists():
            side.unlink()

    create_new_sqlite_database(DB_PATH)
    session = get_scoped_sqlite_session(DB_PATH)

    with session() as s:  # type: Session
        tokens = [
            Erc20TokenTable(
                chain=CHAIN, address=WETH_ADDR, name="WETH", symbol="WETH", decimals=18
            ),
            Erc20TokenTable(chain=CHAIN, address=USDC_ADDR, name="USDC", symbol="USDC", decimals=6),
            Erc20TokenTable(chain=CHAIN, address=USDT_ADDR, name="USDT", symbol="USDT", decimals=6),
            Erc20TokenTable(chain=CHAIN, address=DAI_ADDR, name="DAI", symbol="DAI", decimals=18),
            Erc20TokenTable(
                chain=CHAIN, address=DEAD_ADDR, name="Dead", symbol="DEAD", decimals=18
            ),
        ]
        s.add_all(tokens)
        s.flush()
        weth_id, usdc_id, usdt_id, dai_id, dead_id = (t.id for t in tokens)

        exchanges = [
            ExchangeTable(
                chain_id=CHAIN,
                name="uniswap_v2",
                active=True,
                last_update_block=10_000_000,
                factory=V2_FACTORY,
                deployer=None,
            ),
            ExchangeTable(
                chain_id=CHAIN,
                name="uniswap_v3",
                active=True,
                last_update_block=11_000_000,
                factory=V3_FACTORY,
                deployer=None,
            ),
            ExchangeTable(
                chain_id=CHAIN,
                name="uniswap_v4",
                active=True,
                last_update_block=12_000_000,
                factory=V4_FACTORY,
                deployer=None,
            ),
        ]
        s.add_all(exchanges)
        s.flush()
        ex_v2_id, ex_v3_id, ex_v4_id = (e.id for e in exchanges)
        s.commit()

    # V2 + V3 pools
    with session() as s:  # type: Session
        v2_pools = [
            UniswapV2PoolTable(
                address=_addr("0xAaAa000000000000000000000000000000000001"),
                chain=CHAIN,
                token0_id=weth_id,
                token1_id=usdc_id,
                exchange_id=ex_v2_id,
                fee_token0=30,
                fee_token1=30,
                fee_denominator=10000,
            ),
            UniswapV2PoolTable(
                address=_addr("0xAaAa000000000000000000000000000000000002"),
                chain=CHAIN,
                token0_id=weth_id,
                token1_id=usdt_id,
                exchange_id=ex_v2_id,
                fee_token0=30,
                fee_token1=30,
                fee_denominator=10000,
            ),
            UniswapV2PoolTable(
                address=_addr("0xAaAa000000000000000000000000000000000003"),
                chain=CHAIN,
                token0_id=usdc_id,
                token1_id=dai_id,
                exchange_id=ex_v2_id,
                fee_token0=30,
                fee_token1=30,
                fee_denominator=10000,
            ),
            # degree-1 DEAD token → this edge filtered out by candidate_tokens
            UniswapV2PoolTable(
                address=_addr("0xAaAa000000000000000000000000000000000004"),
                chain=CHAIN,
                token0_id=weth_id,
                token1_id=dead_id,
                exchange_id=ex_v2_id,
                fee_token0=30,
                fee_token1=30,
                fee_denominator=10000,
            ),
        ]
        s.add_all(v2_pools)

        v3_pools = [
            UniswapV3PoolTable(
                address=_addr("0xBbBb000000000000000000000000000000000005"),
                chain=CHAIN,
                token0_id=usdc_id,
                token1_id=dai_id,
                exchange_id=ex_v3_id,
                fee_token0=3,
                fee_token1=3,
                fee_denominator=1000,
                tick_spacing=60,
                liquidity_update_block=None,
                liquidity_update_log_index=None,
            ),
            UniswapV3PoolTable(
                address=_addr("0xBbBb000000000000000000000000000000000006"),
                chain=CHAIN,
                token0_id=usdt_id,
                token1_id=dai_id,
                exchange_id=ex_v3_id,
                fee_token0=3,
                fee_token1=3,
                fee_denominator=1000,
                tick_spacing=60,
                liquidity_update_block=None,
                liquidity_update_log_index=None,
            ),
            # parallel edge: WETH/USDC (same pair as V2 pool 1, different pool)
            UniswapV3PoolTable(
                address=_addr("0xBbBb000000000000000000000000000000000007"),
                chain=CHAIN,
                token0_id=weth_id,
                token1_id=usdc_id,
                exchange_id=ex_v3_id,
                fee_token0=3,
                fee_token1=3,
                fee_denominator=1000,
                tick_spacing=100,
                liquidity_update_block=None,
                liquidity_update_log_index=None,
            ),
        ]
        s.add_all(v3_pools)
        s.flush()
        s.commit()

    # V4 pools + manager
    with session() as s:  # type: Session
        pm = PoolManagerTable(
            address=_addr("0xCcCc0000000000000000000000000000000000F0"),
            chain=CHAIN,
            kind="uniswap_v4",
            state_view=_addr("0x0000000000000000000000000000000000000001"),
            exchange_id=ex_v4_id,
        )
        s.add(pm)
        s.flush()

        v4_pools = [
            UniswapV4PoolTable(
                kind="uniswap_v4",
                manager_id=pm.id,
                pool_hash="0x1111111111111111111111111111111111111111111111111111111111111111",
                hooks=_addr("0x0000000000000000000000000000000000000000"),
                currency0_id=weth_id,
                currency1_id=usdc_id,
                fee_currency0=3,
                fee_currency1=3,
                fee_denominator=1000,
                tick_spacing=60,
                liquidity_update_block=None,
                liquidity_update_log_index=None,
            ),
            UniswapV4PoolTable(
                kind="uniswap_v4",
                manager_id=pm.id,
                pool_hash="0x2222222222222222222222222222222222222222222222222222222222222222",
                hooks=_addr("0x0000000000000000000000000000000000000000"),
                currency0_id=usdt_id,
                currency1_id=dai_id,
                fee_currency0=3,
                fee_currency1=3,
                fee_denominator=1000,
                tick_spacing=60,
                liquidity_update_block=None,
                liquidity_update_log_index=None,
            ),
        ]
        s.add_all(v4_pools)
        s.commit()


_POOL_KIND_V2 = 0
# Mirrors Rust `V4_POOL_ID_OFFSET` (crates/degenbot-db/src/pathfinding.rs):
# V4 pool ids are namespaced above any realistic `pools.id` so a collided
# V2/V3 row cannot alias a V4 pool's edges (116k collisions measured live).
_V4_POOL_ID_OFFSET = 1 << 32
_POOL_KIND_V3 = 1
_POOL_KIND_V4 = 2

POOL_TYPES = [UniswapV2PoolTable, UniswapV3PoolTable, UniswapV4PoolTable]


def _dump_oracle() -> dict:
    """The oracle is the PRODUCTION FFI seam (build_path_graph) for everything
    candidate-filtered, plus raw selects for the unfiltered edge list.

    V4 ids carry the Rust V4_POOL_ID_OFFSET namespace in both the unfiltered
    edges and the seam output (managed_pools.id + 1<<32).
    """
    raw = build_path_graph(
        database_path=str(DB_PATH),
        chain_id=CHAIN,
        pool_kinds={0, 1, 2},
    )

    # Unfiltered edges (mirrors fetch_path_graph_edges exactly, incl. offset).
    session = get_scoped_sqlite_session(DB_PATH)
    edges: list[list[int]] = []
    with session() as s:  # type: Session
        for pool_type in POOL_TYPES:
            if pool_type is UniswapV4PoolTable:
                rows = s.execute(
                    select(
                        pool_type.id,
                        pool_type.currency0_id,
                        pool_type.currency1_id,
                    )
                    .join(pool_type.manager)
                    .where(pool_type.manager.has(chain=CHAIN)),
                ).all()
                for pid, c0, c1 in rows:
                    graph_pid = pid + _V4_POOL_ID_OFFSET
                    edges.append([c0, c1, graph_pid, _POOL_KIND_V4])
            else:
                kind = _POOL_KIND_V2 if pool_type is UniswapV2PoolTable else _POOL_KIND_V3
                rows = s.execute(
                    select(
                        pool_type.id,
                        pool_type.token0_id,
                        pool_type.token1_id,
                    ).where(pool_type.chain == CHAIN),
                ).all()
                for pid, t0, t1 in rows:
                    edges.append([t0, t1, pid, kind])
    edges.sort()

    # Candidate tokens, filtered edges, and the lookup maps are all read
    # straight from the production seam (single source of truth for the
    # offset contract).
    candidate_tokens = sorted(int(t) for t in raw["candidate_tokens"])
    filtered_edges = sorted([
        [int(t0), int(t1), int(pid), int(kind)] for (t0, t1, pid, kind) in raw["edges"]
    ])
    pool_id_to_kind = {str(int(pid)): int(k) for pid, k in raw["pool_id_to_kind"].items()}
    v2v3_addresses = {str(int(pid)): addr for pid, addr in raw["v2v3_addresses"].items()}
    v4_lookups = {str(int(pid)): [mgr, hsh] for pid, (mgr, hsh) in raw["v4_lookups"].items()}

    # Token-by-address for ALL tokens.
    all_addrs = [WETH_ADDR, USDC_ADDR, USDT_ADDR, DAI_ADDR, DEAD_ADDR]
    from degenbot.database.models.erc20 import Erc20TokenTable

    with session() as s:  # type: Session
        rows = s.execute(
            select(Erc20TokenTable.id, Erc20TokenTable.address).where(
                Erc20TokenTable.chain == CHAIN,
                Erc20TokenTable.address.in_(all_addrs),
            ),
        ).all()
        token_by_address = {addr: tid for tid, addr in rows}

    # Sort edges for deterministic JSON comparison.
    return {
        "chain_id": CHAIN,
        "edges": edges,
        "filtered_edges": filtered_edges,
        "candidate_tokens": candidate_tokens,
        "v2v3_addresses": v2v3_addresses,
        "v4_lookups": v4_lookups,
        "pool_id_to_kind": pool_id_to_kind,
        "token_by_address": {addr: int(tid) for addr, tid in token_by_address.items()},
    }


def main() -> None:
    _build_db()
    oracle = _dump_oracle()
    EXPECTED_PATH.write_text(json.dumps(oracle, indent=2, sort_keys=True))
    print(f"Wrote {DB_PATH}")
    print(f"Wrote {EXPECTED_PATH}")
    print(f"  {len(oracle['edges'])} unfiltered edges")
    print(f"  {len(oracle['filtered_edges'])} filtered edges")
    print(f"  {len(oracle['candidate_tokens'])} candidate tokens (degree >= 2)")


if __name__ == "__main__":
    main()
