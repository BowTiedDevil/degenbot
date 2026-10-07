# Async-cancellation audit — epic T4HGK5 close-out record

**Status: closed. Verdict strong; two real exposures found and fixed, one
exposure class fenced mechanically, and the remaining unsafe surface marked in
docs. No ADR was minted — the design decision that changed was amended in place
(ADR-050 D6).**

This is the close-out for the async-cancellation audit of the `rust/`
workspace (epic `T4HGK5`, 2026-10-07). It records the survey's scope, its
verdict, the two genuine cancellation bugs the survey found, the smaller hygiene
findings that came with them, and the rules the epic settled so a later change
does not quietly reintroduce the class.

## Scope and method

The survey covered the whole Rust workspace — **31 crates** in the epic's own
count, the 33 `crates/` members grouped by role under `rust/Cargo.toml`'s
`members` list: `foundation` (14), `engine` (9), `integrations` (6), `shells`
(3), `facade` (1) — at roughly **487k lines**, of which **~153 files** were
async-relevant. (Re-measured at close-out: 865 `.rs` files and 427,713 lines
under `rust/crates`, 489,691 lines under all of `rust/`; 155 files mention
`async fn` or `.await`, 158 match the broader `async fn | .await | select! |
tokio::spawn | JoinHandle` shape. The survey's figures are the counts at survey
time; the small drift is growth, not disagreement.)

The method was two-pass:

1. **Full-pattern census.** A textual sweep for every async-hazard shape —
   `select!`, `tokio::time::timeout`, `.abort()`, `lock().await`,
   channel sends, and blocking calls made from async context — over the whole
   crate tree. At close-out the tree holds 35 files with a `timeout(` call, 6
   with a `select!`, 27 that spawn, 148 with an `.await`, and 7 `.abort()`
   sites (4 in production, 3 inside `#[cfg(test)]` modules).
2. **Line-level reads.** Every production site the census flagged was read in
   full, and each was judged against the three-prong test the source material
   uses: *is the future's cancellation observed*, *does the cancel point carry a
   value*, and *is the resource released on every exit path*.

**Source principles.** The audit's frame comes from Rain's
`cancelling-async-rust` material (RustConf 2025 talk, and its written form plus
companion repo), and from Oxide's RFD 397 ("async/await challenges in the
control plane") and RFD 400 ("dealing with cancel safety"). The RFD 400 posture
shaped two of the epic's decisions directly: cancellation is a *drop* of the
future at its await point, there is no trait or compiler support for cancel
safety, and so **half of any fix is marking the remaining unsafe surface in
docs**. That is why this epic ends with a doc convention
(`# Cancel safety`) and a mechanical gate rather than only with code fixes.

## Verdict

**Strong.** Every risky primitive the census looks for is either absent from
production paths or fenced, and each fence is a mechanism rather than a
convention:

- **No bounded `.send().await` under a `select!` or a `timeout`.** The
  value-carrying side of the event hub uses **drop-oldest / latest-only**
  channels instead (`degenbot-eventhub`'s `OverflowPolicy::DropOldestCounted`
  and `OverflowPolicy::LatestOnly`), so an unconsumed event is superseded rather
  than parked on a full buffer — a ring that can discard the oldest entry cannot
  make a producer wait on a consumer.
- **`await_holding_lock` is `deny` workspace-wide**
  (`rust/Cargo.toml` `[workspace.lints.clippy]`), which is the compiler's title
  on holding a `std`/sync guard across an await.
- **Lock-hold forensics**, because that lint does not cover
  `parking_lot`/`lock_api` guards: `degenbot-substrate`'s `StateLock`
  wrapper was built after a real incident (2026-08-21: one `RwLock<BotState>`
  read guard held by a suspended async task across an `.await`, invisible in
  every thread dump, wedging the settlement bot for ~11 minutes). It registers
  every active read hold with its `#[track_caller]` site and warns on
  over-threshold holds, on both sides of the wait.
- **Aborts are rare, ordered, and joined to completion.** Only four
  production `abort()` sites exist; the driver's ordering contract is stated once in
  ADR-050 D6 (stop the pump → close the delivery channels → the consumer sees
  natural end → *then* cancel/join the consumer), and the shutdown paths
  `block_on` the join so the task's held resources drop before the caller
  returns.

The audit found **two real exposures** where the three-prong test failed
outright, plus a set of smaller hygiene items. None of them was a live
production wedge; both real exposures were reachable-but-rare cancellation
windows, and both are now fixed with a structural backstop rather than a longer
list of terminal paths.

## The two real exposures and their fixes

### 1. `EngineDriver::stop` aborted the pump unconditionally (ergo 2UQ7RO)

**The bug.** `stop()` set the shutdown flag and then called
`handle.abort()` on the pump task immediately, cancelling it at an arbitrary
await point. The pump had built cooperative-exit machinery for exactly this
case: `run_loop.rs` arms a 500 ms `timed_exit_tick` whose select arm polls
the shared shutdown flag, so the loop can unwind through its own span guards and
return normally. The abort bypassed that — the task was cancelled mid-await
instead of being allowed to finish.

**The fix** (commit `5520d1a6b`): a new `PUMP_STOP_GRACE` constant (2 s —
four ticks with margin, documented against the tick interval) now *fronts* the
join. `stop()` waits up to the grace for the pump to exit cooperatively and
escalates to `abort()` + `block_on(handle)` only when the grace expires — the
documented escalation cases being a GIL re-entry park through
`PySubscriberAdapter` or engine-lock contention inside `on_drain`. A distinct
join-error branch names a panicked pump, and **each path logs**, so an
escalation is visible rather than silent.

**ADR-050 D6 was amended in place, not replaced** — the ordering contract above
is unchanged; only the pump-termination mechanism tightened. This is the case
the epic intended to need no new ADR number.

### 2. `monitor_pending_transaction` leaked reservations on abort (ergo LWIAZL)

**The bug.** The pending-transaction monitor releases the transaction's nonce
and pool reservations on each of its three terminal return paths (confirmed,
expired, dispatcher-gone), but the release was **path-enumerated**. Two other
exits stranded it: `Dispatcher::abort_all_tasks` — reachable from Python
through the pyo3 boundary, so a cancellation the monitor does not control — drops
the monitor future mid-await, and a receipt-probe error returns before any
release. The property test `prop_lifecycle_state_machine` asserted no-leak only
for normal completion, so the abort path was unguarded by both code and test.

**The fix** (commit `8f0a363e9`): a `ReservationGuard` value carries the
release in its `Drop`, so it runs on *every* exit including cancellation. The
release has exactly one owner (`release_tx` has one production caller),
idempotent via `Option::take`; terminal paths release eagerly, preserving the
established release-then-telemetry ordering. A new test
(`abort_releases_reservations`) parks the monitor after its first probe,
aborts the task, and asserts the pool and the nonce lane are released after the
join.

## Smaller findings

Each of these is a real, verified finding with a landed fix; none was a live
correctness bug. Commits are `git log --oneline` abbreviations at close-out.

| ergo | finding | disposition | commit |
| --- | --- | --- | --- |
| `4CXRFM` | The txpool and backrun feed session select loops bound their stop arm as `Ok(()) = stop_rx.changed()`. Tokio's `watch` receiver resolves `changed()` to `Err` when the sender is dropped without a send, so that pattern **disabled** the arm on sender-drop — the session then waited out a full `cfg.watchdog` and ended as `Stall` instead of `Stopped`. | Pattern changed to `_ = stop_rx.changed()` at both sites (`txpool_feed.rs:345`, `backrun_feed.rs:346`), so the arm fires on changed *and* sender-dropped; a test drives the Err path the public API cannot otherwise reach. | `02d61747a` |
| `J43GQ2` | `exchange` gave connect, write, and read each a *full* `REQUEST_TIMEOUT` (worst case 3× the intended bound), and a timeout on `read_until` discarded however many partial bytes the dropped future had accumulated. | One `deadline` computed once, every phase spending the remaining budget; the read loop is resumable — `read_until` appends into the caller-owned buffer, so a dropped future leaves its prefix in place, and the terminal timeout reports the discarded partial-byte count. The `write_all_buf` insight applied on the read side. | `35f6cb86f` |
| `DAKKZH` | `SubscriptionHandle::unsubscribe` stores the flag `SeqCst` while the pump-callback loads were all `Relaxed` — a strength mismatch. Not a bug today (the buffer and the closed notify channel are independently coherent), but a half-fence future readers could reason past. | All five load sites (including a fifth the implementer found beyond the audit's four) now load `Acquire`, each with a comment naming the store it pairs with; the store stays `SeqCst` as the stronger side. | `08ebf7324` |
| `6P6OKR` | The batch-executor's `Arc<tokio::sync::Mutex<UnboundedReceiver>>` — `.lock().await.recv().await` — is the workspace's only production `tokio::sync::Mutex` guarding async state (the sole other is a test-only teardown serialization lock in `degenbot-fork`), held across two awaits. Cancel-safe today, but exactly the convenience-API shape RFD 400 warns against. | Kept (the alternative, `async_channel`, is not in the dependency tree, and the only hazard is a fairness caveat). `# Cancel safety` sections on `BatchDrain::next`, `next_outcome`, and `try_next_outcome` state the contract and the fence: the lock guards only a channel receiver and **must never be extended to invariant-bearing state**. | `5e9c68e1a` |
| `B5WCTK` | `raw_uncached_eth_get_code` hand-rolls HTTP JSON-RPC over `std::net::TcpStream` with a 5 s timeout and nothing enforcing it runs off the async runtime; an async caller would park a shared-runtime worker for the full timeout. | Renamed `blocking_raw_uncached_eth_get_code` (the repo's naming discipline for thread-blocking calls), given the thread-blocking doc contract, plus a debug-only `debug_assert!` on `tokio::runtime::Handle::try_current()` that makes a debug-build misinvocation loud. | `0bf6354e6` |
| `RT2XUW` | The pump run loop created its settle/inactivity window fresh each iteration as `timeout(wait_timeout, combined.next())`. A watchdog or exit-tick arm winning the select dropped that future and **restarted** the window, so a tick cadence shorter than the window pushed the quiesce publish past its true deadline. | Converted to a pinned, resettable `Sleep` owned across loop iterations; `settle_window_state(&StageMachine)` is the one point of truth for the window, re-armed only when the `(publish_pending, window)` tuple changes or an event is consumed. Watchdog and exit arms continue without touching `armed_window`, so the deadline survives the arm switch. | `56748250f` |
| `NO7VPZ` | A future bound to `let _ =` is dropped on the spot and its async body never runs, and Rust cannot backstop that with `unused_must_use` because most futures are not `#[must_use]` (the talk's opening example). Nothing in the workspace enforced it. | `let_underscore_future = "warn"` enabled workspace-wide in `rust/Cargo.toml`. Zero current hits across `cargo clippy --workspace --all-targets` — enablement only; the workspace's `warnings = "deny"` posture escalates it at push. | `046cdadf5` |
| `3MKSEA` | No mechanical guard against reintroducing the value-carrying-cancel-unsafe class — a lesson this repo already learned with `just check-no-pyo3-in-cores`. | New architecture gate `no_timeout_wrapped_value_sends_or_write_all` (body in `rust/crates/facade/degenbot/tests/architecture_gates.rs`): a production `timeout(..)` may not wrap a data-carrying `.send(` or a non-resumable `.write_all(`. Exactly one allowlist entry, named file+line with its reason. Exposed as `just check-cancel-safety`. | `085505453` |
| `CWIP3F` | There is no trait or compiler support for cancel safety, so the *docs* are half the fence — and public async fns documented it ad hoc. | `# Cancel safety` sections standardized on the boundary functions the audit identified: `send_request`, `monitor_pending_transaction`, `exchange_within`, `backfill_with_drain`, `PumpState::resume`, `SessionEndDetection::from_future`, `drive_new_heads`, plus the three batch-executor receivers from `6P6OKR`. Each states cancel-safe / mostly-cancel-safe / not cancel-safe, the consequence, and the caller's obligation. Docs-only; zero function bodies changed. | `35bd2d9b3` |

## Standing guidance (do not regress)

These are the epic's rules. They are stated as rules because each one is a
mechanism in the tree today, not a preference:

1. **Drop-oldest / latest-only eventhub channels over a bounded send under a
   select loop.** A bounded `.send().await` inside a `select!` is a deadlock
   waiting for a slow consumer; the hub's `DropOldestCounted` and `LatestOnly`
   policies make supersession the contract. If a new producer needs backpressure,
   it is a policy decision with a name, not a channel constructor argument.
2. **Never extend the `tokio::Mutex` pattern to invariant-bearing state.** The
   one surviving `tokio::sync::Mutex` guards a channel receiver; its
   `# Cancel safety` section carries the explicit fence. A lock whose invariant
   spans an await is the 2026-08-21 class, and the workspace `deny` on
   `await_holding_lock` does not cover `parking_lot` guards.
3. **Abort is an escalation, never the primary mechanism; cooperative exit
   first.** `EngineDriver::stop` is the reference implementation: set the flag,
   wait out the grace, escalate loudly and log it.
4. **Loud abort over silent drop (ADR-021 posture).** A stranded pipe is a
   process abort, not data loss — the worker fleet's stranded-result-pipe
   tripwire is the model. Never heal a divergence quietly; classify, stop
   loudly.
5. **`just check-cancel-safety` is the mechanical fence; the `# Cancel safety`
   doc sections mark the remainder.** The gate is a lexical tripwire for the
   direct form (its own doc comment states the known limits: a send hidden behind
   a helper, or a `.send(` in the timeout's first argument, is not caught). A
   green gate means "the direct shape is absent", not "cancel safety is proven";
   the doc sections are what carry the argument for everything the gate cannot
   see.

## Where the fences live

- `just check-cancel-safety` — the mechanical sweep
  (`rust/crates/facade/degenbot/tests/architecture_gates.rs`, gate
  `no_timeout_wrapped_value_sends_or_write_all`).
- `rust/Cargo.toml` `[workspace.lints.clippy]` — `await_holding_lock = "deny"`
  and `let_underscore_future = "warn"` (escalated by the workspace's
  `warnings = "deny"` at push/CI time).
- `degenbot-substrate` `StateLock` — the hold-forensics wrapper around the
  shared state lock.
- `docs/adr/ADR-050-rust-native-engine-driver.md` D6 — the driver stop/ordering
  contract and the cooperative-then-escalate termination rule.
- The `# Cancel safety` Rustdoc sections on the ten boundary functions listed
  under `CWIP3F`.

## Epic reference

- **Epic:** `T4HGK5` — "Async cancellation safety hardening (degenbot rust
  workspace)". Closed by this record (`NZJZAO`).
- **Child tasks and their commits** (all closed; verified as ancestors of the
  close-out HEAD):

| ergo | title | commit |
| --- | --- | --- |
| `LWIAZL` | P0-1: Drop guard for monitor reservations | `8f0a363e9` |
| `4CXRFM` | P0-2: Fix the disabled stop arm in the feed select loops | `02d61747a` |
| `NO7VPZ` | P0-3: Enable `let_underscore_future` lint | `046cdadf5` |
| `2UQ7RO` | P1-1: Cooperative-first escalation ladder in `EngineDriver::stop` | `5520d1a6b` |
| `J43GQ2` | P1-2: Single-deadline request budget in operator exchange | `35f6cb86f` |
| `DAKKZH` | P1-3: Ordering-atomicity fix in `SubscriptionHandle::unsubscribe` | `08ebf7324` |
| `6P6OKR` | P2-1: Decide the fate of the `Arc<tokio::sync::Mutex<Receiver>>` | `5e9c68e1a` |
| `B5WCTK` | P2-2: Guard the blocking HTTP path in `bot_state_db` | `0bf6354e6` |
| `RT2XUW` | P2-3: Resume-don't-restart the settle timer in the pump select loop | `56748250f` |
| `3MKSEA` | P3-1: CI guard against timeout-wrapped sends/writes | `085505453` (allowlist re-anchored on source text by `68e2d0c4e` after a line-number drift) |
| `CWIP3F` | P3-2: Adopt the `# Cancel safety` doc convention | `35bd2d9b3` |
| `NZJZAO` | P3-3: Epic close-out record | this file |
