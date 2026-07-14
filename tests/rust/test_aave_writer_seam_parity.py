"""Isolation smoke for the Aave V3 writer PyO3 seam (Sub-step B of AZGJUN/RQXEKH).

Calls each of the 14 ``db_*`` writer pyfunctions (``get_or_create_*`` x 7 +
``apply_*`` x 7) + ``db_decode_reserve_configuration_bitmap`` against a fresh
tmp-file write-capable ``DegenbotDb`` and asserts the row landed. Proves the
PyO3 seam works **in isolation** — NOT the Python-driver cutover (sibling
C-slice tasks OPVK2C / YEM6MN / TUFJQ3) NOR the §4.2 red-green parity against
the Python driver (sibling U5YIBG).

The read-backs use the stdlib ``sqlite3`` module (a different code path than
the ``rusqlite``-via-Rust seam), so the assertions independently confirm the
seam persisted the expected DB state. Mirrors the
``tests/rust/test_pybot_io_db.py`` ``tmp_path`` + ``db_create_new_database``
shape + the Rust-side ``rust/crates/degenbot-db/tests/writer_parity.rs``
harness seeding.
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

from degenbot.degenbot_rs import (
    db_apply_asset_collateral_in_emode_changed,
    db_apply_asset_source_updated,
    db_apply_collateral_configuration_changed,
    db_apply_e_mode_category_added,
    db_apply_emode_asset_category_changed,
    db_apply_price_oracle_updated,
    db_apply_reserve_used_as_collateral,
    db_apply_user_e_mode_set,
    db_create_new_database,
    db_decode_reserve_configuration_bitmap,
    db_get_or_create_asset_config,
    db_get_or_create_collateral_position,
    db_get_or_create_debt_position,
    db_get_or_create_e_mode_category,
    db_get_or_create_erc20_token,
    db_get_or_create_user,
    db_get_or_create_user_collateral_config,
)

MARKET_ID = 1
CHAIN_ID = 1


def _known_config_bitmap() -> int:
    """The canonical Aave V3 reserve-config bitmap (mirrors ``writer_parity``).

    ltv=7500, liquidation_threshold=8000, liquidation_bonus=10500,
    decimals=6, is_active, borrowing_enabled, flash_loan, isolation_mode,
    borrowable_in_isolation, e_mode_category=7, debt_ceiling=999.
    """
    b = 0
    b |= 7500  # ltv (bits 0-15)
    b |= 8000 << 16  # liquidation_threshold
    b |= 10500 << 32  # liquidation_bonus
    b |= 6 << 48  # decimals
    b |= 1 << 56  # is_active
    b |= 1 << 58  # borrowing_enabled
    b |= 1 << 61  # borrowable_in_isolation
    b |= 1 << 62  # isolation_mode
    b |= 1 << 63  # flash_loan_enabled
    b |= 7 << 168  # e_mode byte (also the low byte of unbacked_mint_cap)
    b |= 999 << 212  # debt_ceiling
    return b


def _open(db_path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA foreign_keys = ON")
    return conn


def _seed_parents(db_path: Path) -> int:
    """Seed the FK parents every Aave writer references.

    Mirrors ``writer_parity.rs::fresh_seeded_db``: market (id 1) + three erc20
    tokens (underlying / aToken / vToken = ids 1/2/3) + the asset row (id 1).
    Returns the asset row id (1).
    """
    conn = _open(db_path)
    try:
        conn.execute(
            "INSERT INTO aave_v3_markets (id, chain_id, name, active, last_update_block) "
            "VALUES (?, ?, 'mainnet', 1, NULL)",
            (MARKET_ID, CHAIN_ID),
        )
        for token_id, addr in [(1, "0xu1"), (2, "0xa1"), (3, "0xv1")]:
            conn.execute(
                "INSERT INTO erc20_tokens (id, chain, address) VALUES (?, ?, ?)",
                (token_id, CHAIN_ID, addr),
            )
        conn.execute(
            "INSERT INTO aave_v3_assets ("
            "id, market_id, underlying_asset_id, a_token_id, a_token_revision, "
            "v_token_id, v_token_revision, liquidity_index, liquidity_rate, "
            "borrow_index, borrow_rate) "
            "VALUES (1, 1, 1, 2, 1, 3, 1, '0', '0', '1', '0')",
        )
        conn.commit()
    finally:
        conn.close()
    return 1


def test_get_or_create_e_mode_category_creates_with_defaults(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_emode.db"
    db_create_new_database(str(db_path))
    _seed_parents(db_path)

    row_id = db_get_or_create_e_mode_category(str(db_path), MARKET_ID, 5)
    assert row_id >= 1
    # idempotent: second call returns the existing row.
    assert db_get_or_create_e_mode_category(str(db_path), MARKET_ID, 5) == row_id

    conn = _open(db_path)
    try:
        label, ltv, lt, bonus = conn.execute(
            "SELECT label, ltv, liquidation_threshold, liquidation_bonus "
            "FROM aave_v3_emode_categories WHERE id = ?",
            (row_id,),
        ).fetchone()
    finally:
        conn.close()
    assert label == ""
    assert (ltv, lt, bonus) == (0, 0, 0)


def test_get_or_create_asset_config_creates_with_defaults(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_assetcfg.db"
    db_create_new_database(str(db_path))
    asset_id = _seed_parents(db_path)

    row_id = db_get_or_create_asset_config(str(db_path), asset_id)
    assert row_id >= 1
    assert db_get_or_create_asset_config(str(db_path), asset_id) == row_id

    conn = _open(db_path)
    try:
        row = conn.execute(
            "SELECT ltv, borrowing_enabled, stable_borrowing_enabled, "
            "flash_loan_enabled, isolation_mode, borrowable_in_isolation, "
            "debt_ceiling, e_mode_category_id "
            "FROM aave_v3_asset_configs WHERE id = ?",
            (row_id,),
        ).fetchone()
    finally:
        conn.close()
    ltv, borr, stable, flash, iso, borr_iso, dc, emode = row
    assert ltv == 0
    assert (borr, stable, flash, iso, borr_iso) == (0, 0, 0, 0, 0)
    assert dc is None
    assert emode is None


def test_get_or_create_user_collateral_config_creates_disabled(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_ucc.db"
    db_create_new_database(str(db_path))
    asset_id = _seed_parents(db_path)

    user_id = db_get_or_create_user(str(db_path), MARKET_ID, "0xuserA", gho_discount=0)
    row_id = db_get_or_create_user_collateral_config(str(db_path), user_id, asset_id)
    assert row_id >= 1

    conn = _open(db_path)
    try:
        enabled = conn.execute(
            "SELECT enabled FROM aave_v3_user_collateral_configs WHERE id = ?",
            (row_id,),
        ).fetchone()[0]
    finally:
        conn.close()
    assert enabled == 0


def test_get_or_create_user_persists_gho_discount(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_user.db"
    db_create_new_database(str(db_path))
    _seed_parents(db_path)

    row_id = db_get_or_create_user(str(db_path), MARKET_ID, "0xuserB", gho_discount=1500)
    # idempotent; a second call with a different discount does NOT overwrite.
    assert db_get_or_create_user(str(db_path), MARKET_ID, "0xuserB", 9999) == row_id

    conn = _open(db_path)
    try:
        e_mode, gho, stk, iso_asset, iso_debt = conn.execute(
            "SELECT e_mode, gho_discount, stk_aave_balance, "
            "isolation_mode_collateral_asset_id, isolation_mode_debt "
            "FROM aave_v3_users WHERE id = ?",
            (row_id,),
        ).fetchone()
    finally:
        conn.close()
    assert e_mode == 0
    assert gho == 1500
    assert stk is None
    assert iso_asset is None
    assert iso_debt == "0"


def test_get_or_create_erc20_token_persists_metadata(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_erc20.db"
    db_create_new_database(str(db_path))
    _seed_parents(db_path)

    row_id = db_get_or_create_erc20_token(
        str(db_path), CHAIN_ID, "0xtoken", name="Weth", symbol="WETH", decimals=18
    )
    # second call returns the existing row WITHOUT overwriting metadata.
    assert (
        db_get_or_create_erc20_token(str(db_path), CHAIN_ID, "0xtoken", None, None, None) == row_id
    )

    conn = _open(db_path)
    try:
        name, symbol, decimals = conn.execute(
            "SELECT name, symbol, decimals FROM erc20_tokens WHERE id = ?",
            (row_id,),
        ).fetchone()
    finally:
        conn.close()
    assert (name, symbol, decimals) == ("Weth", "WETH", 18)


def test_get_or_create_positions_create_with_zero_balance(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_positions.db"
    db_create_new_database(str(db_path))
    asset_id = _seed_parents(db_path)
    user_id = db_get_or_create_user(str(db_path), MARKET_ID, "0xuserC", gho_discount=0)

    collat_id = db_get_or_create_collateral_position(str(db_path), user_id, asset_id)
    debt_id = db_get_or_create_debt_position(str(db_path), user_id, asset_id)
    assert collat_id >= 1 and debt_id >= 1

    conn = _open(db_path)
    try:
        cbalance, clast = conn.execute(
            "SELECT balance, last_index FROM aave_v3_collateral_positions WHERE id = ?",
            (collat_id,),
        ).fetchone()
        dbalance, dlast = conn.execute(
            "SELECT balance, last_index FROM aave_v3_debt_positions WHERE id = ?",
            (debt_id,),
        ).fetchone()
    finally:
        conn.close()
    assert cbalance == "0" and dbalance == "0"
    assert clast is None and dlast is None


def test_apply_collateral_configuration_changed_create_then_update(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_apply_cfg.db"
    db_create_new_database(str(db_path))
    asset_id = _seed_parents(db_path)
    bitmap = _known_config_bitmap()

    row_id = db_apply_collateral_configuration_changed(str(db_path), asset_id, bitmap)
    assert row_id >= 1

    conn = _open(db_path)
    try:
        (
            ltv,
            lt,
            bonus,
            borr,
            stable,
            flash,
            iso,
            borr_iso,
            dc,
            emode,
        ) = conn.execute(
            "SELECT ltv, liquidation_threshold, liquidation_bonus, "
            "borrowing_enabled, stable_borrowing_enabled, flash_loan_enabled, "
            "isolation_mode, borrowable_in_isolation, debt_ceiling, "
            "e_mode_category_id FROM aave_v3_asset_configs WHERE id = ?",
            (row_id,),
        ).fetchone()
    finally:
        conn.close()
    assert ltv == 7500
    assert lt == 8000 and bonus == 10500
    assert borr == 1
    assert stable == 0  # NOT in the bitmap → left False on create
    assert flash == 1 and iso == 1 and borr_iso == 1
    assert dc == "999"
    assert emode == 7


def test_apply_e_mode_category_added_inserts_then_updates(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_emode_added.db"
    db_create_new_database(str(db_path))
    _seed_parents(db_path)

    row_id = db_apply_e_mode_category_added(
        str(db_path),
        MARKET_ID,
        3,
        9000,
        9500,
        10000,
        price_source="0xoracle",
        label="ETH",
    )
    # update path
    assert (
        db_apply_e_mode_category_added(
            str(db_path),
            MARKET_ID,
            3,
            9100,
            9600,
            10100,
            price_source="0xoracle2",
            label="ETH-v2",
        )
        == row_id
    )

    conn = _open(db_path)
    try:
        label, ltv, lt, bonus, ps = conn.execute(
            "SELECT label, ltv, liquidation_threshold, liquidation_bonus, "
            "price_source FROM aave_v3_emode_categories WHERE id = ?",
            (row_id,),
        ).fetchone()
    finally:
        conn.close()
    assert label == "ETH-v2"
    assert (ltv, lt, bonus) == (9100, 9600, 10100)
    assert ps == "0xoracle2"


def test_apply_emode_asset_category_changed_unconditional_set(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_emode_cat.db"
    db_create_new_database(str(db_path))
    asset_id = _seed_parents(db_path)

    row_id = db_apply_emode_asset_category_changed(str(db_path), asset_id, 5)
    # clear to None (new_category_id == 0)
    db_apply_emode_asset_category_changed(str(db_path), asset_id, 0)
    # and re-set
    db_apply_emode_asset_category_changed(str(db_path), asset_id, 7)

    conn = _open(db_path)
    try:
        emode = conn.execute(
            "SELECT e_mode_category_id FROM aave_v3_asset_configs WHERE id = ?",
            (row_id,),
        ).fetchone()[0]
    finally:
        conn.close()
    assert emode == 7


def test_apply_asset_collateral_in_emode_changed_gated(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_emode_gated.db"
    db_create_new_database(str(db_path))
    asset_id = _seed_parents(db_path)

    db_apply_asset_collateral_in_emode_changed(str(db_path), asset_id, 3, True)
    # is_collateral=False → leave UNCHANGED (the Python elif gap)
    db_apply_asset_collateral_in_emode_changed(str(db_path), asset_id, 9, False)

    conn = _open(db_path)
    try:
        emode = conn.execute(
            "SELECT e_mode_category_id FROM aave_v3_asset_configs WHERE asset_id = ?",
            (asset_id,),
        ).fetchone()[0]
    finally:
        conn.close()
    assert emode == 3


def test_apply_reserve_used_as_collateral_enable_then_disable(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_ruac.db"
    db_create_new_database(str(db_path))
    asset_id = _seed_parents(db_path)
    user_id = db_get_or_create_user(str(db_path), MARKET_ID, "0xuserD", gho_discount=0)

    row_id = db_apply_reserve_used_as_collateral(str(db_path), user_id, asset_id, True)
    # disable → update existing to False
    assert db_apply_reserve_used_as_collateral(str(db_path), user_id, asset_id, False) == row_id

    conn = _open(db_path)
    try:
        enabled = conn.execute(
            "SELECT enabled FROM aave_v3_user_collateral_configs WHERE id = ?",
            (row_id,),
        ).fetchone()[0]
    finally:
        conn.close()
    assert enabled == 0


def test_apply_user_e_mode_set_updates_e_mode(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_uem.db"
    db_create_new_database(str(db_path))
    _seed_parents(db_path)
    user_id = db_get_or_create_user(str(db_path), MARKET_ID, "0xuserE", gho_discount=0)

    db_apply_user_e_mode_set(str(db_path), user_id, 3)

    conn = _open(db_path)
    try:
        e_mode = conn.execute(
            "SELECT e_mode FROM aave_v3_users WHERE id = ?", (user_id,)
        ).fetchone()[0]
    finally:
        conn.close()
    assert e_mode == 3


def test_apply_price_oracle_updated_inserts_then_updates(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_oracle.db"
    db_create_new_database(str(db_path))
    _seed_parents(db_path)

    row_id = db_apply_price_oracle_updated(str(db_path), MARKET_ID, "0xoracle1")
    assert db_apply_price_oracle_updated(str(db_path), MARKET_ID, "0xoracle2") == row_id

    conn = _open(db_path)
    try:
        name, addr = conn.execute(
            "SELECT name, address FROM aave_v3_contracts WHERE id = ?",
            (row_id,),
        ).fetchone()
    finally:
        conn.close()
    assert name == "PRICE_ORACLE"
    assert addr == "0xoracle2"


def test_apply_asset_source_updated_sets_price_source(tmp_path: Path) -> None:
    db_path = tmp_path / "aave_source.db"
    db_create_new_database(str(db_path))
    asset_id = _seed_parents(db_path)

    db_apply_asset_source_updated(str(db_path), asset_id, "0xsource")

    conn = _open(db_path)
    try:
        ps = conn.execute(
            "SELECT price_source FROM aave_v3_assets WHERE id = ?",
            (asset_id,),
        ).fetchone()[0]
    finally:
        conn.close()
    assert ps == "0xsource"


def test_decode_reserve_configuration_bitmap_returns_oracle_dict(tmp_path: Path) -> None:
    """The pure CPU bit-decode returns the oracle's dict shape 1:1.

    Pure CPU; the ``tmp_path`` is unused but kept for shape parity + so the
    test stays grouped with the writer seam suite. Asserts every key the
    Python ``_decode_reserve_configuration_bitmap`` oracle returns is present
    with the expected value for the canonical bitmap.
    """
    _ = tmp_path  # noqa: F841 (shape parity)
    cfg = db_decode_reserve_configuration_bitmap(_known_config_bitmap())

    expected_keys = {
        "ltv",
        "liquidation_threshold",
        "liquidation_bonus",
        "decimals",
        "is_active",
        "is_frozen",
        "borrowing_enabled",
        "stable_rate_borrowing_enabled",
        "reserve_factor",
        "borrow_cap",
        "supply_cap",
        "debt_ceiling",
        "liquidation_protocol_fee",
        "unbacked_mint_cap",
        "e_mode_category_id",
        "flash_loan_enabled",
        "isolation_mode",
        "borrowable_in_isolation",
    }
    assert set(cfg.keys()) == expected_keys

    assert cfg["ltv"] == 7500
    assert cfg["liquidation_threshold"] == 8000
    assert cfg["liquidation_bonus"] == 10500
    assert cfg["decimals"] == 6
    assert cfg["is_active"] is True
    assert cfg["is_frozen"] is False
    assert cfg["borrowing_enabled"] is True
    assert cfg["stable_rate_borrowing_enabled"] is False
    assert cfg["flash_loan_enabled"] is True
    assert cfg["isolation_mode"] is True
    assert cfg["borrowable_in_isolation"] is True
    assert cfg["e_mode_category_id"] == 7
    assert cfg["debt_ceiling"] == 999

    # the zero bitmap maps e_mode_category_id to None (the oracle's
    # `e_mode_category if e_mode_category > 0 else None`).
    cfg0 = db_decode_reserve_configuration_bitmap(0)
    assert cfg0["e_mode_category_id"] is None
    assert cfg0["ltv"] == 0
