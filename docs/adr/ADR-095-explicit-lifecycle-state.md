# ADR-095: Explicit lifecycle state

## Status

Accepted (2026-11-06).

## Context

Five lifecycles across one epic were each governed by an *implicit* representation of
state — a fact inferred from which flag was set, which channel or handle still existed,
or which map a record sat in — rather than by a named state value:

- **Nonce occupancy** (`degenbot-substrate::nonce`) kept five parallel maps — `leases`,
  `broadcasts`, `landed`, `holds`, `lapsed_holds` — so "what occupies nonce N" and "which
  lease does strategy S hold" were two indexes that had to agree by call-site discipline.
  A lease handle was validated against the strategy-keyed map (a second index beside the
  nonce-keyed truth), and the cross-map invariants (one occupant per slot; a hold and its
  lease never both live at one nonce) held only in the transition methods' prose.
- **Sim-submit** (`degenbot-submission::sim_pipeline`) kept the failure in a
  `Mutex<Option<String>>` beside the queue-sender and submitter handles, so "does this
  lane accept work?" was answered from handle presence and one failure slot. `enqueue`
  counted and spawned every batch regardless: a batch offered after shutdown was counted,
  its sim spawned, and the slot silently dropped when no sender remained.
- **Subscription** (`degenbot-rpc::subscription`) kept three lifecycles: control markers
  buffered inside the data channel, lifecycle atomics beside the handle, and terminal
  flags on the Python wrapper. [ADR-094](ADR-094-subscription-lifecycle-one-authority.md)
  records that site's full decision; this ADR cross-links it and does not repeat it.
- **Hosted sources** (`degenbot-bot::hosted_sources`) kept a `started` latch beside
  `clock`/`trigger`/`head_ws_provider` option fields and a `head_edge_stopped` flag, so
  the lifecycle was inferred from resource presence. A start that failed after launching
  some resources left a started owner indistinguishable from a running one, and a second
  `start()` was a silent no-op that hid the failure instead of naming it. The head edge's
  mode was two booleans read off the same presence inference.
- **Subscribe handshake** (`degenbot-ingestion`) was one prose-sequenced loop inside the
  WS transport adapter: anchor-confirmation-settle legality lived in comments over loose
  locals (`prev_header`, `settle_deadline`, `pending`), reachable only through the whole
  connect-and-subscribe path.

Every site had the same class of defect available: two sources of one fact, a transition
that prose forbade but types allowed, a partial result indistinguishable from success, or
a terminal one read path observed and another missed.

## Decision

**Each lifecycle is one explicit state value with exactly one writer. Callers request
transitions and read projections; they never assign state, and no second copy of a
lifecycle fact survives.**

1. **One owner, one writer.** The state value lives inside the type that owns the
   resources, and only its named transition methods write it: `NonceAuthority`'s
   occupancy map under its lock, `PipelineState` on the lane, `SubscriptionPhase` through
   `SubscriptionHandle::apply` (ADR-094), `HostedLifecycle` on the `HostedSources` owner,
   and `HandshakeMachine::handle` for the handshake.
2. **State enums carry payloads; readers get payload-free projections.** The resources a
   phase owns ride in the state (`Provisioned { deferred }`, `Running { live }`, the
   `Settling` header facts, a `Failed`'s `TerminalFailure`), so teardown is a match over
   what the phase actually holds — never a guess at which optional field is populated. A
   caller reads a projection (`HostedPhase`, `HeadEdgeMode`, `phase`, `NonceOccupant`) and
   never the resources behind it.
3. **Typed declines and named transitions.** A refused call is data:
   `DeclineKind`, `EnqueueDecline` via `EnqueueOutcome`, `HostedStartError`,
   `StreamRefusal`, `LifecycleDecline`/`ReadInProgress` (ADR-094). Nothing answers a
   refusal with a silent `Ok`, a bool, or a missing `Option`. A lifecycle read that
   observes nothing (`SimSubmitPipeline::failure`) is separate from an observing
   transition, so reading can never itself change state.
4. **Terminal states are absorbing; the first failure is authoritative.** A terminal
   phase refuses every later event, one recorded failure is never replaced by a later
   one, and teardown is idempotent and total from every phase — a second `shutdown`
   reports the same carried failure rather than a fresh success. Observation is
   bookkeeping: reporting a failure never reopens admission.
5. **Transient and terminal are distinct values.** A watchdog stall is a transient
   `Reconnecting` (ADR-094); a stale head watch is one polled iteration inside a still
   live `WatchLive` edge; but a **partial startup is terminal** — `HalfStarted` is never
   projected as running and is recovered only by dropping the owner and minting again
   (`StartAfterFailure` names the attempt).
6. **Lock/await discipline is stated per machine.** Nonce: every transition is a sync
   method applied wholly under the one authority lock. Sim-submit: the std
   lifecycle/tx/submitter locks are never held across an `.await`; the one async lock
   (`shutdown_serial`) is deliberately held across the submitter join to serialize whole
   shutdown calls. Hosted: the lifecycle lock is leaf-scoped, never held across an
   `.await`, and the pumps lock is never taken under it. Subscription: `lifecycle` ->
   `pending` -> `buffers[*]`, never reversed, never held across an `.await`, and the
   state write and its `watch` publication share one critical section (ADR-094). The
   handshake machine is pure and sync; the adapter around it owns the awaits.

### The five machines

| Site | The one writer | Reader projection | Typed answers |
|---|---|---|---|
| Nonce occupancy | `NonceAuthority`'s one occupancy map under its lock | `NonceOccupant` via `occupant()` / `occupancy()` | `DeclineKind` |
| Sim-submit lane | `PipelineState` (Accepting / Failed / Closing / Closed) | `state()`, `failure()` | `EnqueueDecline` via `EnqueueOutcome` |
| Subscription | `SubscriptionPhase` via `apply(LifecycleEvent)` — see ADR-094 | `phase`, `RawDrain::notice` | `LifecycleDecline`, `ReadInProgress` |
| Hosted sources | `HostedLifecycle` (Provisioned / Starting / Running / HalfStarted / TornDown) | `HostedPhase`, `HeadEdgeStatus` | `HostedStartError`, `StreamRefusal` |
| Subscribe handshake | `HandshakeMachine` (completion emits the effect and spends the state) | `HandshakeState`, `first_log_block()`, `settle_deadline()` | closed `HandshakeEffect` vocabulary |

### Per-site decisions

**Nonce occupancy is a per-nonce FSM.** The occupancy map is the sole slot truth: absence
means free, and one nonce carries at most one occupant — `Lease`, `Broadcast`,
`DeliveryHold` (either `HoldPhase`), or the retained confirmation forms `Landed` and
`LapsedHold`. Confirmation moves an occupant into its retained form *in place*: the slot
stays occupied, and only the reorg rewind (`set_confirmed_reorg`) returns it to the wire
(`landed -> broadcast`, `lapsed hold -> hold`) while releasing every lease reserved
inside the rewound window with one `ReorgAdvisory` per lease. A revival is deliberately
**no notice** — a caller diffs `occupancy()` across the call. A lease handle is verified
against the occupancy map (never a second index), so a forged or consumed handle declines
`UnknownLease`. The head-reconciliation and strategy-host integration pin these facts
(`a_rewind_restores_exactly_the_rewound_window`,
`a_head_update_retains_occupancy_and_rewinds_exactly_the_window`);
`nonce_occupancy.rs` and `nonce_occupancy_matrix.rs` pin the transition table slot by
slot.

**The sim-submit lane's lifecycle is total.** `PipelineState` is the ONE authority for
enqueue admission, the authoritative failure, and whether that failure has been reported.
The queue-sender and submitter handles are teardown state only — their presence says
nothing about admission. Enqueue admission is `Accepting`-only; a decline means the batch
had **no effect** (nothing counted, spawned, or queued). The first leaf (or queue-send)
failure is authoritative and terminal for new work; `observe_failure` reports it once
(between-batch poll) and `shutdown` reports it on every call. Ordering is structural, not
prose: one FIFO queue and one submitter that awaits each batch's own sim before
submitting (`reverse_sim_completion_still_submits_in_fifo_order`). Concurrent `shutdown`
calls serialize whole drains (`a_concurrent_shutdown_waits_for_the_first_drain_to_finish`)
instead of observing consumed handles. The session consumer treats a typed decline as
terminal for its loop: it quiesces and surfaces the lane's own failure text as the
verdict, or `ConsumerEnd::PumpStopped` when the lane closed without one — never a
paraphrased refusal, never a clean end a leaf did not produce.

**Hosted sources' lifecycle is total, including partial startup.** `HostedLifecycle` is
the only home of the resources each phase owns; `start` (`request_start`) is legal from
`Provisioned` (which moves to `Starting` and hands the launch its deferred set) and is an
idempotent no-op from `Running`; every other phase is a typed refusal. A launch that
provisions everything reports `resources_launched` (`Running`); a launch that fails after
creating some resources reports `launch_failed` (`HalfStarted`) carrying exactly what it
created, so the teardown is still ordered and total. `HalfStarted` is terminal: never
running, never retried (`StartAfterFailure`), recovered by drop + recreate. Teardown is
one ordered plan from every phase: the WS transport frontend is dropped first (while the
endpoint may still be alive, so the pubsub service exits cleanly) and the dedicated
pubsub runtime is torn down last (cancelling the service and the driving tasks
deterministically). The head edge owns a mode machine
(`NotSpawned`/`WatchConnecting`/`WatchLive`/`PollFallback(cause)`/`HubHosted`/`Stopped`)
published through one `HeadEdgeStatus` writer: mode is policy and the per-iteration poll
decision is derived from it, a stale live watch polls one iteration while its watchdog
reconnects and *stays* `WatchLive`, and `Stopped` is published exactly once and is
monotone. The stream hand-out (`take_stream`) is once-only with a typed refusal
(`NoPump` vs `AlreadyTaken`).

**The subscribe handshake is a pure machine with boundary effects.** Every input is a
closed `HandshakeEvent`; every transition answers with a closed `HandshakeEffect`
(`Continue`, `FetchLatestBlock`, `Complete`); the async adapter owns only stream polling,
the `eth_blockNumber` degraded path, the injected clock, and re-injection — it never
re-authors boundary legality. The machine anchors on the first header, confirms at its
successor, and names the delivery-hole-free boundary `W`: the first delivered pool log's
block when that is provably covered, otherwise the header-confirmed fallback once the
settle window or the fallback rules say so. Boundary effects are explicit: every pool log
the handshake consumes is retained in arrival order and re-injected ahead of the live
tail (`HandshakeCompletion::into_boundary`), so no delivery falls in a hole at the
handoff to backfill; a blockless log supplies no boundary; a first log above the
confirmed head selects its block only after the deadline; shutdown always completes at
a zero boundary (block and timestamp 0), while stream-end completes at the current
anchor (zero when none) — both with pending preserved; events after completion are
ignored. Total (phase x event) matrices pin the table, so a new phase or event cannot
land with undefined cells.

### Intentional public API and behaviour changes

- **Nonce (`6c83bf2c1`)** — additive Rust API: `NonceOccupant`
  (`Lease`/`Broadcast`/`DeliveryHold`/`Landed`/`LapsedHold`) with `strategy()` and
  `is_outstanding()`, and the `occupant(nonce)` / `occupancy()` projections on
  `NonceAuthority`. The `DeclineKind` vocabulary is unchanged. The observable contract
  change is representational: confirmation now *retains* occupants (a landed or lapsed
  slot is never reusable until a rewind restores it) and a rewind restores exactly the
  rewound window, facts now visible through `occupancy()` instead of five maps' private
  agreement.
- **Sim-submit (`ca76ee92a`)** — `SimSubmitPipeline::enqueue` returns `EnqueueOutcome`
  (`Accepted` / `Declined(EnqueueDecline)`) instead of `()`; new `PipelineState`,
  `TerminalFailure`, `EnqueueDecline`, `EnqueueOutcome`; new observers `state()` and
  `failure()`; `shutdown` is documented idempotent and re-raises the carried failure on
  every call. Python: `SimSubmitPipeline.enqueue` returns `EnqueueDecline | None`, and
  the `EnqueueDecline` class crosses as a value (`==`/`in`), exported from
  `degenbot.dispatch`. `BatchExecutor::enqueue` passes the typed outcome through,
  `BatchExecutor::failure()` is a non-observing read, and its `shutdown` reports the
  stored failure on every call. **Behaviour:** a batch offered to a dead lane is now
  declined with no side effects, where it used to be counted and silently dropped; the
  session consumer ends the pump on a decline instead of miscounting the batch.
- **Subscription (`6297d5972`)** — recorded in
  [ADR-094](ADR-094-subscription-lifecycle-one-authority.md) (the `phase` property, the
  retained `started()` outcome, terminal semantics for `__anext__`/`drain`, the
  typed concurrent-read refusal, and the intentional behaviour change that a watchdog
  stall is a transient reconnect rather than a terminal Python error).
- **Hosted sources (`ce274a1f5`)** — new `HostedPhase`, `HostedStartError`
  (`Hub`/`StartInFlight`/`StartAfterFailure`/`TornDown`), `StreamRefusal`
  (`NoPump`/`AlreadyTaken`), `HeadEdgeMode`, `PollCause`, `HeadEdgeStatus`;
  `take_stream` returns a typed refusal; the `head_edge_stopped` flag is replaced by the
  `HeadEdgeStatus` projection. `SessionBootError` gains `HostedStart` and `Stream`; the
  boot maps `NoPump` to "this arm gets no stream" (its driver halts at its live-mode
  gate) and fails the boot hard on `AlreadyTaken` (a miswired double hand-out used to
  surface as a silent missing stream). Python maps both new boot errors to `RuntimeError`
  ("hosted sources failed to start" / "hosted sources miswired"). **Behaviour:** a
  refused or failed start is now named (`StartInFlight`/`StartAfterFailure`/`TornDown`)
  where a second `start()` used to be a silent no-op; a partial startup is `HalfStarted`
  and never projected as running.
- **Subscribe handshake (`8394a7a9f`)** — new public `degenbot_ingestion::handshake`
  module and crate-root exports (`HandshakeMachine`, `HandshakeState`, `HandshakeEvent`,
  `HandshakeEffect`, `HandshakeCompletion`) plus the injected-clock
  `LOG_CATCHUP_SETTLE_MS`. `WsIngestor::subscribe_with_handshake` and `handshake` keep
  their contracts (same delivery-hole-free boundary, same pending re-injection); the
  behaviour change is that boundary legality is now a machine's transition table with
  named tests instead of comments in the adapter, and post-completion events are
  explicitly inert. **Adapter behaviour:** once settling is armed, the stream-poll budget
  is min(60s stream silence, time-to-the-settle-deadline), so deadline expiry alone can
  complete the header/log fallback — without another stream event or the 60s silence
  timeout; a stream event already buffered at expiry still wins event-first and is
  evaluated at the sampled clock. Public handshake signatures are unchanged.

## Consequences

- "What state is this in?" has one answer per lifecycle, on every read path: fast and
  slow iteration, bulk drain, projections, and repr agree because they read one value.
- An illegal transition is a typed refusal a test asserts by variant; a new phase or
  event that misses a table cell fails a total-matrix test instead of reaching
  production through an untested path.
- Partial startup is honest: a half-started owner says so, tears down what it created in
  order, and cannot be mistaken for running.
- The nonce ledger cannot double-book a slot or disagree with itself about a handle,
  because there is nothing left to disagree with: the occupancy map is the only fact.
- Publication and ordering properties (the subscription's watch publication, the
  sim-submit lane's FIFO order) are properties of the code shape, pinned by regression
  tests over the seam, not of stress timing.
- Comments at these sites carry only the why (why a hold freezes its slot, why the
  runtime tears down last, why a revival is no notice); transition legality lives in the
  enums and acceptance criteria live in named tests (`docs/comments.md`).

## Alternatives rejected

- **Keep the old representation and add a flag or field to agree with it** — a
  compatibility layer over the implicit state (a core flag beside the wrapper's flags, a
  `started` latch kept "in sync" with the lifecycle enum, a lease index kept "in
  agreement" with the occupancy map). Divergence is a property of duplication, not of
  either copy's care; every site's bug class came from two copies of one fact.
- **Permanent parallel implementations** — a legacy method kept beside the new
  transition, or the five-map code path kept "for compatibility". Both are drag past
  their usefulness: the cutovers here are hard (the old maps, the latch, the wrapper's
  flags, the inline handshake loop are deleted, not shadowed), and a parallel path that
  is never deleted re-litigates the decision on every call site.
- **Infer the phase from resource presence and keep the inference** — reading the
  lifecycle off `tx`/`submitter`/`clock`/`head_ws_provider` optionality. That is the
  representation being replaced; a partial start is exactly the state the inference
  cannot express.
- **Bool (or `Option`) returns for refusals** — the typed decline makes the reason a
  value a caller can branch on and a test can assert by variant; a falsy return erases
  "already running" vs "already failed" vs "torn down", which are different operator
  events.
- **Make the transient reconnect terminal and let callers resubscribe** (recorded in
  ADR-094) and its head-edge cousin, **stop the edge when the watch goes stale** — both
  turn a survivable socket blip into operator-visible failure; the watchdog's purpose is
  surviving exactly that.
- **Retry a failed hosted start in place** — a silent re-attempt hides the failure and
  can re-dial resources a previous launch already created; `StartAfterFailure` names the
  state and drop + recreate is the one recovery.
- **Let the handshake adapter keep authoring boundary legality** — prose sequencing over
  loose locals cannot be tested except through the WS path and cannot be total; the pure
  machine makes the legality a table and the adapter a driver.
