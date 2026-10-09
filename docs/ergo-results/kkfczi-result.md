# KKFCZI — C5.4: the re-export tiers collapse to one name per event family

STATUS: **DONE.** All three listed gates are green on the current tree.

Commit 4 of PR C5 (epic ZKHO3T). `config_dispatch/mod.rs`'s three re-export
tiers (`pub use` / `pub(crate) use` / private `use`) collapse to exactly what
the crate consumes: one exported tier per event family, with the dispatch arms
reached only through the module's own dispatch loop (and the in-crate unit
tests) and every defining item dropped to `pub(super)` at the child-module
seam.

## What changed (chunk substance)

- `rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/mod.rs`
  - The re-export block is now four `pub(crate) use` lines (the names `run/`
    consumes) plus three private `use` lines (the dispatch arms):

    ```rust
    pub(crate) use asset_config::{decode_dynamic_string, fetch_erc20_metadata};
    pub(crate) use context::{ChunkContext, ChunkSpan, RevisionMemo};
    pub(crate) use discount::refresh_gho_discount;
    pub(crate) use revision::{match_proxy_id, ProxyCreationResolution};

    use asset_config::{..10 arms..};
    use discount::{..5 arms..};
    use revision::{resolve_contract_revision_updated, resolve_upgraded};
    ```

    The nine `pub use`d asset-config arms, the six `pub use`d discount arms,
    and the public `ChunkContext` re-export are gone; `resolve_reserve_initialized`
    moves from `pub(crate)` to private import.
  - The public module docs no longer link private items: `dispatch_config_events`,
    `resolve_missing_variant_event`, and the private child modules
    `context` / `revision` / `discount` / `asset_config` (plus
    `ChunkContext` / `RevisionMemo`) are now code spans rather than intra-doc
    links — the remedy for the `private_intra_doc_links` diagnostics the public
    `pub mod` docs produced.

- `rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/asset_config.rs`
  - The 8 sync handlers + `resolve_collateral_configuration` + `resolve_reserve_initialized`
    drop `pub` / `pub(crate)` to `pub(super)`.
  - `fetch_erc20_metadata_batched` (no consumer outside its own module) drops
    `pub(crate)` to private.
  - The module `#![expect(clippy::missing_errors_doc, …)]` narrows to
    `#![expect(clippy::doc_markdown)]` (the error-doc lint no longer applies to
    non-exported items), and the two arms that cannot fail
    (`dispatch_price_oracle_updated`, `dispatch_e_mode_category_added`) gain a
    documented `#[expect(clippy::unnecessary_wraps)]` — the uniform `Result`
    arm shape the dispatch match relies on is preserved.

- `rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/discount.rs`
  - The three discount-config dispatchers, `dispatch_stk_aave_transfer`, and
    `build_discount_snapshot` drop to `pub(super)`; `refresh_gho_discount` drops
    to `pub(crate)` (the only cross-module consumer is `run/process.rs`);
    `backfill_user_stk_aave_balance_if_none` drops to private.
  - `#![expect(clippy::missing_errors_doc, …)]` narrows to `#![expect(clippy::doc_markdown)]`;
    the now-unused `#[expect(clippy::missing_errors_doc)]` on `refresh_gho_discount`
    is removed.

- `rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/context.rs`
  - `ChunkContext` drops `pub` to `pub(crate)` (it is `run/process.rs`'s type, never
    an external one).

- `rust/crates/foundation/degenbot-db/src/write/aave.rs`
  - The one doc-comment reference into the module
    (`[`degenbot_aave::config_dispatch::dispatch_e_mode_category_added`]`) is
    de-linked to a code span: the target is now a crate-internal arm, so the
    intra-doc link no longer resolves. Explicitly requested by the task.

Net: the module's exported face is one tier, and the pub arms — which have zero
consumers outside the crate (re-verified: the only non-`degenbot-aave` hit for
`config_dispatch` is the doc comment above) — are no longer published.

## Declared foreign edits (doc-comment repairs only, no code change)

`cargo doc -p degenbot-aave --no-deps` was **already RED at baseline**, before
any edit in this chunk: 36 rustdoc diagnostics (36 = 12 in `config_dispatch` +
24 elsewhere), all from the workspace `-D warnings` escalating
`rustdoc::broken_intra_doc_links` / `private_intra_doc_links`. The task's
acceptance requires this gate green, so the pre-existing rot was repaired too.
Each edit is a doc comment only; no signature, body, or fixture moved. Files
outside my chunk (named per the standing rules):

- `src/analysis.rs` — private `u256_to_f64` link de-bracketed.
- `src/updater/operations.rs` — `amounts_match` (a parser method) named as a
  code span; `build_liquidation_patterns` (private fn) link de-bracketed.
- `src/updater/run.rs` — private `run_aave_update_driver` link de-bracketed;
  `topics[1]/[2]` brackets escaped; feature-gated `degenbot_db::sql_ledger::LedgerDb`
  link de-bracketed.
- `src/updater/run/activate.rs` — two `run_aave_update` links (wrong path from
  this module) de-bracketed.
- `src/updater/run/apply.rs` — the event→fn table's left column (decoded-event
  labels, not items) de-bracketed; `ScaledTokenProcessor::*`,
  `calculate_gho_discount_rate`, and the renamed `process_chunk_on_conn` links
  de-bracketed.
- `src/updater/run/substrate.rs` — private `ChunkSubstrate::reserve_data_updates`
  link de-bracketed.

None of these files are otherwise touched; the changes are 1-2 lines each.

## Gates (foreground, from `rust/`, pinned 1.98.1)

- `cargo test -p degenbot-aave` → **PASS**. Unittests `src/lib.rs` 246 passed /
  0 failed; `aave_cassette_replay` 2 passed; `aave_config_cassette_replay` 2
  passed; `aave_market_reset` 5 passed; `session_positions` 6 passed;
  `wave3_capture_replay` 1 passed; `wave4_upgrade_span_replay` 1 passed (its
  committed SQL/DB goldens asserted byte-identical); doc-tests 0.
- `cargo clippy -p degenbot-aave --all-targets` → **CLEAN** (exit 0). Two
  findings surfaced *by* the visibility drop were fixed in-chunk
  (`unnecessary_wraps`; three now-unfulfilled `#[expect]`s).
- `cargo doc -p degenbot-aave --no-deps` → **CLEAN** (exit 0; `Generated
  …/degenbot_aave/index.html`). Baseline was 36 diagnostics; now 0.
- `cargo fmt -p degenbot-aave` applied; `cargo fmt -p degenbot-aave -- --check`
  → clean.
- `bash scripts/hooks/comment-hygiene.sh` → clean (exit 0).

## PENDING

None.

## Tree

Clean at start. End state shows only the 11 files listed above — no golden, no
fixture, no `.ergo/` change. No other worker's in-progress edits were observed.
