# Alembic revision self-heal fixture matrix

One fixture per **released** Alembic head, consumed by
`tests/auto_heal_matrix.rs`. Each proves the ADR-052 D5 load-bearing promise:
a DB that a released degenbot version stamped self-heals to the current Rust
owner at open — the user never applies a migration.

## Released revisions (how the set was chosen)

"Released revision" = the Alembic head present at each git release tag. The
head was computed per tag as the `revision` in
`src/degenbot/migrations/versions/` that no other migration names as
`down_revision`:

| git tag(s) | Alembic head | fixture |
|---|---|---|
| 0.5.0a1 (pre-Alembic) | — | `9347bbfcd47a.db` (the revision's initial baseline) |
| 0.5.0a2 | `756fba1f75f4` | `756fba1f75f4.db` |
| 0.5.1b1 (`test-0.5.1b1.post2`) | `9c411aeeb15e` | `9c411aeeb15e.db` |
| v0.6.0a1, v0.6.0a1.post1 | `b0b9e84d5527` | `b0b9e84d5527.db` |
| v0.6.0a2 | `e0aaad8ad486` | `e0aaad8ad486.db` |
| v0.6.0a3 … v0.6.0a10 | `2606a6c7f5ee` (= `ALEMBIC_HEAD`) | `2606a6c7f5ee.db` |

Six distinct heads cover all fourteen migration-era releases. `9347bbfcd47a`
is included per ADR-052 D5 even though 0.5.0a1 predates the `versions/`
directory.

## Synthesis

Fixtures were synthesized from the in-tree `src/degenbot/migrations/` scripts
and the Rust head DDL (`SCHEMA_HEAD`, applied by `create_new_database`); the
generator was a throwaway and is not committed. Every fixture is stamped by
writing its revision into `alembic_version.version_num`, and every fixture
carries the same tiny seed (below), so the matrix isolates the *revision stamp*
and the *revision schema shape* rather than seed volume.

Seed (identical in all six; tables whose head shape is unchanged from the
initial revision, so the row transport is under test rather than a
column-mapping edge):

| table | rows |
|---|---|
| `erc20_tokens` | 2 |
| `initialization_maps` | 1 |
| `liquidity_positions` | 2 |

Per-fixture provenance:

- **`9347bbfcd47a`** — `alembic upgrade 9347bbfcd47a` from an empty file (this
  revision is inside the only fully forward-buildable prefix of the chain),
  then the head tables introduced by later migrations were forward-created
  **empty** (“forward-stub”), then the seed, then the (already written)
  revision stamp.
- **`756fba1f75f4`** — same recipe as `9347bbfcd47a` (`alembic upgrade
  756fba1f75f4` builds forward); 11 later head tables forward-stubbed empty.
- **`9c411aeeb15e`** — head-schema template (`create_new_database`) with the
  three changes that landed **after** 0.5.1b1 reversed:
  `DROP INDEX ix_erc20_tokens_chain` (`2606a6c7f5ee`), `DROP COLUMN
  aave_v3_assets.price_source` (`e0aaad8ad486`), `DROP COLUMN
  aave_v3_asset_configs.borrowable_in_isolation` (`b0b9e84d5527`); then seed
  and restamp.
- **`b0b9e84d5527`** — head-schema template with the two post-0.6.0a1 changes
  reversed: `DROP INDEX ix_erc20_tokens_chain`, `DROP COLUMN
  aave_v3_assets.price_source`; then seed and restamp.
- **`e0aaad8ad486`** — head-schema template with the one post-0.6.0a2 change
  reversed (`DROP INDEX ix_erc20_tokens_chain`); then seed and restamp. This
  is the same shape the existing `heal`/`migrate` unit fixtures use for the
  published 0.6.0a2 head.
- **`2606a6c7f5ee`** — head-schema template verbatim (it *is* the head); seed
  only. Serves as the matrix's positive control: the head-stamped case.

All fixtures are `VACUUM`ed with WAL checkpointed/truncated, so each is a
single small file (~324–336 KiB; ~1.9 MiB total).

## What these fixtures exercise

The late fixtures exercise the heal's **added-column** mapping (a head column
absent from the old DB is omitted from the copy) and, for `9c411…`/`b0b9…`, the
head-rebuild's index recreation. The two early fixtures exercise a much larger
delta: the heal's **dropped-column** path (the old subclass tables carry
`token0`/`token1`/`factory`/`deployer`/`has_liquidity` columns the head schema
no longer has — visible as `op_warn!` heal warnings) plus the added-column path
for `pools.token0_id` and friends.

## Out-of-scope blockers (why the fixtures are not verbatim per-revision DDL)

Fully verbatim per-revision schemas are **not reconstructable today**, for two
independent reasons:

1. **The Alembic chain is not forward-buildable** (ADR-011, re-verified here).
   Only `9347bbfcd47a` and `756fba1f75f4` upgrade from empty; every later
   revision fails at `87fd9fc7ae00` (`DROP INDEX ix_managed_pool_hash` — no
   migration creates it), `311beed36e7b` (`ALTER COLUMN … SET NOT NULL`,
   unsupported by SQLite), and `e453c9cd9e51` (imports the *current*
   SQLAlchemy models and selects `pools.token0_id` before the chain adds it).
   Reconstructing the later revisions therefore uses the head DDL with the
   post-revision deltas reversed, as documented above.
2. **The heal cannot consume a genuinely older schema that lacks head tables**
   in the current implementation: `heal_database`'s `verify_row_counts` runs
   `SELECT COUNT(*)` for every head table against the source DB and errors on
   an absent table (`no such table: aave_gho_tokens`). The two early fixtures
   work around this by forward-stubbing the later tables **empty**. Relatedly,
   added `NOT NULL` columns (`pools.token0_id`,
   `aave_v3_asset_configs.borrowable_in_isolation`) cannot be invented by the
   column-mapping copy, so no fixture seeds rows in those tables. A `src/` fix
   to `verify_row_counts` (treat an absent source table as 0 rows) would let
   the forward-stub be removed; that change is outside this task's
   fixtures-and-tests-only scope.
