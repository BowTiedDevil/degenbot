#!/usr/bin/env python3
"""Generate the Aave V3 cross-implementation parity fixture for degenbot-db.

Builds ``aave_parity.db`` — a REAL Alembic-stamped SQLite DB (same open path
the production bot uses: ``create_new_sqlite_database`` runs
``Base.metadata.create_all`` + ``alembic stamp head``) seeded with Aave V3
market / contract / e-mode-category / asset / asset-config / user /
collateral-position / debt-position / user-collateral-config rows, including
U256-range balances/indexes (exceeding ``i64::MAX``) to exercise the
``VARCHAR(78)`` ↔ ``U256`` boundary.

Then runs the Python ``DatabasePositionQuery`` parity oracle against the same
DB and dumps the expected results to ``aave_parity_expected.json`` (U256 values
as decimal strings, addresses verbatim). The Rust integration test
``tests/aave_parity.rs`` opens the frozen ``aave_parity.db`` and asserts
byte-identical results.

Run once (or whenever the seed data should change)::

    uv run python rust/crates/degenbot-db/tests/fixtures/generate_aave_parity.py

Outputs (committed):
    rust/crates/degenbot-db/tests/fixtures/aave_parity.db
    rust/crates/degenbot-db/tests/fixtures/aave_parity_expected.json
"""

from __future__ import annotations

import json
import pathlib
from typing import Any

from sqlalchemy.orm import Session

from degenbot.aave.analysis.orchestrator import DatabasePositionQuery
from degenbot.checksum_cache import get_checksum_address
from degenbot.database.models.aave import (
    AaveV3Asset,
    AaveV3AssetConfig,
    AaveV3CollateralPosition,
    AaveV3Contract,
    AaveV3DebtPosition,
    AaveV3EModeCategory,
    AaveV3Market,
    AaveV3User,
    AaveV3UserCollateralConfig,
)
from degenbot.database.models.erc20 import Erc20TokenTable
from degenbot.database.operations import create_new_sqlite_database, get_scoped_sqlite_session

FIXTURE_DIR = pathlib.Path(__file__).resolve().parent
DB_PATH = FIXTURE_DIR / "aave_parity.db"
EXPECTED_PATH = FIXTURE_DIR / "aave_parity_expected.json"

CHAIN = 8453

# U256-range values (decimal ints, mirrored to VARCHAR(78) by IntMappedToString).
BIG_BEYOND_I64 = 2**70           # 1180591620717411303424 — exceeds i64::MAX
RAY = 10**27                     # liquidity/borrow index scale
SMALL = 1000
ZERO = 0


def _build_db() -> None:
    if DB_PATH.exists():
        DB_PATH.unlink()
    for side in (DB_PATH.with_suffix(".db-wal"), DB_PATH.with_suffix(".db-shm")):
        if side.exists():
            side.unlink()

    create_new_sqlite_database(DB_PATH)
    session = get_scoped_sqlite_session(DB_PATH)

    with session() as s:  # type: Session
        # ── tokens (underlying + aTokens + vTokens) ────────────────────
        weth = Erc20TokenTable(chain=CHAIN, address=get_checksum_address("0x4200000000000000000000000000000000000006"), name="Wrapped Ether", symbol="WETH", decimals=18)
        usdc = Erc20TokenTable(chain=CHAIN, address=get_checksum_address("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"), name="USD Coin", symbol="USDC", decimals=6)
        dai = Erc20TokenTable(chain=CHAIN, address=get_checksum_address("0x50c5725949a6f0c72e6c4a64177c497b4751a5dc"), name="Dai Stablecoin", symbol="DAI", decimals=18)
        a_weth = Erc20TokenTable(chain=CHAIN, address=get_checksum_address("0x57d3c0d67f8105a41d0c7d25c3a1d0b9f6f4a6e1"), name="Aave WETH", symbol="aWETH", decimals=18)
        v_weth = Erc20TokenTable(chain=CHAIN, address=get_checksum_address("0x69e3a8d2f2d3c0d7b1a4c2e5f8a1b3c4d5e6f7a8"), name="Variable Debt WETH", symbol="vWETH", decimals=18)
        a_usdc = Erc20TokenTable(chain=CHAIN, address=get_checksum_address("0x6ab1a3d2f6acd0b1d5e2c3f4d5e6f7a8b9c0d1e2"), name="Aave USDC", symbol="aUSDC", decimals=6)
        v_usdc = Erc20TokenTable(chain=CHAIN, address=get_checksum_address("0x7bc2a4d3f7ace1c2e6f3d4e5f6a7b8c9d0e1f2a3"), name="Variable Debt USDC", symbol="vUSDC", decimals=6)
        # aToken for DAI (the isolation collateral asset)
        a_dai = Erc20TokenTable(chain=CHAIN, address=get_checksum_address("0x8c2a5d4f7baf2d3e6c4d5e6f7a8b9c0d1e2f3a4b"), name="Aave DAI", symbol="aDAI", decimals=18)
        v_dai = Erc20TokenTable(chain=CHAIN, address=get_checksum_address("0x9d2a5e4f8bbf3e4f7d5e6f7a8b9c0d1e2f3a4b5c"), name="Variable Debt DAI", symbol="vDAI", decimals=18)
        s.add_all([weth, usdc, dai, a_weth, v_weth, a_usdc, v_usdc, a_dai, v_dai])
        s.flush()

        # ── market ───────────────────────────────────────────────────────
        market = AaveV3Market(chain_id=CHAIN, name="aave_v3_base", active=True, last_update_block=12_345_678)
        s.add(market)
        s.flush()
        market_id = market.id

        # ── PRICE_ORACLE contract ────────────────────────────────────────
        s.add(AaveV3Contract(market_id=market_id, name="PRICE_ORACLE", address=get_checksum_address("0x595a7e11c2c2c2c5b5d5e5f5a5b5c5d5e5f5a5b5"), revision=1))
        s.flush()

        # ── eMode category (category_id=1, used by the WETH asset) ───────
        emode_cat = AaveV3EModeCategory(market_id=market_id, category_id=1, label="ETH", ltv=9300, liquidation_threshold=9500, liquidation_bonus=200, price_source=None)
        s.add(emode_cat)
        s.flush()

        # ── assets + asset_configs ───────────────────────────────────────
        # WETH asset: in eMode category 1 (so collateral records get emode_lt/ltv)
        weth_asset = AaveV3Asset(
            market_id=market_id, underlying_asset_id=weth.id, a_token_id=a_weth.id, a_token_revision=1,
            v_token_id=v_weth.id, v_token_revision=1, e_mode_category_id=emode_cat.id,
            price_source=None, last_update_block=12_345_678,
            liquidity_index=RAY, liquidity_rate=0, borrow_index=RAY, borrow_rate=0,
        )
        usdc_asset = AaveV3Asset(
            market_id=market_id, underlying_asset_id=usdc.id, a_token_id=a_usdc.id, a_token_revision=1,
            v_token_id=v_usdc.id, v_token_revision=1, e_mode_category_id=None,
            price_source=None, last_update_block=12_345_678,
            liquidity_index=RAY + SMALL, liquidity_rate=0, borrow_index=RAY + SMALL, borrow_rate=0,
        )
        # DAI asset: the isolation collateral asset (has an asset_config with
        # debt_ceiling). NOT in eMode (proves emode_lt/ltv = None on its
        # collateral records). Underlying symbol None? No — give it a symbol.
        dai_asset = AaveV3Asset(
            market_id=market_id, underlying_asset_id=dai.id, a_token_id=a_dai.id, a_token_revision=1,
            v_token_id=v_dai.id, v_token_revision=1, e_mode_category_id=None,
            price_source=None, last_update_block=12_345_678,
            liquidity_index=RAY, liquidity_rate=0, borrow_index=RAY, borrow_rate=0,
        )
        s.add_all([weth_asset, usdc_asset, dai_asset])
        s.flush()

        s.add_all([
            AaveV3AssetConfig(asset_id=weth_asset.id, ltv=8000, liquidation_threshold=8250, liquidation_bonus=500, e_mode_category_id=emode_cat.id, borrowing_enabled=True, stable_borrowing_enabled=False, flash_loan_enabled=True, isolation_mode=False, borrowable_in_isolation=False, debt_ceiling=None),
            AaveV3AssetConfig(asset_id=usdc_asset.id, ltv=7800, liquidation_threshold=8000, liquidation_bonus=500, e_mode_category_id=None, borrowing_enabled=True, stable_borrowing_enabled=False, flash_loan_enabled=True, isolation_mode=False, borrowable_in_isolation=False, debt_ceiling=None),
            AaveV3AssetConfig(asset_id=dai_asset.id, ltv=7500, liquidation_threshold=7700, liquidation_bonus=500, e_mode_category_id=None, borrowing_enabled=True, stable_borrowing_enabled=False, flash_loan_enabled=True, isolation_mode=True, borrowable_in_isolation=True, debt_ceiling=BIG_BEYOND_I64),
        ])
        s.flush()

        # ── users ────────────────────────────────────────────────────────
        # User 1: has debt (WETH); eMode off; NOT isolation mode.
        user1 = AaveV3User(
            market_id=market_id, address=get_checksum_address("0x1111111111111111111111111111111111111111"),
            e_mode=0, gho_discount=0, stk_aave_balance=None,
            isolation_mode_collateral_asset_id=None, isolation_mode_debt=ZERO,
        )
        # User 2: isolation mode (collateral_asset_id = DAI asset). Proves
        # isolation_debt_ceiling materializes from the asset_config join.
        user2 = AaveV3User(
            market_id=market_id, address=get_checksum_address("0x2222222222222222222222222222222222222222"),
            e_mode=0, gho_discount=0, stk_aave_balance=None,
            isolation_mode_collateral_asset_id=dai_asset.id, isolation_mode_debt=BIG_BEYOND_I64,
        )
        # User 3: has collateral but NO debt — must be FILTERED OUT by
        # get_users_with_debt (the EXISTS semi-join).
        user3 = AaveV3User(
            market_id=market_id, address=get_checksum_address("0x3333333333333333333333333333333333333333"),
            e_mode=0, gho_discount=0, stk_aave_balance=None,
            isolation_mode_collateral_asset_id=None, isolation_mode_debt=ZERO,
        )
        # User 4: eMode enabled (e_mode=1) + debt — proves emode matching
        # (asset in same eMode category for the collateral).
        user4 = AaveV3User(
            market_id=market_id, address=get_checksum_address("0x4444444444444444444444444444444444444444"),
            e_mode=1, gho_discount=0, stk_aave_balance=None,
            isolation_mode_collateral_asset_id=None, isolation_mode_debt=ZERO,
        )
        s.add_all([user1, user2, user3, user4])
        s.flush()

        # ── collateral + debt positions (U256-range balances) ────────────
        # User 1: collateral WETH + debt USDC
        s.add_all([
            AaveV3CollateralPosition(user_id=user1.id, asset_id=weth_asset.id, balance=BIG_BEYOND_I64, last_index=RAY),
            AaveV3DebtPosition(user_id=user1.id, asset_id=usdc_asset.id, balance=SMALL, last_index=RAY + SMALL),
        ])
        s.flush()
        # User 2 (isolation): collateral DAI + debt DAI
        s.add_all([
            AaveV3CollateralPosition(user_id=user2.id, asset_id=dai_asset.id, balance=BIG_BEYOND_I64 * 2, last_index=RAY),
            AaveV3DebtPosition(user_id=user2.id, asset_id=dai_asset.id, balance=SMALL, last_index=RAY),
        ])
        s.flush()
        # User 3: collateral USDC but NO debt (filtered out)
        s.add_all([
            AaveV3CollateralPosition(user_id=user3.id, asset_id=usdc_asset.id, balance=SMALL, last_index=RAY + SMALL),
        ])
        s.flush()
        # User 4 (eMode=1): collateral WETH (in eMode cat 1) + debt USDC
        s.add_all([
            AaveV3CollateralPosition(user_id=user4.id, asset_id=weth_asset.id, balance=BIG_BEYOND_I64, last_index=RAY),
            AaveV3DebtPosition(user_id=user4.id, asset_id=usdc_asset.id, balance=SMALL, last_index=RAY + SMALL),
        ])
        s.flush()

        # ── user collateral configs (proves enabled/disabled) ─────────────
        s.add_all([
            AaveV3UserCollateralConfig(user_id=user1.id, asset_id=weth_asset.id, enabled=True),
            AaveV3UserCollateralConfig(user_id=user1.id, asset_id=usdc_asset.id, enabled=False),
            AaveV3UserCollateralConfig(user_id=user2.id, asset_id=dai_asset.id, enabled=True),
            AaveV3UserCollateralConfig(user_id=user4.id, asset_id=weth_asset.id, enabled=True),
        ])
        s.flush()
        s.commit()

    # Stash the market id for the oracle (the only market in this DB).
    with session() as s:
        m = s.query(AaveV3Market).first()
        _build_db.market_id = m.id  # type: ignore[attr-defined]


def _dump_oracle() -> dict[str, Any]:
    session = get_scoped_sqlite_session(DB_PATH)
    with session() as s:
        market_id = s.query(AaveV3Market).first().id
        q = DatabasePositionQuery(s)

        users = q.get_users_with_debt(market_id)
        oracle_users = []
        for u in users:
            collateral = q.get_collateral_positions(u.id)
            debt = q.get_debt_positions(u.id)
            cfg = q.get_collateral_config_map(u.id)
            oracle_users.append({
                "id": u.id,
                "address": u.address,
                "market_id": u.market_id,
                "e_mode": u.e_mode,
                "is_isolation_mode": u.is_isolation_mode,
                "isolation_mode_debt": str(u.isolation_mode_debt),
                "isolation_debt_ceiling": str(u.isolation_debt_ceiling) if u.isolation_debt_ceiling is not None else None,
                "collateral": [
                    {
                        "asset_id": c.asset_id,
                        "balance": str(c.balance),
                        "underlying_address": c.underlying_address,
                        "underlying_symbol": c.underlying_symbol,
                        "liquidity_index": str(c.liquidity_index),
                        "e_mode_category_id": c.e_mode_category_id,
                        "asset_lt": c.asset_lt,
                        "asset_ltv": c.asset_ltv,
                        "emode_lt": c.emode_lt,
                        "emode_ltv": c.emode_ltv,
                    }
                    for c in collateral
                ],
                "debt": [
                    {
                        "asset_id": d.asset_id,
                        "balance": str(d.balance),
                        "underlying_address": d.underlying_address,
                        "underlying_symbol": d.underlying_symbol,
                        "borrow_index": str(d.borrow_index),
                        "e_mode_category_id": d.e_mode_category_id,
                    }
                    for d in debt
                ],
                "collateral_config_map": {str(k): v for k, v in cfg.items()},
            })

        oracle_address = q.get_oracle_address(market_id)
        asset_addresses = sorted(q.get_asset_addresses(market_id))

        return {
            "chain_id": CHAIN,
            "market_id": market_id,
            "users": oracle_users,
            "oracle_address": oracle_address,
            "asset_addresses": asset_addresses,
        }


def main() -> None:
    _build_db()
    print(f"Wrote {DB_PATH} ({DB_PATH.stat().st_size} bytes)")
    expected = _dump_oracle()
    EXPECTED_PATH.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")
    print(f"Wrote {EXPECTED_PATH} ({EXPECTED_PATH.stat().st_size} bytes)")
    print(f"  {len(expected['users'])} users with debt, "
          f"{sum(len(u['collateral']) for u in expected['users'])} collateral positions, "
          f"{sum(len(u['debt']) for u in expected['users'])} debt positions")


if __name__ == "__main__":
    main()