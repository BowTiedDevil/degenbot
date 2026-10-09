# 2KSB6Z — C5.3: the batched revision read moves inside the ChunkContext

STATUS: **DONE for the chunk. One listed gate (clippy --all-targets) is RED,
but caused entirely by a pre-existing foreign file from commit c80a46e63
(BHMQXR), not by this change.** The hard golden acceptance and the `rg`
acceptance both pass.

## What changed (files + intent)

- `rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/context.rs`
  - New `ChunkContext::read_revision_batch(&mut self, pending: &[BatchRead<'_>])`
    — the batched sibling of `read_revision`. It owns the `RevisionMemo` keying
    (derives each revision selector, checks the memo, records each answered
    value under its `(implementation, selector, block)` key) and owns the chunk
    span `debug_assert!` for the read. Revision hits issue no call; the
    remaining revision misses plus every plain slot fold into ONE
    `eth_calls_batched_or_direct` pass in slot order. Results come back aligned
    with `pending`, so callers never touch the memo or the block lane.
  - New `BatchRead<'a>` slot enum (`Revision { target, sig }` /
    `Plain { target, calldata }`) and `BatchReadResult` outcome enum
    (`Revision(U256)` / `Plain(Bytes)`).
  - `read_revision`'s doc now names the two context seams; the now-unused
    `ChunkContext::memo()` accessor is deleted.

- `rust/crates/integrations/degenbot-aave/src/updater/config_dispatch/asset_config.rs`
  - `resolve_reserve_initialized` builds the three-slot `pending` array
    (aToken revision, vToken revision, always-pending `getSourceOfAsset`) and
    calls `ctx.read_revision_batch(&pending)`; the `#[expect]`-free decode of
    the source slot is done locally.
  - The hand-driven helper `read_reserve_init_revisions_and_source` and its
    `PendingRead` enum are deleted. The `eth_calls_batched_or_direct` /
    `revision_selector` imports are dropped; module doc reworded.

Net: `read_revision` / `read_revision_batch` are the only way revisions are
read; "the one seam every revision read goes through" is now true.

## Gates (foreground, from `rust/`, pinned 1.98.1)

- `cargo test -p degenbot-aave` → **PASS (exit 0)**:
  - lib unit tests: 246 passed, 0 failed.
  - `tests/aave_cassette_replay.rs`: 2 passed (byte-identical goldens).
  - `tests/aave_config_cassette_replay.rs`: 2 passed.
  - `tests/aave_market_reset.rs`: 5 passed.
  - `tests/session_positions.rs`: 6 passed.
  - `tests/wave3_capture_replay.rs`: 1 passed.
  - `tests/wave4_upgrade_span_replay.rs`: 1 passed (byte-identical goldens).
  - doc-tests: 0.
- `cargo clippy -p degenbot-aave --lib --message-format short` → **CLEAN
  (exit 0)** — this crate's library, i.e. every line this chunk wrote, is clean.
- `cargo clippy -p degenbot-aave --all-targets` → **RED (exit 101)**, two
  findings, both in a file this chunk did not touch:
  - `tests/wave4_upgrade_span_replay.rs:122:44` — `doc_markdown`
    (`REGENERATE_SQL_GOLDENS=1` in a doc comment).
  - `tests/wave4_upgrade_span_replay.rs:133:9` — `print_stderr`
    (`eprintln!` in the regen arm).
  Both lines were introduced by commit `c80a46e63` ("one always-fresh gho
  accessor", BHMQXR) which added the ADR-068 D4 regen arm without widening that
  file's `#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`
  to cover `print_stderr` / `doc_markdown`. `git log` confirms that commit is
  the file's last writer and `git status` confirms the file is unmodified in
  this chunk's tree, so the failure predates and is independent of C5.3. Left
  untouched per scope discipline (see below).

- `cargo fmt -p degenbot-aave` → applied; `cargo fmt -p degenbot-aave --check`
  → clean (exit 0).
- `bash scripts/hooks/comment-hygiene.sh` → clean (exit 0).

## `rg` acceptance (from repo root)

- `rg "PendingRead" --type rust rust/crates/integrations/degenbot-aave` → **0
  hits**.
- `rg "memo()\.get|memo()\.insert" --type rust rust/crates/integrations/degenbot-aave`
  → **0 hits** (inside or outside the context module).
- `rg "read_reserve_init_revisions_and_source" --type rust rust/crates/integrations/degenbot-aave`
  → 0 hits.

## Foreign tree activity / conditions

- Tree was clean at start. `git status --short` at end shows only the two files
  this chunk edited; no untracked files, no `.ergo/` edits, no golden touched.
- Foreign pre-existing condition (not an edit made during this run): the
  `tests/wave4_upgrade_span_replay.rs` clippy findings carried in from
  `c80a46e63`. Named here so the manager can widen that test file's `#![expect]`
  (a one-line follow-up) or otherwise account for it in the pre-commit gate.

## Note on the golden

No golden moved. Both golden-replay gates reproduce the committed SQL/cassette
artifacts byte-for-byte under the new batched-read seam (the batching transport
shape — misses-then-plain in slot order — is preserved exactly, including the
1-call direct degradation when only the source slot remains).
