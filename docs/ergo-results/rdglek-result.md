# RDGLEK — P5: the ChunkTx guard owns the end-of-chunk contract; RunSpec (C7)

Card C7 of epic FQVFXI. The chunk-rollback ritual (drop the transaction, measure
the write-lock hold, observe the lock-hold telemetry, report a zeroed
`AaveChunkProgress`, return `Err`) is no longer copy-pasted at four exits in the
chunk loop: one guard owns it. The run's twelve-knob positional interface is one
`RunSpec` value.

## What changed

- `rust/crates/integrations/degenbot-aave/src/updater/run.rs`
  - New `ChunkTx<'a>`: owns the chunk's `rusqlite::Transaction` (in an `Option`,
    `None` after the commit). `commit(&ChunkCoreReport, is_final)` is the success
    path exactly once — `COMMIT`, write-lock-hold measurement, the lock-hold +
    four stage-metric observations, the committed progress report. `Drop`
    without commit is the rollback path exactly once — the transaction drops
    (rollback), the hold is measured, and the zeroed failure report lands. A
    failed `COMMIT` takes the rollback path and returns the database error.
  - New `AaveChunkProgress::from_chunk(chain_id, market_id, chunk_start,
    chunk_end, fetch_time)`: the failure-vs-success defaults in ONE place (zeroed
    counters, empty touched set, `committed: false`, `is_final: false`, zeroed
    stage splits + lock hold). The field set is unchanged (the replay harness's
    collector is a consumer and still asserts the full shape).
  - New `RunSpec` + `VerifyPolicy` (both re-exported from `updater::mod`).
    `run_aave_update(database_path, spec)`, `run_aave_update_on_db(db, spec)` and
    the driver take the spec; the driver destructures it into the same local
    names the body already used, so the loop body is otherwise untouched.
  - The chunk body now reads as: open the guard → `process_chunk_on_conn(...).
    await?` → the two verify gates → `chunk_tx.commit(&r, is_final)?`. Each
    former copy-paste exit is one `?`/`return Err(..)`; the guard's drop reports
    it. The "`chunk_lock_hold_time` left uninitialized so the compiler proves
    that" pattern and the caller-local `chunk_verify_time` accumulator are gone
    (they live in the guard now). `is_final` is computed once and reused by both
    the full-verify completion gate and the committed report (same expression as
    before).
  - The `too_many_arguments` expectations on the three signatures are deleted
    (unfulfilled an `#[expect]` is a hard error under the workspace's
    `warnings = deny`). `await_holding_lock` + `too_many_lines` remain on the
    driver.
  - New unit tests: the `from_chunk` failure defaults, and the guard's
    drop-without-commit path (rollback + one zeroed report + the transaction
    rolled back on the in-memory connection).
- `rust/crates/integrations/degenbot-aave/src/updater/mod.rs` — re-exports
  `RunSpec`, `VerifyPolicy`.
- `rust/crates/shells/degenbot-cli-core/src/aave.rs` — `From<&UpdateArgs<'_>>
  for VerifyPolicy` replaces the at-call-site `interval` fold, and the run is
  built as a `RunSpec` literal. Note the honest deviation: a total
  `From<&UpdateArgs<'_>> for RunSpec` is not expressible — the flag bundle holds
  unresolved CLI-side inputs (`to_block` as a string awaiting RPC resolution,
  plus the non-run `dry_run`/`enable_backup`) while `RunSpec` needs the resolved
  per-market target, the injected provider, the cancel flag and the sink. The
  conversion covers the part the bundle owns outright (the verify policy).
- Call-site consumers updated to build a `RunSpec` (semantics preserved arg-for-
  arg; the all-default policy sites use `VerifyPolicy::default()`):
  `degenbot-aave/tests/{aave_cassette_replay,aave_config_cassette_replay,
  wave3_capture_replay,wave4_upgrade_span_replay,aave_market_reset}.rs`,
  `degenbot-python/src/aave_updater.rs`, `degenbot/examples/{generate_aave_
  upgrade_windows,generate_aave_adversarial_captures,record_updater_cassette}.rs`,
  `degenbot/benches/updater_replay_bench.rs`.

## Gates (foreground, from the repo root)

- `cargo test --manifest-path rust/Cargo.toml -p degenbot-aave` — green: 248 lib
  (+2 new) + 2 cassette + 2 config-cassette + 5 market-reset + 4 reset-module +
  6 session-positions + 1 wave3 + 1 wave4 + 0 doc-tests; 0 failed.
- `cargo test --manifest-path rust/Cargo.toml -p degenbot-cli-core` — green: 35
  lib (1 ignored) + `aave_arms` 13 + 12 + 4 + 3 + 17 + ... ; 0 failed.
- `cargo clippy --manifest-path rust/Cargo.toml -p degenbot-aave -p degenbot-cli-core
  --all-targets` — clean (toolchain 1.98.1).
- Goldens BYTE-IDENTICAL: `git status` shows no change under `tests/fixtures/`
  (the replay suite's regen-and-diff gates and the statement-count gate pass
  unchanged). Nothing was re-recorded.
- Extra (crate-scoped, neighbors of the touched call sites):
  `cargo check -p degenbot --all-targets` and `cargo check/clippy -p degenbot_rs`
  — clean; `bash scripts/hooks/comment-hygiene.sh` — exit 0.
- `cargo fmt --manifest-path rust/Cargo.toml -p degenbot-aave -p degenbot-cli-core
  -p degenbot -p degenbot_rs` run before the final gate pass.

No gate was PENDING; nothing detached.

## Not run (not mine)

The workspace ladders (`just test-rust-nextest`, `just test-python`) and the
whole-tree lint/format sweeps.

## Foreign tree activity

None observed: the tree was clean at start and the only modifications are the 13
files listed above.
