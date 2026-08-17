from typing import Any

def db_create_new_database(path: str) -> None:
    """Create a fresh degenbot SQLite DB: WAL + head DDL + VACUUM + Alembic stamp.

    Args:
        path: Filesystem path for the new database (created if absent)

    Raises:
        ValueError: On any connection / PRAGMA / DDL / stamp failure

    """

def db_backup_database(src: str, dst: str) -> None:
    """Back up one SQLite DB into another via online backup.

    Asserts `PRAGMA integrity_check == "ok"` on both source (before) and
    destination (after). `dst` is created if absent and overwritten if present.

    Args:
        src: Source database path
        dst: Destination database path (created/overwritten)

    Raises:
        ValueError: On an open / backup / integrity-check failure

    """

def db_compact_database(path: str) -> None:
    """Compact a SQLite database via `VACUUM` (no-op for `:memory:`).

    Args:
        path: Database path

    Raises:
        ValueError: On a connection / VACUUM failure

    """

def db_upgrade_database(path: str) -> str:
    """Ensure the database is at the Alembic schema head.

    Returns ``"already_at_head"`` if the DB was current (no-op), or
    ``"created_fresh"`` if an empty file was brought up to head. A stale
    Alembic DB raises ``ValueError`` (run ``alembic upgrade head`` from Python).

    Args:
        path: Database path

    Returns:
        ``"already_at_head"`` or ``"created_fresh"``

    Raises:
        ValueError: On a stale / unrecognized schema, or an I/O failure

    """

def db_inspect_schema_state(database_path: str) -> str:
    """Inspect the schema state WITHOUT writing.

    The read-only dry-run companion to
    :func:`db_convert_alembic_to_rust_owned`. Never refuses (reports even
    stale / unrecognized states). Returns one of ``"alembic_current"``,
    ``"alembic_stale"``, ``"fresh_standalone"``, ``"rust_owned"``,
    ``"unrecognized"``.

    Args:
        database_path: Database path

    Returns:
        The schema-state label.

    Raises:
        ValueError: On an open / query failure.

    """

def db_convert_alembic_to_rust_owned(database_path: str) -> str:
    """Perform the opt-in one-way cutover (ADR-010).

    Flip an Alembic-stamped DB into Rust ownership — drops
    ``alembic_version``, stamps ``_degenbot_db_schema_version``.

    Args:
        database_path: Database path

    Returns:
        ``"converted"`` (was AlembicCurrent) or ``"already_rust_owned"``
        (was already Rust-owned → idempotent no-op).

    Raises:
        DatabaseSchemaStale: For a stale Alembic DB (run
            ``degenbot database upgrade`` first).
        ValueError: For an unrecognized (foreign) file or I/O failure.

    """

def db_heal_database(database_path: str) -> dict[str, Any]:
    """Out-of-place dump-and-restore heal (ADR-011).

    Rebuild the DB at the Rust head schema, copy user rows preserving PKs +
    FK integrity (in FK-dependency order), stamp RustOwned directly (never
    runs Alembic code), then atomically swap with a ``*.bak`` backup. Never
    mutates the old DB in place — a read-only open feeds the copy, so the
    old file is left byte-identical until the final ``rename``.

    Args:
        database_path: Database path

    Returns:
        ``{"old_state": str, "rows_copied": dict[str, int],
        "bak_path": str, "new_state": str, "warnings": list[str]}``.
        No-op if old is already ``rust_owned`` (returns
        ``old_state == new_state == "rust_owned"``, empty ``rows_copied``,
        ``bak_path == database_path``).

    Raises:
        ValueError: For an unrecognized (foreign) file, an I/O failure, or a
            post-copy row-count verification failure (live DB untouched in
            both cases).

    """

def db_apply_v3_liquidity_updates(
    database_path: str,
    chain_id: int,
    pool_address: str,
    events: list[LiquidityUpdateEvent],
) -> bool:
    """Apply pre-decoded V3 liquidity events; persist positions/init-maps/marker.

    Returns ``False`` if the pool at ``(chain_id, pool_address)`` isn't found
    (mirrors the Python early-return); ``True`` after a successful apply.
    Raises ``ValueError`` on a DB failure.
    """

def db_apply_v4_liquidity_updates(
    database_path: str,
    pool_hash_hex: str,
    pool_manager_chain: int,
    events: list[LiquidityUpdateEvent],
) -> bool:
    """Apply pre-decoded V4 liquidity events; persist positions/init-maps/marker.

    Returns ``False`` if the pool at ``(pool_hash, pool_manager_chain)`` isn't
    found; ``True`` after a successful apply. Raises ``ValueError`` on a DB
    failure.
    """

def db_fetch_pool_row(
    database_path: str,
    chain_id: int,
    address: str,
) -> LiquidityPoolRow | None:
    """Fetch a `pools` row by ``(chain_id, address)`` (QJSCA5 §4.3).

    The V3 `apply_3_liquidity_updates` shell uses this to read the pool's
    `exchange_id` for the `exchanges_in_scope` precondition. Raises
    ``ValueError`` on a DB failure.
    """

def db_fetch_exchange(
    database_path: str,
    exchange_id: int,
) -> ExchangeRow | None:
    """Fetch an `exchanges` row by its FK id.

    The `cli/pool.py::pool_update` discovery loop reads `last_update_block`
    ground-truth here (a fresh connection → fresh WAL snapshot) rather than
    trusting the long-lived SQLAlchemy session's stale ORM cache, since the
    stamp is written by the Rust `db_set_exchange_last_update_block` seam on
    its own connection. Raises ``ValueError`` on a DB failure.
    """

def db_fetch_exchange_by_name(
    database_path: str,
    chain_id: int,
    name: str,
) -> ExchangeRow | None:
    """Fetch an `exchanges` row by `(chain_id, name)` (the deactivate-CLI resolution)."""

class V2PoolRowInput:
    """One V2 pool-row to upsert (WR7EA6)."""

    def __init__(
        self,
        address: str,
        token0_address: str,
        token1_address: str,
        fee_token0: int,
        fee_token1: int,
        stable: bool | None = ...,
    ) -> None: ...

class V3PoolRowInput:
    """One V3 pool-row to upsert (WR7EA6)."""

    def __init__(
        self,
        address: str,
        token0_address: str,
        token1_address: str,
        fee: int,
        tick_spacing: int,
    ) -> None: ...

class V4PoolRowInput:
    """One V4 pool-row to upsert (WR7EA6)."""

    def __init__(
        self,
        pool_hash: str,
        hooks: str,
        currency0_address: str,
        currency1_address: str,
        fee: int,
        tick_spacing: int,
    ) -> None: ...

def db_upsert_v2_pools(
    database_path: str,
    chain_id: int,
    kind: str,
    exchange_id: int,
    fee_denominator: int,
    rows: list[V2PoolRowInput],
) -> None:
    """Insert a batch of V2 pool rows (WR7EA6).

    The Rust core get-or-create's the two `erc20_tokens` per row + inserts the
    polymorphic base `pools` row + the subclass detail row. Raises
    ``ValueError`` if `kind` is not a known V2 family discriminator.
    """

def db_upsert_v3_pools(
    database_path: str,
    chain_id: int,
    kind: str,
    exchange_id: int,
    fee_denominator: int,
    rows: list[V3PoolRowInput],
) -> None:
    """Insert a batch of V3 pool rows (WR7EA6).

    Same shape as `db_upsert_v2_pools`; subclass detail row carries `tick_spacing`
    + the fee columns. Raises ``ValueError`` if `kind` is not a V3 family.
    """

def db_upsert_v4_pools(
    database_path: str,
    chain_id: int,
    pool_manager_address: str,
    fee_denominator: int,
    rows: list[V4PoolRowInput],
) -> None:
    """Insert a batch of V4 pool rows (WR7EA6).

    The Rust core resolves the `PoolManagerTable` id from
    `(chain_id, pool_manager_address)`, then per row inserts the `managed_pools`
    base + `uniswap_v4_pools` detail row. Raises ``ValueError`` if no
    `PoolManager` row matches.
    """

def db_set_exchange_last_update_block(
    database_path: str,
    chain_id: int,
    exchange_id: int,
    block: int,
) -> None:
    """Stamp an `ExchangeTable.last_update_block` (WR7EA6)."""

def db_upsert_exchange(
    database_path: str,
    chain_id: int,
    name: str,
    factory: str,
    deployer: str | None,
) -> ExchangeRow:
    """Resolve an `exchanges` row by `(chain_id, name)`, inserting `active=False` if absent."""

def db_set_exchange_active(
    database_path: str,
    exchange_id: int,
    active: bool,
) -> None:
    """Flip an `exchanges` row's `active` flag by id (activate/deactivate primitive)."""

def db_upsert_pool_manager(
    database_path: str,
    address: str,
    chain: int,
    kind: str,
    state_view: str | None,
    exchange_id: int,
) -> PoolManagerRow:
    """Upsert a `pool_managers` row by `(address, chain)` (V4 manager get-or-create)."""

class RustDatabaseSnapshot:
    """Read-only V3/V4 snapshot handle over a degenbot SQLite DB file.

    Opens its own connection (WAL, ``query_only=on``) from ``database_path``;
    the Python ``DatabaseSnapshot`` shell constructs one per chain and
    delegates every read to it.

    """

    def __init__(self, chain_id: int, database_path: str) -> None: ...
    def get_liquidity_map_v3(self, pool_address: str) -> dict[str, Any] | None: ...
    def get_liquidity_map_v4(
        self, pool_manager: str, pool_id: bytes | str
    ) -> dict[str, Any] | None: ...
    def get_all_liquidity_maps_v3(self) -> dict[str, dict[int, tuple[int, int]]]: ...
    def get_all_liquidity_maps_v4(
        self,
    ) -> dict[tuple[str, str], dict[int, tuple[int, int]]]: ...
    def get_newest_block_v3(self) -> int | None: ...
    def get_newest_block_v4(self) -> int | None: ...
    def get_pools_v3(self) -> set[str]: ...
    def get_pools_v4(self) -> set[str]: ...

class RustDatabasePositionQuery:
    """Read-only Aave V3 position-query handle over a degenbot SQLite DB file.

    Opens its own connection (WAL, ``query_only=on``) from ``database_path``;
    the Python ``DatabasePositionQuery`` shell constructs one and delegates
    every read to it.

    """

    def __init__(self, database_path: str) -> None: ...
    def get_users_with_debt(
        self, market_id: int, limit: int | None = None
    ) -> list[dict[str, Any]]: ...
    def get_collateral_positions(self, user_id: int) -> list[dict[str, Any]]: ...
    def get_debt_positions(self, user_id: int) -> list[dict[str, Any]]: ...
    def get_collateral_config_map(self, user_id: int) -> dict[int, bool]: ...
    def get_oracle_address(self, market_id: int) -> str | None: ...
    def get_asset_addresses(self, market_id: int) -> list[str]: ...

def analyze_aave_user_position(
    user: dict[str, Any],
    collateral_positions: list[dict[str, Any]],
    debt_positions: list[dict[str, Any]],
    collateral_config_map: dict[int, bool],
    price_map: dict[str, int] | None = None,
) -> UserPositionSummary:
    """Analyze a single user's Aave V3 position for liquidation risk.

    Pure math (no I/O) over the Rust ``degenbot-aave::analysis`` core. Takes
    the plain ``dict`` records ``RustDatabasePositionQuery.get_*`` returns + a
    config map + an optional price map, and returns a
    :class:`UserPositionSummary` with attribute access matching the Python
    ``UserPositionSummary`` dataclass.

    Args:
        user: A ``dict`` with keys ``id``, ``address``, ``market_id``,
            ``e_mode``, ``is_isolation_mode``, ``isolation_mode_debt``,
            ``isolation_debt_ceiling``.
        collateral_positions: A ``list[dict]`` (the
            ``get_collateral_positions`` row shape).
        debt_positions: A ``list[dict]`` (the ``get_debt_positions`` row
            shape).
        collateral_config_map: A ``dict[int, bool]`` (``asset_id`` ->
            enabled).
        price_map: An optional ``dict[str, int]`` (address -> oracle price in
            8 decimals). When ``None``, prices are treated as 1.

    Returns:
        The position summary.

    Raises:
        ValueError: On a missing dict key, a malformed value, or a scaled-
            balance computation overflow.

    """

class CollateralPositionData:
    """Collateral position with calculated values (Rust-backed)."""

    @property
    def asset_address(self) -> str: ...
    @property
    def asset_symbol(self) -> str | None: ...
    @property
    def scaled_balance(self) -> int: ...
    @property
    def actual_balance(self) -> int: ...
    @property
    def liquidation_threshold(self) -> int: ...
    @property
    def ltv(self) -> int: ...
    @property
    def is_enabled_as_collateral(self) -> bool: ...
    @property
    def in_emode(self) -> bool: ...
    @property
    def emode_category_id(self) -> int | None: ...
    @property
    def price(self) -> int | None: ...

class DebtPositionData:
    """Debt position with calculated values (Rust-backed)."""

    @property
    def asset_address(self) -> str: ...
    @property
    def asset_symbol(self) -> str | None: ...
    @property
    def scaled_balance(self) -> int: ...
    @property
    def actual_balance(self) -> int: ...
    @property
    def stable_debt(self) -> bool: ...
    @property
    def in_emode(self) -> bool: ...
    @property
    def emode_category_id(self) -> int | None: ...
    @property
    def price(self) -> int | None: ...

class UserPositionSummary:
    """A user's Aave V3 position summary (Rust-backed).

    Rust-backed mirror of the Python ``UserPositionSummary`` dataclass. The
    Step C cutover swaps the Python dataclass for this ``#[pyclass]`` — the
    CLI + the parity test read the same attribute names.
    """

    @property
    def user_address(self) -> str: ...
    @property
    def market_id(self) -> int: ...
    @property
    def emode_category_id(self) -> int | None: ...
    @property
    def is_isolation_mode(self) -> bool: ...
    @property
    def collateral_positions(self) -> list[CollateralPositionData]: ...
    @property
    def debt_positions(self) -> list[DebtPositionData]: ...
    @property
    def total_collateral_value(self) -> int: ...
    @property
    def total_debt_value(self) -> int: ...
    @property
    def health_factor(self) -> float | None: ...
    @property
    def max_ltv_ratio(self) -> float | None: ...
    @property
    def is_at_risk(self) -> bool: ...
    @property
    def is_liquidatable(self) -> bool: ...
    @property
    def has_debt(self) -> bool: ...

class LiquidityPoolRow:
    """A typed `pools` DB row (QVMWQC)."""

    @property
    def id(self) -> int: ...
    @property
    def address(self) -> str: ...
    @property
    def chain(self) -> int: ...
    @property
    def kind(self) -> str: ...
    @property
    def token0_id(self) -> int: ...
    @property
    def token1_id(self) -> int: ...
    @property
    def exchange_id(self) -> int: ...

class ExchangeRow:
    """A typed `exchanges` DB row (QVMWQC)."""

    @property
    def id(self) -> int: ...
    @property
    def chain_id(self) -> int: ...
    @property
    def name(self) -> str: ...
    @property
    def active(self) -> bool: ...
    @property
    def last_update_block(self) -> int | None: ...
    @property
    def factory(self) -> str: ...
    @property
    def deployer(self) -> str | None: ...

class PoolManagerRow:
    """A typed `pool_managers` DB row (V4) (QVMWQC)."""

    @property
    def id(self) -> int: ...
    @property
    def address(self) -> str: ...
    @property
    def chain(self) -> int: ...
    @property
    def kind(self) -> str: ...
    @property
    def state_view(self) -> str | None: ...
    @property
    def exchange_id(self) -> int: ...

class LiquidityUpdateEvent:
    """A decoded liquidity-update event record (QJSCA5 §4.3).

    The `(block_number, log_index, tick_lower, tick_upper, liquidity_delta)`
    tuple the Rust apply loop consumes. `liquidity_delta` is the signed delta
    (V3 Burn negated; V4 Modify decoded signed). Built by the Python apply
    shells; the Rust core applies + persists.
    """

    def __init__(
        self,
        block_number: int,
        log_index: int,
        tick_lower: int,
        tick_upper: int,
        liquidity_delta: int,
    ) -> None: ...

class DatabaseSchemaStale(ValueError):
    """The DB is stamped at a prior Alembic revision.

    Raised by the degenbot-db PyO3 seam (``DbError::AlembicStale``) when a
    connexion is opened against a DB whose ``alembic_version`` predates the
    compiled head — e.g. a user upgrading from the published 0.6.0a2 schema
    (``e0aaad8ad486``) to the dev head (``2606a6c7f5ee``). The Rust core is
    a reader of Alembic-headed DBs, never a migrator, so it refuses with
    this typed exception instead. Subclasses ``ValueError`` so the
    ``database upgrade`` shell's broad catch keeps working; the CLI root
    group catches it to print a friendly one-line "run ``degenbot database
    upgrade``" hint instead of a Python traceback.
    """

__all__ = [
    "CollateralPositionData",
    "DatabaseSchemaStale",
    "DebtPositionData",
    "ExchangeRow",
    "LiquidityPoolRow",
    "LiquidityUpdateEvent",
    "PoolManagerRow",
    "RustDatabasePositionQuery",
    "RustDatabaseSnapshot",
    "UserPositionSummary",
    "V2PoolRowInput",
    "V3PoolRowInput",
    "V4PoolRowInput",
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
