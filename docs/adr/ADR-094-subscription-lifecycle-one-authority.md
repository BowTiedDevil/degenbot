# ADR-094: The subscription lifecycle is one Rust state machine

## Status

Accepted (2026-11-06).

## Context

The pub-sub handle behind `AlloyProvider.subscribe_*` had three lifecycles
pretending to be one:

- **The data channel carried control.** `RawSubItem::End` and
  `RawSubItem::Disconnected` were buffered *inside* the item stream, so "the
  subscription is over" was a position in a queue rather than a state. A drain
  that took the buffer first could lose the marker (the terminal became
  invisible), and a drain that reported the marker had no way to stop a later
  pump write from appending behind it.
- **The Rust handle kept flags.** `started`, `start_failed`, `start_error`, and
  `unsubscribed` were `AtomicBool`s beside the buffer, written by the pump and
  read by the binding, with no single writer and no ordering story.
- **The binding kept its own pair.** `PyAlloySubscription` carried `ended: bool`
  and `disconnected: Option<String>`, set from whichever drain path observed the
  marker first, plus an `if/else` `__repr__` that inferred the lifecycle from
  them. A terminal reached through one read path was invisible to another: a slow
  `__anext__` could latch `ended` while a later fast `__anext__` still saw
  `active`, and a bulk `drain()` could report a disconnect the iterator never
  learned about.

Startup had the same take-once flaw: `start_notify_rx` was an
`Mutex<Option<Receiver>>` consumed by the first `started()` caller, so a second
waiter hit `"Start notification channel already consumed"` and a third could see
`"notification received but subscription state is ambiguous"`. The wake-up
receiver had the identical shape, which is why a temporarily checked-out
`notify_rx` looked exactly like a closed subscription: concurrent `__anext__`
calls raised a spurious `StopAsyncIteration`.

Finally, a `newHeads` watchdog stall buffered `Disconnected`, so a transient
reconnect — the socket came back, the resubscribe succeeded — surfaced to Python
as a terminal `RuntimeError`. Reconnect and disconnection were the same value.

## Decision

**`SubscriptionHandle` is the sole lifecycle authority. Every pump kind and
every read path reports and observes one state machine.**

1. **One phase enum, one writer.** `SubscriptionPhase` is `Starting`,
   `Streaming`, `Reconnecting { message }`, `Ended`, `Failed { message }`, or
   `Unsubscribed`. Only `SubscriptionHandle::apply(LifecycleEvent)` writes it, so
   the five pump kinds (`pump_blocks` with its watchdog, `pump_full_blocks`,
   `pump_pending_transactions`, `pump_full_pending_transactions`, `pump_logs`)
   report the same vocabulary, and the shared `drive_items` body is the only
   place a pump decides what a stream item, a clean end, or a stream error means.
   `apply` answers with a typed `Result<(), LifecycleDecline>`: `Terminal` when
   the subscription is already over, `Inadmissible` when the event means nothing
   in this phase, both carrying the phase and the event. A refusal is data a
   caller can branch on — and an illegal move is testable by variant instead of
   by a falsy return — while a pump discards it through one named `report`, since
   a stale report against an absorbing phase is not an error for a pump.
2. **Terminal phases are absorbing.** `Ended`, `Failed`, and `Unsubscribed`
   refuse every later event. That is what makes a late pump write, or a start ack
   that arrives after `unsubscribe()`, unable to report the subscription active
   again. `unsubscribe()` is idempotent and latches `Unsubscribed` before
   aborting the pump task.
3. **Reconnect is not terminal.** A watchdog stall moves `Streaming` ->
   `Reconnecting`, a successful resubscribe moves back to `Streaming`, and a
   reader keeps awaiting data across it. Only the three terminal phases end
   iteration.
4. **Startup is retained, not consumed — and published in order.** The lifecycle
   is published on a `tokio::sync::watch`, so `started()` is safe for early, late,
   and repeated waiters: they all await the one latched `StartupResult` (`Started`,
   `Failed { message }`, or `Closed { phase }` when the handle was torn down
   first). The take-once channel and its "already consumed"/"ambiguous" outcomes
   are gone.

   The state write and its publication share ONE critical section (a `watch` send
   is synchronous and cannot block), so the retained publication's total order is
   the authority's. Publishing after releasing the lock leaves a window in which
   two concurrent applies could publish in the opposite order from their state
   writes, stranding subscribers on a stale phase; that window is what
   `a_publication_cannot_be_displaced_by_a_concurrent_transition` pins shut.
5. **Drain results carry data plus a typed notice.** `drain_raw()` returns
   `RawDrain { items, notice }`, where `notice` is a `DrainNotice` — either a
   transient `Reconnecting` or a `Terminal(TerminalNotice)`. The notice comes from
   the same authority the pump writes, so the terminal one read path observed is
   the terminal every other read path reports. Pre-terminal items are delivered
   before it, and `apply`'s terminal transition flushes whatever the pump already
   wrote into the pending queue while `buffer_item` refuses post-terminal writes
   under the same lock, so trailing data behind a terminal notice is impossible.
6. **The binding translates only.** `PyAlloySubscription` owns no lifecycle
   state: `__anext__` and `drain` both funnel through the core's single
   `read_next`/`drain_raw` pair, one `terminal_to_pyerr` maps `Ended`/
   `Unsubscribed` to `StopAsyncIteration` and `Failed` to `RuntimeError`, and
   `__repr__`/`phase` read the published phase instead of inferring one.
7. **The read side is claimed, not checked out.** `claim_read()` hands out an
   RAII `ReadClaim`; a concurrent `__anext__` or `drain` gets the typed
   `ReadInProgress` refusal (`RuntimeError` on the Python side) rather than racing
   the buffer or mistaking a busy reader for closure.
8. **Lock discipline.** Critical sections acquire `lifecycle` -> `pending` ->
   `buffers[*]` and never the reverse, and no lock is held across an `.await`;
   a reader's wait uses `Notify`, which stores a permit rather than a guard. The
   atomics that remain (`active`, `signaled`) are data-plane flags, not authority.

### The intentional behaviour change

A `newHeads` watchdog stall is **no longer a terminal error in Python**. Before
this ADR, a stall buffered a disconnect marker and iteration raised
`RuntimeError: Subscription disconnected: ...` even though the pump then
resubscribed and the subscription continued. Now the stall publishes
`Reconnecting`, iteration keeps awaiting data, and `phase` exposes the transient
state for observability. A driver that wants to observe reconnects reads `phase`;
it cannot use iteration as a liveness clock. This is named in
`a_watchdog_stall_is_a_transient_reconnect_not_a_terminal_end` and
`a_reconnecting_subscription_keeps_the_read_side_open`, and it is the behaviour
the live fork lane certifies against a real reth subscription.

## Consequences

- A terminal is latched once and observed identically by fast iteration, slow
  iteration, bulk drain, `started()`, and `repr`, because there is one place to
  read it. The wrapper's `ended`/`disconnected` pair and the handle's lifecycle
  atomics are deleted rather than kept in agreement.
- `drain()` no longer reports a disconnect and returns to "active" on the next
  call: a terminal phase is absorbing, so every later drain reports it.
- The double buffer keeps only data. `RawSubItem` is `Header`/`Block`/
  `PendingTxHash`/`FullPendingTx`/`Log`; control is out of band.
- A `newHeads` reconnect that never succeeds leaves the handle in
  `Reconnecting`: iteration waits rather than closing, which is the honest
  description of a subscription whose pump is still retrying.
- Tests address the seams the production paths use — `apply`, `drive_items`, and
  `drain_raw` for the core, `drain`/`repr`/`phase`/`started` for the binding —
  and a total (phase × event) matrix pins the transition table, so a new phase or
  event cannot land with undefined cells. The seam tests cannot prove reth
  transport failover; the live fork lane does that.
- Publication ordering is a property of the code, not of a stress test: the
  regression test installs a test-only hook in the state-write/publication window
  and proves a concurrent apply cannot complete there (it blocks on the authority
  lock), so the retained snapshot can never end behind the last transition.

## Alternatives rejected

- **Keep the wrapper's terminal flags and just add a core flag.** Two copies of
  one fact is what produced the divergence in the first place; the failing
  property (a terminal reached through one read path is invisible to another) is
  a property of duplication, not of either copy's correctness.
- **Make `Reconnecting` terminal and let callers resubscribe.** That moves the
  watchdog's whole purpose — surviving a half-open socket without an operator
  restart — into every caller, and turns a 48-second silence into a dead bot.
- **Serialize concurrent reads behind one queue owner instead of refusing.**
  Both are admissible; refusal is chosen because a second concurrent `__anext__`
  on the same handle is a driver bug, and silently interleaving two readers'
  items hides it.
- **Box the item variant of `ReadOutcome`** to shrink the enum. `RawSubItem` is
  large and boxing would allocate once per item read, on the pending-transaction
  path that reads most often; the outcome is moved, never collected.
- **Publish outside the authority lock, or behind a second publication mutex.**
  Neither orders the publication with the state write: the window between the
  write and taking a second lock reorders the same way, and the stale publication
  is exactly the class of bug the single authority exists to prevent.
- **Have `apply` answer with a bool** (or a bool plus an out-param reason). The
  task's typed-refusal contract makes the reason a value: `Terminal` and
  `Inadmissible` are different events for a caller, and a test asserts the
  variant rather than the falsiness of a return.
