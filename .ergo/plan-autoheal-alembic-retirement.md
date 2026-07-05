# Auto-healed Alembic retirement (dump-and-restore cutover)

Retire the incoherent, non-forward-buildable Alembic migration chain
(`src/degenbot/migrations/`) without rugpulling users who have built up large
in-place databases of pools and liquidity positions. The mechanism is an
**out-of-place dump-and-restore "heal"**: build a fresh DB at the Rust-DDL
head schema, copy user rows from the old DB into it preserving primary keys
and FK integrity, stamp it `RustOwned` directly (never running any Alembic
code), then atomically swap it into place (old DB preserved as `*.bak`).

Once `heal` ships and is proven, the Alembic dependency, the migrations
directory, and the `alembic_version`-reading branch of `ensure_schema` can be
retired (the 0.7.0 release boundary) **non-breaking-ly**: any legacy DB — at
head, stale, or unrecognized — can `degenbot database heal` into a RustOwned
DB regardless of its old state, so removing Alembic strands nobody.

This epic delivers the durable, rugpull-protected Alembic retirement that the
cutover epic (2Z3Y46 / `cutover` command, `1b66da1f`) deliberately deferred.

## Why out-of-place, not in-place

The existing `database cutover` (`1b66da1f`) is an **in-place** ownership
flip — it assumes the old DB is already at the Alembic head schema (the common
case), so it only drops `alembic_version` and stamps
`_degenbot_db_schema_version`. It is fast and instant, but it cannot repair a
stale or divergent schema.

`heal` is the **general** complement: it reads the old DB as raw SQLite
(whatever columns actually exist there), rebuilds against the coherent Rust
head schema, and applies a **column-mapping table** derived from the migration
history (renames → map, adds → fill default, drops → ignore). It never invokes
`alembic upgrade`, so the non-forward-buildable chain is bypassed entirely.
Both commands coexist: `cutover` for the fast head→RustOwned path,
`heal` as the safe retire / divergent-schema path.

## Non-goals

- **Repairing the Alembic chain in place** (adding the missing
  `add_column`s, guarding the drops, batch-wrapping the alters across all 43
  migrations). The chain is structurally incoherent (e.g. `311beed36e7b`
  "add factory and deployer" alters a `factory` column no migration creates;
  `87fd9fc7ae00` drops `ix_managed_pool_hash` no migration creates). Repair is
  large, fragile, and spent on something about to be deleted. `heal` removes
  the need for forward-buildability; retirement removes the chain.
- **Auto-heal-on-open.** `degenbot database heal` is opt-in through 0.7.0.
  Automatic heal-on-`DegenbotDb::open` (mirroring the eventual automatic
  cutover-on-open) is a 0.8 concern, not built here. Rationale: a full
  out-of-place copy + atomic swap is a surprising, slow operation to trigger
  implicitly; an explicit command lets users back up first and cutover
  deliberately.
- **The `cutover` command itself** (already shipped, `1b66da1f`). This epic
  only adds `heal`; it does not modify `cutover`.

## Constraints

- **Standalone-Rust constraint (AGENTS.md).** The data-copy logic, the
  column-mapping table, the FK-ordered write plan, and the atomic swap all
  belong in `degenbot-db` (zero `pyo3`). A `cargo add degenbot` consumer must
  be able to call `heal_database(path)` directly. The PyO3 wrapper is arg
  extraction → GIL release → core call → result wrap; no logic.
- **Never mutate the old DB in place.** The heal reads the old DB read-only,
  writes a new file at a temp path, and only swaps via `rename`. If any step
  fails, the live DB is untouched. The old DB is preserved as `*.bak`.
- **Preserve primary keys and FK integrity.** Row `id` values are copied as-is
  so foreign-key references survive the rebuild. `sqlite_sequence` is reset
  to the max copied id per table.
- **`heal` must not depend on Alembic at runtime.** Once shipped, retiring
  `src/degenbot/migrations/` and the `alembic` dependency must not break
  `heal`. The column-mapping table is a static Rust constant derived once
  from the migration history, not a runtime call into Alembic.
- **No kill-list items touched other than the explicit retirement task.**
  `src/degenbot/migrations/` is on the 0.6.x kill-list; only the 0.7.0
  retirement task (gated, last) may delete it.

## Architecture (three-layer placement, per ADR-005)

- **Rust core** (`rust/crates/degenbot-db/src/ops.rs` +
  `rust/crates/degenbot-db/src/heal.rs` new module): `heal_database(old_path)
  -> Result<HealReport, DbError>`. Owns: open-old-read-only, schema
  introspection, the static column-mapping table (`COLUMN_MAP: &[(table,
  old_col, new_col, HealAction)]`), FK-ordered copy plan, PK preservation +
  `sqlite_sequence` reset, stamp-as-RustOwned, atomic `rename` swap +
  `*.bak`, row-count verification. The mapping table encodes: added column
  → fill default/NULL, renamed → map read, dropped → ignore. Pure data
  movement; no business logic.
- **PyO3 seam** (`rust/crates/degenbot-python/src/db/mod.rs` +
  `src/degenbot/degenbot_rs.pyi`): `db_heal_database(database_path) -> dict`
  returning `{old_state, rows_copied_per_table, bak_path, new_state}`. Thin
  wrapper — GIL released across the copy.
- **Python shell** (`src/degenbot/cli/database.py` + operations wrapper in
  `src/degenbot/database/operations.py`): `degenbot database heal
  [--dry-run] [--force]` Click command. `--dry-run` reports old state +
  per-table planned row counts + column-mapping diff, writes nothing.
  `--force` skips the confirm prompt. Default (no flag) → `click.confirm`.

## Map of the heal operation

```
old DB (any state)              new DB (fresh Rust DDL)
┌─────────────────────┐         ┌──────────────────────────┐
│ alembic stamp (any) │         │ full head schema (Rust)  │
│ head-or-stale schema│  copy   │ NO alembic_version table  │
│ user data           │  rows   │ _degenbot_db_schema_     │
│                     │  ────►  │   version = RustOwned     │
└─────────────────────┘         └──────────────────────────┘
         │  preserved as .bak (atomic swap at the end)
         ▼
   old.db.bak
```

1. **Inspect** old DB state (read-only). If `rust_owned`, no-op
   (`already_rust_owned`).
2. **Build** fresh DB at a temp path via Rust DDL
  (`db_create_new_database` — the coherent head schema). This is the same
  primitive `create_new_sqlite_database` uses.
3. **Copy** all rows from old → new, table-by-table in FK-dependency order
   (parents before children: `erc20_tokens` → `exchanges` → `pool_managers`
   → `pools` → `uniswap_v2_pools`/subclass tables → `liquidity_positions` →
   `initialization_maps` → …), applying the column-mapping table per table,
   **preserving PK `id` values** so FK references survive.
4. **Reset** `sqlite_sequence` to max copied id per table (autoincrement
   continues correctly after heal).
5. **Stamp** new DB `RustOwned` directly — no `alembic_version` table is
   created. (The new DB never carried one; `db_create_new_database` stamps
   Alembic-head for back-compat, but heal drops it before stamping RustOwned.
   Alternatively, skip the Alembic stamp entirely on the heal path and go
   straight to RustOwned — cleaner; decide at implementation.)
6. **Verify** per-table row counts: old read count == new read count. Refuse
   the swap on mismatch.
7. **Atomic swap**: `rename(new, live)` then `rename(old, *.bak)` (or copy
   old→bak first if atomic rename isn't safe across the temp dir boundary).
   On any failure mid-heal, the live DB is untouched; partial new file is
   cleaned up.

## Old DB → new DB column-mapping table (static Rust constant)

Derived once from `src/degenbot/migrations/versions/*.py`. Three action
classes:

- **Added-after** (column exists on head/new, may be absent on old):
  `exchanges.factory`, `exchanges.deployer`, `aave_v3_emode_categories.*`,
  `aave_v3_users.*`, `pools.tick_spacing`, `uniswap_v4_pools.*`, …
  → fill with column default / NULL on the new DB.
- **Renamed** (old name → new name): `6a77c4e07151` ("rename_column" —
  `pools.token0_id_` → `pools.token0_id`?), `a4f59783919f` ("rename
  pool_hash_column"), `5c8805573ab3` ("convert_pool_hash_to_text" — type
  change, value-preserving). Map the read.
- **Dropped** (column on old, not on new): `04f858f979a9` ("drop
  factory_column" — but note `factory` still exists on head; verify the
  actual end-state against the Rust DDL, not the migration titles), 
  `901adb947000` ("drop deployer"), `b781499591ed` ("remove
  has_liquidity"), `3199199def8c` ("drop binary pool_hash"). Ignore the
  column on read.

The mapping is **schema-derived, not migration-title-derived**. The
authoritative source is: `PRAGMA table_info` on the old DB vs `PRAGMA
table_info` on a fresh `db_create_new_database` DB. The migration history is
only consulted to disambiguate renamed columns (where a value must be
carried forward under a new name) from dropped columns (where it mustn't).
The implementation should **auto-derive the mapping at heal time** from the
two `PRAGMA table_info` snapshots, with a small explicit override table for
the renamed cases — this is more robust than a hand-maintained 43-entry
constant and survives future head-schema drift.

## Tasks (in dependency order)

### T1 — ADR-011: auto-healed Alembic retirement (design record)

Write `docs/adr/ADR-011-auto-healed-alembic-retirement.md`. Captures: the
out-of-place dump-and-restore decision (vs in-place cutover, vs in-place
chain repair, vs hard retirement); the column-mapping auto-derivation
approach; the atomic-swap + `.bak` guarantee; the relationship to the
existing `cutover` command and to `JFFQV2`; the standalone-Rust placement;
the non-goal of auto-heal-on-open (deferred to 0.8). Update `AGENTS.md` ADR
index to include ADR-011 + the kill-list note that `src/degenbot/migrations/`
retirement is gated on `heal` shipping (not just on 0.7.0 the version).
**Depends on:** nothing. **Unblocks:** T2.

### T2 — Rust core: `heal_database` op + column-mapping auto-derivation

Implement `rust/crates/degenbot-db/src/heal.rs` (+ `ops.rs` re-export +
`lib.rs` pub use). `heal_database(old_path) -> Result<HealReport, DbError>`.
Owns the 7-step operation above. The column mapping is **auto-derived at
heal time** from `PRAGMA table_info(old)` vs `PRAGMA table_info(new
db_create_new_database)`, plus an explicit `RENAME_OVERRIDES` constant for the
handful of renamed columns. FK-dependency order is derived from the
`FOREIGN KEY` declarations in the new schema (a static topological sort, also
a Rust constant). PK preservation: copy `id` as-is. `sqlite_sequence` reset.
Stamp RustOwned (do NOT stamp Alembic head on the heal path — go straight to
RustOwned). Verifies row counts; refuses swap on mismatch. Atomic `rename`
swap + `*.bak`. Refuses unrecognized old DB (no `alembic_version` AND no
`_degenbot_db_schema_version` AND tables-present-but-foreign — i.e. a
genuinely foreign file) with a clear error. Returns `HealReport { old_state,
rows_copied: HashMap<String, u64>, bak_path, new_state }`.
Tests: head→heal (rows preserved, alembic_version gone, RustOwned stamped,
FK integrity intact, sqlite_sequence correct), stale→heal (older-revision
old DB reconstructed via the verified `create_new_sqlite_database` + drop-index
+ stamp-rewrite construction from `YWN7Z6`), unrecognized-refusal,
already-rustowned no-op, row-count-mismatch refusal (inject a corrupt old
DB), atomic-swap-keeps-bak (kill mid-copy is impossible by construction,
but verify `*.bak` exists post-heal). **Depends on:** T1.

### T3 — PyO3 seam: `db_heal_database`

`rust/crates/degenbot-python/src/db/mod.rs` + `src/degenbot/degenbot_rs.pyi`.
Thin: extract `database_path: &str`, `allow_threads(|| heal_database(path))`,
wrap `HealReport` as a `PyObject` dict. No logic. `#[pyfunction]`,
`__all__` entry. Test: round-trip via the seam mirrors the core test.
**Depends on:** T2.

### T4 — CLI: `degenbot database heal [--dry-run] [--force]`

`src/degenbot/cli/database.py` (Click command) + `src/degenbot/database/operations.py`
(thin wrapper `heal_database` + `inspect_heal_plan` for dry-run). `--dry-run`
reports old state + per-table planned row counts + the auto-derived column
mapping (added/renamed/dropped), writes nothing. `--force` skips
`click.confirm`. Default → confirm prompt. Mirrors the `database cutover`
command style (`1b66da1f`). Output: old state, rows copied per table, `.bak`
path, new state. Tests (Click `CliRunner`, mirroring
`tests/cli/test_database_cutover.py`): dry-run-on-alembic-current,
force-heal-on-alembic-current (asserts `*.bak` exists, alembic_version gone,
RustOwned, a pre-written exchange row still reads back — proving
rugpull-protection), force-heal-on-stale, force-heal-on-already-rustowned
no-op, dry-run-on-unrecognized-refusal, confirm-prompt-default. **Depends
on:** T3.

### T5 — pytest: full heal boundary (large-DB rugpull-protection proof)

`tests/cli/test_heal_boundary.py` (or co-located). The contract artifact:
build a representative large old DB (head-stamped, populated with a few
hundred pools + liquidity positions across multiple chains via the existing
PyO3 write seams), then `degenbot database heal --force`, then assert: (a)
every row in every user table survives with identical PKs and FK integrity,
(b) `alembic_version` is gone and `_degenbot_db_schema_version` is stamped
RustOwned, (c) the `.bak` file exists and is openable read-only and still
contains the old data (full recoverability), (d) post-heal writes (a new
exchange/pool upsert) work against the RustOwned DB. This is the artifact
that proves the user-facing promise: no rugpull, full data survival, full
recoverability. **Depends on:** T4.

### T6 — 0.7.0 retirement: drop Alembic dep + migrations dir (gated)

**Gated on T5 passing + an explicit "0.7.0 release" decision.** Delete
`src/degenbot/migrations/` (the kill-listed dir), remove the `alembic` +
`alembic-utils` deps from `pyproject.toml`, remove the
`alembic_version`-reading branch of `rust/crates/degenbot-db/src/migrate.rs::ensure_schema`
(AlembicCurrent/AlembicStale states collapse — every DB is either
FreshStandalone / RustOwned / Unrecognized), remove the Alembic fall-back
from `upgrade_existing_sqlite_database` (it becomes `cutover`-or-error, or
better, aliases to `heal` for safety). Update ADR-010 + ADR-011 to mark
retirement complete. Remove the kill-list entry for
`src/degenbot/migrations/` from `AGENTS.md`. This is the
`JFFQV2`-placeholder successor from epic 2Z3Y46, executed now that `heal`
makes it non-breaking. **Depends on:** T5 + 0.7.0-release-decision (human
checkpoint).