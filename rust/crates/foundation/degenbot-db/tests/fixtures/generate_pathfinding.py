#!/usr/bin/env python3
"""Generate the frozen pathfinding graph fixture and JSON oracle."""

from __future__ import annotations

import json
import pathlib
import sqlite3
from contextlib import closing

from degenbot.checksum_cache import get_checksum_address
from degenbot.db import (
    db_create_new_database,
    db_set_exchange_active,
    db_set_exchange_last_update_block,
    db_upsert_exchange,
    db_upsert_pool_manager,
    db_upsert_v2_pools,
    db_upsert_v3_pools,
    db_upsert_v4_pools,
)
from degenbot.pathfinding import PoolKind, build_path_graph
from degenbot.updater import V2PoolRowInput, V3PoolRowInput, V4PoolRowInput

FIXTURE_DIR = pathlib.Path(__file__).resolve().parent
DB_PATH = FIXTURE_DIR / "pathfinding.db"
EXPECTED_PATH = FIXTURE_DIR / "pathfinding_expected.json"
CHAIN = 8453
_V4_POOL_ID_OFFSET = 1 << 32

WETH = get_checksum_address("0x4200000000000000000000000000000000000006")
USDC = get_checksum_address("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")
USDT = get_checksum_address("0xcC30Df1a5BeDe10eD3f9B2f23B0fFd0bD2f2eC3F")
DAI = get_checksum_address("0x50c7fCD0a6809b1c5f0c1c0b1c0c1c0c1c0c1c0c")
DEAD = get_checksum_address("0x000000000000000000000000000000000000dEaD")
V2_FACTORY = get_checksum_address("0x0000000000000000000000000000000000000002")
V3_FACTORY = get_checksum_address("0x33128a8fC17869897dcE68Ed026d6946217E4f5F")
V4_FACTORY = get_checksum_address("0x000000000004444c5dc75cB358380D2e3dE08A90")
V4_MANAGER = get_checksum_address("0xCcCc0000000000000000000000000000000000F0")
ZERO = "0x" + "00" * 20


def _addr(value: str) -> str:
    return get_checksum_address(value)


def _build_db() -> None:
    for suffix in ("", "-wal", "-shm"):
        DB_PATH.with_name(DB_PATH.name + suffix).unlink(missing_ok=True)
    db_create_new_database(str(DB_PATH))
    exchanges = {}
    for name, factory, block in (
        ("uniswap_v2", V2_FACTORY, 10_000_000),
        ("uniswap_v3", V3_FACTORY, 11_000_000),
        ("uniswap_v4", V4_FACTORY, 12_000_000),
    ):
        exchange = db_upsert_exchange(str(DB_PATH), CHAIN, name, factory, None)
        db_set_exchange_active(str(DB_PATH), exchange_id=exchange.id, active=True)
        db_set_exchange_last_update_block(str(DB_PATH), CHAIN, exchange.id, block)
        exchanges[name] = exchange

    db_upsert_v2_pools(
        str(DB_PATH),
        CHAIN,
        "uniswap_v2",
        exchanges["uniswap_v2"].id,
        10_000,
        [
            V2PoolRowInput(_addr("0xAaAa000000000000000000000000000000000001"), WETH, USDC, 30, 30),
            V2PoolRowInput(_addr("0xAaAa000000000000000000000000000000000002"), WETH, USDT, 30, 30),
            V2PoolRowInput(_addr("0xAaAa000000000000000000000000000000000003"), USDC, DAI, 30, 30),
            V2PoolRowInput(_addr("0xAaAa000000000000000000000000000000000004"), WETH, DEAD, 30, 30),
        ],
    )
    db_upsert_v3_pools(
        str(DB_PATH),
        CHAIN,
        "uniswap_v3",
        exchanges["uniswap_v3"].id,
        1000,
        [
            V3PoolRowInput(_addr("0xBbBb000000000000000000000000000000000005"), USDC, DAI, 3, 60),
            V3PoolRowInput(_addr("0xBbBb000000000000000000000000000000000006"), USDT, DAI, 3, 60),
            V3PoolRowInput(_addr("0xBbBb000000000000000000000000000000000007"), WETH, USDC, 3, 100),
        ],
    )
    db_upsert_pool_manager(
        str(DB_PATH),
        V4_MANAGER,
        CHAIN,
        "uniswap_v4",
        _addr("0x0000000000000000000000000000000000000001"),
        exchanges["uniswap_v4"].id,
    )
    db_upsert_v4_pools(
        str(DB_PATH),
        CHAIN,
        V4_MANAGER,
        1000,
        [
            V4PoolRowInput("0x" + "11" * 32, ZERO, WETH, USDC, 3, 60),
            V4PoolRowInput("0x" + "22" * 32, ZERO, USDT, DAI, 3, 60),
        ],
    )


def _dump_oracle() -> dict:
    raw = build_path_graph(str(DB_PATH), CHAIN, {PoolKind.V2, PoolKind.V3, PoolKind.V4})
    edges: list[list[int]] = []
    token_by_address: dict[str, int] = {}
    with closing(sqlite3.connect(DB_PATH)) as connection:
        for pool_id, token0, token1 in connection.execute(
            "SELECT id, token0_id, token1_id FROM pools WHERE chain = ?", (CHAIN,)
        ):
            edges.append([
                token0,
                token1,
                pool_id,
                0
                if connection.execute(
                    "SELECT 1 FROM uniswap_v2_pools WHERE pool_id = ?", (pool_id,)
                ).fetchone()
                else 1,
            ])
        for pool_id, currency0, currency1 in connection.execute(
            "SELECT managed_pool_id, currency0_id, currency1_id FROM uniswap_v4_pools"
        ):
            edges.append([currency0, currency1, pool_id + _V4_POOL_ID_OFFSET, 2])
        token_by_address = dict(
            connection.execute(
                "SELECT address, id FROM erc20_tokens WHERE chain = ? AND address IN (?, ?, ?, ?, ?)",
                (CHAIN, WETH, USDC, USDT, DAI, DEAD),
            )
        )
    edges.sort()
    return {
        "chain_id": CHAIN,
        "edges": edges,
        "filtered_edges": sorted(
            [int(t0), int(t1), int(pool_id), int(kind)] for t0, t1, pool_id, kind in raw["edges"]
        ),
        "candidate_tokens": sorted(int(token) for token in raw["candidate_tokens"]),
        "v2v3_addresses": {
            str(int(pool_id)): address for pool_id, address in raw["v2v3_addresses"].items()
        },
        "v4_lookups": {
            str(int(pool_id)): [manager, pool_hash]
            for pool_id, (manager, pool_hash) in raw["v4_lookups"].items()
        },
        "pool_id_to_kind": {
            str(int(pool_id)): int(kind) for pool_id, kind in raw["pool_id_to_kind"].items()
        },
        "token_by_address": {
            address: int(token_id) for address, token_id in token_by_address.items()
        },
    }


def main() -> None:
    _build_db()
    oracle = _dump_oracle()
    EXPECTED_PATH.write_text(json.dumps(oracle, indent=2, sort_keys=True))
    print(f"Wrote {DB_PATH}")
    print(f"Wrote {EXPECTED_PATH}")


if __name__ == "__main__":
    main()
