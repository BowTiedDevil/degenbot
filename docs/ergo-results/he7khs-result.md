# HE7KHS result — shared deferred-feed shell

## What shipped

- **NEW** `rust/crates/foundation/degenbot-rpc/src/feed_shell.rs` (67 lines): the one home of the deferred-start discipline. `DeferredFeedShell` holds the one-shot start latch + the stop channel; `spawn_feed_loop` holds the runtime dispatch.
- `backrun_feed.rs` / `txpool_feed.rs` each embed `shell: DeferredFeedShell`; `build` mints it, `start` delegates to `shell.start_once(..)` + `spawn_feed_loop(..)`, `stop` delegates to `shell.stop()`. Both bespoke `impl Drop for <Feed>` bodies (the stop send) are deleted — the shell field's `Drop` carries it.
- `pending_tx_stream.rs` is **unchanged**: `PendingTxPump` owns no latch/stop channel of its own — its `start`/`stop` already delegate to the inner feeds, so it now delegates transitively through the shell. Adding a shell there would mint a redundant second latch.
- `lib.rs`: `mod feed_shell;` (crate-private).

## Shell API (all `pub(crate)`)

```rust
pub(crate) struct DeferredFeedShell { /* stop_tx, started */ }
impl DeferredFeedShell {
    pub(crate) fn new() -> Self;
    pub(crate) fn start_once(&self, spawn: impl FnOnce(watch::Receiver<bool>));
    pub(crate) fn stop(&self);
}
impl Drop for DeferredFeedShell { /* stop_tx.send(true) */ }
pub(crate) fn spawn_feed_loop(fut: impl Future<Output=()> + Send + 'static);
```

`start_once` takes the latch, then subscribes the loop's stop receiver and hands it to `spawn` — the same order as before (latch swap, then `stop_tx.subscribe()`), so a stop before or after start is honored. `spawn_feed_loop` is `Handle::try_current().spawn(fut)` else `degenbot_core::runtime::get_runtime().spawn(fut)`.

## Public APIs unchanged

`BackrunFeed::{spawn,build,start,stop,status,drain}`, `TxpoolFeed::{…}`, `PendingTxPump::{provision_on_ring,source,start,status,stop,push}`, `PendingTxStream::{source,drain,dropped}` — identical signatures/semantics. The `spawn()` shims are kept and now carry the intent comment (eager Python single-feed path = exact build+start, behavior-identical).

## Per-feed before/after (lines)

| file | before | after | Δ |
|---|---|---|---|
| backrun_feed.rs | 541 | 532 | −9 |
| txpool_feed.rs | 653 | 644 | −9 |
| pending_tx_stream.rs | 345 | 345 | 0 |
| feed_shell.rs | 0 | 67 | +67 |

PM vocabulary grep `provision|started|latch|stop`: backrun 30→23, txpool 46→39, pending_tx_stream 12→12. The latch + stop channel + launch dispatch now live once, in the shell.

## Scoped gates (all foreground, all green)

- `cargo test -p degenbot-rpc` — 207 lib tests + integration suites green (incl. `pending_tx_stream::tests::*`, `txpool_feed::tests::dropped_stop_sender_ends_the_session_as_stopped`, `tests/backrun_feed.rs`, `tests/txpool_feed.rs`).
- `cargo test -p degenbot-bot` — 735 green, incl. the safety net: `minting_performs_zero_connection_attempts`, `starting_performs_the_deferred_dials`, `dropping_a_never_started_owner_tears_down_cleanly`, `dropping_the_owner_mid_storm_leaves_no_new_pubsub_reconnect`, `dropping_the_owner_stops_the_head_edge`, `dropping_the_owner_leaves_no_pubsub_reconnect_storm`.
- `cargo test -p degenbot_rs` — green.
- `cargo clippy -p degenbot-rpc --all-targets` — clean.
- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` — exit 0.

## Assertions touched

**None.** The feed unit tests and all E4/storm guards are untouched (no edit, no weakening). No test needed to change; the `Shared` counter structs were deliberately left in place precisely so the txpool test helper's literal `Shared` construction stays untouched.

## Tree

Only my four files are dirty (3 modified + 1 new). No foreign tree activity observed.
