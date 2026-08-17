"""Rust-backed database row types + operations — the stable mirror home for ``_ffi.db``.

Re-exports the Rust-backed database surface from the ``degenbot._ffi.db``
submodule. Importers should use::

    from degenbot.db import db_backup_database

    (the pool-updater row-input/event types now live exclusively in
    ``degenbot.updater`` — I4H7EH)

rather than reaching into ``degenbot._ffi`` directly — this path is stable
across future Rust reshuffles, and lets the Rust crate structure
(``degenbot-db``) show through to Python.

The functions are thin PyO3 wrappers over the pure-Rust ``degenbot-db``
core crate; the ``db_`` prefix is retained on the submodule names (unlike
the math submodules which dropped their prefix) because ``db_`` is a clear
functional namespace marker for the ~45 database operations.

The classes are ADR-005 ``Py*`` aliases / ``*Row``/``*RowInput`` types.
The ``DatabaseSchemaStale`` exception is raised by these seams; its
single public import home is ``degenbot.exceptions`` (4JASRW) — import
it from there, not from this module.

Split from ``degenbot.database`` (ADR-013): ``degenbot.db`` owns the
Rust-backed row types + operations; ``degenbot.database`` keeps the
SQLAlchemy ORM (``DatabaseSessionManager``, ``models/``). The split
respects ADR-010's Alembic-retention constraint — the ORM layer is
untouched until the 0.7 cutover.
"""

from degenbot._ffi.db import (
    CollateralPositionData,
    DebtPositionData,
    ExchangeRow,
    LiquidityPoolRow,
    PoolManagerRow,
    RustDatabasePositionQuery,
    RustDatabaseSnapshot,
    UserPositionSummary,
    analyze_aave_user_position,
    db_apply_v3_liquidity_updates,
    db_apply_v4_liquidity_updates,
    db_backup_database,
    db_compact_database,
    db_convert_alembic_to_rust_owned,
    db_create_new_database,
    db_fetch_exchange,
    db_fetch_exchange_by_name,
    db_fetch_pool_row,
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
    "ExchangeRow",
    "LiquidityPoolRow",
    "PoolManagerRow",
    "CollateralPositionData",
    "RustDatabasePositionQuery",
    "RustDatabaseSnapshot",
    "DebtPositionData",
    "UserPositionSummary",
    "analyze_aave_user_position",
    "db_apply_v3_liquidity_updates",
    "db_apply_v4_liquidity_updates",
    "db_backup_database",
    "db_compact_database",
    "db_convert_alembic_to_rust_owned",
    "db_create_new_database",
    "db_fetch_exchange",
    "db_fetch_exchange_by_name",
    "db_fetch_pool_row",
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
