# WQICQU — P4: one solve-cycle scope owns the block boundary (C9)

Card C9 of epic FQVFXI. The block boundary is no longer choreographed in prose at
the call site: the memo owns drain-and-advance in one signature, and a single
scope value owns the epoch the per-path gate deps are built from.

## What changed

- `rust/crates/engine/degenbot-solvers/src/cl/memo.rs`
  - `WalkMemo::begin_block(epoch)` is now drain-and-advance in ONE call and
    returns `WalkMemoStats` for the epoch that just ended. The stats are
    snapshotted (and the per-epoch counters reset) before any census swap, so the
    ordering is enforced by the signature, not by a comment.
  - Added a private `WalkMemoState::drain_epoch` shared by `begin_block` and the
    teardown `take_stats` (kept, unchanged surface, for the final drain).
  - Reorg warn-and-hold and the census eviction are byte-identical: equal-epoch
    and regression paths still snapshot the current epoch first, then hold.
- `rust/crates/engine/degenbot-bot/src/arb_engine/walk_telemetry.rs`
  - Now render-only: `emit_epoch_boundary(stats, incoming_block)` +
    `emit_epoch_boundary_into` replace `drain_epoch_boundary[_into]`, operating
    on the stats the memo returned. `final_drain` still takes the memo and calls
    `take_stats`. The retired ordering-contract prose is gone from the docs.
  - The 4 inline tests migrate to the new shape with **unchanged assertions**
    (JSONL fields, cadence, and the double-drain-silence latch in
    `final_drain_emits_the_last_epoch` are pinned exactly as before).
- `rust/crates/engine/degenbot-bot/src/arb_engine/solve_cycle.rs`
  - New `BlockScope` (Clone) holding `Arc<WalkMemo>` + `Arc<PrefixCache>` and the
    boundary `epoch`. `advance(block)` sets the epoch and calls the memo's
    drain-and-advance, returning its stats.
  - `SolveCycle` and `SolveCycleShared` now carry the scope instead of separate
    `walk_memo`/`prefix_cache`/`epoch` fields, so the epoch, memo, and cache come
    from one value.
  - `run_epoch`'s boundary is now `let stats = self.scope.advance(solve_block);
    walk_telemetry::emit_epoch_boundary(&stats, solve_block);`.
  - The cold-start path clones the memo/cache handles from the scope (behavior
    unchanged).
- `rust/crates/engine/degenbot-bot/src/arb_engine/lane_walk.rs`
  - `GateDeps::per_block(ctx.scope.epoch(), ctx.scope.prefix_cache(), ...,
    Some(&**ctx.scope.walk_memo()), ...)` — a block's deps read their epoch from
    the same scope the memo advanced, so they can never disagree.
- `rust/crates/engine/degenbot-bot/src/arb_engine/engine_stages.rs` and `mod.rs`
  - Teardown drain and engine construction route through `cycle.scope`
    (`BlockScope::new`).

`logs/walkmemo_stats.jsonl` emission is byte-equivalent per epoch: same fields,
same cadence (one line per boundary plus the teardown drain), same activity gate.

## Gates (foreground, from `rust/`)

- `cargo test -p degenbot-solvers` — green (177 + 7 + 20 + ... all ok; 3 ignored).
- `cargo test -p degenbot-bot` — green (715 lib + 9 + 1 + 2; 3 ignored).
- `cargo clippy -p degenbot-bot -p degenbot-solvers --all-targets` — clean,
  no warnings (toolchain 1.98.1).
- Extra: `bash scripts/hooks/comment-hygiene.sh` — exit 0.
- `cargo fmt -p degenbot-bot -p degenbot-solvers` run on the touched crates.

No FFI surface change; `just gen-stubs` not needed.

## Unmodified behavior tests

`rust/crates/engine/degenbot-solvers/src/cl/tests/memo.rs` (reorg warn-and-hold,
census eviction, negative gauge) is **untouched** (empty diff) and green.
