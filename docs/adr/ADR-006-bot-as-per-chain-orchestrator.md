# ADR-006: Bot as the per-chain orchestrator (unified state, RPC ownership, Engine-as-EventSink)

**Status: accepted (decisions recorded; implementation pending).** Recorded during the
unify-the-two-Bots grilling, June 2026. Revises the "Rust Core" enumeration in
ADR-005 and resolves ADR-005's deferred "UniswapEngine lock unification" item, and
supersedes the ADR-003 arrangement where the engine held its own `Arc<Mutex<Bot>>`.
Implementation is a separate body of work; this ADR records the settled shape only.
The `EventSink` / `on_block` interface signature is intentionally *not* specified here —
see "Deferred" — only the topology is decided.

## Context

ADR-005 (Polars-Inspired Three-Layer Architecture) left two things open that turned out
to be load-bearing:

1. **Two `Bot` instances.** `PyBot` holds `Arc<parking_lot::RwLock<Bot>>` (instance A —
   the library session, what `PyLiquidityPool`/`PyErc20Token` read through); the
   `UniswapEngine` holds its *own* `Arc<parking_lot::Mutex<Bot>>` (instance B — a
   separate `Bot::new()` at `uniswap_engine/mod.rs:401`, what the pump mutates). The two
   registries never share state. The backrun example registers the *same* pools into both
   (once via `bot.build_pool()` → Bot A, once via `engine.register_v2_pool()` reading the
   Python pool's reserves out → Bot B). The duplication is load-bearing for *correctness*
   today: `Bot::register_v2_pool`/`register_v3_pool` **panic** on duplicate address,
   `register_v4_pool` returns `Err` — so the two registries *must* be separate or the
   double-registration flow panics. The symptom is the documented stale-state caveat
   (`docs/architecture/rust-owned-bot.md` §17: "Encoding uses amounts from the same
   block (before dispatch); long-term fix is Rust-owned encoding" — encoding reads Bot A,
   the pump mutates Bot B).
2. **The deferred "UniswapEngine lock unification."** ADR-005 deferred unifying the
   engine onto the *shared* `Arc<RwLock<Bot>>` "until the engine's access pattern is
   ready to give up its independent lock." The stale-state mitigation is now friction,
   not a placeholder — the trigger has been met.

Two other facts surfaced during grilling:

- **Today's `Bot` (Rust core) is pure state.** `rust/src/bot_core/mod.rs` `Bot` struct:
  `pools`/`pool_addresses`/`tokens` registries, reorg journal, V3/V4 liquidity-event
  buffers — no `chain_id`, no RPC, no I/O. ADR-005's standalone-core consequence was read
  as "a `cargo add`-able math crate" under that reading.
- **Today's Python `bot.py` is multi-chain by accident.** It "swallowed a multi-chain
  connection manager, pool managers, token managers." The multi-chain-ness is not a
  designed invariant to preserve — nothing prevents a Python user from instantiating two
  single-chain Bots.

The grilling user's intent, restated to remove an inversion in the early framing:
*"preserve the ability for a Rust user to operate a bot just like a Python user. Polars
offers this same split-UX."* Standalone-Rust-core therefore means **a Rust user runs the
whole bot — state + math + RPC + subscriptions + chain I/O — without Python**, not "a
pure-data math crate." Putting RPC + I/O on the Rust core *enables* full standalone in
the strong form; keeping `Bot` pure-data would hand a Rust user the math and no way to
drive it — the weak form.

## Decision

Adopt five sub-decisions.

### D1 — One shared `Bot`; the `Arc<RwLock<Bot>>` is the canonical state owner

`PyBot` and `UniswapEngine` adopt a clone of the *same* `Arc<parking_lot::RwLock<Bot>>`
instead of each constructing their own `Bot`. Constructed via two layered constructors
neither of which is Python-privileged:

- `Bot::new(chain_id, rpc_url)` — allocates the `Arc` and constructs a complete Bot
  (state + RPC + pump + engine vec). Standalone-Rust canonical path.
- `UniswapEngine::with_core(core: Arc<RwLock<Bot>>)` — adopts an existing Arc.
- `PyBot::from_core(core: Arc<RwLock<Bot>>)` — adopts an existing Arc (mirrors the engine).
- The no-arg `UniswapEngine::new()` / `PyBot::new()` sugar is **kept** for standalone
  no-pyo3 tests and the cold-start path, defined as `with_core` over a self-allocated
  Arc. The ~10 `tests.rs` sites calling `UniswapEngine::new()` keep working.

On the *live* Python path, the session allocates the Arc once (via `PyBot`) and the engine
adopts a clone. No branch on "which runtime am I in" — the shared-buffer pattern is a Rust
pattern Python happens to participate in, matching Polars' `RwLock<DataFrame>` +
`Arc`-shared storage.

Resolves ADR-005's deferred "UniswapEngine lock unification." Supersedes ADR-003's
engine-holds-its-own-`Arc<Mutex<Bot>>` arrangement.

### D2 — Lock type `RwLock` on the shared core; engine keeps its own `Mutex`

The shared `Arc<RwLock<Bot>>` is `parking_lot::RwLock` (not `Mutex`), matching today's
`PyBot` tier and the hot loop's read-dominant access pattern (per-pool calcs, tick reads
during solves — concurrent readers under Python 3.13+ free-threading). The pump's
brief `apply_*` write windows are the only exclusivity; Python reads stay concurrent.

`UniswapEngine` **keeps** its own `parking_lot::Mutex<UniswapEngine>` for genuinely
engine-level state (`path_pools`, `path_resolved`, `pool_to_paths`, `results`,
`results_block`, `dirty_v2/v3/v4`, `delivered`, `deregistered`, `next_path_id`,
`pending_new_paths`, `result_tx`, `min/max_profit`, `last_processed_block` — ~15 fields,
all solver-dispatch/result-batching/pump-coordination). ADR-003's peer-module split —
*Bot owns pool/token state; engine owns path/solver state* — is preserved.

Lock order is **unchanged**: engine-then-core. The pump's `engine.lock()` call sites
(~10, in `uniswap_engine_pump.rs`) are untouched. The change is *inside* the engine: the
~30 `self.core.lock()` sites in `event_routing.rs`/`lifecycle.rs`/`solver_dispatch.rs`/
`diagnostic.rs`/`mod.rs` become `self.core.read()` / `self.core.write()` (a mechanical
classification — `apply_*`/`buffer_*`/`register_*` → write; `get_*`/`*_pool_count`/
resolve/solve reads → read). No engine field acquisition changes.

`PyBot`/`PyLiquidityPool` methods take the core `RwLock` *alone* and never touch the
engine `Mutex`; the pump and `PyUniswapArbEngine` take engine-`Mutex` *then* core-`RwLock`.
ADR-003's deadlock-surface-empty rule stays intact.

### D3 — Pool construction is a `Bot` concern only; the engine never constructs pools

The engine-level `register_v2_pool(params)` / `register_v3_pool(params)` /
`register_v4_pool(params)` methods **are deleted** (the three currently at
`uniswap_engine/mod.rs:448–494` that delegate to `self.core.lock().register_*`). Pool
registration lives on `Bot` (`Bot::register_v2_pool` / `register_v3_pool` /
`register_v4_pool`), where it already is. The engine discovers pools at `register_path`
time by resolving `pool_id` against its associated `Bot`.

- **Python-driven path:** the builder writes through `PyBot`'s Arc into the shared `Bot`
  (`py_bot.register_v2_pool(...)` → `pool_id`); the engine's intake is
  `engine.register_path([pool_ids...])`, resolving each `pool_id` against the shared Bot.
  No second registration, no panic — the duplicate panic/Err `Bot` raises on real
  double-registration stays meaningful rather than becoming a false positive.
- **Rust-only path:** `core.write().register_v2_pool(...)` → `pool_id`, then
  `engine.register_path([pool_id])`. Same call shape as Python; no Python in sight.

This deepens the engine module (intake shrinks to `register_path`; pool-creation
concentrates in `Bot`) and enforces "engine is math/path focused, owns no pool state"
literally.

### D4 — `Bot` is the per-chain orchestrator: owns RPC, `chain_id`, the pump, and its engines

`Bot` absorbs what ADR-005 enumerated as "Python session" orchestration, on the Rust side:

- **`chain_id` becomes a `Bot`-level construction-time field** (`Bot::new(chain_id, rpc_url)`).
  Today `chain_id` lives per-`TokenEntry` only; after D4 the Bot-level invariant lets the
  engine validate that every pool in its paths shares its Bot's chain, and prevents
  cross-chain pool-ID collisions from going undetected once `Bot`s can be shared.
- **The RPC `AlloyProvider` lives on `Bot`** (one — since one Bot = one chain).
  `subscribe`/`start`/backfill are Bot-owned I/O. The `UniswapEnginePump`
  (`uniswap_engine_pump.rs`) generalizes to a chain pump living on `Bot` (the per-block
  WS `newHeads`+`logs` loop, address filtering, gap/timeout backfill — unchanged
  mechanics, relocated owner).
- **A `Vec<Box<dyn EventSink>>` on `Bot`** holds zero or more attached engines. Bot owns
  the per-block loop that drives them.

**Naming (ADR-005 layer-naming rule preserved):** `Bot` remains the name of the callable
orchestrator — the bare noun reserved for the Python companion matching `PyBot` minus
`Py`. Today's pure-state `Bot` fields fold *into* `Bot` as private fields. There is **no**
separate *public* `BotDriver`/`BotSession` module — the orchestrator is one deep
interface.

**Bot is a thin deep interface over cohesive private services — not one monolithic struct.**
The responsibilities D4 names (RPC, decode dispatch, state mutation, subscriber
notification, pump, solve-trigger, reorg restore) are *not* all on one struct. `Bot` is a
low-method-count facade (caller surface ≈ `new(chain_id, rpc_url)`, `attach_engine`,
`register_pool`, `start` — four methods) delegating to `pub(crate)` helper modules, each its
own private deep module with its own test seam. This is the codebase-design "interface
vs implementation" distinction: `Bot`'s *interface* stays small (callers and tests cross
that seam); the helpers are private to the implementation and never widen the public
surface. Per-helper modules under `bot_core/`:

| Helper (`pub(crate)`) | Responsibility | Owns dirty-set? | Test seam |
|---|---|---|---|
| `Bot` (the interface) | Owns `Arc<RwLock<BotState>>`, `chain_id`, the helpers; delegates. Thin facade. | no | — (calls through helpers) |
| `BotState` (today's pure-data `Bot`) | Pool/token registries, per-pool swap math, reorg journal, V3/V4 liquidity-event buffers. | no | math-in-isolation tests, zero I/O |
| `LogDispatcher` (decoder registry / event bus) | Holds `Vec<Box<dyn LogDecoder>>`; receives raw logs; produces typed events targeting state-subjects; owns the `StateSubscriber` (`Weak<dyn>`) registry; notifies subscribers after `BotState` mutation releases the core write lock. | no | give it logs + a fake `BotState`, assert notify ordering |
| `BlockPump` (today's `uniswap_engine_pump.rs`, generalized) | WS `newHeads`+`logs` transport, Rust-side address/topic filtering, gap/timeout backfill, the drain loop. Owns the tokio task. | no | give it an in-memory provider, assert block delivery |
| `SolveCoordinator` | The drain-point solve trigger + `SolvePolicy`. Subscribes to `BlockPump`'s drain tick and block-boundary; asks attached engines (each keeping its own per-pool-subject dirty-set, seeded by `LogDispatcher` notifications) to solve dirties per policy. | no (dirties live per-engine) | give it a fake sink + fake dirty-set, assert Eager vs Drain timing |
| `ReorgCoordinator` | `removed`-flag handling, `restore_before_block` over `BotState`, snapshot/restore. | no | proptest on the journal, no I/O |

The dirty-sets stay **on each engine** (an engine knows which pools are in *its* paths —
today's `dirty_v2/v3/v4` on `UniswapEngine`), seeded by subscriber notifications from
`LogDispatcher`. `SolveCoordinator` fires the drain tick; each engine owns its own
`solve_dirty`. `LogDispatcher` and `SolveCoordinator` stay distinct ("which-pool-changed"
vs "when-to-solve") and don't collapse into one module. The helpers are not given `PyO3`
wrappers of their own — `Bot` and engines are the only Python-visible surfaces.

**This revises ADR-005's "Rust Core" enumeration** (which listed `Bot`'s contents as
"data + state-machine logic + `DexIdentity` preset registry," zero I/O). It revises the
enumeration in the direction of *more* standalone-Rust coverage, not less: standalone now
means a Rust user runs the whole bot (state + math + RPC + subscriptions + chain I/O),
matching the Polars split-UX (`pl.DataFrame` Rust == `pl.DataFrame` Python, same core,
two driving surfaces). It revises the *shape* argument behind ADR-005's "Rejected: the
Python `Bot` class *is* the `#[pyclass]`" (orchestration coupled to the state owner) —
the orchestration now lives on the pyo3-free Rust core, so the GIL/lifetime objection
shrinks; the *positive* decision (Bot is the deep callable module) is taken deliberately.

### D5 — One `Bot` per chain; multi-chain is N Bots

A `Bot` is scoped to exactly one chain + one RPC. A user running two strategies on two
chains — e.g. a mainnet V2/V3/V4 arbitrage `Bot` (via `UniswapArbEngine`) and a Polygon
Aave-liquidation `Bot` (via a future `AaveLiquidationEngine`) — instantiates two Bots.
The Python `bot.py` "swallowed multi-chain connection manager, pool managers, token
managers" is an accident to unwind: `bot.py` becomes a single-chain facade over one
`PyBot`; multi-chain is the caller instantiating multiple facades. There is **no**
multi-Bot coordinator layer — two chains → two `Bot.from_config_file()` calls by the
user, two `PyBot`s, no coordination between them.

## The Engine-as-EventSink topology (cycle-free)

D4 + D1 imply a reference problem: Bot owns the pump that drives `engine.process_block`,
so Bot must reference its engines; engines reference Bot (to read pools, request I/O).
Both strong → an `Arc` cycle neither can drop. Resolved by **dependency inversion via a
sink**:

- **Bot → Engine:** only a `Box<dyn EventSink>` (a one-method trait). No strong
  type-bound knowledge the sink is `UniswapArbEngine`.
- **Engine → Bot:** no strong ref. The `&Bot` the engine needs to read pool state is
  passed *in* with each `on_block` call by the pump. Ad-hoc I/O mid-solve (a one-off
  `eth_call`, backfill) goes through a passed `&dyn BotIo` trait or a `Weak<RwLock<Bot>>`
  resolved at call time — the Weak breaks the cycle in both directions.

`on_block(&mut self, bot: &Bot, ...)` — the engine implements `EventSink`,
`UniswapArbEngine` today; a future `AaveLiquidationEngine` implements the same trait with
no `Bot`/pump/`PyBot` change. This is the leverage: N strategies reuse one Bot topology;
the blast radius of "add a new strategy" is one new `Engine` impl. Locality: a solving bug
lives in the engine; a subscription/pump bug lives in Bot; never smeared across one.

The previously-trapped pure helpers from candidate 2 (`SnapshotStore`,
`register_with_cl_buffers`, `verify` plumbing) move onto `Bot`/the chain pump where
they're testable without pyo3. Candidate 2's `py_binding.rs` lift-out is absorbed into
this work.

## Considered options (the load-bearing reversals)

- **Two `Bot`s (status quo / ADR-005 deferred).** *Rejected:* the stale-state mitigation
  (§17) is real friction, the duplicate-registration panic makes double-registration
  fragile, two lock disciplines + two registries is un-canonicalized state this ADR
  replaces. The deferral trigger ("until the engine's access pattern is ready to give up
  its independent lock") is met.
- **Single `Bot` is pure state + a separate `BotDriver` orchestrator (Design Y).**
  *Rejected:* preserves ADR-005's pure-data enumeration but bifurcates "the bot" into two
  structs with an awkward seam; the user's stated model ("Bot should act as the
  orchestrator") wants one callable thing. Folding state into `Bot` as private fields
  gives the same testability (private internal seam) without the public split.
- **Make `engine.register_*` idempotent instead of deleting them.** *Rejected:* silently
  merges two distinct construction-intents ("I'm the authority creating this pool" vs "I'm
  subscribing to one someone else created") behind one call; a misconfigured path or stale
  handle reuse would silently succeed instead of failing loudly; `register_v4_pool`'s
  hook/dynamic-fee filtering is ambiguous on the second call. Two intents want two
  methods — and D3 collapses both onto `Bot` (the engine neither registers nor attaches,
  it resolves `pool_id`s at `register_path` time).
- **Keep RPC/connection management Python-side (topology i).** *Rejected:* contradicts
  "a Rust user operates a bot just like a Python user." Both users must construct
  `Bot::new(chain_id, rpc_url)` and own the connection Rust-side; a Python-side RPC
  authority strands the Rust-only user without I/O.
- **Collapsing to a single `Arc<RwLock<Bot>>` with no engine-level lock.** *Rejected:*
  the engine's ~15 engine-level fields are genuine solver/batching/pump-coordination
  state (not pool state); forcing them onto the Bot's lock either conflate two peer
  modules' state (widening critical sections, reintroducing serialization) or force
  `&self` + per-field cells (complexity the dirty-tracking sets genuinely need atomic
  with the solve). Keep two locks, engine-then-core.

## Consequences

- **The §17 stale-state caveat closes.** Encoding reads the same `Bot` the pump updated
  (one shared Arc). "Long-term fix is Rust-owned encoding" becomes reachable — the
  future-Rust-owned-encoding path reads through the same `Bot` the engine wrote.
- **`Bot::new(chain_id, rpc_url)` is the canonical construction** for both runtimes.
  Rust-only and Python-driven users hit identical code. `alloy-provider` + tokio become
  `degenbot-core` dependencies — accepted: this is what "full standalone" costs, and the
  crate split (ADR-005 deferred) will carry them on the binding/core crates as needed.
- **Adding a strategy = one new `Engine` impl.** `AaveLiquidationEngine` proof: no
  `Bot`/pump/`PyBot` change. The `EventSink` seam is the strategy-extension point.
- **`bot.py` shrinks** toward a single-chain facade; its swallowed multi-chain managers
  move to the caller or retire. A user wanting two chains writes two `Bot`s.
- **Engine construction surface shrinks** (D3 deletes `register_*` on the engine; intake
  becomes `register_path` resolving `pool_id`s). The engine module deepens: one intake
  method, one intent.
- **Lock discipline gains a classification pass** (~30 `self.core.lock()` →
  `read`/`write`). Mechanical; ADR-005's read/write guard split invariant applies to these
  newly-shared sites.
- **Two ADRs revised, recorded here not silently:** ADR-005's "Rust Core" enumeration +
  "Rejected: Python Bot is the #[pyclass]" shape argument; ADR-003's
  engine-holds-own-`Arc<Mutex<Bot>>` arrangement. The revision is toward more standalone.

## ADR conflicts (recorded, not silent)

- **Contradicts ADR-005 "Rust Core" enumeration** (`Bot` = pure state, zero I/O). Reopened
  deliberately: standalone-Rust-core redefined to the strong form (full bot, no Python),
  which the user affirmed is the intended meaning, and which Polars' split-UX models. The
  positive construction (`Bot` is the deep callable orchestrator) is taken deliberately;
  the GIL objection to coupling shrinks because the orchestration is pyo3-free.
- **Supersedes ADR-003's engine-holds-`Arc<Mutex<Bot>>` arrangement** (the engine adopts
  the shared `Arc<RwLock<Bot>>` per D1+D2). ADR-003's *peer-module* split
  (Bot=state, engine=paths) and *engine-then-core lock order* are **preserved
  unchanged**.
- **Contradicts `docs/architecture/rust-owned-bot.md` §13.2 + §17** (two-Bot description,
  stale-state caveat). To be updated at implementation; this ADR is authoritative in the
  interim.

## Related

- **ADR-005** (Polars-Inspired Three-Layer Architecture) — this ADR revises the "Rust
  Core" enumeration and resolves the "Deferred: UniswapEngine lock unification" item.
  ADR-005's Py-prefix naming, the `PyBot`/`PyLiquidityPool`/`PyErc20Token` handle topology,
  and the wrapper-is-the-sharing-mechanism principle are **preserved**.
- **ADR-003** (BotCore as the state layer, peer to UniswapEngine) — the *peer-module*
  split and *engine-then-core* lock order are preserved; the
  engine-holds-its-own-`Arc<Mutex<Bot>>` arrangement is superseded by D1+D2.
- **`rust/CONTEXT.md`** — {Bot}/{PyBot}/{Polars-Inspired Three-Layer Architecture} terms
  gain forward pointers to this ADR; a new {EventSink} term records the decided concept.
- **Architecture review `/tmp/architecture-review-20260617-200733.html` candidate #1** —
  the candidate this grilling opened on.

## Deferred

- **The solve-notification protocol — PARTIALLY RESOLVED.** The `EventSink` topology is
  refined to a per-state-subject publisher/subscriber event bus: `LogDispatcher` (a
  `pub(crate)` helper on `Bot`) owns a decoder registry + a `Weak<dyn StateSubscriber>`
  registry; pool-state events are decoded and applied to `BotState` by `Bot` itself
  (state owner decodes the events that mutate its owned state); after the core write
  lock releases, `LogDispatcher` notifies subscribers per `pool_id`. The engine
  implements `StateSubscriber` (`on_state_updated(pool_id)`) — it dirties `pool_id` in its
  own per-engine dirty-set (the dirty-set stays on the engine, not moved to `Bot`)
  taking the engine `Mutex` *alone* (core write already released — D2's engine-then-core
  order preserved). A `SolveCoordinator` helper fires the drain-point solve tick
  (coalesced re-solve on empty log queue, Idea 1 — the existing eager-processing
  invariant restated in bus vocabulary; no new solve debounce — solves keep coalescing
  at the drain point as today). Today's existing reverse index is recognized as this
  pub/sub realized centrally.
- **A pluggable per-state-subject `SolvePolicy` (Idea 2a) — DEFERRED.** A `SolvePolicy`
  enum (`Drain` default / `Eager` / `Block` / `Manual`) live-set on `SolveCoordinator`,
  runtime-mutable, would let a live searcher (`Eager`/`Drain`) and a batch backtester
  (`Block`/`Manual`) reuse one engine. Idea 2 composes *on top of* Idea 1 (Idea 1 is the
  `Drain` default instance — not mutually exclusive). Deferred on evidence: one concrete
  second consumer short of the "two adapters = real seam" bar; recorded so future
  reviews don't re-derive it. (Note: `SolvePolicy` (when-to-solve) is kept **distinct**
  from the lifecycle `EnginePhase` state machine (what's-legal-to-call-now, Idea 2b) —
  orthogonal axes, never folded into one state machine.)
- **The reorg handling protocol — RESOLVED: optimistic per-event journal rollback,
  no `on_reorg` method.** The WS `removed: true` replay ordering is **not specified**
  by any standard, so the design makes no ordering assumption. On *every* `removed:
  true` log for pool P at block `B` (the removed event's own block, present on every
  log), `ReorgCoordinator` calls `ReorgJournal::restore_before_block(B)` for P,
  writes the landed-at state into current `BotState`, and fires the **same**
  `on_pool_state_updated(P)` notification as a forward update — no separate method.
  This is correct-by-construction because `restore_before_block` is idempotent and
  order-insensitive: newest `< B` → no-op returning current state (harmless);
  newest `≥ B` → pops all deltas at/after B and lands at the largest-block delta
  `< B` (a controlled unwind to exactly the pre-B state, which naturally also handles
  any intermediate-block deltas since the while-loop pops every `back().block() >= B`).
  Chronological arrival → controlled single-block unwinds; reverse-chronological →
  first call pops multiple blocks, subsequent calls no-op; out-of-order/interleaved
  across pools → each pool restores against its own journal independently. The removed
  event's *content* is unused (only its block number + pool identity), because the
  journal's stored "before" values are the source of truth. The `max_depth` bound is
  unchanged — a `removed: true` event whose block is below the journal's earliest
  surviving delta hits `Err(NoStatePriorToBlock)` → fail-stop, exactly as today. The
  single-method `PoolStateSubscriber` covers both forward and reverted updates
  uniformly; reorg is just a burst of the same per-pool notify. (Retracts the earlier
  "separate `on_reorg(target_block)` method" recommendation, which assumed a single
  bulk reorg signal — the WS protocol actually replays unwound events per-log, and the
  ordering is unspecified, so the design must handle any order.)
- **The subscriber-notify payload — RESOLVED: shape (i), bare `pool_id`.** The engine's
  sole per-update input is `on_pool_state_updated(&mut self, pool_id: u64)` (the trait ships
  as `PoolStateSubscriber` until a second state-subject type proves generality). Engine
  decodes nothing and takes no `Bot`/core lock at notify — it dirties `pool_id` in its own
  set and returns, reading `BotState` only later inside `solve_dirty` under engine-then-
  core-read (preserves the coalesced α design literally). `pool_to_paths` indexes on bare
  `pool_id`; the solver re-derives `IntHopState`/`IntV3TickRangeSequence` from current
  `BotState` on every solve (today's `rebuild_and_solve_affected` already does this —
  event-kind agnostic). Event-kind payloads (shape (ii)) rejected as speculative
  machinery for a solver short-circuit no current consumer uses — widen later if and only
  if a concrete short-circuit emerges; a future `AaveLiquidationEngine` needing richer
  payloads for *its* (non-pool) state-subjects is a different trait instance, not a
  widening of the pool-state one.
- **`AaveLiquidationEngine`** — the proof-of-seam future strategy. Referenced to justify
  the `EventSink` trait; not designed in this ADR.
- **`bot.py` unwinding** (removing the swallowed multi-chain managers, becoming a
  single-chain facade). Direction decided (D5); concrete migration is implementation.
- **`SnapshotStore` / `register_with_cl_buffers` / verify-plumbing relocation** (candidate
  2 + D4 absorption). Target owner decided (`Bot`/chain pump); concrete file moves are
  implementation.
- **ADR-005 crate split** (`degenbot-core` / `degenbot-python` / umbrella) — remains
  deferred as in ADR-005; D4's `alloy-provider`/tokio dependency lands on the core crate
  when the split occurs, consistent with "full standalone" now being the target.
