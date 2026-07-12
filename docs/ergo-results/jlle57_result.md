## Result
- Extended `rust/crates/degenbot/examples/standalone_consumer.rs` to drive the
  full P73ER6 contract end-to-end against a real fixture DB (`parity.db`,
  chain 8453) instead of in-memory cold-start.
- The example now opens the file-backed DB via `DegenbotDb::open(...)`,
  constructs `Bot::new(8453)`, calls `load_snapshot_from_db(&db, 8453)`, and
  asserts `S = Some(12_340_000)` = MIN(V3 aerodrome_v3 12_345_000, V4 12_340_000).
- The full `subscribe` + `resume` lifecycle (auto-backfill of `S+1..W-1`
  inside `resume_from_subscribe` via `BlockPump::backfill_from_snapshot(W)`
  → `BotState::process_backfill_logs`, then live loop with `current_block = W`)
  is documented inline and remains CI-runnable by gating the live-WS portion
  behind `SMOKE_RPC_URL` — `subscribe()` requires a real WS endpoint, so
  `cargo run --example standalone_consumer` (validation gate #2) must succeed
  against the fixture DB without a node. The auto-backfill path itself is
  asserted directly by `block_pump::tests::resume_anchors_to_subscribe_block`
  and the J3FMDO backfill tests in the same crate.

## Validation
- `cargo build --example standalone_consumer` ✅
- `cargo run --example standalone_consumer` ✅ (exits 0 against the fixture DB)
- `cargo clippy --example standalone_consumer -- -D warnings` ✅
- `just test-rust` ✅
