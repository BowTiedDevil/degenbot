#!/usr/bin/env python3

"""Generate the frozen V3/V4 liquidity-updater parity fixtures + JSON goldens.

The fixtures are a pair per family: a seed DB (`*_initial.db`) carrying three
committed tick positions, and an expected-state golden (`*_expected.json`)
carrying the pool's post-sequence positions + update marker.

The golden is FROZEN, not derived: the fixture event sequence was applied once
by the updater implementation that owned it, and these are the rows that run
produced. Regenerating the golden from the current implementation would pin the
implementation against itself, so this script only WRITES the frozen state —
`liquidity_updater_parity.rs` then proves the Rust `compute` + `persist` pair
reproduces it from the seed DB.

Run: `uv run python rust/crates/foundation/degenbot-db/tests/fixtures/generate_liquidity_updater_parity.py`
"""

from __future__ import annotations

import json
import pathlib
import shutil
import sqlite3
from contextlib import closing

from degenbot.checksum_cache import get_checksum_address
from degenbot.db import (
    db_set_exchange_active,
    db_upgrade_database,
    db_upsert_exchange,
    db_upsert_pool_manager,
    db_upsert_v3_pools,
    db_upsert_v4_pools,
)
from degenbot.types.chain import ChecksummedAddress
from degenbot.updater import V3PoolRowInput, V4PoolRowInput

FIXTURE_DIR = pathlib.Path(__file__).resolve().parent
CHAIN = 1
TICK_SPACING = 10
V3_POOL_ADDRESS: ChecksummedAddress = get_checksum_address("0x" + "a" * 40)
V3_FACTORY: ChecksummedAddress = get_checksum_address("0x" + "f" * 40)
V4_POOL_MANAGER_ADDRESS: ChecksummedAddress = get_checksum_address("0x" + "b" * 40)
V4_POOL_HASH = "0x" + "c" * 64
V4_HOOKS = "0x" + "0" * 40

# The seed DB's committed map: (tick, liquidity_net, liquidity_gross).
SEED_TICKS = [(-10, 1_000_000, 1_000_000), (10, -1_000_000, 1_000_000), (12345, 500, 500)]

# The frozen post-sequence state. The sequence mints -10..10 (500_000) and
# 100..110 (250_000), burns 200_000 from -10..10, then burns the remaining
# 1_300_000 — draining the -10..10 pair entirely. The 100..110 pair and the
# untouched 12345 seed survive.
FROZEN_TICKS = [(100, 250_000, 250_000), (110, -250_000, 250_000), (12345, 500, 500)]
V3_FROZEN_MARKER = (102, 0)
V4_FROZEN_MARKER = (202, 0)


def _checkpoint(path: pathlib.Path) -> None:
    with closing(sqlite3.connect(path)) as connection:
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")


def _build_v3(path: pathlib.Path) -> None:
    path.unlink(missing_ok=True)
    db_upgrade_database(str(path))
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
        connection.commit()
    _checkpoint(path)


def _build_v4(path: pathlib.Path) -> None:
    path.unlink(missing_ok=True)
    db_upgrade_database(str(path))
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
        connection.commit()
    _checkpoint(path)


def _dump_v3(path: pathlib.Path, pool_id: int) -> dict[str, object]:
    with closing(sqlite3.connect(path)) as connection:
        positions = connection.execute(
            "SELECT tick, liquidity_net, liquidity_gross FROM liquidity_positions WHERE pool_id=? ORDER BY tick",
            (pool_id,),
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
        "liquidity_update_block": marker[0],
        "liquidity_update_log_index": marker[1],
    }


def _dump_v4(path: pathlib.Path, managed_id: int) -> dict[str, object]:
    with closing(sqlite3.connect(path)) as connection:
        positions = connection.execute(
            "SELECT tick, liquidity_net, liquidity_gross FROM managed_pool_liquidity_positions WHERE managed_pool_id=? ORDER BY tick",
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
        "liquidity_update_block": marker[0],
        "liquidity_update_log_index": marker[1],
    }


def _write_frozen_v3(path: pathlib.Path) -> dict[str, object]:
    """Write the frozen post-sequence V3 state onto the seed DB copy."""
    with closing(sqlite3.connect(path)) as connection:
        pool_id = connection.execute(
            "SELECT id FROM pools WHERE address=?", (V3_POOL_ADDRESS,)
        ).fetchone()[0]
        connection.execute("DELETE FROM liquidity_positions WHERE pool_id=?", (pool_id,))
        connection.executemany(
            "INSERT INTO liquidity_positions (pool_id, tick, liquidity_net, liquidity_gross) VALUES (?, ?, ?, ?)",
            [(pool_id, tick, str(net), str(gross)) for tick, net, gross in FROZEN_TICKS],
        )
        block, log_index = V3_FROZEN_MARKER
        connection.execute(
            "UPDATE uniswap_v3_pools SET liquidity_update_block=?, liquidity_update_log_index=? WHERE pool_id=?",
            (block, log_index, pool_id),
        )
        connection.commit()
    return _dump_v3(path, pool_id)


def _write_frozen_v4(path: pathlib.Path) -> dict[str, object]:
    """Write the frozen post-sequence V4 state onto the seed DB copy."""
    with closing(sqlite3.connect(path)) as connection:
        managed_id = connection.execute("SELECT managed_pool_id FROM uniswap_v4_pools").fetchone()[
            0
        ]
        connection.execute(
            "DELETE FROM managed_pool_liquidity_positions WHERE managed_pool_id=?",
            (managed_id,),
        )
        connection.executemany(
            "INSERT INTO managed_pool_liquidity_positions (managed_pool_id, tick, liquidity_net, liquidity_gross) VALUES (?, ?, ?, ?)",
            [(managed_id, tick, str(net), str(gross)) for tick, net, gross in FROZEN_TICKS],
        )
        block, log_index = V4_FROZEN_MARKER
        connection.execute(
            "UPDATE uniswap_v4_pools SET liquidity_update_block=?, liquidity_update_log_index=? WHERE managed_pool_id=?",
            (block, log_index, managed_id),
        )
        connection.commit()
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
    v3_expected = _write_frozen_v3(v3_copy)
    v4_expected = _write_frozen_v4(v4_copy)

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
