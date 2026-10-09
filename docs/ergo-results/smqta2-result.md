# SMQTA2 — C5.1: every config_dispatch arm takes only the ChunkContext

Commit 1 of PR C5 (epic ZKHO3T). The exploded dispatch parameter list is gone:
every arm in `degenbot-aave/src/updater/config_dispatch/` now takes the
`ChunkContext` alone (plus its decoded event and, where it still needs it, the
pre-existing `gho_asset` snapshot param).

## What changed

- `config_dispatch/context.rs`
  - `ChunkContext<'span>` now borrows the chunk `Connection` (`conn`) and the
    chunk `ChunkSubstrate` (`substrate`, by `&mut`) alongside the existing span,
    provider, ids, addresses, and `RevisionMemo`, and carries the current
    log-transaction's `block_number`.
  - `for_chunk` takes `conn` + `&mut substrate`; the block starts at
    `span.chunk_start` and each transaction enters its own block through the new
    `begin_transaction(block_number)`.
  - New accessors: `begin_transaction`, `block_number`, `conn` (returns the
    context-lifetime reference, so a caller may hold it while borrowing the
    substrate), `substrate_mut`.
  - `apply_events(events)` and `dispatch_transaction(tx_logs)` drop their
    `conn`/`substrate`/`block_number` parameters and read them off the context;
    `resolve_proxy_created` drops `block_number`.
  - The type becomes `pub` (it is now the sole parameter of the `pub` arms and
    of the `pub use` re-exports).
  - A `#[cfg(test)] whole_range_span()` gives the unit tests a shared,
    whole-range span to build contexts against.
- `config_dispatch/mod.rs`
  - `dispatch_config_events(ctx, tx_logs, gho_asset)`,
    `dispatch_single_config_event(decoded, ctx, gho_asset)`, and
    `resolve_missing_variant_event(decoded, ctx, gho_asset)` — connection,
    substrate, and block all ride the context.
  - Every arm call site shortened accordingly; `pub use context::ChunkContext;`
    added beside the existing (crate-internal) span/memo re-exports.
  - The unit-test mod builds a context via a small `ctx_for` helper (dead-URL
    provider + whole-range span) and sets the fixture block; the two arms that
    previously needed no DB now seed the in-memory fixture for the helper.
- `config_dispatch/asset_config.rs` — the 8 sync handlers +
  `resolve_collateral_configuration` + `resolve_reserve_initialized` +
  `read_reserve_init_revisions_and_source` take `ctx` (provider, pool, ids,
  block, connection all off the context; the `ChunkSubstrate` import is gone).
- `config_dispatch/revision.rs` — `resolve_upgraded(ctx, ev, gho_asset)` and
  `resolve_contract_revision_updated(ctx, new_address, name, revision_fn)`.
- `config_dispatch/discount.rs` — `dispatch_discount_percent_updated(ctx, ev)`,
  `dispatch_stk_aave_transfer(ctx, ev, gho_asset)`, and the
  `_with_fresh_resolution` / `_with_backfill` wrappers take `ctx` + the
  `gho_asset` snapshot (the wrappers' fresh re-resolution and the snapshot
  freshness semantics are untouched — that consolidation is C5.2).
- `updater/run/process.rs` (the dispatch-loop caller) — passes `conn` +
  `&mut substrate` into `for_chunk`, calls `begin_transaction` per tx group,
  and reaches the substrate through `ctx.substrate_mut()` for the ops parser
  and the end-of-chunk flush.

Not touched: the six missing-variant events' resolution logic, the discount
arms' freshness rule / revision reads (C5.2), the batched revision helper's
memo keying (C5.3), the re-export tiers beyond the one `ChunkContext`
re-export (C5.4).

## Gates (foreground, from `rust/`, toolchain 1.98.1)

- `cargo fmt -p degenbot-aave -- --check` → clean.
- `cargo test -p degenbot-aave` → ok. 246 lib unit tests + 2
  `aave_cassette_replay` + 2 `aave_config_cassette_replay` + 5
  `aave_market_reset` + 6 `session_positions` + 1 `wave3_capture_replay` + 1
  `wave4_upgrade_span_replay`; 0 failed, 0 ignored. The replay goldens
  (cassette, statement ledger, DB dump) matched the committed files
  byte-identically — no golden was re-recorded and no fixture is dirty.
- `cargo clippy -p degenbot-aave --all-targets` → clean (no warnings).

## PENDING

None.

## Tree

`git status --porcelain` shows only the six files above (five under
`config_dispatch/` plus the dispatch-loop caller `run/process.rs`). No foreign
edits observed; no golden or `.ergo/` file touched.
