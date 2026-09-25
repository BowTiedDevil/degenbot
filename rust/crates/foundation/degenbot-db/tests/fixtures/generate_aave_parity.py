#!/usr/bin/env python3
"""Generate the frozen Aave V3 position-query fixture and JSON oracle."""

from __future__ import annotations

import json
import pathlib
import sqlite3
from contextlib import closing
from typing import Any

from degenbot.aave.analysis.orchestrator import DatabasePositionQuery
from degenbot.checksum_cache import get_checksum_address
from degenbot.db import db_create_new_database

FIXTURE_DIR = pathlib.Path(__file__).resolve().parent
DB_PATH = FIXTURE_DIR / "aave_parity.db"
EXPECTED_PATH = FIXTURE_DIR / "aave_parity_expected.json"
CHAIN = 8453
BIG = 2**70
RAY = 10**27


def _address(seed: str) -> str:
    return get_checksum_address("0x" + seed * 20)


def _build_db() -> None:
    for suffix in ("", "-wal", "-shm"):
        DB_PATH.with_name(DB_PATH.name + suffix).unlink(missing_ok=True)
    db_create_new_database(str(DB_PATH))

    tokens = [
        (1, "4200000000000000000000000000000000000006", "Wrapped Ether", "WETH", 18),
        (2, "833589fcd6edb6e08f4c7c32d4f71b54bda02913", "USD Coin", "USDC", 6),
        (3, "50c5725949a6f0c72e6c4a64177c497b4751a5dc", "Dai Stablecoin", "DAI", 18),
        (4, "57d3c0d67f8105a41d0c7d25c3a1d0b9f6f4a6e1", "Aave WETH", "aWETH", 18),
        (5, "69e3a8d2f2d3c0d7b1a4c2e5f8a1b3c4d5e6f7a8", "Variable Debt WETH", "vWETH", 18),
        (6, "6ab1a3d2f6acd0b1d5e2c3f4d5e6f7a8b9c0d1e2", "Aave USDC", "aUSDC", 6),
        (7, "7bc2a4d3f7ace1c2e6f3d4e5f6a7b8c9d0e1f2a3", "Variable Debt USDC", "vUSDC", 6),
        (8, "8c2a5d4f7baf2d3e6c4d5e6f7a8b9c0d1e2f3a4b", "Aave DAI", "aDAI", 18),
        (9, "9d2a5e4f8bbf3e4f7d5e6f7a8b9c0d1e2f3a4b5c", "Variable Debt DAI", "vDAI", 18),
    ]
    with closing(sqlite3.connect(DB_PATH)) as connection:
        connection.executemany(
            "INSERT INTO erc20_tokens (id, chain, address, name, symbol, decimals) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            [
                (token_id, CHAIN, get_checksum_address(address), name, symbol, decimals)
                for token_id, address, name, symbol, decimals in tokens
            ],
        )
        connection.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) "
            "VALUES (1, ?, 'aave_v3_base', 1, 12345678)",
            (CHAIN,),
        )
        connection.execute(
            "INSERT INTO aave_v3_contracts (market_id, name, address, revision) "
            "VALUES (1, 'PRICE_ORACLE', ?, 1)",
            (get_checksum_address("0x595a7e11c2c2c2c5b5d5e5f5a5b5c5d5e5f5a5b5"),),
        )
        connection.execute(
            "INSERT INTO aave_v3_emode_categories "
            "(market_id, category_id, label, ltv, liquidation_threshold, liquidation_bonus) "
            "VALUES (1, 1, 'ETH', 9300, 9500, 200)"
        )
        connection.executemany(
            "INSERT INTO aave_v3_assets (id, market_id, underlying_asset_id, a_token_id, "
            "a_token_revision, v_token_id, v_token_revision, e_mode_category_id, last_update_block, "
            "liquidity_index, liquidity_rate, borrow_index, borrow_rate) "
            "VALUES (?, 1, ?, ?, 1, ?, 1, ?, 12345678, ?, '0', ?, '0')",
            [
                (1, 1, 4, 5, 1, str(RAY), str(RAY)),
                (2, 2, 6, 7, None, str(RAY + 1000), str(RAY + 1000)),
                (3, 3, 8, 9, None, str(RAY), str(RAY)),
            ],
        )
        connection.executemany(
            "INSERT INTO aave_v3_asset_configs (asset_id, ltv, liquidation_threshold, "
            "liquidation_bonus, e_mode_category_id, borrowing_enabled, stable_borrowing_enabled, "
            "flash_loan_enabled, isolation_mode, borrowable_in_isolation, debt_ceiling) "
            "VALUES (?, ?, ?, ?, ?, 1, 0, 1, ?, ?, ?)",
            [
                (1, 8000, 8250, 500, 1, 0, 0, None),
                (2, 7800, 8000, 500, None, 0, 0, None),
                (3, 7500, 7700, 500, None, 1, 1, str(BIG)),
            ],
        )
        connection.executemany(
            "INSERT INTO aave_v3_users (id, market_id, address, e_mode, gho_discount, "
            "isolation_mode_collateral_asset_id, isolation_mode_debt) "
            "VALUES (?, 1, ?, ?, 0, ?, ?)",
            [
                (1, _address("11"), 0, None, "0"),
                (2, _address("22"), 0, 3, str(BIG)),
                (3, _address("33"), 0, None, "0"),
                (4, _address("44"), 1, None, "0"),
            ],
        )
        connection.executemany(
            "INSERT INTO aave_v3_collateral_positions (user_id, asset_id, balance, last_index) "
            "VALUES (?, ?, ?, ?)",
            [
                (1, 1, str(BIG), str(RAY)),
                (2, 3, str(BIG * 2), str(RAY)),
                (3, 2, "1000", str(RAY + 1000)),
                (4, 1, str(BIG), str(RAY)),
            ],
        )
        connection.executemany(
            "INSERT INTO aave_v3_debt_positions (user_id, asset_id, balance, last_index) "
            "VALUES (?, ?, ?, ?)",
            [
                (1, 2, "1000", str(RAY + 1000)),
                (2, 3, "1000", str(RAY)),
                (4, 2, "1000", str(RAY + 1000)),
            ],
        )
        connection.executemany(
            "INSERT INTO aave_v3_user_collateral_configs (user_id, asset_id, enabled) "
            "VALUES (?, ?, ?)",
            [(1, 1, 1), (1, 2, 0), (2, 3, 1), (4, 1, 1)],
        )
        connection.commit()
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")


def _dump_oracle() -> dict[str, Any]:
    query = DatabasePositionQuery(DB_PATH)
    market_id = 1
    users = []
    for user in query.get_users_with_debt(market_id):
        user_id = user["id"]
        users.append({
            "id": user_id,
            "address": user["address"],
            "market_id": user["market_id"],
            "e_mode": user["e_mode"],
            "is_isolation_mode": user["is_isolation_mode"],
            "isolation_mode_debt": str(user["isolation_mode_debt"]),
            "isolation_debt_ceiling": (
                None
                if user["isolation_debt_ceiling"] is None
                else str(user["isolation_debt_ceiling"])
            ),
            "collateral": [
                {
                    "asset_id": position["asset_id"],
                    "balance": str(position["balance"]),
                    "underlying_address": position["underlying_address"],
                    "underlying_symbol": position["underlying_symbol"],
                    "liquidity_index": str(position["liquidity_index"]),
                    "e_mode_category_id": position["e_mode_category_id"],
                    "asset_lt": position["asset_lt"],
                    "asset_ltv": position["asset_ltv"],
                    "emode_lt": position["emode_lt"],
                    "emode_ltv": position["emode_ltv"],
                }
                for position in query.get_collateral_positions(user_id)
            ],
            "debt": [
                {
                    "asset_id": position["asset_id"],
                    "balance": str(position["balance"]),
                    "underlying_address": position["underlying_address"],
                    "underlying_symbol": position["underlying_symbol"],
                    "borrow_index": str(position["borrow_index"]),
                    "e_mode_category_id": position["e_mode_category_id"],
                }
                for position in query.get_debt_positions(user_id)
            ],
            "collateral_config_map": {
                str(key): value for key, value in query.get_collateral_config_map(user_id).items()
            },
        })
    return {
        "chain_id": CHAIN,
        "market_id": market_id,
        "users": users,
        "oracle_address": query.get_oracle_address(market_id),
        "asset_addresses": sorted(query.get_asset_addresses(market_id)),
    }


def main() -> None:
    _build_db()
    expected = _dump_oracle()
    EXPECTED_PATH.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")
    print(f"Wrote {DB_PATH} ({DB_PATH.stat().st_size} bytes)")
    print(f"Wrote {EXPECTED_PATH} ({EXPECTED_PATH.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
