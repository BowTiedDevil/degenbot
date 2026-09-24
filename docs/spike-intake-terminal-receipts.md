# spike: intake terminal-receipt blast radius (Faulted, build_paths window, _consume)

**Task:** TB4QGX T5 / `QNYYOB` — **spike**, no production code changed.
**Status:** decision note produced; **awaiting the terminal-semantics decision (checkpoint).**
**Date:** 2026-09-12

## 1. Why this spike exists

The registration intake host can now (after T2/T3/T4) no longer park silently
with a non-empty backlog. But the planned `Faulted` terminal (the sticky
`LaneDeath` latch, VXP27K) *intentionally stops taking new units*. If the
driver has receipts outstanding when that latch lands, nothing resolves them —
and the **Python driver hangs one layer above the Rust FSM**. This note pins the
exact semantics and blast radius before VXP27K is implemented.

## 2. The hang mechanism (the root of the blast radius)

`PyIntakeReceipt` (`rust/crates/shells/degenbot-python/src/bot/intake.rs`) is filled
**only inside the submitted closure**:

```rust
intake.spawn(Box::new(move || {
    let outcome = Python::attach(|py| fn_work.call0(py));
    *outcome_slot.lock()... = Some(outcome);
    done.store(true, Ordering::Relaxed);
    let _ = sig_tx.send(());
}));
```

The fields: `outcome: Arc<Mutex<Option<IntakeOutcome>>>`, `signal_rx`,
`done: Arc<AtomicBool>`. If the host latches `Faulted` and **never runs the
closure**, then for every outstanding unit:

- `done()` stays `false`;
- `result()` raises `RuntimeError("intake unit has not completed yet")` if polled early;
- `wait(timeout=None)` parks on `recv()` **forever**;
- `wait(timeout=t)` is the **only bounded escape** (`TimeoutError`);
- `wait_async()` parks on the blocking `recv()` with **no timeout at all**.

`run_registration`'s completion clause is `while inflight: await _resolve(...)`
with `_resolve` = `await receipt.wait_async()` + `_absorb_outcome(result())`.
So a Faulted host **hangs the crawl at the receipt join** — the same observed
shape as the 9,955 freeze, just above the Rust layer. **Therefore the Faulted
drain MUST resolve every outstanding receipt** (backlog + role queue +
in-flight seats) to a terminal outcome; fixing the Rust FSM alone is not enough.

## 3. Who can resolve a receipt (the seam problem)

The bot core's `seat_host` only holds `InnerWork = Box<dyn FnOnce() + Send + 'static>`
(`arb_engine/fleet_intake.rs`) — **opaque**. It cannot fill a `PyIntakeReceipt`.
The receipt is a pyo3-leaf construct. Two viable seams:

- **(S1) Host-side terminal injection.** Extend the intake so the host can
  *terminally fail* a held unit, not just run it. The bot core needs a per-unit
  terminal hook (or a fault broadcast the unit body observes before running).
  **This breaks the `fleet_intake` HARD RATCHET** ("the surface is this port +
  2 pub fns — add nothing").
- **(S2) Receipt-side fault watch.** No port change: `registration_intake()`
  hands the pyo3 leaf a fault handle at submit time; the receipt stores it and
  `wait_async`/wait/result race the unit delivery against the fault signal,
  resolving terminally when it fires. The bot core stays pyo3-free and the
  ratchet holds; the cost is a small watch handle + a resolver race in
  `intake.rs`.

**Recommendation: S2** (no ratchet break, fault logic stays in the pyo3 leaf
where the receipt already lives). The spike flags S1 only because VXP27K also
needs the host to *stop* admitting, which is independent of receipt resolution.

## 4. Chosen typed error (proposal — needs sign-off)

Reuse the existing lane-death vocabulary (`LaneFailure::LaneDeath { unit, seat }`,
`DrainFailure` in `arb_engine/executor.rs`) at the policy level, and add a
distinct Python-facing typed error:

- Rust: a small `IntakeFault { role, cause: PostureCause, held: usize }`
  (or reuse `LaneFailure::LaneDeath` semantics — intake carries no path, so the
  lane-death pair is not a perfect fit).
- Python: a new exception `FleetIntakeFaultedError` (subclass of `RuntimeError`)
  so `run_registration`/`_consume` can distinguish it from the existing fatal
  `VerificationMismatchError`/`VerificationRpcError`/`DirectionResolutionError`.
  The message carries the cause + the number of held units that were resolved.

## 5. No unit dropped; at-most-once preserved

**Resolution ≠ execution.** A terminally-resolved held unit ran **0 times**; a
unit whose closure already completed ran **exactly once** and its receipt is
already filled — it is never re-resolved. So at-most-once holds. "Never drop"
(§10) is preserved by making the resolution **typed + counted + loud**, never a
silent discard: a new counter `degenbot_fleet_intake_fault_resolved_total`
(a `held` label), the sticky `LaneDeath` posture cause, and a completion-log
clause. `_absorb_outcome` must **not** fold a fault into `register-fail`
(that would masquerade as a real build failure) — use a dedicated counter and a
distinct summary line.

## 6. Crawl / operator semantics — the decision to make

- **(A) Fatal / loud (recommended for the crawl).** Held receipts resolve to
  the typed error; `receipt.result()` re-raises; `run_registration` propagates
  → the crawl **aborts loudly**, matching the existing "shut down" contract.
  No degraded count; the Progress summary ends with the fault line. Mirrors
  solve-lane `LaneDeath` (sticky cordon, process lives, no stranded submitter).
- **(B) Degraded completion.** Held receipts resolve to a typed *outcome* folded
  by `_absorb_outcome` as a new `kind="fault"`; the crawl finishes with a lower
  `registered_paths` + a loud log. Non-fatal; risks a soft undercount unless
  the completion line states the faulted count explicitly.
- **(C) Hybrid (recommended overall).** `run_registration` (the discovery
  crawl) uses **(A)** — abort loudly; the operator surfaces
  (`enqueue_path`/`trigger_discovery`, `_consume`) **raise the typed error to
  the caller** rather than killing the bot process. One receipt-resolution
  mechanism (S2 or S1), two propagation policies chosen at the two call sites.

## 7. Exact call sites that change

| Layer | File | Change |
|---|---|---|
| Rust port | `rust/crates/engine/degenbot-bot/src/arb_engine/fleet_intake.rs` | fault handle (S2) **or** per-unit terminal hook (S1) |
| Rust host | `rust/crates/engine/degenbot-bot/src/arb_engine/seat_host.rs` | Faulted arm drains backlog + role queue + in-flight, resolves each |
| Rust executor | `rust/crates/engine/degenbot-bot/src/arb_engine/fleet_registration_executor.rs` | expose the fault state / wire the drain |
| Rust policy | `rust/crates/engine/degenbot-bot/src/arb_engine/executor.rs` | reuse `drain_death_response` shape (counter + sticky cause + loud log) |
| pyo3 | `rust/crates/shells/degenbot-python/src/bot/intake.rs` | receipt watches fault; `wait_async`/`wait`/`result` resolve terminally; map the typed error |
| Python driver | `src/degenbot/runner/build_paths.py` | `run_registration` (`_resolve` + drain) and `_consume` (operator) policy |
| Python API | `src/degenbot/bot/_bot.py` | `submit_registration_unit` surface unchanged (docstring only) |

## 8. Telemetry / counters to add

- `degenbot_fleet_intake_fault_resolved_total` (counter; `role=pool_state_updater`).
- Reuse `degenbot_fleet_intake_backlog` (gauge) to show the drain to zero at fault.
- Sticky `PostureCause::LaneDeath` (already the solve-lane cause) + a once-per-K
  loud log in the `drain_death_response` cadence.
- A completion-log clause "`{n} intake unit(s) unresolvable (faulted)`".

## 9. Decision adopted (operator sign-off, 2026-09-12)

1. **Semantics: (C) hybrid.** `run_registration` (discovery crawl) aborts loudly
   on the typed fault; the operator surfaces (`enqueue_path` / `trigger_discovery`
   via `_consume`) raise the typed error to the caller.
2. **Typed error: a new `FleetIntakeFaultedError`** (distinct from the existing
   fatal `VerificationMismatchError` / `VerificationRpcError` /
   `DirectionResolutionError`), carrying the cause + held-unit count.
3. **Seam: (S2), receipt-side fault watch.** No `fleet_intake` port change; the
   pyo3 leaf races the unit delivery against a fault handle.

**VXP27K implements exactly this.**
