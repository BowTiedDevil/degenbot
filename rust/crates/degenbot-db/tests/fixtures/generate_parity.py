#!/usr/bin/env python3
"""Generate the cross-implementation parity fixture for degenbot-db.

Builds ``parity.db`` — a REAL Alembic-stamped SQLite DB (the same path the
production bot uses: ``create_new_sqlite_database`` runs ``Base.metadata.create_all``
+ ``alembic stamp head``) seeded with V3 + V4 pool data containing U256-range
liquidity values (including values exceeding ``i64::MAX`` and ``U256::MAX`` to
exercise the ``VARCHAR(78)`` ↔ ``U256`` boundary).

Then runs the Python ``DatabaseSnapshot`` parity oracle (V3 + V4) against the
same DB and dumps the expected results to ``parity_expected.json`` (U256 values
as decimal strings, addresses as EIP-55 checksum strings, V4 pool hashes as
0x-prefixed lowercase hex). The Rust integration test ``tests/parity.rs`` opens
the frozen ``parity.db`` and asserts byte-identical results.

Run once (or whenever the seed data should change)::

    uv run python rust/crates/degenbot-db/tests/fixtures/generate_parity.py

Outputs (committed):
    rust/crates/degenbot-db/tests/fixtures/parity.db
    rust/crates/degenbot-db/tests/fixtures/parity_expected.json
"""

from __future__ import annotations

import json
import pathlib
from typing import Any

from sqlalchemy import select
from sqlalchemy import text as sa_text
from sqlalchemy.orm import Session

from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.base import ExchangeTable
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.models.pools import (
    InitializationMapTable,
    LiquidityPoolTable,
    LiquidityPositionTable,
    ManagedLiquidityPoolTable,
    ManagedPoolInitializationMapTable,
    ManagedPoolLiquidityPositionTable,
    PoolManagerTable,
    UniswapV3PoolTable,
    UniswapV4PoolTable,
)
from degenbot.database.operations import create_new_sqlite_database, get_scoped_sqlite_session
from degenbot.uniswap.v3_snapshot import DatabaseSnapshot as V3DatabaseSnapshot
from degenbot.uniswap.v4_snapshot import DatabaseSnapshot as V4DatabaseSnapshot

FIXTURE_DIR = pathlib.Path(__file__).resolve().parent
DB_PATH = FIXTURE_DIR / "parity.db"
EXPECTED_PATH = FIXTURE_DIR / "parity_expected.json"

CHAIN = 8453

# U256 boundary values (decimal strings, mirroring IntMappedToString VARCHAR(78)).
# U256 boundary values (decimal strings, mirroring IntMappedToString VARCHAR(78)).
# The DB column is VARCHAR(78) (full 32-byte EVM range); the Rust `decode_u256`
# unit test covers U256::MAX. The *parity* fixture must stay within the Python
# oracle's `LiquidityAtTick` pydantic bounds (gross <= u128::MAX,
# net <= i128::MAX) so both implementations agree — these still exceed i64::MAX,
# exercising the VARCHAR(78) <-> U256 decimal-string round-trip.
U128_MAX = str(2**128 - 1)            # 340282366920938463463374607431768211455 (39 digits)
BIG_BEYOND_I64 = str(2**70)           # 1180591620717411303424 — exceeds i64::MAX
SMALL = "1000"
ZERO = "0"


def _build_db() -> None:
    if DB_PATH.exists():
        DB_PATH.unlink()
    # Side-files from a previous WAL run.
    for side in (DB_PATH.with_suffix(".db-wal"), DB_PATH.with_suffix(".db-shm")):
        if side.exists():
            side.unlink()

    create_new_sqlite_database(DB_PATH)
    session = get_scoped_sqlite_session(DB_PATH)

    with session() as s:  # type: Session
        # ── tokens ─────────────────────────────────────────────────────
        tok_a = Erc20TokenTable(
            chain=CHAIN,
            address=get_checksum_address("0x236aa50979d5f3de3bd1eeb40e81137f22ab794b"),
            name="Token A",
            symbol="TKA",
            decimals=18,
        )
        tok_b = Erc20TokenTable(
            chain=CHAIN,
            address=get_checksum_address("0xd9aaec86b65d86f6a7b5b1b0c42ffa531710b6ca"),
            name="Token B",
            symbol="TKB",
            decimals=18,
        )
        usdc = Erc20TokenTable(
            chain=CHAIN,
            address=get_checksum_address("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
            name="USD Coin",
            symbol="USDC",
            decimals=6,
        )
        s.add_all([tok_a, tok_b, usdc])
        s.flush()

        # ── exchanges (V3 + V4 names, with last_update_block) ──────────
        ex_v3 = ExchangeTable(
            chain_id=CHAIN,
            name="aerodrome_v3",
            active=True,
            last_update_block=12_345_000,
            factory=get_checksum_address("0x1ae92f98d07affd821725cd463c223c1e8c5a6d2"),
            deployer=None,
        )
        ex_v4 = ExchangeTable(
            chain_id=CHAIN,
            name="uniswap_v4",
            active=True,
            last_update_block=12_340_000,
            factory=get_checksum_address("0x000000000004444c5dc75cb358380d8e63429569"),
            deployer=None,
        )
        s.add_all([ex_v3, ex_v4])
        s.flush()
        # capture the auto-assigned ids before the session commits/expires them
        tok_a_id, tok_b_id, usdc_id = tok_a.id, tok_b.id, usdc.id
        ex_v3_id, ex_v4_id = ex_v3.id, ex_v4.id
        s.commit()

    # create the V3 subclass + V4 chain in their own transaction:
    with session() as s:  # type: Session
        v3 = UniswapV3PoolTable(
            address=get_checksum_address("0x7b8c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c"),
            chain=CHAIN,
            token0_id=tok_a_id,
            token1_id=tok_b_id,
            exchange_id=ex_v3_id,
            fee_token0=3,
            fee_token1=3,
            fee_denominator=1000,
            tick_spacing=60,
            liquidity_update_block=12_345_000,
            liquidity_update_log_index=42,
        )
        s.add(v3)
        s.flush()
        v3_id = s.scalar(
            select(LiquidityPoolTable).where(LiquidityPoolTable.address == v3.address)
        ).id

        # liquidity positions: U256-range values + negative ticks
        lp_positions = [
            (v3_id, -100, SMALL, SMALL),
            (v3_id, -10, BIG_BEYOND_I64, BIG_BEYOND_I64),  # > i64::MAX
            (v3_id, 0, BIG_BEYOND_I64, U128_MAX),          # gross at u128 boundary
            (v3_id, 10, ZERO, ZERO),
            (v3_id, 100, BIG_BEYOND_I64, SMALL),  # asymmetric gross/net
        ]
        s.add_all(
            LiquidityPositionTable(
                pool_id=pid,
                tick=tick,
                liquidity_net=int(net),
                liquidity_gross=int(gross),
            )
            for pid, tick, net, gross in lp_positions
        )
        s.flush()

        # init map: U256 bitmaps at word 0 and 7
        s.add_all([
            InitializationMapTable(pool_id=v3_id, word=0, bitmap=int(U128_MAX)),
            InitializationMapTable(pool_id=v3_id, word=7, bitmap=int(BIG_BEYOND_I64)),
        ])
        s.flush()

        # ── V4 pool manager + managed pool + uniswap_v4 ────────────────
        pm = PoolManagerTable(
            address=get_checksum_address("0x498581ff718922c3f8e6a244956af099b2652b2b"),
            chain=CHAIN,
            kind="uniswap_v4",
            state_view=get_checksum_address("0x0000000000000000000000000000000000000001"),
            exchange_id=ex_v4_id,
        )
        s.add(pm)
        s.flush()

        # Creating the UniswapV4PoolTable subclass auto-creates the
        # `managed_pools` base row (joined inheritance); do NOT create a
        # separate ManagedLiquidityPoolTable — that would insert a second
        # base row and desync the liquidity-position foreign key.
        v4 = UniswapV4PoolTable(
            kind="uniswap_v4",
            manager_id=pm.id,
            pool_hash="0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a",
            hooks=get_checksum_address("0x0000000000000000000000000000000000000000"),
            currency0_id=tok_a_id,
            currency1_id=usdc_id,
            fee_currency0=0,
            fee_currency1=0,
            fee_denominator=1_000_000,
            tick_spacing=60,
            liquidity_update_block=12_340_000,
            liquidity_update_log_index=7,
        )
        s.add(v4)
        s.flush()

        mp_id = v4.managed_pool_id

        # managed liquidity positions: U256-range values
        mlp = [
            (mp_id, -50, SMALL, SMALL),
            (mp_id, 0, BIG_BEYOND_I64, U128_MAX),
            (mp_id, 50, BIG_BEYOND_I64, BIG_BEYOND_I64),
        ]
        s.add_all(
            ManagedPoolLiquidityPositionTable(
                managed_pool_id=pid,
                tick=tick,
                liquidity_net=int(net),
                liquidity_gross=int(gross),
            )
            for pid, tick, net, gross in mlp
        )
        s.add_all([
            ManagedPoolInitializationMapTable(
                managed_pool_id=mp_id, word=0, bitmap=int(BIG_BEYOND_I64)
            ),
        ])
        s.commit()

    session.remove()

    # checkpoint WAL so only parity.db is committed (no -wal/-shm side files).
    import sqlite3

    conn = sqlite3.connect(str(DB_PATH))
    conn.execute("PRAGMA wal_checkpoint(TRUNCATE);")
    conn.close()
    for side in (DB_PATH.with_suffix(".db-wal"), DB_PATH.with_suffix(".db-shm")):
        if side.exists():
            side.unlink()


def _snap_to_jsonable(snap: dict | None) -> dict[str, Any] | None:
    if snap is None:
        return None
    return {
        "tick_bitmap": {
            str(k): {"bitmap": str(v.bitmap)} for k, v in snap["tick_bitmap"].items()
        },
        "tick_data": {
            str(k): {
                "liquidity_gross": str(v.liquidity_gross),
                "liquidity_net": str(v.liquidity_net),
            }
            for k, v in snap["tick_data"].items()
        },
    }


def _build_expected() -> dict[str, Any]:
    v3_snap = V3DatabaseSnapshot(chain_id=CHAIN, database_path=DB_PATH)
    v4_snap = V4DatabaseSnapshot(chain_id=CHAIN, database_path=DB_PATH)

    v3_pool_addr = get_checksum_address("0x7b8c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c")
    v3_liquidity = v3_snap.get_liquidity_map(v3_pool_addr)
    v3_all = v3_snap.get_all_liquidity_maps()
    v3_newest = v3_snap.get_newest_block()
    v3_pools = sorted(v3_snap.get_pools())

    v4_pm = get_checksum_address("0x498581ff718922c3f8e6a244956af099b2652b2b")
    v4_hash = "0x96d4b53a38337a5733179751781178a2613306063c511b78cd02684739288c0a"
    v4_liquidity = v4_snap.get_liquidity_map(v4_pm, v4_hash)
    v4_all = v4_snap.get_all_liquidity_maps()
    v4_newest = v4_snap.get_newest_block()
    v4_pools = sorted(v4_snap.get_pools())

    expected: dict[str, Any] = {
        "chain_id": CHAIN,
        "v3_pool_address": v3_pool_addr,
        "v3_liquidity_map": _snap_to_jsonable(v3_liquidity),
        "v3_all_liquidity_maps": {
            addr: {str(tick): [str(gross), str(net)] for tick, (gross, net) in ticks.items()}
            for addr, ticks in v3_all.items()
        },
        "v3_newest_block": v3_newest,
        "v3_pools": v3_pools,
        "v4_pool_manager": v4_pm,
        "v4_pool_hash": v4_hash,
        "v4_liquidity_map": _snap_to_jsonable(v4_liquidity),
        "v4_all_liquidity_maps": [
            {
                "pool_manager": pm,
                "pool_hash": h,
                "ticks": {
                    str(tick): [str(gross), str(net)] for tick, (gross, net) in ticks.items()
                },
            }
            for (pm, h), ticks in v4_all.items()
        ],
        "v4_newest_block": v4_newest,
        "v4_pools": v4_pools,
    }
    return expected


def main() -> None:
    _build_db()
    expected = _build_expected()
    EXPECTED_PATH.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")
    print(f"Wrote {DB_PATH} ({DB_PATH.stat().st_size} bytes)")
    print(f"Wrote {EXPECTED_PATH} ({EXPECTED_PATH.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
