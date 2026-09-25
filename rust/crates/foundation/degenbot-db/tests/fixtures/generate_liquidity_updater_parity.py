#!/usr/bin/env python3
"""Generate frozen V3/V4 liquidity-updater fixtures and JSON oracles."""

from __future__ import annotations

import json
import pathlib
import shutil
import sqlite3
from contextlib import closing

from degenbot.abi import encode as abi_encode
from degenbot.checksum_cache import get_checksum_address
from degenbot.db import (
    db_create_new_database,
    db_fetch_exchange,
    db_fetch_exchange_by_name,
    db_set_exchange_active,
    db_upsert_exchange,
    db_upsert_pool_manager,
    db_upsert_v3_pools,
    db_upsert_v4_pools,
)
from degenbot.types.chain import ChecksummedAddress
from degenbot.types.rpc_types import LogReceipt
from degenbot.updater import V3PoolRowInput, V4PoolRowInput
from degenbot.updater.pool_updater_configs import (
    UNISWAP_V3_BURN_EVENT_HASH,
    UNISWAP_V3_MINT_EVENT_HASH,
    apply_v3_liquidity_updates,
    apply_v4_liquidity_updates,
)
from degenbot.utils.bytes import to_bytes

FIXTURE_DIR = pathlib.Path(__file__).resolve().parent
CHAIN = 1
TICK_SPACING = 10
V3_POOL_ADDRESS: ChecksummedAddress = get_checksum_address("0x" + "a" * 40)
V3_FACTORY: ChecksummedAddress = get_checksum_address("0x" + "f" * 40)
V4_POOL_MANAGER_ADDRESS: ChecksummedAddress = get_checksum_address("0x" + "b" * 40)
V4_POOL_HASH = "0x" + "c" * 64
V4_HOOKS = "0x" + "0" * 40
V4_MODIFY_TOPIC = to_bytes("0xf208f4912782fd25c7f114ca3723a2d5dd6f3bcc3ac8db5af63baa85f711d5ec")
SEED_TICKS = [(-10, 1_000_000, 1_000_000), (10, -1_000_000, 1_000_000), (12345, 500, 500)]
SEED_MAPS = [(-1, 1 << 255), (0, 1 << 1), (4, 1 << 210)]


def _checkpoint(path: pathlib.Path) -> None:
    with closing(sqlite3.connect(path)) as connection:
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")


def _v3_mint(block: int, index: int, lower: int, upper: int, amount: int) -> LogReceipt:
    return LogReceipt({
        "blockNumber": block,
        "logIndex": index,
        "address": V3_POOL_ADDRESS,
        "topics": [
            UNISWAP_V3_MINT_EVENT_HASH,
            to_bytes(b"\x00" * 12 + V3_POOL_ADDRESS.encode()),
            to_bytes(abi_encode(["int24"], [lower])),
            to_bytes(abi_encode(["int24"], [upper])),
        ],
        "data": to_bytes(
            abi_encode(
                ["address", "uint128", "uint256", "uint256"], ["0x" + "1" * 40, amount, 0, 0]
            )
        ),
    })


def _v3_burn(block: int, index: int, lower: int, upper: int, amount: int) -> LogReceipt:
    return LogReceipt({
        "blockNumber": block,
        "logIndex": index,
        "address": V3_POOL_ADDRESS,
        "topics": [
            UNISWAP_V3_BURN_EVENT_HASH,
            to_bytes(b"\x00" * 12 + V3_POOL_ADDRESS.encode()),
            to_bytes(abi_encode(["int24"], [lower])),
            to_bytes(abi_encode(["int24"], [upper])),
        ],
        "data": to_bytes(abi_encode(["uint128", "uint256", "uint256"], [amount, 0, 0])),
    })


def _v4_modify(block: int, index: int, lower: int, upper: int, delta: int) -> LogReceipt:
    return LogReceipt({
        "blockNumber": block,
        "logIndex": index,
        "address": V4_POOL_MANAGER_ADDRESS,
        "topics": [
            V4_MODIFY_TOPIC,
            to_bytes(b"\x00" * 12 + V4_POOL_MANAGER_ADDRESS.encode()),
            to_bytes(b"\x00" * 12 + V4_POOL_HASH[2:].encode()),
        ],
        "data": to_bytes(
            abi_encode(["int24", "int24", "int256", "bytes32"], [lower, upper, delta, b"\x00" * 32])
        ),
    })


class _Provider:
    chain_id = CHAIN


def _build_v3(path: pathlib.Path) -> None:
    path.unlink(missing_ok=True)
    db_create_new_database(str(path))
    exchange = db_upsert_exchange(str(path), CHAIN, "uniswap_v3", V3_FACTORY, None)
    db_set_exchange_active(str(path), exchange_id=exchange.id, active=True)
    db_upsert_v3_pools(
        str(path),
        CHAIN,
        "uniswap_v3",
        exchange.id,
        1_000_000,
        [
            V3PoolRowInput(
                V3_POOL_ADDRESS,
                get_checksum_address("0x" + "1" * 40),
                get_checksum_address("0x" + "2" * 40),
                0,
                TICK_SPACING,
            )
        ],
    )
    with closing(sqlite3.connect(path)) as connection:
        pool_id = connection.execute(
            "SELECT id FROM pools WHERE address = ?", (V3_POOL_ADDRESS,)
        ).fetchone()[0]
        connection.executemany(
            "INSERT INTO liquidity_positions (pool_id, tick, liquidity_net, liquidity_gross) VALUES (?, ?, ?, ?)",
            [(pool_id, tick, str(net), str(gross)) for tick, net, gross in SEED_TICKS],
        )
        connection.executemany(
            "INSERT INTO initialization_maps (pool_id, word, bitmap) VALUES (?, ?, ?)",
            [(pool_id, word, str(bitmap)) for word, bitmap in SEED_MAPS],
        )
        connection.commit()
    _checkpoint(path)


def _build_v4(path: pathlib.Path) -> None:
    path.unlink(missing_ok=True)
    db_create_new_database(str(path))
    exchange = db_upsert_exchange(str(path), CHAIN, "uniswap_v4", V4_POOL_MANAGER_ADDRESS, None)
    db_set_exchange_active(str(path), exchange_id=exchange.id, active=True)
    db_upsert_pool_manager(
        str(path), V4_POOL_MANAGER_ADDRESS, CHAIN, "uniswap_v4", None, exchange.id
    )
    db_upsert_v4_pools(
        str(path),
        CHAIN,
        V4_POOL_MANAGER_ADDRESS,
        1_000_000,
        [
            V4PoolRowInput(
                V4_POOL_HASH,
                V4_HOOKS,
                get_checksum_address("0x" + "1" * 40),
                get_checksum_address("0x" + "2" * 40),
                0,
                TICK_SPACING,
            )
        ],
    )
    with closing(sqlite3.connect(path)) as connection:
        managed_id = connection.execute("SELECT managed_pool_id FROM uniswap_v4_pools").fetchone()[
            0
        ]
        connection.executemany(
            "INSERT INTO managed_pool_liquidity_positions (managed_pool_id, tick, liquidity_net, liquidity_gross) VALUES (?, ?, ?, ?)",
            [(managed_id, tick, str(net), str(gross)) for tick, net, gross in SEED_TICKS],
        )
        connection.executemany(
            "INSERT INTO managed_pool_initialization_maps (managed_pool_id, word, bitmap) VALUES (?, ?, ?)",
            [(managed_id, word, str(bitmap)) for word, bitmap in SEED_MAPS],
        )
        connection.commit()
    _checkpoint(path)


def _dump_v3(path: pathlib.Path, pool_id: int) -> dict[str, object]:
    with closing(sqlite3.connect(path)) as connection:
        positions = connection.execute(
            "SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions WHERE pool_id=? ORDER BY tick",
            (pool_id,),
        ).fetchall()
        maps = connection.execute(
            "SELECT word, bitmap FROM initialization_maps WHERE pool_id=? ORDER BY word", (pool_id,)
        ).fetchall()
        marker = connection.execute(
            "SELECT liquidity_update_block, liquidity_update_log_index FROM uniswap_v3_pools WHERE pool_id=?",
            (pool_id,),
        ).fetchone()
    return {
        "positions": [
            {"tick": row[0], "liquidity_net": str(row[1]), "liquidity_gross": str(row[2])}
            for row in positions
        ],
        "initialization_maps": [{"word": row[0], "bitmap": str(row[1])} for row in maps],
        "liquidity_update_block": marker[0],
        "liquidity_update_log_index": marker[1],
    }


def _dump_v4(path: pathlib.Path, managed_id: int) -> dict[str, object]:
    with closing(sqlite3.connect(path)) as connection:
        positions = connection.execute(
            "SELECT tick, liquidity_net, liquidity_gross FROM managed_pool_liquidity_positions WHERE managed_pool_id=? ORDER BY tick",
            (managed_id,),
        ).fetchall()
        maps = connection.execute(
            "SELECT word, bitmap FROM managed_pool_initialization_maps WHERE managed_pool_id=? ORDER BY word",
            (managed_id,),
        ).fetchall()
        marker = connection.execute(
            "SELECT liquidity_update_block, liquidity_update_log_index FROM uniswap_v4_pools WHERE managed_pool_id=?",
            (managed_id,),
        ).fetchone()
    return {
        "positions": [
            {"tick": row[0], "liquidity_net": str(row[1]), "liquidity_gross": str(row[2])}
            for row in positions
        ],
        "initialization_maps": [{"word": row[0], "bitmap": str(row[1])} for row in maps],
        "liquidity_update_block": marker[0],
        "liquidity_update_log_index": marker[1],
    }


def _apply_v3(path: pathlib.Path) -> dict[str, object]:
    events = [
        _v3_mint(100, 0, -10, 10, 500_000),
        _v3_mint(100, 1, 100, 110, 250_000),
        _v3_burn(101, 0, -10, 10, 200_000),
        _v3_burn(102, 0, -10, 10, 1_300_000),
    ]
    with closing(sqlite3.connect(path)) as connection:
        pool_id, exchange_id = connection.execute(
            "SELECT id, exchange_id FROM pools WHERE address=?", (V3_POOL_ADDRESS,)
        ).fetchone()
    exchange = db_fetch_exchange(str(path), exchange_id)
    assert exchange is not None
    apply_v3_liquidity_updates(
        provider=_Provider(),
        pool_address=V3_POOL_ADDRESS,
        liquidity_events=events,
        exchanges_in_scope={exchange},
        database_path=str(path),
    )
    return _dump_v3(path, pool_id)


def _apply_v4(path: pathlib.Path) -> dict[str, object]:
    events = [
        _v4_modify(200, 0, -10, 10, 500_000),
        _v4_modify(200, 1, 100, 110, 250_000),
        _v4_modify(201, 0, -10, 10, -200_000),
        _v4_modify(202, 0, -10, 10, -1_300_000),
    ]
    manager = db_upsert_pool_manager(
        str(path),
        V4_POOL_MANAGER_ADDRESS,
        CHAIN,
        "uniswap_v4",
        None,
        db_fetch_exchange_by_name(str(path), CHAIN, "uniswap_v4").id,
    )
    with closing(sqlite3.connect(path)) as connection:
        managed_id = connection.execute("SELECT managed_pool_id FROM uniswap_v4_pools").fetchone()[
            0
        ]
    apply_v4_liquidity_updates(
        pool_id=to_bytes(V4_POOL_HASH),
        liquidity_events=events,
        pool_manager=manager,
        database_path=str(path),
    )
    return _dump_v4(path, managed_id)


def main() -> None:
    v3 = FIXTURE_DIR / "liquidity_updater_v3_initial.db"
    v4 = FIXTURE_DIR / "liquidity_updater_v4_initial.db"
    _build_v3(v3)
    _build_v4(v4)
    v3_copy = FIXTURE_DIR / "_liquidity_updater_v3_apply_copy.db"
    v4_copy = FIXTURE_DIR / "_liquidity_updater_v4_apply_copy.db"
    shutil.copy2(v3, v3_copy)
    shutil.copy2(v4, v4_copy)
    v3_expected = _apply_v3(v3_copy)
    v4_expected = _apply_v4(v4_copy)
    (FIXTURE_DIR / "liquidity_updater_v3_expected.json").write_text(
        json.dumps(
            {
                "chain_id": CHAIN,
                "pool_address": V3_POOL_ADDRESS,
                "tick_spacing": TICK_SPACING,
                **v3_expected,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    (FIXTURE_DIR / "liquidity_updater_v4_expected.json").write_text(
        json.dumps(
            {
                "chain_id": CHAIN,
                "pool_hash": V4_POOL_HASH,
                "pool_manager_chain": CHAIN,
                "tick_spacing": TICK_SPACING,
                **v4_expected,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    v3_copy.unlink(missing_ok=True)
    v4_copy.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
