# BHMQXR — C5.2: one gho(conn) freshness rule; the fresh-resolution arms deleted

STATUS: **STOPPED — a wave4 golden moved (statement-ledger reorder). Not re-recorded.**

The mandated design is implemented and clippy/fmt clean, but the hard
byte-identical golden acceptance does NOT hold: the wave4 span's
statement-ledger golden drifts by one read position. Per the task
("if a golden legitimately moves, STOP and report — never re-record") the tree
is left for review and the task is NOT marked done.

## What changed (the settled shape, implemented)

One ctx-owned always-fresh accessor, one freshness rule:

- `config_dispatch/context.rs` — new `ChunkContext::gho_asset(&mut self)` that
  re-resolves the GHO row through the substrate's read-your-own-writes overlay
  (`self.substrate.gho_asset(conn)`); the dirty-mark contract makes a repeat a
  cache hit until an apply that writes `aave_gho_tokens` invalidates it.
  `dispatch_transaction` now calls `self.gho_asset()` for its stage-0 row and
  drops the `gho_asset` argument from the dispatch call.

- `config_dispatch/mod.rs` — the `gho_asset` snapshot parameter is gone from
  `dispatch_config_events`, `dispatch_single_config_event`, and
  `resolve_missing_variant_event`; every arm resolves the asset itself through
  the accessor. The `dispatch_stk_aave_transfer_with_backfill` and the two
  `_with_fresh_resolution` re-exports are deleted.

- `config_dispatch/discount.rs` — the three wrapper arms collapse into their
  cores: `dispatch_stk_aave_transfer(ctx, ev)` is now the single async core
  (fresh resolve + emitter scope + NULL-balance backfill), and
  `dispatch_discount_token_updated(ctx, ev)` /
  `dispatch_discount_rate_strategy_updated(ctx, ev)` read the asset through the
  accessor before their emitter-validation guard. ~90 lines of guard-and-delegate
  deleted; the cores keep their emitter-validation logic.

- `config_dispatch/revision.rs` — `resolve_upgraded(ctx, ev)` drops the snapshot
  param and reads `ctx.gho_asset()` for the GHO-deprecation gate (only on the
  vToken path, the only path that consults it).

- `config_dispatch/asset_config.rs` — `resolve_reserve_initialized(ctx, ev,
  oracle)` drops the snapshot param and reads `ctx.gho_asset()` for the
  GHO-vToken-FK link.

- `updater/transaction_processor.rs` — a stale comment naming the deleted
  `*_with_fresh_resolution` wrapper is reworded.

- `config_dispatch/mod.rs` unit tests — the pure `sample_gho_asset` helper is
  replaced by a `seed_gho_row` DB seeder; the guard tests and the intra-tx
  v-token-link test now build a `ChunkContext` and exercise the accessor.

## Gates (foreground, from `rust/`, pinned 1.98.1)

- `cargo fmt -p degenbot-aave -- --check` → clean.
- `cargo test -p degenbot-aave` → **FAILED**:
  - `degenbot-aave` lib unit tests: 246 passed, 0 failed.
  - `tests/aave_cassette_replay.rs`: 2 passed (byte-identical).
  - `tests/aave_config_cassette_replay.rs`: 2 passed.
  - `tests/aave_market_reset.rs`: 5 passed.
  - `tests/session_positions.rs`: 6 passed.
  - `tests/wave3_capture_replay.rs`: 1 passed.
  - `tests/wave4_upgrade_span_replay.rs`: 0 passed, **1 FAILED** —
    `wave4_upgrade_span_replays_to_the_committed_goldens`, window
    `w4_aave_gho_deprecation_at_chunk_boundary`: **statement-ledger drift**.
- `cargo clippy -p degenbot-aave --all-targets` → clean (no warnings).

## The moved golden (exact evidence)

The replay failure is a byte-diff of the statement-ledger JSON at
`assert_eq!(ledger_json, committed_ledger)` (test line 379); the DB-dump
assertion is never reached. Comparing the two ledgers statement-by-statement:

- statement count is identical (160 vs 160);
- exactly ONE position differs, at index 119:
  - actual (left): `SELECT g.id, g.token_id, ... FROM aave_gho_tokens g JOIN ...`
  - golden (right): `UPDATE aave_v3_assets SET v_token_revision = ? WHERE id = ?`
- the golden's GHO read appears one statement later (index 120).

All UPDATE/INSERT/DELETE statements are identical in order and count, so the
observed DB state is unchanged and the DB-dump golden is unaffected.

**Root cause.** The command stream around the drift is:
`... UPDATE aave_gho_tokens SET v_gho_discount_token = NULL ...` (the
`Upgraded`-deprecation apply, which calls `substrate.mark_gho_dirty()`),
`UPDATE aave_v3_users SET gho_discount ...` (the bulk reset), then a further
vToken `Upgraded` in the same log-transaction.

- Before: that further `Upgraded`'s resolver used the per-tx snapshot (no
  read); the next GHO read was the next transaction's stage-0 snapshot, which
  consumed the dirty mark → the golden's read at index 120.
- After: the further `Upgraded`'s resolver reads `ctx.gho_asset()`, which
  consumes the dirty mark one read earlier → the read at index 119.

This is inherent to the mandated change ("`resolve_upgraded` switches to it"):
reading the always-fresh accessor when the overlay is dirty re-locates the
dirty-consuming read from the next transaction's stage-0 snapshot into the
current transaction's dispatch. The read is relocated, never added or removed
(total GHO reads are unchanged), and no row value changes.

A first run also drifted `w4_aave_upgrade_plus_same_block_config` at an aToken
`Upgraded`; making the read lazy (vToken path only, where the value is actually
consulted) removed that drift and left only the vToken case above. That confirms
`resolve_upgraded`'s accessor read is the sole cause.

## `rg "with_fresh_resolution|with_backfill" --type rust rust/crates`

`rust/crates/integrations/degenbot-aave/...` → zero hits (wrappers deleted; the
stale comment reworded). One residual hit remains, an unrelated foreign test
name outside this chunk's scope:

- `rust/crates/engine/degenbot-bot/src/bot_core/stage_handlers.rs:1315`
  `fn full_lifecycle_with_backfill_episode_and_reorg_passes()`
  (a stream/reorg harness test; its "gap-backfill episode" is unrelated to the
  GHO assets). Left untouched — not this chunk's file, and renaming it only to
  satisfy the substring search would be a scope violation. Flagged so the
  literal `rg` gate shows one hit that is not a leftover of this refactor.

## Foreign tree activity

None observed. `git status --porcelain` shows only the six files this chunk
edited (above); no golden or `.ergo/` file was touched.

## Recommendation (for the manager)

The drift is a pure read-relocation in the SQL trace with identical data. Either

1. re-record `tests/fixtures/sql_goldens/wave4/w4_aave_gho_deprecation_at_chunk_boundary.statement-ledger.json`
   (the manager owns re-recording; the DB-dump golden is unchanged), or
2. if the trace shape must be preserved, keep `resolve_upgraded` on the per-tx
   snapshot (i.e. drop the "resolve_upgraded switches" clause) — but that
   reintroduces the two-semantics split C5.2 exists to remove.

I did not re-record the golden.
