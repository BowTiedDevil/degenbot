## Result

Fixed — the pyo3 resume path now drains the WS stream during the synchronous snapshot backfill.

### Changes
- `rust/crates/degenbot-bot/src/bot_core/block_pump.rs`: new `pub async fn backfill_with_drain(first_block, combined)` — single owner of the "backfill while draining + re-inject drained events ahead of the live tail (MJXP5Z)" discipline; documents why the drain is not optional (alloy capacity-16 broadcast ring drops OLDEST). `resume_from_subscribe` refactored onto it.
- `rust/crates/degenbot-python/src/bot/pump.rs`: `PumpState::resume` now calls `backfill_with_drain` inside the existing `py.detach`+`block_on`, and spawns `run_with_stream` on the re-injected stream. J3FMDO contract preserved: the backfill still completes synchronously before `resume()` returns.

### TDD
- RED: `backfill_with_drain_reinjects_events_present_during_backfill` (compile failure on missing API), then GREEN: live events queued during the backfill are captured by the drain and re-injected in arrival order; V3 burn buffered before return (J3FMDO).

### Verification
- `cargo test -p degenbot-bot --lib`: 528 passed / 0 failed.
- `cargo test -p degenbot_rs --lib`: 46 passed / 0 failed.
- clippy --all-targets --all-features clean; rustfmt clean; `just check-no-pyo3-in-cores` OK.
- Dev extension rebuilt (`just dev`) and live-probed: `examples/eth_settlement_arbitrage_v2_v3_v4_rust.py` ran 150s through resume + 10 live headers (blocks → 25801487, 785 logs) with solves dispatching and ZERO `[WS-INVARIANT]` lines (the two matches in pane scrollback are the pre-fix 04:04 run). Previously the bot aborted at the first live block tombstone.

### Notes
- Handshake `pending` logs needed no fix: `subscribe_with_stream` already re-injects them into `combined_stream` before `SubscribeState` is stored (the task's secondary-defect hypothesis was wrong; verified while implementing).
- Pre-existing, benign-in-production quirk left as-is: `drain_stream_during_backfill` returns early (abandoning the backfill) if the combined stream ENDS mid-backfill; a live WS never ends. Worth revisiting if a reconnect path ever surfaces it.
