# A2O7OP — C6.3: the CLI reset arm rewires through the module

STATUS: **DONE.** All three scoped gates are green on the current tree; nothing
PENDING.

Commit 3 of PR C6 (epic ZKHO3T). The `aave reset` arm no longer runs the
five-open / two-transaction purge-then-activate choreography: it takes the
`ResetLock`, keeps the dry-run preview, builds ONE provider, then opens the
database once and runs the purge PLUS the cold-boot re-seed inside ONE
transaction through `degenbot_aave::reset_market_to_cold_boot`. The report now
renders the typed facts that module read back from that transaction instead of
the old `market_activated: true` / `contract_row_present: true` constants.

## What changed

- `rust/crates/shells/degenbot-cli-core/src/aave.rs` (the `reset` arm)
  - Drop `DegenbotDb::reset_aave_market_on_conn` + `activate_aave_market_on_conn`
    in two separate `open_for_writes` transactions; call
    `reset_market_to_cold_boot(&tx, market_id, ap, gho, &provider)` on ONE
    `guard.transaction()` and commit once. The kill-between-transactions window
    is gone: a failure drops the transaction and the DB stays byte-identical.
  - Build ONE `AlloyProvider` (the module's activation RPC — the on-chain market
    name plus the GHO metadata) from `ctx.node_request_uri_for(chain_id)`, with
    the same `shared_runtime_block_on` + `RPC_MAX_RETRIES` shape the activate
    and update arms use. The module's own `get_runtime().block_on` runs outside
    that block_on, so there is no nesting.
  - The dry-run preview and the `ResetLock` are unchanged. Replaced the
    papered-over `market_activated: true` / `contract_row_present: true` literals
    and the post-commit cursor re-read with `facts.market_activated`,
    `facts.contract_row_present`, `facts.last_update_block`.
  - The purge counts are now read on the same transaction ahead of the write
    (`count_aave_market_rows_on_conn`) so `counts` still names the rows the
    purge removes — previously they came off the purge's own return value,
    which is no longer surfaced by the module.
  - Removed the now-unused `activate_aave_market_on_conn` /
    `ETHEREUM_AAVE_V3_BOOTSTRAP_BLOCK` imports; updated the arm + module docs.
- `rust/crates/shells/degenbot-cli-core/src/report.rs` — `AaveReinit` docs now
  describe the facts the reset transaction verified (field docs say "read from
  the row", not "the activate seam ran"). `AaveReport::Reset.reinit` docs
  likewise. The rendered line text is UNCHANGED.
- `rust/crates/shells/degenbot-cli-core/tests/aave_arms.rs` — the reset test
  exercised a pure-SQL arm offline; the arm now dials the activation RPC, so it
  gains a minimal local JSON-RPC stub (`ResetRpcStub`: `eth_blockNumber`,
  `getMarketId()`, GHO `name()`/`symbol()`/`decimals()`) plus `env_with_rpc_url`.
  Assertions are unchanged: facts `(active=true, ap_row=true, cursor=bootstrap)`,
  counts, the purged rows, and the report line.
- `rust/crates/integrations/degenbot-aave/src/updater/run/activate.rs` — the
  `activate_aave_market_on_conn` doc claimed `aave reset` "never builds an RPC
  transport"; corrected to say the reset module reuses the same RPC fetch. Doc
  only; no behavior change.
- `docs/cli/aave.md` — the reset section now states the purge + re-seed commit
  in ONE transaction and that the re-seed reads the on-chain market name / GHO
  metadata through the configured RPC (an operator-visible new requirement).

## Python FFI surface

`rust/crates/shells/degenbot-python/src/aave_updater.rs` has NO reset binding:
its pyfunctions are `run_aave_update`, `verify_touched_positions_on_chain`,
`verify_all_positions_on_chain`, `cleanup_zero_balance_positions`,
`activate_aave_market`, `deactivate_aave_market`. There is no FFI reset path to
rewire, so the FFI surface is unchanged and `just gen-stubs` was not needed.

## Gates (foreground, scoped)

- `cargo test -p degenbot-cli-core` — GREEN: 35 lib (1 ignored) + aave_arms 13 +
  block_offset_resolution 3 + block_offset_resolution_ws 2 + config_arms 19 +
  database_arms 21 + exchange_arms 13 + pool_arms 12 + pool_verify_lifecycle 4 +
  registrations 3 + rpc_frames 17 + doc-tests 0, 0 failed.
- `cargo test -p degenbot-aave` — GREEN: 246 lib + 2 + 2 + 5 (aave_market_reset)
  + 4 (aave_market_reset_module) + 6 + 1 + 1, 0 failed. The dump-equality /
  cold-boot pins are byte-identical (the crate's behavior was not modified).
- `cargo clippy -p degenbot-cli-core --all-targets` — clean (pinned 1.98.1).
- `rg "market_activated: true" rust/crates/shells/degenbot-cli-core` — zero hits.
- `cargo fmt -p degenbot-cli-core -p degenbot-aave` — run before finishing.
- `bash scripts/hooks/comment-hygiene.sh` — exit 0.

Nothing PENDING. No foreign tree edits observed (git status showed only the five
files this chunk touched).
