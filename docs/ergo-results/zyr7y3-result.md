# ZYR7Y3 — C6.1: activate_aave_market takes an injected provider (ADR-068 D5 completed)

STATUS: **DONE.** All four listed gates are green on the current tree.

Commit 1 of PR C6 (epic ZKHO3T). The core `activate_aave_market` entry no longer
builds a transport from a `rpc_url`: it takes an already-built `AlloyProvider`
and the three shells (CLI, Python FFI, cassette recorder) each construct their
own. `activate_aave_market_on_conn` is untouched. This completes ADR-068 D5 for
the activation path.

## Settled shape — `&AlloyProvider`, per clippy

The task text wrote `&AlloyProvider` and said "the exact borrow/ownership shape
follows the loop entries' convention". The first cut took the provider by value
(mirroring `run_aave_update`'s `provider: AlloyProvider`), and the pinned 1.98.1
clippy rejected it with `needless_pass_by_value` — `activate_aave_market` only
borrows the provider (no `Arc` wrap, no move into a fetch). The loop entries take
it by value only because they wrap it in an `Arc` for their `LogFetcher`; the
one-shot seed does not. So the borrow shape is `&AlloyProvider`, matching the
literal task text and the lint.

## What changed (chunk substance)

- `rust/crates/integrations/degenbot-aave/src/updater/run/activate.rs`
  - `activate_aave_market(db, chain, pool_address_provider, gho_token_address,
    provider: &AlloyProvider)` — the `rpc_url: &str` parameter is gone. The
    body opens the writeable DB, parses the two addresses, then the ONE
    `get_runtime().block_on(activate_aave_market_rpc(...))` with the injected
    provider.
  - `activate_aave_market_rpc` takes `provider: &AlloyProvider`; the
    `Arc::new(AlloyProvider::new(rpc_url, RPC_MAX_RETRIES).await?)` construction
    is deleted, and the two fetch call sites use the injected `provider` directly.
  - `use std::sync::Arc;` and the `RPC_MAX_RETRIES` import are removed (both were
    only there for the deleted construction site).
  - The `# Args` doc drops `rpc_url` and documents the injected `provider`
    (ADR-068 D5).
- `rust/crates/integrations/degenbot-aave/src/updater/run.rs`
  - The now-unused `const RPC_MAX_RETRIES: u32 = 5;` and its doc comment are
    deleted (its only consumer was the removed activation build site).
- `rust/crates/shells/degenbot-cli-core/src/aave.rs` (the `aave activate` arm)
  - Builds the live provider once via
    `crate::pool::shared_runtime_block_on(async { AlloyProvider::new(&rpc_url,
    RPC_MAX_RETRIES).await })` — the exact shape the `aave update` arm uses — and
    hands `&provider` to `activate_aave_market`. Build failures fold into
    `RunError::from` / the arm's `CliError::AaveUpdate`, as before. The `aave
    reset` arm already calls `activate_aave_market_on_conn` (no RPC) and is
    unchanged.
- `rust/crates/shells/degenbot-python/src/aave_updater.rs` (the FFI shell)
  - The `#[pyfunction]` signature is unchanged (`database_path, chain_id,
    pool_address_provider, gho_token_address, rpc_url`). Inside the `py.detach`
    closure the shell now builds the live transport on the shared runtime
    (`get_runtime().block_on(AlloyProvider::new(rpc_url, 5))`) and passes
    `&provider` into the core. The doc comment is updated to say the shell, not
    the core, owns the build.
- `rust/crates/facade/degenbot/examples/record_updater_cassette.rs` (`--kind
  aave-run`)
  - The worker thread builds the activation provider from `node_uri` (a live
    transport; activate's RPC is a harness input, not part of the recorded run
    surface — cassette bytes do not move) and passes `&activate_provider` to
    `activate_aave_market`. The `run_aave_update` call still uses
    `recorder.as_alloy_provider()`.

Net: the core builds no transport on the activation path; every construction
site now lives in a shell.

## Gates (foreground, from `rust/`, pinned 1.98.1)

- `cargo test -p degenbot-aave` → **PASS**. Lib unittests 246 passed / 0 failed;
  `aave_cassette_replay` 2 passed; `aave_config_cassette_replay` 2 passed (both
  assert the committed SQL/DB dumps byte-identical);
  `aave_market_reset` 5 passed; `session_positions` 6 passed;
  `wave3_capture_replay` 1 passed; `wave4_upgrade_span_replay` 1 passed; doc-tests 0.
- `cargo test -p degenbot-cli-core` → **PASS** (`aave_arms` 13 passed; the rest of
  the crate's suites green; doc-tests 0).
- `cargo check -p degenbot_rs` → **PASS** (the FFI crate compiles).
- `cargo clippy -p degenbot-aave -p degenbot-cli-core --all-targets` → **CLEAN**
  (exit 0). The first run surfaced `needless_pass_by_value` on the by-value
  signature; fixed by the `&AlloyProvider` shape above.
- Also run: `cargo check -p degenbot --example record_updater_cassette` →
  PASS (the recorder example is outside the listed gates but is a touched
  consumer, so it is checked). `cargo fmt -p degenbot-aave -p degenbot-cli-core
  -p degenbot_rs -p degenbot` applied. `bash scripts/hooks/comment-hygiene.sh` →
  clean (exit 0).

## PENDING

None.

## Tree

Clean at start. End state shows only the 5 files listed above — no golden, no
fixture, no `.ergo/` change. No foreign edits and no other worker's in-progress
work was observed.
