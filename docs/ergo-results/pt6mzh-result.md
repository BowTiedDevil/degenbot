# PT6MZH — C6.2: the market reset module (one transaction, typed verified facts)

STATUS: **DONE.** Both scoped gates are green on the current tree.

Commit 2 of PR C6 (epic ZKHO3T). The new module `reset_market_to_cold_boot`
runs the purge and the activation seam inside ONE caller-supplied transaction
and returns facts it read back from that transaction — never constants. The CLI
rewiring is C6.3 and is NOT touched here.

## What changed

- `rust/crates/integrations/degenbot-aave/src/updater/run/reset.rs` (new)
  - `reset_market_to_cold_boot(conn, market_id, pool_address_provider,
    gho_token_address, provider)` — reads the market's `chain_id` from its row
    (a missing row becomes `RunError::MarketNotFound`), parses the two
    addresses, runs the shared activation RPC, then calls
    `DegenbotDb::reset_aave_market_on_conn` followed by
    `activate_aave_market_on_conn` on the caller's `conn` (its open
    `rusqlite::Transaction`). It refuses (`RunError::Db`) when the on-chain
    market name resolves to a row other than `market_id`, so a name drift can
    never strand the purged market with a sibling row.
  - `MarketResetFacts { market_activated, contract_row_present,
    last_update_block }` — each field comes from a post-op read of the same
    transaction: the market row's `active` flag, the `POOL_ADDRESS_PROVIDER`
    contract row's presence, and the cursor. No literal is returned.
- `rust/crates/integrations/degenbot-aave/src/updater/run/activate.rs`
  - Extracted the RPC half into `fetch_activation_metadata` (+ the
    `ActivationMetadata` carrier) so the activation entry and the reset module
    share one fetch; `activate_aave_market_rpc` now calls it. Behavior is
    unchanged.
- `rust/crates/integrations/degenbot-aave/src/updater/run.rs` and
  `updater/mod.rs` — declare/re-export `reset_market_to_cold_boot` and
  `MarketResetFacts`.
- `rust/crates/integrations/degenbot-aave/tests/aave_market_reset_module.rs`
  (new) — four tests through the real module interface (seeded DB + cassette
  transport):
  - the warm populated market is reset; every returned fact is checked against
    an independent read of the committed transaction, and the purged rows are
    gone while the cold-boot substrate stays;
  - a bare inactive pre-registered market is completed and reported;
  - an on-chain name that names another row is refused and the transaction
    rolls back (the purge reverts with it);
  - an unknown market is a typed failure before any RPC.

## Gates (foreground, scoped)

- `cargo fmt -p degenbot-aave` — clean.
- `cargo clippy -p degenbot-aave --all-targets` — clean (pinned 1.98.1).
- `cargo test -p degenbot-aave` — green: 246 lib + 2 + 2 + 5 + 4 + 6 + 1 + 1
  tests, 0 failed. The cassette/golden suites (aave_cassette_replay,
  aave_config_cassette_replay, wave3, wave4) pass, so the goldens are
  byte-identical.

Nothing PENDING. No foreign tree edits observed.
