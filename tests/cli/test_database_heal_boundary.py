"""Boundary contract test for `degenbot database heal` (ADR-011, ergo task XXAMGS).

This is the **contract artifact gating the 0.7.0 Alembic retirement**
(`OXKANZ`): it proves the out-of-place heal preserves EVERY user row + FK
integrity across a representative large DB, that the ``*.bak`` is fully
recoverable, and that post-heal writes work against the now-RustOwned DB.

Mirrors ``tests/cli/test_database_cutover.py::
test_alembic_to_rust_cutover_boundary_end_to_end`` — the sibling contract
artifact for the in-place cutover — in framing + journey narrative. The
heal path is the general, schema-agnostic retirement route (it accepts
stale, unlike cutover), so this test's fixture spans MULTIPLE tables + FK
relationships (not a single row) and asserts the full rugpull-protection
invariant: nothing is lost, nothing is orphaned, the backup is complete,
and the healed DB is write-correct.
"""

from __future__ import annotations

import pathlib
import sqlite3
from types import SimpleNamespace

import pytest
from click.testing import CliRunner

from degenbot.cli.database import database_heal
from degenbot.database.operations import (
    get_scoped_sqlite_session,
    inspect_schema_state,
)
from degenbot.database.session_manager import DatabaseSessionManager
from degenbot.db import (
    db_create_new_database,
    db_fetch_exchange_by_name,
    db_inspect_schema_state,
    db_upsert_exchange,
    db_upsert_pool_manager,
    db_upsert_v2_pools,
)
from degenbot.updater import V2PoolRowInput

# Real mainnet factory addresses — distinct rows + realistic FK targets.
_UNISWAP_V2_FACTORY = "0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f"
_SUSHISWAP_V2_FACTORY = "0xC0aEe478E3658E261011544Ef30ab5cd06Af0E68"
# A made-up-but-distinct factory for the post-heal write (different chain).
_QUICKSWAP_FACTORY = "0x57573714144171686668996471BE1AbaB84CE565"


def _addr(n: int) -> str:
    """A distinct 20-byte hex address (``0x`` + zero-padded hex of ``n``)."""
    return "0x" + f"{n:040x}"


class _StubBot:
    """Stub bot exposing the attributes the database heal command touches.

    `database_heal` only reads `bot.config.database.path` (it never opens
    `bot.db()` — the heal op opens its own connection in the Rust core).
    A real `DatabaseSessionManager` is attached for parity with the cutover
    boundary test's stub + the autouse `dispose_all` teardown in
    `tests/conftest.py`.
    """

    def __init__(self, path: str) -> None:
        self.config = SimpleNamespace(database=SimpleNamespace(path=path))
        self.db = DatabaseSessionManager(
            get_scoped_sqlite_session(database_path=pathlib.Path(path)),
        )


@pytest.fixture
def populated_alembic_db(tmp_path: pathlib.Path) -> _StubBot:
    """A head-stamped Alembic DB populated across 5+ user tables with FK refs.

    Populated entirely via the Rust write seams (exercises the same write
    path the bot uses, more realistic than raw SQL): two exchanges, two
    batches of V2 pools (each pool row get-or-creates its two `erc20_tokens` +
    the base `pools` row + the V2 subclass detail row), and a pool manager.
    Result: `exchanges`, `erc20_tokens`, `pools`, `uniswap_v2_pools`,
    `sushiswap_v2_pools`, `pool_managers` all populated, with cross-table FK
    references (pools.token0_id/token1_id → erc20_tokens; pools.exchange_id →
    exchanges; pool_managers.exchange_id → exchanges).
    """
    db_path = tmp_path / "large.db"
    db_path_str = str(db_path)
    db_create_new_database(db_path_str)
    db_path.chmod(0o644)

    # Two exchanges (chain 1): uniswap_v2 + sushiswap_v2.
    uniswap = db_upsert_exchange(
        database_path=db_path_str,
        chain_id=1,
        name="uniswap_v2",
        factory=_UNISWAP_V2_FACTORY,
        deployer=None,
    )
    sushiswap = db_upsert_exchange(
        database_path=db_path_str,
        chain_id=1,
        name="sushiswap_v2",
        factory=_SUSHISWAP_V2_FACTORY,
        deployer=None,
    )

    # 5 V2 pools under uniswap_v2. Each distinct pool address + two distinct
    # token addresses; reused tokens where addresses collide (the Rust core
    # get-or-creates erc20_tokens by (address, chain), so re-use is fine).
    uniswap_rows = [
        V2PoolRowInput(
            address=_addr(0x1000 + i),
            token0_address=_addr(0x2000 + 2 * i),
            token1_address=_addr(0x2000 + 2 * i + 1),
            fee_token0=3000,
            fee_token1=3000,
            stable=None,
        )
        for i in range(5)
    ]
    db_upsert_v2_pools(
        database_path=db_path_str,
        chain_id=1,
        kind="uniswap_v2",
        exchange_id=uniswap.id,
        fee_denominator=1000,
        rows=uniswap_rows,
    )

    # 3 V2 pools under sushiswap_v2 (smaller batch — second exchange).
    sushi_rows = [
        V2PoolRowInput(
            address=_addr(0x3000 + i),
            token0_address=_addr(0x4000 + 2 * i),
            token1_address=_addr(0x4000 + 2 * i + 1),
            fee_token0=3000,
            fee_token1=3000,
            stable=None,
        )
        for i in range(3)
    ]
    db_upsert_v2_pools(
        database_path=db_path_str,
        chain_id=1,
        kind="sushiswap_v2",
        exchange_id=sushiswap.id,
        fee_denominator=1000,
        rows=sushi_rows,
    )

    # A pool manager (V4) → populates `pool_managers` (FK exchange_id → exchanges
    # + the eventual managed_pools/uniswap_v4_pools graph). state_view=None.
    db_upsert_pool_manager(
        database_path=db_path_str,
        address=_addr(0x5000),
        chain=1,
        kind="uniswap_v4",
        state_view=None,
        exchange_id=uniswap.id,
    )

    return _StubBot(db_path_str)


def _run(runner: CliRunner, args: list[str], bot: _StubBot):
    return runner.invoke(database_heal, args, obj=bot, catch_exceptions=False)


def _table_exists(db_path: str, name: str) -> bool:
    conn = sqlite3.connect(db_path)
    try:
        return (
            conn.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                (name,),
            ).fetchone()[0]
            == 1
        )
    finally:
        conn.close()


def _row_counts(db_path: str) -> dict[str, int]:
    """Per-table row counts for user content tables (excludes Alembic/Rust ownership tables)."""
    conn = sqlite3.connect(db_path)
    try:
        tables = [
            r[0]
            for r in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table' "
                "AND name NOT LIKE 'sqlite_%' "
                "AND name NOT IN ('alembic_version','_degenbot_db_schema_version')",
            ).fetchall()
        ]
        return {
            t: conn.execute(f'SELECT COUNT(*) FROM "{t}"').fetchone()[0]  # noqa: S608 - table name from sqlite_master (trusted)
            for t in tables
        }
    finally:
        conn.close()


def _assert_fk_integrity(db_path: str) -> None:
    """Every declared FOREIGN KEY ... REFERENCES child column resolves.

    `PRAGMA foreign_key_check` is SQLite's built-in orphan detector: it walks
    every FK across every table and returns one row per orphaned reference.
    Empty result = integrity holds.
    """
    conn = sqlite3.connect(db_path)
    try:
        conn.execute("PRAGMA foreign_keys=ON;")
        orphans = conn.execute("PRAGMA foreign_key_check").fetchall()
        assert orphans == [], f"FK integrity violations on {db_path}: {orphans}"
    finally:
        conn.close()


def _bak_path_for(db_path: str) -> str:
    return db_path + ".bak"


def test_heal_boundary_preserves_all_rows_and_fk_integrity(
    populated_alembic_db: _StubBot,
) -> None:
    """Contract artifact for the heal-based Alembic retirement (ADR-011).

    This is the path a ``pip`` user takes across the 0.6.x → 0.7 boundary via
    heal (the general, schema-agnostic route — unlike cutover which refuses
    stale). It proves ADR-011's rugpull-protection promise end-to-end:
    nothing is lost, nothing is orphaned, the backup is complete, and the
    healed RustOwned DB is write-correct.

    Journey:
      1. Start from a populated head-stamped Alembic DB
         (``populated_alembic_db`` fixture) — a representative large fixture
         spanning 6 user tables with cross-table FK references.
      2. Snapshot the pre-heal per-table row counts + confirm the
         FK-integrity invariant holds on the old DB.
      3. ``degenbot database heal --force`` → ``rust_owned``: drop
         ``alembic_version``, stamp ``_degenbot_db_schema_version``.
      4. Post-heal: per-table row counts match the snapshot EXACTLY (every
         user row preserved); FK integrity still holds on the healed DB.
      5. The ``.bak`` exists + is readable + its per-table counts match the
         snapshot too (full recoverability). The WAL sidecars
         (``.bak-wal``/``.bak-shm``) travel alongside if the old DB was WAL
         mode (FCA2HP substrate — the ``.bak`` is a complete recoverable
         snapshot).
      6. Post-heal WRITE works against the now-RustOwned DB: a NEW exchange
         row on a different chain reads back via the Rust seam — proving the
         healed DB is write-correct in its new ownership state.

    The substrate shipped across FCA2HP (``ca782237`` — the heal op) →
    MZ55NP (``67691f7c`` — the PyO3 seam) → WEDVGE (``dbf591d1`` — the CLI
    command); this test only exercises the assembled pipeline.
    """
    bot = populated_alembic_db
    db_path_str = str(bot.config.database.path)

    # ── Step 1-2: snapshot pre-heal state + FK integrity on the old DB. ──
    assert db_inspect_schema_state(db_path_str) == "alembic_current"
    assert _table_exists(db_path_str, "alembic_version")
    pre_counts = _row_counts(db_path_str)
    # Sanity: the fixture populated ≥5 user tables with ≥2 rows each.
    assert len(pre_counts) >= 5, f"fixture too small: {pre_counts}"
    populated = {t: n for t, n in pre_counts.items() if n > 0}
    assert len(populated) >= 5, f"expected ≥5 populated tables: {populated}"
    assert populated["exchanges"] == 2
    assert populated["pools"] == 8  # 5 uniswap_v2 + 3 sushiswap_v2
    assert populated["erc20_tokens"] >= 8  # ≥2 distinct tokens per pool, some reuse
    assert populated["uniswap_v2_pools"] == 5
    assert populated["sushiswap_v2_pools"] == 3
    assert populated["pool_managers"] == 1
    _assert_fk_integrity(db_path_str)  # old DB is clean before heal.

    # ── Step 3: heal --force → rust_owned. ───────────────────────────────
    runner = CliRunner()
    result = _run(runner, ["--force"], bot)
    assert result.exit_code == 0, result.output
    assert "healed" in result.output.lower()
    assert "rust_owned" in result.output

    # ── Step 4: every user row preserved + FK integrity on the healed DB. ─
    assert not _table_exists(db_path_str, "alembic_version")
    assert _table_exists(db_path_str, "_degenbot_db_schema_version")
    assert inspect_schema_state(database_path=pathlib.Path(db_path_str)) == "rust_owned"
    post_counts = _row_counts(db_path_str)
    assert post_counts == pre_counts, (
        f"row-count mismatch after heal (pre vs post):\n  pre:  {pre_counts}\n  post: {post_counts}"
    )
    _assert_fk_integrity(db_path_str)  # healed DB is clean.

    # ── Step 5: the .bak is fully recoverable (holds the OLD data). ───────
    bak_path = _bak_path_for(db_path_str)
    assert pathlib.Path(bak_path).exists(), f".bak missing at {bak_path}"
    bak_counts = _row_counts(bak_path)
    assert bak_counts == pre_counts, (
        f".bak row-count mismatch (backup should hold OLD data):\n"
        f"  pre:  {pre_counts}\n  bak:  {bak_counts}"
    )
    _assert_fk_integrity(bak_path)  # the backup is clean too.
    # The backup retains the OLD Alembic ownership shape.
    assert _table_exists(bak_path, "alembic_version")
    assert not _table_exists(bak_path, "_degenbot_db_schema_version")
    # The WAL sidecars travel alongside the backup (FCAHP substrate: the old
    # DB was WAL mode, so its -wal/-shm are relocated next to the .bak to
    # make it a complete recoverable snapshot). At least the main .bak file
    # is present; sidecars may or may not exist depending on WAL flush state.
    assert pathlib.Path(bak_path).exists()

    # ── Step 6: post-heal WRITE against the now-RustOwned DB. ────────────
    # A new exchange on a DIFFERENT chain (137 = Polygon) — a genuinely new
    # row, distinct from the pre-heal chain-1 rows. Proves the healed DB is
    # write-correct in its new RustOwned state, not just read-correct.
    new_row = db_upsert_exchange(
        database_path=db_path_str,
        chain_id=137,
        name="quickswap",
        factory=_QUICKSWAP_FACTORY,
        deployer=None,
    )
    assert new_row.id is not None
    fetched = db_fetch_exchange_by_name(
        database_path=db_path_str,
        chain_id=137,
        name="quickswap",
    )
    assert fetched is not None
    assert fetched.id == new_row.id
    # The new row is distinct from the pre-heal chain-1 exchanges.
    assert (
        fetched.id
        != db_fetch_exchange_by_name(
            database_path=db_path_str,
            chain_id=1,
            name="uniswap_v2",
        ).id
    )
