"""Rust-backed database row types + operations — the stable mirror home for ``_ffi.db``.

Re-exports the Rust-backed database surface from the ``degenbot._ffi.db``
submodule. Importers should use::

    from degenbot.db import db_backup_database, V2PoolRowInput

rather than reaching into ``degenbot._ffi`` directly — this path is stable
across future Rust reshuffles, and lets the Rust crate structure
(``degenbot-db``) show through to Python.

The functions are thin PyO3 wrappers over the pure-Rust ``degenbot-db``
core crate; the ``db_`` prefix is retained on the submodule names (unlike
the math submodules which dropped their prefix) because ``db_`` is a clear
functional namespace marker for the ~45 database operations.

The classes are ADR-005 ``Py*`` aliases / ``*Row``/``*RowInput`` types;
``DatabaseSchemaStale`` is the typed ``ValueError`` subclass for the
"DB is stamped at a prior Alembic revision" rejection.

Split from ``degenbot.database`` (ADR-013): ``degenbot.db`` owns the
Rust-backed row types + operations; ``degenbot.database`` keeps the
SQLAlchemy ORM (``DatabaseSessionManager``, ``models/``). The split
respects ADR-010's Alembic-retention constraint — the ORM layer is
untouched until the 0.7 cutover.
"""

from degenbot._ffi.db import (
    DatabaseSchemaStale,
    ExchangeRow,
    InitializationMapRow,
    LiquidityPoolRow,
    LiquidityPositionRow,
    LiquidityUpdateEvent,
    PoolKindRow,
    PoolManagerRow,
    PyCollateralPositionData,
    PyDatabasePositionQuery,
    PyDatabaseSnapshot,
    PyDebtPositionData,
    PyUserPositionSummary,
    V2PoolRowInput,
    V3PoolRowInput,
    V4PoolRowInput,
    analyze_aave_user_position,
    db_apply_asset_collateral_in_emode_changed,
    db_apply_asset_source_updated,
    db_apply_collateral_configuration_changed,
    db_apply_e_mode_category_added,
    db_apply_emode_asset_category_changed,
    db_apply_price_oracle_updated,
    db_apply_reserve_used_as_collateral,
    db_apply_user_e_mode_set,
    db_apply_v3_liquidity_updates,
    db_apply_v4_liquidity_updates,
    db_backup_database,
    db_compact_database,
    db_convert_alembic_to_rust_owned,
    db_create_new_database,
    db_decode_reserve_configuration_bitmap,
    db_fetch_exchange,
    db_fetch_exchange_by_name,
    db_fetch_pool_row,
    db_get_or_create_asset_config,
    db_get_or_create_collateral_position,
    db_get_or_create_debt_position,
    db_get_or_create_e_mode_category,
    db_get_or_create_erc20_token,
    db_get_or_create_user,
    db_get_or_create_user_collateral_config,
    db_heal_database,
    db_inspect_schema_state,
    db_set_exchange_active,
    db_set_exchange_last_update_block,
    db_upgrade_database,
    db_upsert_exchange,
    db_upsert_pool_manager,
    db_upsert_v2_pools,
    db_upsert_v3_pools,
    db_upsert_v4_pools,
)

__all__ = [
    "DatabaseSchemaStale",
    "ExchangeRow",
    "InitializationMapRow",
    "LiquidityPoolRow",
    "LiquidityPositionRow",
    "LiquidityUpdateEvent",
    "PoolKindRow",
    "PoolManagerRow",
    "PyCollateralPositionData",
    "PyDatabasePositionQuery",
    "PyDatabaseSnapshot",
    "PyDebtPositionData",
    "PyUserPositionSummary",
    "V2PoolRowInput",
    "V3PoolRowInput",
    "V4PoolRowInput",
    "analyze_aave_user_position",
    "db_apply_asset_collateral_in_emode_changed",
    "db_apply_asset_source_updated",
    "db_apply_collateral_configuration_changed",
    "db_apply_e_mode_category_added",
    "db_apply_emode_asset_category_changed",
    "db_apply_price_oracle_updated",
    "db_apply_reserve_used_as_collateral",
    "db_apply_user_e_mode_set",
    "db_apply_v3_liquidity_updates",
    "db_apply_v4_liquidity_updates",
    "db_backup_database",
    "db_compact_database",
    "db_convert_alembic_to_rust_owned",
    "db_create_new_database",
    "db_decode_reserve_configuration_bitmap",
    "db_fetch_exchange",
    "db_fetch_exchange_by_name",
    "db_fetch_pool_row",
    "db_get_or_create_asset_config",
    "db_get_or_create_collateral_position",
    "db_get_or_create_debt_position",
    "db_get_or_create_e_mode_category",
    "db_get_or_create_erc20_token",
    "db_get_or_create_user",
    "db_get_or_create_user_collateral_config",
    "db_heal_database",
    "db_inspect_schema_state",
    "db_set_exchange_active",
    "db_set_exchange_last_update_block",
    "db_upgrade_database",
    "db_upsert_exchange",
    "db_upsert_pool_manager",
    "db_upsert_v2_pools",
    "db_upsert_v3_pools",
    "db_upsert_v4_pools",
]
