#!/usr/bin/env python3
"""Generate the frozen V3/V4 snapshot fixture and JSON oracle."""

from __future__ import annotations

import json
import pathlib
import sqlite3
from contextlib import closing
from typing import Any

from degenbot.checksum_cache import get_checksum_address
from degenbot.db import (
    db_create_new_database,
    db_set_exchange_active,
    db_set_exchange_last_update_block,
    db_upsert_exchange,
    db_upsert_pool_manager,
    db_upsert_v3_pools,
    db_upsert_v4_pools,
)
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot
from degenbot.updater import V3PoolRowInput, V4PoolRowInput

FIXTURE_DIR = pathlib.Path(__file__).resolve().parent
DB_PATH = FIXTURE_DIR / "parity.db"
EXPECTED_PATH = FIXTURE_DIR / "parity_expected.json"
CHAIN = 8453
U128_MAX = str(2**128 - 1)
BIG = str(2**70)
V3_ADDRESS = get_checksum_address("0x7b8c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c")
V3_TOKEN0 = get_checksum_address("0x236aa50979d5f3de3bd1eeb40e81137f22ab794b")
V3_TOKEN1 = get_checksum_address("0xd9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca")
V4_TOKEN1 = get_checksum_address("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913")
V3_FACTORY = get_checksum_address("0x1ae92f98d07affd821725cd463c223c1e8c5a6d2")
V4_FACTORY = get_checksum_address("0x000000000004444c5dc75cb358380d8e63429569")
V4_MANAGER = get_checksum_address("0x498581ff718922c3f8e6a244956af099b2652b2b")
V4_STATE_VIEW = get_checksum_address("0x0000000000000000000000000000000000000001")
V4_HASH = "0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a"


def _build_db() -> None:
    for suffix in ("", "-wal", "-shm"):
        DB_PATH.with_name(DB_PATH.name + suffix).unlink(missing_ok=True)
    db_create_new_database(str(DB_PATH))
    exchange_v3 = db_upsert_exchange(str(DB_PATH), CHAIN, "aerodrome_v3", V3_FACTORY, None)
    db_set_exchange_active(str(DB_PATH), exchange_id=exchange_v3.id, active=True)
    db_set_exchange_last_update_block(str(DB_PATH), CHAIN, exchange_v3.id, 12_345_000)
    exchange_v4 = db_upsert_exchange(str(DB_PATH), CHAIN, "uniswap_v4", V4_FACTORY, None)
    db_set_exchange_active(str(DB_PATH), exchange_id=exchange_v4.id, active=True)
    db_set_exchange_last_update_block(str(DB_PATH), CHAIN, exchange_v4.id, 12_340_000)
    db_upsert_pool_manager(
        str(DB_PATH), V4_MANAGER, CHAIN, "uniswap_v4", V4_STATE_VIEW, exchange_v4.id
    )
    db_upsert_v3_pools(
        str(DB_PATH),
        CHAIN,
        "uniswap_v3",
        exchange_v3.id,
        1000,
        [V3PoolRowInput(V3_ADDRESS, V3_TOKEN0, V3_TOKEN1, 3, 60)],
    )
    db_upsert_v4_pools(
        str(DB_PATH),
        CHAIN,
        V4_MANAGER,
        1_000_000,
        [V4PoolRowInput(V4_HASH, "0x" + "00" * 20, V3_TOKEN0, V4_TOKEN1, 0, 60)],
    )
    with closing(sqlite3.connect(DB_PATH)) as connection:
        connection.executemany(
            "UPDATE erc20_tokens SET name = ?, symbol = ?, decimals = ? WHERE address = ?",
            [
                ("Token A", "TKA", 18, V3_TOKEN0),
                ("Token B", "TKB", 18, V3_TOKEN1),
                ("USD Coin", "USDC", 6, V4_TOKEN1),
            ],
        )
        v3_id = connection.execute(
            "SELECT id FROM pools WHERE address = ?", (V3_ADDRESS,)
        ).fetchone()[0]
        connection.executemany(
            "INSERT INTO liquidity_positions (pool_id, tick, liquidity_net, liquidity_gross) "
            "VALUES (?, ?, ?, ?)",
            [
                (v3_id, -100, "1000", "1000"),
                (v3_id, -10, BIG, BIG),
                (v3_id, 0, BIG, U128_MAX),
                (v3_id, 10, "0", "0"),
                (v3_id, 100, BIG, "1000"),
            ],
        )
        connection.executemany(
            "INSERT INTO initialization_maps (pool_id, word, bitmap) VALUES (?, ?, ?)",
            [(v3_id, 0, U128_MAX), (v3_id, 7, BIG)],
        )
        connection.execute(
            "UPDATE uniswap_v3_pools SET liquidity_update_block = 12345000, "
            "liquidity_update_log_index = 42 WHERE pool_id = ?",
            (v3_id,),
        )
        managed_id = connection.execute(
            "SELECT managed_pool_id FROM uniswap_v4_pools WHERE pool_hash = ?", (V4_HASH,)
        ).fetchone()[0]
        connection.executemany(
            "INSERT INTO managed_pool_liquidity_positions "
            "(managed_pool_id, tick, liquidity_net, liquidity_gross) VALUES (?, ?, ?, ?)",
            [
                (managed_id, -50, "1000", "1000"),
                (managed_id, 0, BIG, U128_MAX),
                (managed_id, 50, BIG, BIG),
            ],
        )
        connection.execute(
            "INSERT INTO managed_pool_initialization_maps (managed_pool_id, word, bitmap) "
            "VALUES (?, 0, ?)",
            (managed_id, BIG),
        )
        connection.execute(
            "UPDATE uniswap_v4_pools SET liquidity_update_block = 12340000, "
            "liquidity_update_log_index = 7 WHERE managed_pool_id = ?",
            (managed_id,),
        )
        connection.commit()
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")


def _jsonable(snap: dict | None) -> dict[str, Any] | None:
    if snap is None:
        return None
    return {
        "tick_bitmap": {str(k): {"bitmap": str(v.bitmap)} for k, v in snap["tick_bitmap"].items()},
        "tick_data": {
            str(k): {
                "liquidity_gross": str(v.liquidity_gross),
                "liquidity_net": str(v.liquidity_net),
            }
            for k, v in snap["tick_data"].items()
        },
    }


def _expected() -> dict[str, Any]:
    v3 = V3DatabaseSnapshot(chain_id=CHAIN, database_path=DB_PATH)
    v4 = V4DatabaseSnapshot(chain_id=CHAIN, database_path=DB_PATH)
    try:
        v3_map = v3.get_liquidity_map(V3_ADDRESS)
        v3_all = v3.get_all_liquidity_maps()
        v4_map = v4.get_liquidity_map(V4_MANAGER, V4_HASH)
        v4_all = v4.get_all_liquidity_maps()
        return {
            "chain_id": CHAIN,
            "v3_pool_address": V3_ADDRESS,
            "v3_liquidity_map": _jsonable(v3_map),
            "v3_all_liquidity_maps": {
                address: {str(tick): [str(gross), str(net)] for tick, (gross, net) in ticks.items()}
                for address, ticks in v3_all.items()
            },
            "v3_newest_block": v3.get_newest_block(),
            "v3_pools": sorted(v3.get_pools()),
            "v4_pool_manager": V4_MANAGER,
            "v4_pool_hash": V4_HASH,
            "v4_liquidity_map": _jsonable(v4_map),
            "v4_all_liquidity_maps": [
                {
                    "pool_manager": manager,
                    "pool_hash": pool_hash,
                    "ticks": {
                        str(tick): [str(gross), str(net)] for tick, (gross, net) in ticks.items()
                    },
                }
                for (manager, pool_hash), ticks in v4_all.items()
            ],
            "v4_newest_block": v4.get_newest_block(),
            "v4_pools": sorted(v4.get_pools()),
        }
    finally:
        v3.close()
        v4.close()


def main() -> None:
    _build_db()
    expected = _expected()
    EXPECTED_PATH.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")
    print(f"Wrote {DB_PATH} ({DB_PATH.stat().st_size} bytes)")
    print(f"Wrote {EXPECTED_PATH} ({EXPECTED_PATH.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
