# ADR-011: Auto-healed Alembic Retirement (Dump-and-Restore Cutover)

**Status: proposed.** Epic `TGIP5N` filed; implementation gated on tasks
T2-T5 landing before the 0.7.0 retirement is executed in T6 / the
`OXKANZ` release checkpoint. ADR-010's retention/cutover path remains in
force until `heal` ships and is proven.

## Context

ADR-010 established the in-place `cutover` (the fast ownership flip
assuming the head schema) and gated Alembic retirement to a 0.7 release
on the assumption that the 0.7 step would be a clean deletion. Scoping
that retirement (ergo task `JFFQV2`, superseded by `TGIP5N`) surfaced a
blocking defect: **the Alembic migration chain is structurally
incoherent and not forward-buildable**, so a clean deletion would strand
every legacy database at a stale or unrecognized state.

### The migration chain is structurally incoherent

Concrete defects, verified by the orchestrator during `JFFQV2`/`YWN7Z6`
scoping:

- **`311beed36e7b` ("add factory and deployer to exchange table")** does
  `op.alter_column("exchanges", "factory", ..., nullable=False)` — but
  **no migration ever `add_column`s `factory` or `deployer`**. The
  columns exist on the head schema only because
  `create_new_sqlite_database` applies the full Rust DDL directly and
  stamps `alembic_version=ALEMBIC_HEAD` for back-compat. The migration
  "adds" columns it never creates; it only "works" for incremental
  upgraders who already carried the columns from a pre-Rust-DDL era.
  It also uses bare `op.alter_column(..., nullable=False)`, which SQLite
  does not support (it requires `batch_alter_table`).
- **`87fd9fc7ae00` ("update uniswap v4 table indices")** does
  `op.drop_index("ix_managed_pool_hash")` — but **no migration ever
  creates** `ix_managed_pool_hash`. Fails on any forward build.

### Empirical verification

`alembic upgrade head` (or to any revision ≥ `87fd9fc7ae00`) from an
empty file fails. Only revisions below `87fd9fc7ae00` build forward;
upgrading from any of those to head still crosses the broken migration.
The standard remediation paths are also blocked: `alembic downgrade`
from the head is unsupported (the head migration raises
`NotImplementedError`), and a `2606a6c7f5ee`-stamped DB re-stamped to
its parent and re-upgraded hits the head migration's non-idempotent
`CREATE INDEX ix_erc20_tokens_chain`.

### Root cause

The Alembic migrations were written as mirrors of SQLAlchemy model
changes, not as self-contained schema operations. When a model gained a
`factory` column, the column was actually applied by
`Base.metadata.create_all()` (or carried from a pre-Alembic era) for new
DBs, and someone wrote `311beed36e7b` to alter it — assuming presence
rather than adding it. The chain was never forward-buildable; it only
"works" for DBs that already carried the columns from the ORM /
`create_all` path.

### Why real users never hit this

Real users apply migrations **incrementally** — one or a few per
release, never re-running old ones — so `ix_managed_pool_hash` and
`factory`/`deployer` existed in their DB at the time those migrations
ran. Forward-build-from-empty re-runs everything fresh and exposes the
gaps. This is why the in-place `cutover` (ADR-010) works for head-stamped
DBs and the Alembic fall-back in `database upgrade` works for genuinely
stale (incrementally-arrived-at) DBs: neither re-runs the chain from
scratch.

The problem is solely the **retirement step**: deleting
`src/degenbot/migrations/` and the Alembic dependency without a
replacement strands every legacy DB that is not exactly at the head
schema — the rugpull ADR-010 explicitly promised to avoid.

## Decision

**Out-of-place dump-and-restore "heal" operation.** A new command,
`degenbot database heal`, reads the old DB read-only as raw SQLite,
rebuilds a fresh DB against the Rust head schema, copies rows preserving
primary keys and foreign-key integrity with an auto-derived column
mapping, stamps it `RustOwned` directly (running **no** Alembic code),
and atomically swaps it into place with a `*.bak` backup.

### Considered alternatives

Four approaches were evaluated; the out-of-place heal is selected.

1. **In-place chain repair** — add the missing `add_column`s, guard the
   drops, `batch_alter_table`-wrap the alters across all 43 migrations.
   **Rejected.** Large, fragile effort spent on a retirement-bound
   artifact whose only consumer is the test suite. *"Don't repair what
   you're about to delete."*
2. **Squash to a single coherent baseline** — the standard Alembic
   remedy for rotted history (replace the 43 migrations with one
   baseline whose `upgrade()` creates the complete head schema).
   Legitimate, but mid-sized effort on something about to be deleted,
   and `heal` makes forward-buildability irrelevant entirely.
   **Rejected as bridge.**
3. **Hard retirement** — just delete Alembic + the migrations.
   **Rejected.** Strands every legacy DB at a stale/unrecognized state
   (the rugpull ADR-010 promised to avoid).
4. **Out-of-place heal** — **Selected.** Read old DB read-only, rebuild
   against the Rust head schema, copy rows preserving PKs + FK integrity
   + column mapping, stamp `RustOwned` directly (never runs Alembic
   code), atomic swap with `*.bak` backup. Makes the migration chain's
   forward-buildability — and its very existence — irrelevant.

### The heal operation

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

Seven steps (from the epic plan body):

1. **Inspect old** (read-only). If the old DB is already `RustOwned`,
   no-op. If `Unrecognized`, refuse (a foreign SQLite file passed by
   mistake). `AlembicCurrent`, `AlembicStale`, and any
   head-or-stale-schema state proceed.
2. **Build fresh DB** at a temp path via `db_create_new_database` — the
   coherent head schema (the same primitive
   `create_new_sqlite_database` uses).
3. **Copy all rows** old → new in FK-dependency order, applying the
   auto-derived column-mapping (below) and **preserving PK `id` values**.
4. **Reset `sqlite_sequence`** to the max copied `id` per table, so
   subsequent inserts don't collide with preserved PKs.
5. **Stamp `RustOwned` directly.** No `alembic_version` table is created
   on the new DB; the Alembic-head stamp step is **skipped entirely** on
   the heal path.
6. **Verify per-table row counts.** Refuse the swap on any mismatch.
7. **Atomic swap + preserve old as `*.bak`.** Rename the new DB over the
   old path; preserve the old file as `<path>.bak`.

On any failure mid-heal, the live DB is untouched and the partial new
file is cleaned up.

### Column-mapping auto-derivation (NOT a hand-maintained constant)

The column mapping is **derived at heal time** from
`PRAGMA table_info(old)` vs `PRAGMA table_info(new)` (where `new` is the
fresh `db_create_new_database` output). Three action classes:

- **added-after** — column on `new`, absent on `old`: fill with the
  column default / `NULL`.
- **renamed** — old name `X` → new name `Y`: map the read.
- **dropped** — column on `old`, not on `new`: ignore the read.

Renamed-vs-dropped is disambiguated via a small explicit
`RENAME_OVERRIDES: &[(table, old_col, new_col)]` Rust constant covering
the rename migrations:

- `6a77c4e07151` (rename_column),
- `a4f59783919f` (rename_pool_hash_column),
- `5c8805573ab3` (convert_pool_hash_to_text — type change,
  value-preserving).

Migration history is consulted **only** for this disambiguation.
Auto-derivation survives future head-schema drift; the constant is small
and explicit, and the FK-dependency order is derived from the schema's
own FK declarations (not hand-maintained).

### Atomic-swap + `.bak` guarantee

`heal` reads the old DB read-only, writes the new DB at a temp path,
and swaps via `rename`. Any failure leaves the live DB untouched. The
old file is preserved as `*.bak` for full recoverability — openable
read-only post-heal, contains the old data. This is the property that
protects users with large in-place DBs (the rugpull-protection promise
from ADR-010).

## Consequences

- **Positive:** Alembic retirement (0.7.0) becomes **non-breaking** —
  any legacy DB (`alembic_current`, `alembic_stale`, or unrecognized
  with content) can `degenbot database heal` into a `RustOwned` DB
  regardless of its old state. The incoherent migration chain's
  forward-buildability stops mattering; the chain can be deleted
  without stranding users.
- **Positive:** `heal` does **not** depend on Alembic at runtime (the
  column mapping is auto-derived + a small static constant), so deleting
  `src/degenbot/migrations/` does not break `heal`.
- **Positive:** Both `cutover` (fast in-place, head-only) and `heal`
  (general, schema-agnostic) coexist; `heal` is the canonical retirement
  mechanism for T6 / `OXKANZ`.
- **Negative:** `heal` is slower than `cutover` (full out-of-place copy
  — minutes for large DBs vs instant). Users on head-stamped DBs should
  prefer `cutover`; `heal` is for cautious users, divergent/stale DBs,
  and the retirement path.
- **Negative:** The column-mapping approach requires the
  FK-dependency order and rename-overrides to be maintained as the head
  schema evolves. Auto-derivation mitigates this: the FK order is
  derived from the schema's own FK declarations; the rename overrides
  are a small explicit list.
- **Neutral:** The 0.6.x kill-list on `src/degenbot/migrations/` is
  retained through 0.6.x. Retirement (deletion) is gated on `heal`
  shipping (T2-T5) + the 0.7.0 release decision (T6 / `OXKANZ`).

## Relationship to ADR-010

ADR-010 established the in-place `cutover` (fast ownership flip assuming
the head schema). ADR-011 adds the general out-of-place `heal`
complement. ADR-010's "0.7 retirement" gate is now **executed** via
ADR-011's `heal` (T6 / `OXKANZ`), making the retirement non-breaking.
When retirement lands, ADR-010's status updates to "superseded by
implementation" (folded with ADR-011).

ADR-010's `AlembicCurrent` / `AlembicStale` states in `ensure_schema`
collapse on retirement — every DB becomes `FreshStandalone` /
`RustOwned` / `Unrecognized`.

## Non-goals

- **Auto-heal-on-open.** `degenbot database heal` is opt-in through
  0.7.0 via the explicit command. Auto-heal-on-`DegenbotDb::open`
  (mirroring the eventual auto-cutover-on-open) is a 0.8 concern, not
  built here — a full out-of-place copy is too slow and surprising to
  trigger implicitly.
- **Repairing the Alembic chain in place.** Out of scope (and pointless
  — about to be deleted).
- **Modifying the existing `cutover` command.** Shipped (`1b66da1f`);
  this ADR adds `heal` only.

## Architectural placement (ADR-005 three-layer)

- **Rust core** (`degenbot-db/src/ops.rs` + new `heal.rs`):
  `heal_database(old_path) -> Result<HealReport, DbError>`. Owns all
  logic. Zero `pyo3` (enforced by `just check-no-pyo3-in-cores`).
- **PyO3 seam** (`degenbot-python/src/db/mod.rs` + `.pyi`):
  `db_heal_database(database_path) -> dict`. Thin — GIL released across
  the copy; no logic.
- **Python shell** (`cli/database.py` + `operations.py`):
  `degenbot database heal [--dry-run] [--force]` Click command.
  Orchestration only.

## Related

- **ADR-010** (Alembic retention + Rust schema cutover) — the in-place
  `cutover` and the `ensure_schema` `RustOwned` state; ADR-011 is its
  retirement complement.
- **ADR-005** (Polars-inspired three-layer architecture) — the layer
  placement for `heal` (Rust core owns the logic; PyO3 seam + Python
  shell are thin).
- **Epic `TGIP5N`** (`.ergo/plan-autoheal-alembic-retirement.md`) — the
  task graph that builds and proves the `heal` path; tasks T2-T5 ship
  the implementation, T6 / `OXKANZ` is the release-decision checkpoint.