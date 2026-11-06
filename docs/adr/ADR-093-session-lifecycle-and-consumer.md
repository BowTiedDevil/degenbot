# ADR-093: The settlement session — a lifecycle-only Driver seam, one ordered Session event stream, one Session consumer

**Status: accepted / implemented** — accepted 2026-11-05 (PM review after a fix
round); every landing slice §6.7 schedules, plus this ADR's own certification
slice (§6.7.6), landed the same day, and §6.8 records each one against the
commit that shipped it. Drafted against
the current code and the ADR-050 / ADR-049 / ADR-075 / ADR-057 / ADR-084 record,
and reviewed against each of §8's three questions before the status flip.

> **Supersession header — read this before the Context.** ADR-050 is
> **partially superseded** (consumer attachment, the driver-resident
> registration lifecycles, and the synchronous `stop()` mechanism; §6.1).
> ADR-075 is
> **superseded by subtraction** — the driver's engine surface is deleted, so
> there is no mirror left to police (§6.2). ADR-049 is **narrowly amended** so
> pose-independent capabilities keep their own core doors beside
> `EngineStages` (§6.3). ADR-041, ADR-055, ADR-018, ADR-022, ADR-057 and
> ADR-084 keep their decisions intact.

## 1. Context

`EngineDriver` is the session: it owns the connection, the backfill, the live
loop, the crawl booted over the same `Bot`, the verify claim table, and the
engine's two delivery channels. It grew one member at a time, and the growth
produced six distinct problems that share one cause — the session's interface
and the session's *data* are separate facts nobody keeps in step.

### 1.1 One shallow interface over six conversations

`rust/crates/engine/degenbot-bot/src/arb_engine/driver.rs` exposes **41
public members**. Grouped by what they actually talk to:

| Group | Count | Members |
|---|---|---|
| adoption / composition | 3 | `new`, `from_stages`, `from_stages_with_hub` |
| pump lifecycle | 4 | `start`, `subscribe`, `resume`, `stop` |
| session-state reads | 6 | `current_phase`, `is_stopped`, `pump_handle_armed`, `hub`, `snapshot_seed_block`, `set_snapshot_seed_block` |
| channel hand-off | 2 | `take_result_receiver`, `take_block_receiver` |
| crawl | 3 | `begin_crawl`, `begin_crawl_with`, `crawl_handle` |
| verify configuration | 2 | `set_verify_rpc_url`, `set_verify_state_view` |
| verify reads | 4 | `pool_id_for_identity`, `is_pool_verified`, `live_verify_claim_count`, `verified_pool_count` |
| registration lifecycles | 4 | `run_registration_lifecycle` (+ `_sync`, + `_with_retry`, + `_with_retry_sync`) |
| path registry | 7 | `register_path`, `register_and_solve_path`, `deregister_path`, `set_path_cap`, `path_count`, `path_dedups`, `path_info_for` |
| retune / inline sim | 2 | `apply_retune`, `set_inline_simulator` |
| results read | 1 | `latest_results` |
| seam access | 3 | `core`, `bot`, `stages` |

Four of the 41 sequence a session. Sixteen forward an engine or core operation
to `EngineStages` or a `bot_core` door without changing it, six more read a
capability's state for the caller, and six report the session's own state. A
consumer that wants a path registered must
hold the object that also owns a WebSocket, and a consumer that wants to verify
one pool must hold the same object — so a capability's reach is bounded by an
unrelated lifecycle's existence. That is the definition of a shallow interface:
the driver's surface is wide, and everything behind it is one hop away.

### 1.2 The pose is a projection; the session's data lives in independent optional latches

The lifecycle truth is one total transition table over 9 poses × 10 events
(`arb_engine/pump_session.rs`): `Detached`, `Connecting`, `Subscribed`,
`Resuming`, `Running`, `Ended`, `Abandoned`, `Stopping`, `Stopped` ×
`SubscribeRequested`, `Subscribed`, `SubscribeFailed`, `SubscribeAbandoned`,
`ResumeRequested`, `ResumeSpawned`, `ResumeAbandoned`, `PumpExited`, `Stop`,
`StopCompleted`. It answers legality and nothing else — by design the events
"carry no payload: the payload moves are the caller's" (`pump_session.rs:110`).

So the session's *data* lives in five independent slots beside the pose, each
`None`/`false` until some caller installs it:

- `pump_handle: Mutex<Option<JoinHandle<()>>>` (`driver.rs:487`)
- `crawl: Mutex<Option<CrawlTeardown>>` (`driver.rs:491`)
- `pump_finished_tx: Mutex<Option<watch::Sender<bool>>>` (`driver.rs:496`)
- `subscribe_state: Mutex<Option<DriverSubscribeState>>` (`driver.rs:501`)
- `result_receiver_taken: AtomicBool` (`driver.rs:530`)

The pose is additionally projected into a *second* owner,
`EngineStages::phase: PhaseCell(Mutex<PumpPhase>)` (`engine_stages.rs:50,141`),
published through `advance_phase`/`publish_phase`. Legal poses and present
payloads are therefore two facts, and every entry point re-establishes their
agreement by hand: `subscribe` checks `pump_handle` and `subscribe_state`
(`driver.rs:918,923`), `resume` checks `result_receiver_taken`
(`driver.rs:1062`), `stop` takes the crawl and the handle
(`driver.rs:1257,1275`). Nothing in the type system says a payload and a pose
must travel together, which is why the next two subsections are possible.

### 1.3 Two live ordering holes

**(a) `resume()` can arm a live loop after `stop()` has landed `Stopped`.**
`resume` (`driver.rs:1046`) lands `ResumeSpawned` at `:1124` — the pose is now
`Running` — then takes the completion sender at `:1130`, spawns the live loop at
`:1131`, and *unconditionally* stores the handle at `:1135`. `stop()`
(`driver.rs:1245`) only returns early when the pose it landed *from* was
already terminal (`:1249`), so a `stop()` that interleaves between `:1124` and
`:1135` runs the whole teardown: it sets the shutdown flag, finds no handle at
`:1275`, logs "not running", lands `Stopped`. The `resume` thread then stores
`Some(handle)` at `:1135` — a live pump armed after the terminal pose, joined by
nobody. A later `stop()` returns `Ok` at `:1249` without joining it. ADR-050
D6's contract ("the WS subscription futures have dropped before return") is
false for every such interleaving, and `pump_handle_armed() == true` on a
`Stopped` session.

**(b) `begin_crawl_with()` accepts a crawl after the session is `Stopped`.**
`begin_crawl_with` (`driver.rs:1186`) guards only "one crawl per driver"
(`self.crawl.lock()` at `:1193`); the two `is_stopped()` guards in the file are
`subscribe` (`:904`) and `resume` (`:1047`). A crawl booted after `Stopped`
therefore installs its `CrawlTeardown` at `:1217`–`:1221` and spawns
`runner.run()`, and `stop()` will never join it: the next `stop()` returns at
`:1249` before the crawl take at `:1257`. The crawl keeps running for the
process's lifetime over a session that is gone, holding
`CrawlHost::driver: Arc<EngineDriver>` (`crawl/controller.rs:658`).

Both holes are the same defect: a payload move that the pose table no longer
admits still installs its payload.

### 1.4 The driver/stages mirror, accepted and then unguarded

ADR-075 accepted the ~15-name mirror between `EngineDriver` and
`EngineStages` — `register_path`, `deregister_path`, `set_path_cap`,
`path_count`, `path_dedups`, `path_info_for`, `apply_retune`,
`set_inline_simulator`, `latest_results`, plus the reads — and two structural
gates policed it. Those gates were deleted under ADR-087 D1/D5, which leaves
the compiler and the one-impl-block census as the only protection. The mirror
is not a defect on its own; it is a defect *in combination with §1.1*, because
the mirrored members are exactly the ones that make the driver wide and the
capability's reach conditional. ADR-075's own reopen condition — "revisit if
drift between the two surfaces starts costing real incidents repeatedly" — is
not what this ADR invokes; this ADR removes one side, which empties the
question without re-litigating it.

### 1.5 Independent block and result clocks

The Python cockpit consumes two streams concurrently: `bot.block_stream()` for
the block clock and the engine's result batches for dispatch
(`src/degenbot/runner/_consume.py:70`–`135`). The pure-Rust example consumes
one stream and derives the clock from batch metadata — `BlockClock::advance`
reads `ResultBatch::solve_block`, and the module doc records the block receiver
as a follow-up (`rust/examples/settlement_bot/src/consume.rs:27`–`33`). Two
drivers, two clock sources, and one of them ("clock from `solve_block`") cannot
express the fact the other one exists to protect: that a block's view was
published before that block's batches.

### 1.6 Duplicated consume loops, duplicated admission choreography

`consume.rs` states its own provenance: it "mirrors
`src/degenbot/runner/_consume.py::consume_result_batches`"
(`consume.rs:1`–`16`). Two implementations of one concept, one of which is the
other's specification — ADR-046's twin-drift class, with a language boundary
supplying the drift.

Admission is duplicated the same way. The crawl unit runs
memo → build → eligibility → verify → register (`arb_engine/crawl/unit.rs:1`–
`30`) over `CrawlHost { bot, driver: Arc<EngineDriver>, io, snapshot }`
(`crawl/controller.rs:654`–`676`); the pure-Rust driver hand-wires the same
pre-registration stages in `RegistrationPipeline::prepare_candidate`
(`examples/settlement_bot/src/pipeline.rs:137`–`241`); Python runs a third
copy in `build_paths.py`. Every one of them reaches verification and the path
registry *through the session object*, so admission — a pose-independent
capability by ADR-022's own terms — cannot run in a process that does not pump.

## 2. Decision

Vocabulary: **module**, **interface**, **implementation**, **depth**, **seam**,
**adapter**, **leverage** and **locality** carry their codebase-design
meanings; **Driver seam**, **Verify lifecycle**, **Registration admission**,
**Session event stream**, **Consumer claim**, **Session consumer**, **Driver
cockpit**, **Session watch** and **Run ritual** carry the GLOSSARY.md
definitions recorded there (§7).

### D1 — The Driver seam is lifecycle-only

`EngineDriver` keeps the session lifecycle and hands every other capability
back to its own home. Its public interface becomes:

```rust
impl EngineDriver {
    // adoption / composition
    pub fn new(bot: Arc<Bot>, cfg: &Arc<BotConfig>) -> Self;
    pub fn from_stages(bot: Arc<Bot>, stages: Arc<EngineStages>) -> Self;
    pub fn from_stages_with_hub(bot: Arc<Bot>, stages: Arc<EngineStages>, attached: HostHub<EngineChannelHandles>) -> Self;

    // the lifecycle: one hand-off, three moves, one total close
    pub fn claim_consumer(&self) -> Result<EventHandoff, ClaimRefusal>;                     // D5
    pub async fn connect(&self, node: NodeEndpoints) -> Result<Connected, ConnectRefusal>;  // Detached -> Connecting -> Subscribed
    pub async fn run(self: &Arc<Self>, claim: ConsumerClaim) -> Result<(), DriverError>;    // Subscribed -> Resuming -> Running
    pub async fn close(&self) -> TeardownReport;                                            // total from every pose

    // observation of the session itself
    pub fn current_phase(&self) -> PumpPhase;
    pub fn is_stopped(&self) -> bool;
    pub async fn wait_session_end(&self) -> SessionEndCause;   // already in `session_end.rs`
    pub fn bot(&self) -> &Arc<Bot>;
    pub fn stages(&self) -> &Arc<EngineStages>;
    pub fn hub(&self) -> &Arc<Hub>;
}
```

`connect` and `run` are the two public lifecycle moves the old `subscribe` and
`resume` names described, and `close` replaces `stop`. `connect(node)` is the
`Detached → Connecting → Subscribed` move — the WS handshake plus the first
complete block — and reports `Connected { link_block: W }` (today's
`subscribe(&self, node_ws: &str) -> Result<u64, DriverError>`, `driver.rs:903`,
already returns that first complete block; the move changes name and report
shape, not protocol). A handshake failure
or a cancelled `connect` lands `Detached` and stays retryable; an `Abandoned`
session refuses it by name. `run(claim)` is the `Subscribed → Resuming →
Running` move: it closes the `S+1..W` gap and then arms the live loop, consuming
the claim that proves a consumer already holds the stream. Each move refuses a
concurrent lifecycle move with a typed in-flight refusal; `close` alone is total
from every pose (D2).

Configuration is not a lifecycle argument. `connect` takes node endpoints and
nothing else: the verify provider and its retry policy are construction values
on the verify home (D4), so no move smuggles configuration and no caller
re-points a live session's provider.

The ordering the pair used to enforce by convention — consumer attached *before*
the live loop arms — is enforced by the hand-off instead: the stream leaves the
driver only together with its claim (D5), and `run` cannot be called without
one. `start`'s separate existence (ADR-050 D2's "stops short of `resume`") has
no remaining reader once the claim is the proof.

Everything else leaves: path registry (7), retune and inline sim (2), results
read (1), verify configuration and reads (6), registration lifecycles (4),
crawl (3, restated by D4), channel hand-offs (2, replaced by D5),
`pump_handle_armed` (the pose answers it: `PumpPhase::live_loop_armed`,
`pump_session.rs:83`), and the snapshot-seed pair (2 — S moves to the `Bot` door
that already owns it, ADR-050 D2). The 41 members in `driver.rs`'s single
`impl EngineDriver` block become 12; with `wait_session_end`, already in its own
`impl EngineDriver` block (`session_end.rs:220`), the seam is 13 members — and
the four that sequence a session are the ones that remain.

**As shipped (§6.8).** The seam carries twenty-one public members: those
thirteen, the three crawl moves D2/D4 keep (`begin_crawl`, `begin_crawl_with`,
`crawl_handle`), and five one-line projections of state another home owns
(`core`, `verify`, `pool_id_for_identity`, `snapshot_seed_block`,
`set_snapshot_seed_block`). None of the five keeps a second copy of anything —
each reads or writes the shared `Bot`, `BotState`, or verify home through that
home's own door — so the decision (no capability state on the seam) stands
while the count moved from thirteen to twenty-one. The snapshot-seed pair's
authority is the shared `BotState` (`Bot::snapshot_seed_block` is the same
read), and the FFI's engine getter goes through the shared core rather than
through the driver. A caller inventory of the five
([driver-seam projection audit](../architecture/driver-seam-projection-audit.md))
leaves them in place: `verify`'s callers hold only the driver because no other
home carries a verify lifecycle (`EngineStages` has no such member), and the
other four each carry a read/write discipline — a `LockSite` claim, and in the
shell the guard-inside-`py.detach` rule — that a caller reaching through
`stages().core()` would have to restate.

### D2 — The pose is private and data-carrying; moves are a serialized take-out / land pair, and no lock is held across an await

One private `PumpSession` value in `degenbot-bot` owns the session:

```text
PumpSession
├─ pose: PumpPose          // each variant OWNS exactly what that pose holds
├─ crawl: Option<CrawlTeardown>
├─ completion: CompletionBroadcast
└─ claim: ClaimState       // Unclaimed | Claimed
```

`PumpPose` carries its payload rather than pointing at a slot beside it: a
`Subscribed` pose owns the pump and the live stream, a `Running` pose owns the
live-loop handle and the completion broadcast, a `Stopping` pose owns the
teardown plan. The table keeps its total answer over (pose, event) — plus the
one event §1.3(b) needs, `CrawlAttached` — 9 poses × 11 events — but legality
and payload are now the same move. `CrawlAttached` is a table event and not a
side guard inside `begin_crawl_with`, because crawl-attach legality must be
answered by the table alone: a guard beside the table makes the table a partial
authority and leaves §1.3(b) to whatever an entry point remembers. The table's
edge-count test moves from 9×10 to 9×11 with it.

The move protocol is two statically ordered steps:

- **take-out.** `PumpSession::take(event) -> Result<SessionMove, PumpDecline>`
  decides legality and *moves the payload out* under the lock, which is then
  released. This is where every await begins; the driver's awaits (the WS
  handshake, the backfill) already sit between a take and a land today, and
  this makes that structure explicit instead of incidental.
- **land.** `SessionMove::land(event, payload)` is the only way to install a
  payload, and it re-acquires the lock and re-checks the pose. A move whose
  payload no longer fits the pose declines. `Drop` publishes the move's abandon
  edge (`SubscribeAbandoned` / `ResumeAbandoned`) — the shape `PhaseLeg`
  already has (`driver.rs:256`–`300`), so a cancelled move never strands the
  session.

**`close()` is the teardown move.** It is asynchronous, total from every pose —
`Detached` through `Running`, both terminals, and while another move is in
flight — idempotent, and shared: concurrent callers join the *one* teardown and each receives the same
typed `TeardownReport` — the from-pose, the pump exit (`Cooperative | Aborted |
JoinError`), the crawl exit, and the delivery closure. When another lifecycle
move is in flight, `close` requests termination and waits for that move to
unwind (its guard lands the abandonment edge) before tearing down. Nothing parks
a runtime: the old `stop()`'s `block_on`-inside-the-runtime hazard disappears
with the synchronous signature, and a caller that must block does it at its own
adapter boundary. The report is *returned*, not a `Result` union — `close` has
no error arm, because it reports what it tore down rather than refusing.

A `Copy` projection (`PumpPhase`) survives for readers — telemetry, the FFI's
name translator, `is_stopped` — and is *derived* from the pose rather than
stored beside it, so the projection and the truth cannot disagree. `PhaseCell`
retires with it.

The two holes close structurally rather than by an added guard:

- **(a)** the live-loop handle is a `Running` payload. A `close()` that beats the
  landing takes the `Resuming` payload (nothing to join yet), requests
  termination, and lands terminal; the landing then declines, and the handle
  drops with its move guard instead of being installed onto a dead session.
- **(b)** crawl attachment is a session move. `CrawlAttached` is refused from
  `Ended`, `Abandoned`, `Stopping` and `Stopped` by the same table, and the
  close move takes the crawl payload under the lock that lands the terminal
  pose — so a crawl cannot outlive the session that owns it, and a second
  `close()` has nothing left to take.

### D3 — `EngineStages` is the direct engine interface; the mirror is deleted

Every engine operation a consumer needs is reached at `EngineStages` (path
registry, retune, inline simulator, results, phase read) or at the
pose-independent core door that owns it (D4). `EngineDriver` stops forwarding,
and the ~15-name mirror disappears — superseding ADR-075 by subtraction, not by
reversal. A consumer that built the seam (`from_stages`) already holds it; a
consumer that took the driver asks `driver.stages()` once and keeps the handle.

### D4 — The Verify lifecycle is pose-independent; Registration admission is one implementation behind two adapters; `CrawlHost` loses its driver back-reference

- **Verify lifecycle.** `run_verify_lifecycle` and `run_registration_lifecycle`
  (`bot_core/registration_lifecycle.rs:96,216`) are already runtime-agnostic,
  pose-free functions over `core`, a `PoolLookup`, and the two verify closures,
  with at-most-once claims held by `PoolVerifications`
  (`bot_core/verify_claims.rs:267`). What the driver adds is retry policy and a
  provider, so the retry loop (`bot_core/verification_retry.rs`) moves beside
  the machine and the *core* owns the bounded retry the glossary already claims.
  Verify configuration becomes a property of the verify home plus the one
  provider the `Bot` owns (ADR-022 D3), not a session poke. Callers configure
  the home directly through its own `config()`/`configure()` door;
  `bot_core::build_verify_provider` is the one provider-construction home and
  refuses a dead endpoint at that door (`Result`, never a swallowed `Option`).
- **Registration admission.** One implementation of the stage order (memo →
  construction → eligibility → Verify lifecycle → registration) lives at
  `arb_engine::crawl::admission`, beside the two adapters it feeds and
  composing the `bot_core` homes it stages — the ledger's mechanical gate is
  `RegistrationLedger::admit_path_with_policy` (`registration_ledger.rs:1323`).
  A `bot_core` home would invert the module direction: the admission composes
  `EngineStages`, which lives in `arb_engine`.
  Two **adapters** feed it: the crawl (`degenbot_bot::arb_engine::crawl`) for
  discovered candidates, and direct registration for a caller that already has
  a candidate path. Neither adapter owns the order, and neither can drift it.
- **The back-reference dies.** `CrawlHost`/`UnitHost` carry what admission
  actually needs — `Arc<Bot>`, `Arc<ConstructionIo>`, the snapshot arm, the
  ledger, the flights, the settings, and the two capability handles (the verify
  claim table and the path registry) — instead of `Arc<EngineDriver>`
  (`crawl/controller.rs`, `crawl/unit.rs`). `degenbot_bot::crawl`
  then runs in a process that never pumps, which is the leverage the
  back-reference was withholding.

### D5 — One ordered Session event stream and one non-cloneable Consumer claim

**The stream.** The engine's two named hub channels
(`RESULT_CHANNEL_NAME` / `BLOCK_CHANNEL_NAME`, `driver.rs:108`–`112`) collapse
into one ordered source:

```rust
pub enum SessionEvent {
    Block(BlockNotification),   // exactly once per epoch, before that epoch's batches
    Batch(ResultBatch),         // zero or more, ascending solve_block
}
```

The ordering rule is in the type, not in a consumer's poll order: **`Block(N)`
precedes every `Batch(N)`**, and a `Batch` never advances a block clock — the
`Block` does. The enum has exactly two arms and **no `End` variant**. Natural
end is the stream's exactly-once *closure*, and the delivery lifecycle stays its
sole owner (`arb_engine/delivery_lifecycle.rs`): the session's pump loop maps
that closure to the typed terminal fact — one per pump exit, stream end, panic,
reorg/shutdown — where a *cause* is needed. The fact rides the session-end
surface (`wait_session_end`, `session_end.rs:223`) and lands `Ended` or the
teardown progression there; it is never a stream item. The stall arm leaves that
vocabulary too: the Session consumer owns the watchdog that detects a stall (D6),
so the verdict is the consumer's own report — a cause the reader produces cannot
arrive on the stream the reader is consuming.

Exactly-once natural end keeps ADR-050 D3's mechanism: the delivery lifecycle is
the one site that closes the stream, and closing it once is what makes the end a
single observation.

The stream carries **no** `Rewind` fact and **no** census event: reorg rewind in
the stream is a rejected alternative (§3, alternative 5), and the pipeline
census is a separate channel by ADR-043/ADR-083.

**The claim.** `take_result_receiver`'s `AtomicBool` latch and `resume`'s
`NoResultReceiver` check become one hand-off carrying both halves:

```rust
pub struct ConsumerClaim(());                       // non-Clone; driver-minted only
pub struct EventHandoff {
    pub stream: SessionStream,                      // the ordered source, handed out up front
    pub claim: ConsumerClaim,
}
pub fn claim_consumer(&self) -> Result<EventHandoff, ClaimRefusal>;
pub async fn run(self: &Arc<Self>, claim: ConsumerClaim) -> Result<(), DriverError>;
```

One hand-off per session: the consumer receives the stream *and* the claim, and
`run` consumes the claim to **start production over the already-handed-out
stream**. `run` returns no stream, so a `SessionStream` in a consumer's hand
before production exists is the type-level proof that a consumer is attached. A
second `claim_consumer` declines with `ClaimRefusal::AlreadyClaimed`; a `run`
without a claim does not compile; and a cloneable receiver can no longer stand
in for the attached-consumer fact. The claim is a lifecycle fact, not a receiver
and not a second legality table.

### D6 — The Session consumer lives above `degenbot-bot`, in `degenbot-batch-executor`

The `Session consumer` is one core module: it takes the stream, folds
`Batch(N)` into `BatchWork`, drives `BatchExecutor`, emits the `BatchOutcome`
records the executor already owns (`BatchOutcomeSet` / `BatchOutcome`,
`degenbot-batch-executor/src/{executor,record}.rs`), owns stall detection and
loud-abort propagation, and closes the driver on failure or termination.

Its home is `degenbot-batch-executor`, which already depends on `degenbot-bot`
(its `Cargo.toml` carries `degenbot-bot`, `degenbot-submission`,
`degenbot-executor`, `degenbot-simulation`) and already owns the per-batch
dispatch choreography. The consumer's whole knowledge of the session is what it
was handed — the `EventHandoff` (stream and claim) plus `close()` — never the
engine, and never an end verdict on the stream it reads: **it feeds stall
detection** from stream activity, and it returns the typed end/outcome report
whose Session-end ranking is the core's own pure fold (below). The block
clock (`consume.rs`'s `BlockClock`) and the progress/heartbeat record
(`SessionProgress`) move down beside it: they are session facts that two drivers
had begun to spell twice.

**Session-end ranking is core-owned and pure.** The gathered end facts — the
detection cause, the translated consumer outcome, the registration outcome, and
the consumer's already-retrieved output — fold through one closed
`SessionEndObservation` and one pure `rank` in `arb_engine::session_end`:

| Priority | Observation | `SessionEndVerdict` | `RaiseSlot` |
|---|---|---|---|
| 1 | registration outcome `Failed` (a task panic included — a panic is never clean completion) | `RegistrationFailed` | `Registration` |
| 2 | consumer outcome `QuietEnd` (the intended consumer end) | `PumpEnded` | `Nothing` |
| 3 | consumer outcome `PumpStopped` (the unexpected session end) | `WatchdogTripped` | `SessionEnd` |
| 4 | a detection cause, without an intended consumer end | `WatchdogTripped` | `SessionEnd` |
| 5 | consumer outcome `Failed`, no higher-priority fact | `PumpEnded` | `Consumer` |
| 6 | the empty remainder (no consumer outcome, no detection cause) | `PumpEnded` | `Nothing` |

Registration failure is maximal; the intended consumer end disarms a
simultaneous detection fact, because the expected close also publishes
`PumpFinished`; the fold is total over every input combination and independent
of fact arrival order — task completion order never re-authors the verdict.
The core module never names `degenbot-batch-executor::ConsumerEnd`: the
translation lives in that crate (the `From` impls on its own `ConsumerEnd` /
`ConsumerReport`) and in the adapters. The fold returns the retained consumer
output, which an adapter delivers at most once — in exactly the one
`RaiseSlot` the fold named — and never raises again over a registration or
session-end failure. Both Session watches become adapters that gather and
retrieve facts, cancel/reap, and render the ranked product; the core fold owns
no collector FSM, no task spawning, no cancellation, and no rendering.

Pulling that loop *into* `degenbot-bot` is rejected (§3, alternative 3): it would drag
`degenbot-submission`, `degenbot-executor` and `degenbot-simulation` into the
engine crate and invert ADR-084 D4's layering. Putting a port trait in
`degenbot-bot` for `degenbot-batch-executor` to implement is rejected (§3, alternative 4):
the stream is already the interface, so no upward edge is needed.

### D7 — Typed operation errors, and public-interface tests

Three of the driver's seven error arms carry prose (`driver.rs:196`–`220`):

| Today | Becomes |
|---|---|
| `SessionState(String)` | `SessionState(SessionGuard)` — a closed enum: `PumpAlreadyStarted`, `SubscribePending`, `NotSubscribed`, `Terminal(PumpPhase)`, `MoveInFlight(PumpPhase)` |
| `Subscribe(String)` | `Subscribe(SubscribeRefusal)` — the transport refusal kept as `source` |
| `Backfill(String)` | `Backfill(BackfillGap { from, to })` — the unapplied range as data |
| `NoResultReceiver` | *deleted as unrepresentable*: the hand-off carries the claim and `run` takes it by value, so "no claim" has no state to name and the `result_receiver_taken` atomic disappears with it. The rule is refused at the hand-off instead, as `ClaimRefusal::AlreadyClaimed` |
| `Registration(PathRegistrationError)` | leaves with the operation (D4); the refusal is raised at its own door |
| `Verify(RefusalKind)` | leaves with the operation (D4); `RefusalKind` stays the one verify vocabulary (ADR-077) |

`Phase(PhaseError)` is already typed and stays. A fatal error stops being a
union of three capability vocabularies plus three strings, and a consumer
matches a variant instead of a message. The two lifecycle moves carry their own
refusals — `ConnectRefusal` for `connect`, `ClaimRefusal::{AlreadyClaimed, …}`
for the hand-off — and `close`'s `TeardownReport` is a report, not an error arm:
there is nothing to refuse about a teardown that is total by construction.

Each refusal is asserted at the seam by a test named for the rule it enforces —
the driver's own suite is that acceptance home today (`driver.rs` tests) and
the same names carry over to the pose value's suite.

### D8 — Hard cutover, no aliases, no parallel implementation

One cut, no deprecated forwarders, no compatibility shim, no feature flag
(AGENTS.md; ADR-084 D7's "hard cutover, not silence"). Every removed member
fails to compile at its former call site, and the change ships with the §6.5
was→is table plus a `rust/CHANGELOG.md` entry. The consumers cut in the same
change: `PyArbEngine` (`rust/crates/shells/degenbot-python/src/bot/engine/`),
`examples/settlement_bot`, and the Python cockpit (`src/degenbot/runner/`).

## 3. Rejected alternatives

1. **A three-entry-point session program** (`subscribe` / `resume` / `stop`
   kept public beside a `run_until_stopped` convenience loop). Rejected: a
   convenience loop on the driver is an *implementation* of dispatch ownership
   living in the engine crate, which D6 places above it; and the ordering rule
   it would have to reproduce (attach before arm) is exactly the fact the claim
   turns into a type. Keeping the three entry points also keeps the two
   ordering holes open, because the payload moves stay the caller's.
2. **Fragmented capability-value surfaces** (handing out `DriverPaths`,
   `DriverVerify`, `DriverCrawl` values from the driver). Rejected: the driver
   stays a bag of capability handles — the same shallow surface with more
   names, no depth gained — and the pose-independence of verification becomes
   *implied* by a handle's existence rather than stated by a door that does not
   mention the session at all.
3. **`EngineDriver`-owned dispatch** (folding `BatchExecutor` driving into the
   session). Rejected: it re-mixes pump-session sequencing with per-batch
   dispatch policy — the two conversations ADR-046 split — and it drags
   `degenbot-submission`, `degenbot-executor` and `degenbot-simulation` into the
   engine crate's dependency set, which is the dependency-charter leak ADR-084
   D3 exists to prevent.
4. **A seventh upward port** (a consumer trait declared in `degenbot-bot` for
   `degenbot-batch-executor` to implement). Rejected: ADR-084 D4 already counts
   six upward edges and permits no new ones; a port is only needed when the
   lower crate must call the upper one, and here the lower crate hands back a
   stream, so the direction stays down with no trait at all.
5. **Unified `Rewind` facts in the first cut** (`SessionEvent::Rewind`).
   Rejected, and *excluded from this ADR's scope*: ADR-041's `Rewind` is the
   stage machine's per-epoch edge, and today's reorg handling is the reorg
   coordinator's journal rollback, not a consumer-visible fact. Neither the
   cockpit nor the example can act on a rewind fact, so emitting one would force
   every consumer to answer a question no consumer asks. It stays out of the
   first cut — not decided, not deferred by halves.
6. **Generic `EngineDriver` / generic stage payloads against one settled-block
   family** (`EngineDriver<S>`, parameterized `StageHandlers` payloads).
   Rejected now: ADR-018's extraction trigger is a second engine family and
   ADR-055 Phase C still schedules parameterized stage payloads; one family is a
   sample of one, and designing the parameter over it would fix a shape nothing
   has exercised. The trigger stays where ADR-018/ADR-055 put it.
7. **Keeping the status quo** (the mirror plus the two channel hand-offs).
   Rejected: it is only coherent while verification and registration are the
   driver's to sequence, and §1.6 shows they are not — a pose-independent
   capability reached through a session object is reach by accident.

## 4. Consequences

**The session becomes deep.** A caller learns twenty-one public members instead
of forty-one — thirteen lifecycle moves, the three crawl moves, and five
one-line projections of shared state (D1, as shipped) — and the sixteen
forwarded operations move to the seam that performs them, where they are
already the interface. Locality improves in the
opposite direction too: a failure in path registration no longer names the
session, and a session failure no longer surfaces as a registration error arm.

**The two ordering holes close structurally.** No new guard is added; the pose
carries its payload, so a move the table does not admit cannot install one.
ADR-050 D6's ordering contract ("the subscription futures have dropped before
the pump is gone") becomes a property of the landing and of `close()`'s
teardown, rather than of the interleaving.

**Close is total, asynchronous and shared.** `close()` has no refusal arm: it
tears down from every pose, joins concurrent callers onto one teardown, and
returns the typed `TeardownReport` that says what it actually tore down. The
`stop()` parking hazard (`block_on` inside the runtime) disappears with the
synchronous signature; a caller that must block waits at its adapter boundary.

**One stream, one clock, one end, one verdict.** `Block(N)` before `Batch(N)` is
a type guarantee; exactly-once natural end is the stream's closure; one typed
terminal fact per pump exit lands `Ended` or the teardown progression on the
session-end surface; and the stall verdict is the Session consumer's own report,
so no verdict has to travel back through the stream that produced it. The
two-clock class of bug (a frozen `[block: N]` behind a debounced batch) is
unrepresentable.

**One consume loop and one admission choreography.** The example and the
cockpit become adapters over `SessionConsumer`, closing ADR-046's twin-drift
class across the language boundary; crawl discovery and direct registration
become adapters over one admission implementation, so a rule learned in one
holds in the other by construction.

**Costs and risks.**

- The cut removes public members two consumer surfaces use today, so
  `PyArbEngine`'s FFI surface and its generated stubs move in the same change
  (ADR-066; `just gen-stubs` is part of it) and Python callers see a breaking
  `0.6.0-alpha.*` change.
- The pose value is a new internal seam. Its tests must be ported from the
  driver suite without re-asserting the shape being retired (ADR-087).
- A take-out/land discipline is only as good as its payload typing: a payload
  installed outside `land` re-opens §1.3. That is why D2 requires a
  *data-carrying* pose rather than a pose plus slots, and why the first landing
  slice (the private pose) ships before any public cut.
- Telemetry labels (ADR-043) and the pipeline census vocabulary (ADR-083) are
  byte-identical across the cut: the stream carries products, and the census
  stays the only place a metric is named.

## 5. Capability homes and the dependency charter

`docs/capabilities.md` gains rows only for homes that resolve to real code
today (ADR-084 D1's catalog gate), and both halves of that gate are live. Every
row's crate path must resolve, and every backticked door-item token of any case
— a bare function name (`run_verify_lifecycle`) or a qualified path
(`sim::registry::PoolEntry`, the checker's own documented example) — must
resolve to a declared `pub` item or a `pub use` re-export in the crate it names
(`assert_catalog_door_items_resolve` over `resolve_door_item`,
`rust/crates/facade/degenbot/tests/architecture_gates.rs`). ergo **6OABWT**
closed the case filter that once skipped lowercase function doors, so a broken
lowercase door item now fails `just check-capability-catalog`: the rows below are
gate-checked claims rather than this ADR's word alone.

| Capability | Home | Door items | Row added |
|---|---|---|---|
| Driver seam | `degenbot_bot::arb_engine` | `EngineDriver`, `DriverError`, `PhaseError`, `PumpPhase`, `PumpEvent`, `PumpDecline` | yes |
| Verify lifecycle | `degenbot_bot::bot_core` | `run_verify_lifecycle`, `run_registration_lifecycle`, `PoolVerifications`, `VerifyClaims`, `RefusalKind` | yes |
| Registration admission | `degenbot_bot::bot_core` + `degenbot_bot::arb_engine::crawl` | `RegistrationLedger`, `PathAdmission`, `CrawlPathPolicy`, `CrawlLedger`, `BuildFlights` | yes |

Three capabilities were recorded here without catalog rows while their homes did
not exist yet. All three have landed, so they are gate-checked catalog rows
rather than promises:

| Capability | Home | Door items | Row added |
|---|---|---|---|
| Session event stream | `degenbot_bot::arb_engine` | `SessionEvent`, `SessionStream` | yes — the Driver seam row |
| Consumer claim | `degenbot_bot::arb_engine` | `ConsumerClaim`, `EventHandoff`, `ClaimRefusal` | yes — the Driver seam row |
| Session consumer | `degenbot_batch_executor` | `SessionConsumer`, `ConsumerPolicy`, `SessionClock`, `ConsumerReport`, `SessionReport` | yes — the Batch execution row |

**Dependency charter (ADR-084 D3): no rule widens.** The charter's live rules are
`* : pyo3 zstd ; except degenbot-python`. `degenbot-batch-executor` already
declares `degenbot-bot` (its `Cargo.toml`), and `SessionConsumer` adds no pyo3
and no C-build dependency; the relocation moves code *down* the existing edge.
No per-crate rule needs a new line, and nothing is whitelisted: if the consumer
slice discovers it needs a dependency the charter forbids, the leak is fixed
rather than the charter widened, and this ADR is amended with the rationale.
As shipped the charter holds unchanged — `just check-dependency-charter` passes
over the landed `SessionConsumer` with no per-crate line added — and
`just check-capability-catalog` resolves every row above.

## 6. Migration

### 6.1 ADR-050 — partially superseded

- **D3's consumer-before-resume ordering** (the `take_result_receiver` latch and
  `DriverError::NoResultReceiver`) is superseded by the `EventHandoff` and its
  Consumer claim (D5). The *invariant* — a consumer is attached before
  production starts — is preserved and strengthened; the mechanism changes, and
  `NoResultReceiver` is deleted with its atomic rather than retyped (§6.5).
- **D4's lifecycle bullets** — the four `run_*_registration_lifecycle` members
  leave the driver for their core door (D4 above). ADR-022's choreography is
  preserved exactly; the retry policy it left consumer-side moves with it, and
  the provider and that policy become construction values (D1).
- **D6's mechanism** — a synchronous `stop()` mirroring `BotRunner.shutdown` /
  `__aexit__` — is superseded by `close()` (D2): asynchronous, total from every
  pose, idempotent, shared by concurrent callers, and reporting one typed
  `TeardownReport`. D6's *ordering contract* survives verbatim as the Session
  consumer's close rule: stop the pump first, then the stream ends naturally,
  then the consumer ends and is joined/cancelled. Only the synchronous mechanism
  goes, and no `stop` alias survives the cutover (D8).
- **D2's snapshot-seed bullet is honored, not superseded**: S is a shared
  `BotState` fact, so the accessor pair leaves `EngineDriver` for the `Bot` door
  that already owns it (§6.5).
- **D2/D7 (the pre-pump ritual, the PyO3 re-parent)** are preserved under new
  names: `connect` performs the handshake plus the first complete block, `run`
  performs the backfill and arms the live loop, and `PyArbEngine` remains an
  adapter over the same driver.

### 6.2 ADR-075 — superseded by subtraction

The driver's engine surface is deleted (D1/D3), so there is no mirror to police.
ADR-075's judgement about the mirror at the time it was recorded stands
unchallenged; this ADR removes the second surface instead of re-opening the
first. The reopen condition ("repeated incident cost from drift") is not
invoked.

### 6.3 ADR-049 — narrowly amended for pose-independent capabilities

ADR-049 D2's "every external consumer crosses the stage surface" keeps its
meaning for engine *operations*. Pose-independent capabilities — Registration
admission and the Verify lifecycle — keep their own `bot_core` doors, reachable
without the stage seam and without a session. ADR-049 D1's one-door engine
invariant is untouched: `ArbitrageEngine` stays `pub(crate)` with one inherent
impl block, and no engine type becomes public.

### 6.4 ADR-057 — the Session boot stays loop-free

The strategy host still boots drivers and still runs no consume loop; the
settlement pump arm's loop moves from the Python cockpit and the example into
`degenbot-batch-executor` (D6), not into the boot. This ADR closes only the
seam half of ADR-057's recorded R-4 residual ("folding the engine session into
a host-owned `Live` sub-state"), which stays open on the host side.

### 6.5 The complete public was → is table

| Surface | Was | Is |
|---|---|---|
| consumer attachment | `take_result_receiver() -> Option<NamedReceiver<ResultBatch>>` and `take_block_receiver() -> Option<NamedReceiver<BlockNotification>>`, two once-only hand-offs latched by `result_receiver_taken` | `claim_consumer() -> Result<EventHandoff, ClaimRefusal>` — one hand-off carrying the ordered `SessionStream` and the `ConsumerClaim`; then `run(claim)` starts production over that stream |
| stream item | `ResultBatch` on one channel, `BlockNotification` on another | `SessionEvent::{Block, Batch}` — two arms, and no end item |
| start ordering | `subscribe()` … `resume()` with the consumer attached between them by convention | `connect(node)` → `Connected { link_block }`, then `run(claim)`; each move refuses a concurrent one, and the claim is the attachment proof |
| close | `stop() -> Result<(), DriverError>` — synchronous, `block_on` inside the runtime | `close() -> TeardownReport` — asynchronous, total from every pose, idempotent, shared; the report is *returned*, not a `Result` union |
| resume gate | `DriverError::NoResultReceiver` — deleted with its atomic as unrepresentable | a second `claim_consumer()` declines with `ClaimRefusal::AlreadyClaimed`; `run` cannot be called without a claim |
| session end | natural end indistinguishable from a dropped channel; the stall verdict published on `SessionEndCause` beside the crate-level heartbeat; each driver ranked its own end verdict locally | the stream's exactly-once closure, plus one typed terminal fact per pump exit on the session-end surface; one core pure fold (`arb_engine::session_end::rank` over `SessionEndObservation`) returns the `SessionEndVerdict` + `RaiseSlot` + retained consumer output, and both drivers render that product |
| snapshot seed | `EngineDriver::{snapshot_seed_block, set_snapshot_seed_block}` | `Bot::snapshot_seed_block` over the shared `BotState` fact it already owns (ADR-050 D2); the driver keeps a derived read/write pair over that same fact, which no consumer routes through (the FFI's engine getter reads the shared core) |
| path registry | `EngineDriver::{register_path, register_and_solve_path, deregister_path, set_path_cap, path_count, path_dedups, path_info_for}` | `EngineStages::{…}` (same names) |
| retune / inline sim | `EngineDriver::{apply_retune, set_inline_simulator}` | `EngineStages::{apply_retune, set_inline_simulator}` |
| results read | `EngineDriver::latest_results()` | `EngineStages::latest_results()` |
| verify configuration | `EngineDriver::{set_verify_rpc_url, set_verify_state_view}` | the verify home's own `config()`/`configure()` door (`VerifyLifecycle`), over `bot_core::build_verify_provider` for the one provider (ADR-022 D3) |
| registration lifecycles | `EngineDriver::{run_registration_lifecycle, _sync, _with_retry, _with_retry_sync}` | `bot_core::{run_registration_lifecycle, run_verify_lifecycle}` over the core-owned retry policy |
| verify reads | `EngineDriver::{pool_id_for_identity, is_pool_verified, live_verify_claim_count, verified_pool_count}` | `PoolVerifications::{is_verified, live_claim_count, verified_pool_count}`; the identity join stays `EngineDriver::pool_id_for_identity`, a derived `BotState` read whose readers are the FFI identity adapter and the driver's own tests |
| live-loop health | `EngineDriver::pump_handle_armed()` | `PumpPhase::live_loop_armed` on the pose projection |
| crawl | `EngineDriver::{begin_crawl, begin_crawl_with, crawl_handle}` reachable through `CrawlHost::driver` | a crawl attach session move plus its handle; `CrawlHost` holds the capability handles it needs, never `Arc<EngineDriver>` |
| pose storage | `PumpPhase` stored in `EngineStages::phase: PhaseCell` beside five payload slots | a private data-carrying `PumpPose`; `PumpPhase` is a derived `Copy` projection |
| errors | `DriverError::{SessionState(String), Subscribe(String), Backfill(String), Registration(_), Verify(_)}` | `DriverError::{Phase(PhaseError), SessionState(SessionGuard), Subscribe(SubscribeRefusal), Backfill(BackfillGap)}` |
| the mirror | ~15 names on both `EngineDriver` and `EngineStages` | one surface each: the seam for operations, the driver for the lifecycle |
| consume loops | `examples/settlement_bot/src/{consume,dispatch}.rs` and `src/degenbot/runner/_consume.py` | `degenbot_batch_executor::SessionConsumer`; both become adapters |
| block clock | Python's forwarded `newHeads` tick vs the example's `ResultBatch::solve_block` | the stream's `Block(N)` |
| admission | the crawl unit's order, `RegistrationPipeline::prepare_candidate`, and `build_paths.py` | one `bot_core` implementation behind the crawl and direct-registration adapters |

### 6.6 Consumer migration notes

**The public was → is table.** The eight rows a consumer migrates by, with the
retired name in the "was" column only (the same rows the `CHANGELOG` carries):

| # | Was | Is |
|---|---|---|
| 1 | consumer attachment: `take_result_receiver` / `take_block_receiver`, two once-only channel hand-offs | the Session event stream plus the Consumer claim: `claim_consumer()` → `EventHandoff`, then `run(claim)` starts production over the stream |
| 2 | `subscribe` → `resume` → `stop` | `connect` → `run` → `close`: two serialized moves with typed refusals, and one asynchronous total close that returns the shared `TeardownReport` |
| 3 | driver engine mirrors: the nine forwarding members (`register_path`, `register_and_solve_path`, `deregister_path`, `set_path_cap`, `path_count`, `path_dedups`, `path_info_for`, `apply_retune`, `set_inline_simulator`, `latest_results`) | `EngineStages` directly: take `driver.stages()` once and keep that handle |
| 4 | verify method matrix: the verify-config setters, the four verify reads, and the four `run_*_registration_lifecycle` wrappers | one Verify lifecycle — `VerifyLifecycle` in `degenbot_bot::bot_core` with its provider and bounded retry as construction values — plus `PoolVerifications` for the claim facts and `bot_core::{run_verify_lifecycle, run_registration_lifecycle}` |
| 5 | hand-wired registration: the crawl unit's stage order, `RegistrationPipeline::prepare_candidate`, and `build_paths.py` | one Registration admission (`arb_engine::crawl::admission::RegistrationAdmission`) behind the crawl and direct-registration adapters |
| 6 | shell consume loops: `examples/settlement_bot/src/consume.rs`'s loop and `src/degenbot/runner/_consume.py`'s loop | the Session consumer (`degenbot_batch_executor::session::SessionConsumer`); both shells keep only the display drain and the report rendering |
| 7 | `ConsumerError` with no outcome class: a `resume`/`run` refusal read as a leaf failure, which on the Python surface made every `SessionConsumerError` wear the loud-abort prefix | `ConsumerError { kind: ConsumerErrorKind, .. }` — `NoSession` / `ClaimSpent` / `AlreadyAttached` / `DriverRefusedTheMove` / `LeafFailure`, stamped only where the core raises the error; the Python `SessionConsumerError` maps the four refusal kinds prefix-free with the refusal's own text, only the leaf failure under the prefix, and carries the classification as its typed `kind` attribute (`None` for an adapter-local slot refusal) |
| 8 | `SessionEventSource` — one name for two roles: the batch-executor's consumer-input trait and `degenbot-bot`'s event producer | `EventFeed` for the input trait (`degenbot_batch_executor::session::EventFeed`); the bot's producer keeps `SessionEventSource` and the FFI-private origin enum is `EventOrigin` — three roles, three names |

- **`PyArbEngine`** keeps the lifecycle adapters the cockpit calls — `connect`
  (the `Detached → Subscribed` move, returning the link block), `start` (this
  shell's own one-call ritual: `connect` plus the verify home's configuration),
  `close` as an awaitable returning the rendered teardown report, and
  `session_end_future` (the core detection fact) — and loses the lifecycle/path
  forwarders; its `map_path_registration_err`/verify mappers re-point at the
  doors that now raise those refusals. The hand-off is not a pymethod:
  `SessionConsumer.attach(engine)` takes the engine's one `EventHandoff`, and
  the consumer's own `resume`/`run` are the session moves. Python awaits
  `close()`; the FFI blocks, where it must, at its own boundary and never parks
  the core runtime. The generated stub set moves in the same change (ADR-066),
  and the FFI stays private (ADR-013).
- **`examples/settlement_bot`** deletes `consume.rs`'s loop and `dispatch.rs`'s
  seam plumbing, keeps the resolver/receipt-probe adapters, and drives
  `SessionConsumer`; `run_loop.rs` keeps the teardown ordering and awaits
  `close()`, and `session_watch.rs` is the Session-end ranking's ADAPTER — it
  gathers the facts, retrieves the consumer output on every path, translates
  `ConsumerEnd` (in `degenbot-batch-executor`), and renders the core fold's
  verdict + raise slot.
- **The Driver cockpit** (`src/degenbot/runner/`) reduces to an adapter: the Run
  ritual claims the hand-off, connects, and runs, `_consume.py` keeps the
  display drain and the report rendering (its loop and the block-clock mutator
  it fed are gone), and `_session_watch.py` gathers the task facts and renders
  the core's ranked verdict + raise slot (over `session_end_rank`, the `_ffi`
  door) instead of racing two tasks into a local task-completion table.
- **The dict boundary after the cut-over.** Exactly two shapes cross the Python
  seam as dicts: a *scripted* session's ordered events (`block` / `batch`, the
  engine result-channel shape, accepted only by `scripted()` — the live door
  attaches the engine's hand-off in Rust, so the production path crosses no
  dict) and the drained submit-lane records, decoded into
  `degenbot.dispatch`'s typed submit records. The seam's other product is typed
  pyclasses: the outcome records, both verdicts, and the `report` counts the
  exception carries. Translation is the adapter's whole share —
  `payload_row_from_dict` parses a caller's payload dict into a `PayloadRow`,
  and the projection back into the engine's `SimulatedPathResult` is the core's
  own (`degenbot_batch_executor::session::simulated_path`, the pinned inverse of
  `payload_row`) rather than a second field list maintained in the shell.
- **One FFI error class, a classified mapping.** `PySessionConsumer` raises one
  exception type for two different facts — an abort and a refusal both become
  `degenbot.dispatch.SessionConsumerError` — and that stays: the class is the
  pipeline's error, not the leaf's. What the mapping no longer does is report a
  refusal AS a leaf failure. The classification is the core's, stamped at the
  one site that raises the error (`ConsumerError.kind`): `NoSession` from
  `SessionConsumer`'s `unattached` constructor, `ClaimSpent` from `claim_spent`,
  `AlreadyAttached` from `already_attached` (the second-attach refusal),
  `DriverRefusedTheMove` from the driver-refused arm of `resume`, and
  `LeafFailure` from the two consume-loop sites that surface a stored pipeline
  failure. The shell reads that kind and re-classifies nothing, so a refusal
  cannot be relabelled downstream. The mapping splits on it: the three refusal
  kinds carry the refusal's own text with NO prefix — an unattached `resume()`
  reads `the session consumer has no attached session to resume`, a second
  `resume` reads `the consumer's claim was already used; resume refused`, and a
  driver refusal reads the pump table's own decline — while `LeafFailure` keeps
  the executor's shared loud-abort prefix (`sim-submit-pipeline leaf task failed
  - aborting the consumer loudly: `) with the leaf's detail under it. The
  prefix string has one home, `submission::sim_pipeline`'s `pub(crate)`
  constant, shared by the pipeline's own re-raise and this mapping. The class
  carries its classification as the typed `kind` attribute (a `ConsumerErrorKind`
  member, exported as `degenbot.dispatch.ConsumerErrorKind`) for every
  core-stamped refusal or failure, `None` for an adapter-local slot refusal, so
  a caller reads the shape instead of parsing the text; `batch_index`/`report`
  stay uniform across every shape.
- **The one shared attach rule, and the deliberately stricter Python slot.**
  `SessionConsumer::attach` is a typed move: it returns `Result<(),
  AttachRefusal>`, and `AttachRefusal { error, handoff, driver }` hands the
  un-installed values back, so a refusal consumes nothing by construction — the
  refused hand-off and driver attach to another consumer unchanged, and the
  first session keeps working. `attach_with(driver, acquire)` is the
  transactional form for a caller that takes a hand-off from somewhere else:
  admission is checked FIRST and the acquisition closure runs only when
  attachment can proceed, so the shell's `engine.take_handoff()` cannot run for
  an already-attached consumer and installation after acquisition has no
  refusal path. Its `AttachFailure` keeps the stages distinct — `Refused`
  (the core's `AlreadyAttached`, the closure returned UN-invoked) versus
  `Acquisition` (the caller's own error, passed through untouched) — and
  `ConsumerErrorKind::AlreadyAttached` is stamped at the core's
  `already_attached()` constructor — the ONE stamping site, read (never
  re-stamped) by the shell. `attach_admissible()` and `already_attached()` are
  private admission internals: no shell-side pre-check survives, because the
  core orders the check itself. The Python `ConsumerSlot` keeps
  exactly the lifecycle rules the core does not model — one in-flight move at a
  time, terminal `Spent`, and the scripted origin's attach refusal — and is
  therefore DELIBERATELY stricter than the core: the slot refuses
  attach-after-run while core `attach` admits a fresh hand-off once `run`
  consumed the session. The asymmetry is not a divergence to close: the slot
  guards the cockpit's calling conventions (GIL-tied moves), the core guards
  session legality. Every refusal at the seam raises the one
  `SessionConsumerError` — `kind=None` for the adapter-local ones — so the
  shared class is the whole refusal surface.

### 6.7 Landing order

The order is the epic's dependency order, not the order of the defects' severity:
the capability doors settle first, the session's interior is rewritten on top of
them, and each public surface cuts whole in its own slice — `EngineStages` in
§6.7.3, the lifecycle in §6.7.4 — with certification last, so no consumer is
ever left driving a half-migrated seam. Each slice lands green on the one before
it; all six have landed, and §6.8 pairs each one with the commit that shipped
it.

1. **This ADR** — ergo `O5RI26`. The record is accepted before any dependent
   slice opens.
2. **Verify lifecycle, Registration admission, and the `CrawlHost`
   back-reference removal** — ergo `MKXFYI`. The pose-independent capability and
   the one admission implementation arrive first, behind the crawl and
   direct-registration adapters (D4).
3. **The `EngineStages` direct cutover** — ergo `KKUMXS`. Every engine caller
   moves to the seam and the driver's forwarders delete (D3).
4. **The private pose, the Session event stream, the Consumer claim, and
   connect/run/close** — ergo `CGXZYJ`. The session's interior becomes
   data-carrying and closes §1.3(a) and (b), the two channel hand-offs collapse
   into one ordered source with its claim, and the three lifecycle moves land
   (D1, D2, D5).
5. **The Session consumer** — ergo `ZJZU27`. `SessionConsumer` lands in
   `degenbot-batch-executor`; the example and the cockpit become adapters (D6).
6. **Certification and the final public cut** — ergo `BGZQFG`. The was→is table
   executes to the letter, stubs regenerate, both first-class consumers are
   certified, and the `CHANGELOG` carries the breaking change (D8).

**Considered and rejected: pose-first.** An earlier order ran the slices the
other way — the private pose first (closing the two ordering holes soonest), then
the ordered source and the claim, then the Session consumer, then the
admission/verify relocation, with the public cut last. Its virtue is real: the
two live holes in §1.3 die in slice one. But it enlarges the driver's interior
churn while the `CrawlHost` back-reference and the pure-Rust admission twin still
reach into that same object, so the first slice would have to carry the pose
rewrite *and* re-point every reach-through caller. The confirmed foundation-first
order settles those callers first and shrinks each slice's blast radius instead.

### 6.8 Landing record

Every slice §6.7 schedules landed green on the slice beneath it. Each row names
the ergo task and the commit that shipped it:

| Slice | ergo | Commit | What shipped |
|---|---|---|---|
| §6.7.1 — this ADR | `O5RI26` | `1db7332ab` (with `ac56bf7f0`, the truthful-lifecycle doc chunk) | the record, plus the capability and lifecycle claims the code answers |
| §6.7.2 — Verify lifecycle, Registration admission, `CrawlHost` back-reference | `MKXFYI` | `3e4d7f6d6` | one `VerifyLifecycle` home (`bot_core::verify_lifecycle`), one `RegistrationAdmission` behind the crawl and direct-registration adapters, `CrawlHost { admission, snapshot }` with no driver handle |
| §6.7.3 — the `EngineStages` direct cutover | `KKUMXS` | `74a724170` | every engine caller at the seam, the nine-name mirror deleted, ADR-075 superseded in the same change |
| §6.7.4 — private pose, Session event stream, Consumer claim, connect/run/close | `CGXZYJ` | `b59d46d4a` | the data-carrying pose machine (9 poses × 11 events), `SessionEvent`/`SessionStream`/`ConsumerClaim`/`EventHandoff`/`claim_consumer`, `connect`/`run`/`close`, the shared `TeardownReport`, typed refusals, and the total idempotent close |
| §6.7.5 — the Session consumer | `ZJZU27` | `80a9e8b45`, `f350c36db` | `degenbot_batch_executor::session` with the shared event-sequence oracle (`tests/standalone_parity/fixtures/session_consumption.json` and its Rust and Python halves); the pure-Rust example and the Python cockpit become adapters |
| §6.7.6 — certification and the final public cut | `BGZQFG` | this change | this ADR's status and landing record, the `CHANGELOG`'s public was→is rows, both consumers' docs, regenerated stubs, and the full gate battery |

Two details the shipped surface settled differently from the decision sketches;
both are recorded rather than re-litigated:

- **`connect`'s signature.** It takes the WS endpoint (`connect(&self, node_ws:
  &str)`) and reports `Connected { link_block }`; D1's `NodeEndpoints` sketch was
  shorthand written before the slice. The intent is unchanged — the move carries
  no configuration, because the provider and its retry policy are construction
  values on the verify home.
- **The seam's member count.** Twenty-one, not thirteen (D1, as shipped): the
  thirteen lifecycle members, the three crawl moves, and five one-line
  projections of shared state.

**Certification evidence.** The accepted gate set ran in order on this tree:
`just test` (lint-rust-check's sub-gates including `check-capability-catalog`
and `check-dependency-charter`, the retirement-tripwire ratchet, 5382 nextest
tests, the release-profile cargo suite, 3232 Python tests, and the C5 consumer
probes), `just test-rust` (306 cargo test suites plus the standalone-consumer
smoke), `just test-flake-probe` (3/3 runs of the two ordering-sensitive Python
suites, 47 tests each), `just test-fork` against the reth node this environment
declares (`just node-identity` → `reth/v2.10.0-f5192ac`, chain 1), and
`just pre-push` (every pre-push hook, the whole-tree battery).

The targeted evidence was repeated rather than sampled once:

| Suite | Repeats | Result |
|---|---|---|
| `degenbot-bot` session selection — `arb_engine::{tests::session_lifecycle, driver::tests, pump_session::tests, delivery_lifecycle::tests, session_end::tests}`, 69 tests | 20 | 20/20 green |
| the shared event-sequence oracle, Rust half (`binary(session_consumption)`, 5 tests) | 10 | 10/10 green |
| the shared event-sequence oracle, Python half (`test_session_consumption_gate.py`, 2 tests) | 10 | 10/10 green |
| the Python cockpit's session suite (`test_arbitrage_session.py`, 39 tests) | 10 | 10/10 green |

`just test-fork`'s Rust live fork-truth legs ran 11/11 green against that node.
Its Python `online_rpc` legs left five failures and one setup error, all outside
this change's surface and recorded here rather than papered over: two
`tests/test_anvil_fork.py` pinned-block assertions expect `20987963` where the
installed anvil 1.8.7 reports `20987964`; one anvil spawn timed out against the
rate-limited Base endpoint (dRPC free tier, HTTP 429) and took a Base fork test's
setup with it; one Base construction test lost a required read to the same
upstream limit; and
`tests/provider/test_provider_interface.py::TestForkProvider::test_fork_provider_delegates_to_eth_namespace`
asserts an `AlloyProvider.chain_id` attribute the class does not expose.

Certification also found one generated-artifact gap and closed it through the
owning pipeline: `SessionConsumerError` is exported on
`degenbot._ffi.simulation` but carries no `_PYO3_DEF`, so the introspection
chunks never described it and `just lint-stubtest` reported it missing from the
stub. The fix is its declaration in the stub generator's exception manifest
(`rust/tools/stubgen/src/exception_manifest.rs`) — the ADR-066 D3 pattern —
followed by `just gen-stubs`; no stub was hand-edited.

## 7. Vocabulary

GLOSSARY.md is canonical for every term below; this ADR uses them in the
recorded sense and records nothing new:

- **Driver seam** — the arb engine's session-lifecycle interface: consumer
  attachment, connect/run/close, crawl attachment, session-end observation.
  Engine operations cross `EngineStages` directly and the Verify lifecycle is
  pose-independent. Distinct from a Strategy driver and a Pending-transaction
  driver.
- **Verify lifecycle** — the pose-independent per-pool capability: quarantine,
  seed verification, drain, post-drain verification, live, with its
  block-resolution, config-gating policy, and bounded retry, core-owned.
- **Registration admission** — the core path a candidate takes to registration:
  memoization, construction, eligibility, Verify lifecycle, registration — one
  implementation, with crawl-discovery and direct-registration adapters;
  benign-skip and loud-abort semantics preserved.
- **Session event stream** — the single ordered data-plane drain of one session:
  `Block(N)` precedes every `Batch(N)` and no batch carries an end item; natural
  end is the stream's exactly-once closure, delivered by its one owner, with the
  typed terminal fact on the session-end surface. Engine products only — no
  census events, no rewind fact.
- **Consumer claim** — the non-cloneable proof that a session's event consumer
  was attached before production started: it travels in the `EventHandoff`
  beside the stream, and moving it into `run` consumes the right to start that
  session's event production.
- **Session consumer** — the core owner of a session's consume loop above
  `degenbot-bot`: folds the stream into executor work, drives `BatchExecutor`,
  emits Batch outcome records, feeds stall detection from stream activity,
  owns loud-abort propagation, closes the driver on failure or termination, and
  returns the typed end/outcome report the *Session-end ranking* folds. Pure
  Rust and Python are adapters over it.
- **Driver cockpit** — the Python companion presenting one session behind the
  `BotRunner` lifecycle; session consumption and end detection are core-owned.
- **Session watch** — the Driver cockpit's adapter over *Session-end ranking*:
  it owns the typed watch-set, cancellation/reaping, and teardown, then renders
  or propagates the ranked verdict. Detection and ranking are core-owned; the
  watch never infers a verdict from which task happened to finish first.
- **Session-end ranking** — the core's deterministic fold of typed session-end
  causes, consumer outcomes, and registration outcomes into `PumpEnded`,
  `RegistrationFailed`, or `WatchdogTripped` (registration failure highest
  priority), plus the one `RaiseSlot` a primary failure may be delivered in.
- **Run ritual** — the cockpit's thin startup adapter over the Driver seam and
  the Session consumer; it owns no consume choreography.

## 8. Review before acceptance

Acceptance required the review the ADR's own claims stand on. It was performed
before the status flip and is recorded here because an amendment re-opens it:

- **Against the code.** Re-read §1's evidence — 41 members, the five payload
  slots, both ordering holes with their line anchors, the mirror, the two
  clocks, the two consume loops, the two admission choreographies — against
  `arb_engine/{driver,pump_session,engine_stages}.rs`, `arb_engine/crawl/`,
  `bot_core/registration*.rs`, `degenbot-batch-executor/`,
  `examples/settlement_bot/`, and `src/degenbot/runner/`. A stale claim is a
  defect in this ADR, not a detail to absorb.
- **Against the rejected designs.** Each of §3's seven alternatives must still
  be rejectable on its stated reason. An alternative that becomes acceptable
  under D1–D8 is a hole in the decision, not a preference.
- **Against the supersessions.** §6 must name every ADR whose decision this one
  changes, and no other ADR's decision may be moved by it.

That review is complete for this record. §6.7.1 is this ADR; §6.7.2 — Verify
lifecycle, Registration admission, and the `CrawlHost` back-reference removal —
is the first implementation slice and is internal, so it can open now. Slices
§6.7.3–§6.7.5 wait for the acceptance of the migration table in §6.5, and
§6.7.6 certifies both first-class consumers.
