# degenbot — Domain Glossary

Canonical names for the degenbot codebase. Definitions say what a term **IS** — one or two
tight sentences, with the rejected synonyms under `_Avoid_`. Design decisions, rationale,
ship history, and measurements live in `docs/adr/` and `docs/architecture/`, not here.

## Strategy

**Strategy**:
A top-level label for one kind of profit opportunity the bot executes, composed as values
over six capability slots: source (where opportunities are found), infrastructure
(pool/account/token/path loaders), calculation (solver), encoder (opportunity → onchain
executable payload), simulator (validity check of the payload), and submission (delivery
to an endpoint that can land it onchain). Distinct ecosystems get distinct strategies;
shared reaction capabilities are mixed in by composition, never by sub-traits.
_Avoid_: treating "lane", the frame pipeline, or the pending-tx driver loop as the
strategy unit; using "strategy" for the executor-contract adapter (the
`ExecutionAdapter` seam).

**Strategy plane**:
The shared selection surface every concrete strategy participates in: its plane name
(also its config facet name and host registration id) and selection through
`StrategyName`. Extracted by subtraction — a slot with one consumer stays out.
_Avoid_: folding per-strategy knobs or stage machinery into the plane.

**SubmissionSlot**:
The value that distinguishes the two backrun compositions: the `MEVBlocker`
bundle-auction slot or the public-mempool fan-out slot. The reaction machinery is
shared; only the submission slot varies by ecosystem.
_Avoid_: conflating with `SubmissionTarget` (the typed channel at dispatch).

**Settlement arbitrage**:
Arbitrage of the cross-pool price discrepancies a settled block's trades leave behind,
executed as a single transaction at the head of the next block. The opportunity is the
settled pool-state discrepancy itself; there is no identified victim transaction
([ADR-026](docs/adr/ADR-026-backrun-to-settlement-arbitrage-terminology.md)).
_Avoid_: describing this bot's mechanism as "backrun"; "victim transaction"
framing; an audit gate counting residual "backrun" hits.

**Backrun (classic MEV)**:
A transaction positioned immediately after a specific, identified victim transaction in
mempool order, profiting from the victim's price impact. Settlement arbitrage is not
backrunning (no victim transaction); the bot's pending-transaction backrun strategies
(whose submissions land after an identified victim) are classic-MEV backruns. The word
also survives as the legacy name of the `degenbot-arbitrage` crate.

**Searcher adapter**:
A foreign searcher's contract-specific execution adapter (encoding + probes + assess),
authored against the `ExecutionAdapter` seam in their own crate. Composition of the
bot's own strategies is in-core first-class; only the foreign contract surface stays
out.
_Avoid_: "searcher strategy" for this concept; wedging a strategy into the
simulation/solve engine.

## Strategy reaction kinds (ADR-055)

**Settled-block strategy**:
A strategy reacting to *sealed* blocks (settlement arbitrage today). Drives the block
pump/`StageHandlers` seam; its product types are deliberately settlement-shaped until the
ADR-018-named generalization (ADR-055 Phase C).

**Pending-transaction strategy**:
A strategy whose source is *observed mempool transactions*, implementing
`PendingTxReaction` (`degenbot-strategy/src/pending_tx.rs`): `admit` → `discover` →
`evaluate` → `compose` → `decide`, threaded by the strategy-neutral artifacts
`ComposedIntent` and `Decided`. Backrun is the reference implementation.
_Avoid_: "lane", "frame pipeline" as the strategy unit.

**Hosted sources**:
The process-lifetime owner of the strategies' source-slot provisioning and the head
clock (`degenbot-bot/src/hosted_sources.rs`): one typed pending-tx stream per source
kind, the single head watch + stale-fallback head poller, the feed-telemetry sampler,
and the head-edge reconciliation trigger. Constructed once at the boot; drivers drain
their arm's stream and read the hub's latest head.
_Avoid_: per-driver feed spawning, a per-driver head watch or head poller, a per-driver
telemetry scrape, driver-owned reconciliation triggers.

**Pending-transaction driver**:
The `degenbot-strategy` driver owning the strategy-neutral loop around a strategy's
stages: pending-tx replay (ADR-054 seam 1), journal extraction (seam 2), the bundle-sim
gate, timings/tracing, liveness, and submission. It drains the **Hosted sources** stream
for its arm and reads the hosted head clock; it owns no feed, head watch, poller, or
sampler.

**MarketContext**:
The process-lifetime shared caches for pending-transaction strategies — the connector
index + DFS graph, token id/address joins, and the warm code cache
(`degenbot-strategy/src/market_context.rs`). Substrate, never strategy identity.
_Avoid_: "StrategyRuntime" (retired).

**Strategy substrate**:
The shared state-provisioning surface every strategy composes — the state owner
(`BotState`), the planning workspace (`planning::Workspace`), pool ingress, the
connector index, executor hop views, the sign-time nonce-lease mechanics (the
`nonce` module: `StrategyId`/`NonceAuthority`/`NonceLease`/`DeclineKind`/
`ReorgAdvisory`), the driver spawn seam, and the session object registry — with its one
home in `degenbot-substrate` (ADR-067); `degenbot-bot` composes it as a peer, and every consumer imports it
directly from `degenbot-substrate` — the historical `bot_core::*` path layer
is deleted (hard cutover; no aliases).
_Avoid_: "application core" (the substrate is not the arb application); folding
strategy-plane selection into the substrate.

**SubmissionTarget**:
The typed channel vocabulary at `dispatch_and_submit`: `Bundle(BundleTarget)` (exclusive
single-destination auction entry) or `Public` (relay fan-out with read-provider
fallback). Strategy bid/observe policy stays with the strategy; nonce/fee/sign/monitor
machinery stays with the channel.

**Strategy facet**:
One typed per-strategy config section at the schema's single declaration site; its
`active` key (plus its own knobs) is that strategy's activation and operator surface.
There is no single-arm selector; `StrategyName` is the plane's selection key, not a
config key.
_Avoid_: strategy-scoped ad-hoc env reads outside the schema.

**Loud-abort rule**:
At a use site, an unknown/unsupported pool *family* aborts loudly (typed fatal), never a
silent skip; transient RPC/timing/fetch failures keep their skip semantics. ADR-055 D4.

## Strategy host (ADR-057)

The runbook for adding a family over these seams is
[docs/architecture/adding-a-strategy.md](docs/architecture/adding-a-strategy.md)
(the seam map is [docs/architecture/strategy-seams.md](docs/architecture/strategy-seams.md)).

**StrategyHost**:
The per-process owner of the shared strategy services — the event hub, the boot-snapshot
route registry, and the nonce authority — that registers strategies and drives their
lifecycle FSM. A strategy driver attaches to the host; the host never names a strategy
family.
_Avoid_: "sidecar" (the standalone two-process deployment), "orchestrator", "manager".

**Strategy driver**:
The host's unit of strategy admission: a named runnable loop registered as a `DriverPose`
instance and attached to the host's shared services. `BackrunDriver` is the reference; the
settlement pump arm registers none because the engine's pump already drives it.
_Avoid_: using bare "driver" for the engine session (**Driver seam**) or the Python session
(**Driver cockpit**).

**NonceAuthority**:
The host's single owner of the operator account's nonce space, leased at sign time: a
strategy receives the lowest free nonce at or above the confirmed chain nonce, so leases
and broadcasts form a contiguous prefix above it.
_Avoid_: "nonce manager", "nonce pool".

**NonceLane**:
The nonce-flighting seam the NonceAuthority exposes to one strategy: a host-minted binding
to the shared authority and the strategy's submission ledger, whose `stamp()` is the only
issuance entry.
_Avoid_: a private reservation table; using "lane" for the fleet's **Lane** (thread
ownership) or for the authority itself.

**Relay posture**:
The session's choice of broadcast destination for signed bytes: configured private relay
URLs fan the same bytes out instead of the public mempool. A posture holder, never a nonce
issuer.
_Avoid_: "nonce lane", "private-lane reservation".

**Lane namespace**:
A driver's run-artifact root (`<state_root>/<strategy>`, with `session/` and
`quarantine/` subdirectories) that keeps two drivers on one host from colliding on a
shared path.
_Avoid_: "lane root", "strategy directory", "sandbox".

**Submission ledger**:
The per-strategy record of every signed submission — nonce, followed target, bundle hash,
built-at head, and state `Signed → Broadcast → Landed | Stale | Orphaned` — whose
reconcile outcomes are nonce-level facts, not byte-level ones.
_Avoid_: "submission table", "tx log", "journal".

**Head reconciliation**:
The once-per-head fold of the operator account's nonce-lifespan facts into each hosted
strategy's default policy: one claim per chain head drives the refresh and the reconcile,
and every typed notice folds through the owning lane's **HeadPolicy**. Extra triggers on
an already-reconciled head are free.
_Avoid_: "head tick" (the feed's trigger, not the reconciliation); "nonce refresh" (only
one step of it); per-driver reconcile loops.

**HeadPolicy**:
The default per-driver reaction to a typed head notice produced by **Head reconciliation**:
re-stamp on an orphaned submission or a revoked lease, re-evaluate at the decide stage on
a stale one, retire on a landed one.
_Avoid_: "retry policy", "rebroadcast policy".

**Ordered sim-submit pipeline**:
The one core owner of a block's concurrent simulate fan-out plus its submit lane: every
batch's simulate work runs bounded by an injected in-flight cap, and a single submitter
drains batches in arrival order, awaiting each batch's own sim before submitting.
Submission order is therefore nonce order, and the loud-abort contract re-raises the
first leaf failure in the caller's frame. The simulate and submit leaves are core-owned production code shared by every
consumer; a driver injects values — the cap as a plain count, the policy values, the
relay posture and budgets — never choreography code. Its product is the stream of
Batch outcome records.
_Avoid_: "sim queue", "submit queue" (one shared arrival-ordered lane, not a queue per
concern); "task pool" (the bound is a cap the driver injects, not a pool the pipeline
sizes); driver-authored leaf closures over the seam.

**Batch outcome record**:
The typed result of one candidate's ordered pass through the pipeline — the assembly
verdict, the simulate verdict, and the submit receipt or typed failure — delivered as
the stream both first-class consumers drain: the pure-Rust bot into its sinks, the
Python companion's renderers. One record vocabulary, so a driver's remaining
responsibility is display.
_Avoid_: per-driver result tuples; renderers reading raw engine structures; an outcome
callback seam.

## Session objects

The vocabulary for the per-session entity layer. Design record:
[docs/architecture/session-object-registry.md](docs/architecture/session-object-registry.md).
All four kinds are implemented and the Python cutover is complete: the Python registries
are adapters over core identity, and the superseded key maps and claim tables are
deleted. Decision record: [ADR-064](docs/adr/ADR-064-session-object-registry-cutover.md).

**Object**:
A session-resident entity the session recognizes by canonical identity and hands out by
reference — a pool, a token, a path. An object is identity plus whatever is intrinsically
its own; it is not the session's live view of chain state.
_Avoid_: "asset" or "resource" (both untyped here); "manager" for this concept.

**Session object registry**:
The single per-session owner of object identity: one keyed entry per object, get-or-create
on first request, and the one place that answers "is this the same object?". It holds
identity only — no chain or database I/O, no solver behavior, no submission policy.
_Avoid_: "object store", "entity manager", "universe", or **PoolManager** (that stays the
Uniswap V4 contract role) for this concept.

**Canonical identity**:
The key one object is registered under, so a later request for the same thing returns the
first object instead of a twin. Identity is per session and does not cross sessions.
_Avoid_: treating "has a unique id" as the whole characterization (how the key is
constructed is the decision); "handle" for the reference itself.

**Get-or-create**:
The one registration semantic: an existing object is returned, a new one is registered and
returned, and a concurrent second request joins the first rather than racing it. There is
no separate add-then-lookup path, and creation never replaces an existing object.
_Avoid_: "insert", "upsert", or spelling it as two steps.

**Live state**:
The session's mutable, block-advanced copy of a pool's on-chain state. It stays with the
state owner; an object's registry entry is identity, not this. Joining an object to live
state is a lookup, not a copy.
_Avoid_: folding live state into the object entry, or re-deriving it per strategy.

**Object reference**:
What a consumer holds: a borrowed reference to a canonical object. Consumers never hold a
private copy and never register through a per-consumer path.
_Avoid_: "strategy pool cache", "per-strategy handle", "mirror".

**Position identity**:
The key one position is named by in a session: chain, market (its pool contract), and account.
Identity only — the value decays while a caller holds it, so the session names the position and
never stores one.
_Avoid_: "position object" or "canonical position" (both imply a stored value); a
get-or-create verb for a position.

**Position reading**:
One fresh observation of one position: the identity, the block it was observed at, and the
liquidation posture that block reported. Produced per read by a position observer, at a
freshness the caller states, and refused — never defaulted — when the source cannot answer.
_Avoid_: "cached position", "position snapshot" held across a solve, or any read that returns a
value alongside a refusal.

## System layers

**Rust core**:
The pyo3-free crates in the `foundation`, `engine`, and `integrations` roles
under `rust/crates/` own all state, math, I/O orchestration, simulation, and
submission — everything a standalone pure-Rust MEV bot needs.

**PyO3 wrapper**:
The thin `#[pyclass]`/`#[pyfunction]` layer that translates Python calls into core calls.
No business logic.

**Python companion**:
The user-facing Python layer: public API, docstrings, I/O orchestration, and display — a
driver over the Rust core, never a co-implementation.
_Avoid_: "driver shell" for anything smaller than this whole layer.

**argv façade**:
The one place the `degenbot` binary's console command vocabulary is declared; both the
pure-Rust operator and the Python passthrough drive the same façade.
_Avoid_: "CLI layer", "Python CLI".

## Driver cockpit

**Driver cockpit**:
The Python-companion module (`degenbot.runner`) that owns one settlement-arbitrage
session behind the `BotRunner` lifecycle. Its block loop, dispatch, renderers, and session
coordination are private internals.
_Avoid_: "driver shell" (that is the whole Python companion), "backrun session".

**Registration outcome ledger**:
The core's owner of the registration memo: the hop-identity key, the typed
stable-vs-transient build refusal, the bounded outcome vocabulary behind the metric
tags, and the **Refusal vocabulary** (ADR-077). The cockpit consumes it through a thin
adapter — outcomes and tags are minted from the core's exported lists, never re-owned.
_Avoid_: "skip gate", "class-name refusal set"; a Python-side outcome type that mirrors
the core's.

**Refusal vocabulary**:
The one closed `RefusalKind` the registration outcome ledger owns (ADR-077): every
registration-phase refusal is a variant with family and phase riding as data, never as
per-family enum types. Load-bearing classifications (stable-vs-transient build refusal,
fatal-vs-transient verify failure) ride on the variant. Swap-calculation refusals are
NOT this vocabulary — they ride the family read door's `SimulateSwapError`.
_Avoid_: per-family registration error enums; a second identity-mismatch vocabulary.

**Session state**:
The cockpit's one owner of a session's coordination state (dispatcher, sim context,
current block, provider, credentials).
_Avoid_: "session dict", "cockpit config".

**Session watch**:
The cockpit's one owner of a session's end-state RANKING: the typed watch-set
transitions (``_WatchSet`` / ``on_task_done``), the end-verdict ranking, and teardown.
Detection of *why* the session ended is core-owned
(``degenbot_bot::arb_engine::session_end``: ``SessionEndCause`` delivered once via
``SessionEndFacts`` over ``EngineDriver::wait_session_end`` / the heartbeat stall
watchdog); the watch reads that fact and applies its own ranking.
_Avoid_: "await loop", "fail-fast wrapper", "detection owner".

**Run ritual**:
The cockpit's one owner of a running session's startup ordering — consumer-attach,
watch-attach, resume, registration, main loop — as a state machine; the registration
mode and the task scheduler are data it consumes, never branches tests select. It
sits strictly behind the ``run()`` phase gate and re-legislates no legality the Rust
``SessionPhase`` table already owns.
_Avoid_: "run loop" (the main loop is the session watch's), "startup sequence"
(prose, not a module), exposing ritual states as a public lifecycle vocabulary.

## Provider surface

**Provider surface**:
The one home of the sync/async provider trees' shared contract logic
(`degenbot.provider.surface`): `get_logs` filter normalization and validation, and
the typed payload coercions both trees call. The two trees are two call-convention
adapters over the Rust pyclass pair; `OfflineProvider` implements the same surface
without a backend, and every surface joins the same offline parity walk (the census
records each surface's adapters/own code — the shared contract lives on the
surface), so agreement is pinned by the walk, never maintained by hand (ADR-086).
_Avoid_: call-convention unification (the seam pair is real — ADR-086); shared
logic living in one tree because the other does not need it yet.

## Pool construction

**Pool construction**:
The core's one entry from a requested pool to a constructed, registered pool: the
construction-route order, construction identity (the DB two-step — manager row → pool
row → token rows), and get-or-register into session state. A construction failure
classifies stable-vs-transient on the build-refusal taxonomy: a stable refusal aborts
loudly under the loud-abort rule, a transient one keeps skip semantics. The cockpit
supplies resolved policy values, never construction code.
_Avoid_: a driver-side construction fallback chain; bare except-and-continue over
construction failures.

**Construction request**:
The one family-tagged value a caller hands the construction entry: the pool's identity,
the resolved policy values, and the family's own parameters riding as additive variants.
A family the value cannot name does not exist at the interface, so a new family adds a
variant rather than a verb.
_Avoid_: a family×verb method set on any consumer; per-family optional arguments on one
flat call.

**Construction route**:
One ordered attempt to construct a pool at a factory or the generic builder; the
module's policy ordering, tried in turn. A route failure is a typed build refusal, never
a bare exception swallowed into the next route.
_Avoid_: "fallback rung" living at the driver's edge.

## Pool registration lifecycle

Canonical phases for the concentrated-liquidity (V3/V4) registration verify lifecycle:
`Quarantined → drain + verify → Live`.

**Registration lifecycle**:
The per-pool state a registered concentrated-liquidity pool occupies: a Sparse pool is always `Live`; a
Tracked pool is `Quarantined` until its verification passes.

**Quarantined**:
A registered concentrated-liquidity pool whose live events are deferred to the pump buffer until registration
verification completes. A Quarantined pool is not solvable.

**Live**:
The steady-state direct-apply contract, and the only solvable state.

**Tracked** (a `PoolTickCoverage`):
A pool whose snapshot provided complete tick data, so solver results are trustworthy.
Registers `Quarantined`; must pass verification before `Live`.

**Sparse** (a `PoolTickCoverage`):
A pool for which no complete tick data exists, so solver results may be inaccurate.
Registers `Live` immediately and is never verified.

**Pool lookup** (a `PoolLookup`):
The one value an orchestration verb takes to name its pool — `Address` for a V3 pool,
`V4 { pool_manager, pool_id }` for a V4 pool — so a verb exists once instead of once per
key shape. Lives in `degenbot-substrate` beside the state owner; resolves the shape at
the seam and delegates to a single interior (ADR-081).
_Avoid_: pool key, registration key (the retired prototype), pool ref.

**Pre-build identity** (a `PreBuildIdentity`):
The identity a registration memo keys on before a build has resolved anything — a contract
address for the address families, the on-chain pool id for a V4 pool — so a refused pool is
recognizable without an RPC. One identity per pool, whatever its family; the text form
(`p:0x…` / `v4id:0x…`) is only the wire form. Distinct from a **pool lookup**, which names a
registered pool and addresses V4 by `(pool_manager, pool_id)` (ADR-081, ADR-082).
_Avoid_: pool key string, `v3:`/`v4:` prefix, memo key format.

**Effect vs view** (naming rule):
An *effect* acts on a pool and is named family-free over a `PoolLookup`; a *view* that can
only exist for one family may keep the family word; a verb whose payload narrows per family
keeps its suffix rather than hiding the narrowing (ADR-082).
_Avoid_: `_by_key` on a new effect, a family word in an effect name.

**Known bitmap word**:
A tick-bitmap word whose whole tick set has been established by a checked source (sparse
fetch, full-sync replace, snapshot tick keys, or explicit update input). Exists only on
Sparse pools; the on-disk bitmap itself is derived from the pool's tick rows.

**Checked-empty word**:
A checked bitmap word that holds no initialized ticks, kept present-but-zero. A word
*absent* from a bitmap snapshot is indeterminate on a Sparse pool (fetch before use) and
known-empty on a Tracked pool (the map is complete).

**Tick-word source**:
The core's one capability that backfills a tick-bitmap word over RPC, answering the
initialized ticks of one word at one block. One trait serves both call sites — the
registration-time seed and the live-miss backfill — addressed by one key value
(`TickWordKey`: the pool's read descriptor — V3 address + spacing, or V4 `StateView` +
pool id + spacing, the word position being a per-call argument). The V4 identity carries
its `StateView` so the registered pool can name its own read descriptor. Under the
trait sits one transport-generic choreography (bitmap read → 256-bit enumeration →
per-tick liquidity reads) shared by both arms; the arms differ only in the
unreadable-bitmap policy — registration degrades to an empty word (the graceful
fallback), the miss path refuses.
_Avoid_: one trait per call site for one choreography; a second hand-rolled copy of
the enumerate loop per transport; a driver-supplied fetch callback; a fetcher that
reads the state owner it is fetching for.

**Snapshot seed**:
The registration-time (pinned) tick data captured for a Tracked pool, verified exactly
once against on-chain state at the snapshot block.

**Last complete block**:
The highest block the pump has fully delivered, tombstoned by the first `removed:false`
log of the next block. A registration's state application may not advance past it.

**Verify lifecycle**:
The per-pool choreography — quarantine, seed verification, drain, post-drain
verification, live — plus its block-resolution and config-gating policy and a bounded
retry for transient failures, owned by the Rust core. A per-call RPC or
provider-construction failure is retried under the core's backoff with the
driver-injected policy; an on-chain snapshot mismatch is fatal and never retried.

_Avoid_: a driver-side retry loop over the lifecycle; the dance and its failure
classification belong to the core.

**State tripwire**:
The verification failure raised as the terminal gate so `Live` is unreachable while
tracked state is unverified. Never auto-repaired. Distinct from the solver-state
tripwire.

**Orphan sweep**:
Cleanup releasing pools that were built but whose paths never registered. Never a
productivity dependency.

## Solver-state tripwire (retired)

**Solver-state tripwire**:
RETIRED (ADR-021 era): in-process chain-vs-solver-state verification is retired with the
stage-separated data plane — the desync it gated on is unrepresentable. The surviving
solve-time gate is the solve-anchor future-hop rule. Distinct from the registration
**State tripwire**, which remains live.
_Avoid_: "solver-state tripwire" for any new gate; the term naming the solve-anchor
future-hop rule or the registration **State tripwire**.

**Tripwire class**:
The defect class a tripwire verdict names (`MissedLog`, `StorageMutated`,
`DeliveryLag`, `UnhandledReorg`, `Unclassified`). Evidence that cannot distinguish
classes lands in `Unclassified` rather than a forced label.

## Pool families and hops

The seven `PoolEntry` variants fall into three structural families, grouped by state and
delta shape — not by DEX.

**Reserve-pair**:
A family holding two `U112` reserves plus `update_block`, with full-state block deltas
(V2, Aerodrome V2).

**Balance-vector**:
A family holding a balance vector plus `update_block`, with full-state block deltas
(Curve, Balancer weighted, Balancer stable).

**Concentrated liquidity**:
A family holding slot0 scalars plus per-tick data, with partial-prior block deltas
(V3, V4).

**Pool arm**:
The typed view a `Pool` handle exposes for one structural family, narrowing the
registered entry before any read: `reserve_pair()`, `balance_vector()`,
`concentrated_liquidity()`, `curve()`, `balancer_weighted()`, `balancer_stable()`.
The family-specific accessors (`curve()`, `balancer_weighted()`,
`balancer_stable()`) refuse a handle registered under any other family — including
the Balancer sibling — so a wrong-family call raises instead of answering a
sentinel a caller could read as live state; the shape-level accessors serve a set
of sibling entries (`reserve_pair()`: V2 + Aerodrome V2; `concentrated_liquidity()`:
V3 + V4; `balance_vector()`: Curve + both Balancer arms). Identity and
family-specific state reads live on the arm and never on the root handle, so the
family a read belongs to is the accessor that produced it. A companion narrows
once in `from_handle` and holds the arm (`CurveStableswapPool._curve`,
`BalancerV2StablePool._arm`), which is also how its family gate is spelled.
_Avoid_: a family-prefixed root method (`pool.curve_get_dy()`,
`pool.balancer_amp`), or a `pool_family` string compare as a family gate.

_Naming_: code identifiers spell the family name out — `concentrated_liquidity` /
`ConcentratedLiquidity…` — matching the names already in force
(`degenbot-concentrated-liquidity-math`, `concentrated_liquidity_math`); the
`cl` / `Cl…` abbreviations in code (`cl/`, `cl.rs`, `ClOrchestration`, `ClHop`,
`PreparedClHop`, `BotState::cl()`) are renamed to the expanded form, and living
prose spells the name out too ("concentrated liquidity"; "concentrated-liquidity"
as a compound modifier). Wire values spell the family out too — the capture `kind` discriminant is
`"concentrated_liquidity"` — and the bare abbreviation survives only in the
dated historical record (ADR bodies 001-074), never as shorthand in living prose.
_Avoid_: `cl` as a module, type, method, or crate-fragment name; `CL` as a prose
shorthand; two conventions for one family in one tree.

**Hop**:
The solver's snapshot-and-classifier adapter observed at resolve time: it captures pool
state (a selective projection for concentrated liquidity) so the solve runs lock-free off a copied value, and
its variants let the solver pick the algorithm from path composition. Not a pool concept
and not math-leaf vocabulary.

**Pool family axes**: two axes a species' family can live on — the **structural** axis (registry entry kind and state/delta shape: V2, Curve, Balancer meta/weighted/stable, concentrated liquidity) and the **behavioral** axis (math family: reserve-pair constant product, Curve, Balancer, Solidly — Camelot/Aerodrome stable — concentrated liquidity). They coincide for every species but two: Camelot-stable (V2-shaped entry, Solidly math) and Aerodrome-volatile (its own entry kind, reserve-pair math). **Reads route at the behavioral axis; state lives at the structural axis** — a `stable_swap` V2 entry reads through the Solidly door's Camelot arm with token scales carried at construction (ADR-003), never through constant-product math (ADR-073, ADR-059 D3).

**Family module**:
The solve tree's module owning everything one hop family contributes to a solve and
its bound: the gate projection (`to_math`), the V2-equivalent projection,
the simulate triple, the solve entry (built on the shared golden template where the
stable-family search applies), and the family's envelope lines (its rise line beside its
cap, or a concentrated-liquidity family's tangent fan). It is the nested
`pub(crate) mod solve` inside the family's **Family read module** file (ADR-073
amendment A5), so a family's read side and solve side are one file; that read module
defines the hop payload this module solves over.
The residual central sites are one dispatch row and one
enum arm in each of `ResolvedHop` and `HopMath`; the shared kernels — the golden-section template,
the integer-verification order, the envelope composition and caches — stay monomorphic and
family-blind (ADR-059 D6).
_Avoid_: a family's functions scattered across `solve/mixed/mod.rs`, `solve/mixed/solve.rs`,
and `solve/profit_envelope/derive.rs`; per-family copies of the search template.

**Family read module**:
The foundation-layer module owning everything one hop family contributes to being read and
resolved: the state slice, the swap-read arm (every variant, miss-recovery and
disarm-fetch included), the resolve projection as a pure function of the family state
slice, and the family-local apply/get/calc behaviour over that slice. It reads no state
owner and consumes no solver kernel, so the family's read story is one file named for the
family. The state owner (`BotState`) and the central dispatchers (`simulate_swap`,
`resolve`, `swap_simulation`) keep one dispatch row per family and no family-shaped
method set — since the C6 sweep, ZERO family-shaped names: the registry's rows name the
operation (typed lens `pool_slice::<T>`, `apply_swap_by_pool_id`,
`restore_pool_before_block`), never the family or design; the solve side is the **Family
module**. Both homes carry the family's name,
so one family is read as `<family>.rs` twice, at two depths, with a fixed layout between
them.

_Avoid_: per-family arms inside the central matchers; family behaviour hanging off the
state owner as inherent methods; a family read module that reaches back into `BotState`;
a family- or design-shaped method name on the state owner's surface (the taxonomy lives
in the slice type and the family paths, not in row names).

**Capability home**:

The one crate (and module inside it) that owns a user-visible capability
(ADR-084 D1): the answer to "where do I call this" in `docs/capabilities.md`.
Every publishable crate appears in the catalog at least once, every home path
resolves to real code, and the catalog gate fails when a new capability lands
without a row.

_Avoid_: a second home for one capability; a capability that exists only as an
uncatalogued module tree; a catalog row whose path does not compile.

**Capability door**:

The curated `pub` surface a capability home presents (ADR-084 D2): intent-named
entry points plus the types reachable through their public fields (the door is
*closed* under that rule, enforced by `clippy --deny warnings`). Distinct from
ADR-078's **Family write door**, which is the registration seam specifically.

_Avoid_: exposing a module tree where a door was asked for; a public field
whose type is `pub(crate)` (private-types-in-public-interface); a lab example
that reaches past the door (it earns door items, moves into `#[cfg(test)]`, or
is deleted).

**Dependency charter**:

The declared set of dependency prefixes a crate may carry, treated as its
published contract (ADR-084 D3) and asserted by `just check-dependency-charter`
over `cargo tree -p X -e normal`. Seam crates add "no C-build dep" and
"no pyo3".

_Avoid_: growing a foundation crate's dependency set for fixture or lab
machinery; a charter row without a rationale; whitelisting a leak instead of
fixing it.

**Family write door**:
The one registration/construction entry on the state owner (ADR-078): ONE
`PoolRegistration` request whose per-family params ride as additive variants, ONE
verdict speaking the **Refusal vocabulary**. The per-family orchestration modules stay
behind the door; every consumer (PyO3 wrapper, bot core, pure-Rust driver) crosses the
same seam, and a new family adds a params variant, never a method.
_Avoid_: a family×verb method matrix on any consumer; payload construction in the PyO3
wrapper; a driver-side construction fallback chain (see **Pool construction**).

**Concentrated-liquidity state module**:
The concentrated-liquidity family tree's state half (ADR-080): the journals,
bitmap-word seeding and coverage, viability, and tick-range cache shared by the V3/V4
designs, data-generic and monomorphic in hot loops (ADR-059 D6). `v3_state`/`v4_state`
are the two storage adapters (slot0 vs state-view/hook-keyed PoolId); the kernel
(ADR-071) and the family read module (ADR-073) are its siblings.
_Avoid_: a second copy of journal/bitmap/viability logic per design; a trait per pool.

**Zero-output sentinel (retired)**:
The pre-port contract where a family read arm answered `Ok(U256::ZERO)` for a swap it
could not compute — a "not-yet-Rust-side" placeholder that read as a real zero-output
swap. The last arm carrying it (non-`STANDARD` Curve swap styles at the Curve family
read door) was retired to the typed `SimulateSwapError::NotComputable` (ADR-073 C6)
after the companion's Curve read path was verified to carry no zero-vs-error
distinguishing consumer. NOT the zero-input rule: `getAmountOut(0) = 0` is real
on-chain semantics and stays in every read arm.

_Avoid_: reintroducing a zero answer for "this door does not compute this"; a new
not-computed case gets a typed refusal (`NotComputable` or a family-specific refusal
vocabulary), never a placeholder value.

**Solve anchor**:
The block at-or-above the pool-state head that a block's solve, verification, and
simulation run against. A hop whose price clock runs ahead of the anchor is *future* and
never legitimate.

**Pool-state head**:
The maximum `update_block` across all registered pools — the state clock. During a
backfill/drain desync it can run ahead of the pump's header clock; the solve anchor takes
the max of the two.

**Species manifest**:
The single-source record of a pool *species* — a fork or manager deployment that differs
from its family only in identifiers (factory, init codehash, manager address, chain
scope, fee denominator, storage-layout id). A species is a manifest entry, never a new
type; the manifest is single-sourced and consumed by both the Rust core and the Python
companion (companion class selection, DB subclass tables). Behavior that genuinely
diverges — different storage slots or event shapes — stays typed code, defaulted by the
manifest's layout reference (ADR-059 D3).
_Avoid_: per-language species constants (`V3_FAMILIES`, `_POOL_VERSION_MAP`, dual
deployments tables); a DEX type per fork.

## Verification idioms

**Drift gate**:
The regenerate-and-diff family (`docs/rust-config-keys.md`'s `REGEN_*` test, `just
gen-stubs --check`): a committed artifact that is machine-emitted must fail a plain
test run the moment source and artifact disagree. The pipeline that WRITES the
artifact is the same one the gate runs — if those diverge, the gate is theater.
_Avoid_: a gate whose argument forwarding is inert (a vacuous green); hand edits over
generated output.

**Negative probe**:
The acceptance test for a gate itself: its red path demonstrated once by deliberate
fault injection (edit the artifact, rerun, observe the failure) before a green run is
treated as evidence. Cheap insurance against inert pass-throughs, stale comparisons,
and accidentally-dead checks.
_Avoid_: trusting an unprobed pass; running a gate whose failure branch is unknown.

**Retirement tripwire**:

A test, gate, or prose prohibition whose purpose is keeping a deliberately deleted thing
gone — absent-API assertions, retired-name text greps, "must not reappear" gates,
"do not revive" prose. Deletion is the complete retirement
([ADR-087](docs/adr/ADR-087-deletion-is-the-retirement.md)): the ban is absolute, and
the tripwire-ratchet commit gate enforces it.

_Avoid_: adding one for any surface; citing live-invariant gates (drift gates,
negative probes) as precedent for one

**Live-invariant gate**:
The other species: a test, gate, or source scan that guards a rule **present, living
code** could silently violate — a dependency edge a `cargo add` breaks, a private seam
leaf code could reach around, a generated artifact that drifts from its generator, a
manifest whose wiring a shortened copy would silently drop
([ADR-087](docs/adr/ADR-087-deletion-is-the-retirement.md) D2). What it watches is alive
today, so the gate's red path is a live change, never a resurrection.

Every live-invariant gate carries this label where it lives (its doc comment, or its
owning ADR when the gate has no doc home), stating what present code it watches. The
marker string is fixed and machine-readable — the tripwire ratchet (ADR-087 D4)
recognizes it as the exemption marker:

`live-invariant gate (ADR-087 D2): watches <present code>; not tripwire precedent.`

Rust and justfile/prek/YAML/shell artifacts carry it as a `//` or `#` comment (Rust doc
comments use `///`); Python artifacts use `#`.

_Avoid_: conflating it with a retirement tripwire (they differ in what they watch — the
living versus the dead); labeling an artifact without naming the present code it
watches; any second spelling of the marker.

**Push-gate manifest**:
prek.toml's `stages = ["pre-push"]` hook entries — the single declaration of the
push-gate list. `just pre-push` is a one-line adapter over `prek run --stage pre-push`,
never a transcription, so the recipe and an actual `git push` run the same gates by
construction ([ADR-074](docs/adr/ADR-074-push-gate-manifest.md)).
_Avoid_: a second gate list (a `run_gate` transcription, a generated recipe); calling
`just pre-push` a mirror of the hook.

## Capture and replay

**Golden capture**:
The committed recording of a pinned span's real chain answers and database effects;
the fixed truth that replay tests and replay benches compare against.
_Avoid_: "fixture" for recorded traffic, "snapshot"

**Cassette**:
The wire-form half of a golden capture — the ordered RPC question→answer ledger for
the pinned span, replayable in place of a live node.
_Avoid_: "mock", "stub", "recording"

**Statement ledger**:
The SQL half of a golden capture — the normalized statement sequence one chunk apply
runs, in order, with argument shapes and effects.
_Avoid_: "query log", "SQL trace"

**Replay bench**:
The benchmark that runs a fixed workload against a golden capture rather than a live
node.
_Avoid_: "offline bench", "fixture benchmark"

## Aave updater and digest

**Market reset**:
Purging one market's rows and re-initializing through the activate seam under a single
transaction, leaving the market observationally identical to a fresh cold boot.
_Avoid_: shell-assembled purge-then-re-init across separate transactions.

**Completed-market manifest**:
The typed record of a completed market — the market facts, the drive block, the
serialization stamp, and the per-table digests — owned in both directions (render and
validated parse) by the digest writer.
_Avoid_: opaque pre-rendered strings; hand-parsed JSON inverses.

## Piecewise concentrated-liquidity solving

**Concentrated Liquidity Kernel**:
The data-parameterized module that is the concrete home of everything the
concentrated-liquidity solve shares across the Uniswap V3 and V4 designs: crossing
derivation from a tick-range sequence, the active-set walk over the piecewise-Möbius
segments, the concentrated-liquidity hop and path simulation, and the word-profile build over the prepared
hop's tables. Concentrated liquidity is a mild generic over the V3/V4 designs — the two differ in storage
slots, event shapes, and state reads (slot0 vs state-view, hooks hashed into the PoolId),
not in solve structure — so the kernel is generic over *data* (the prepared hop value,
ADR-069 D2), never over a trait per pool, and the hottest walk in the solver stays
monomorphic (ADR-059 D6). The concrete V3 and V4 pool adapters stay typed code — species
that genuinely diverge stay typed per ADR-059 D3 — and call the kernel at the resolve
seam to produce the one prepared hop. The kernel carries no envelope vocabulary: no
`Line`, no verdict, no cap. The tangent fan is derived from the kernel's crossing tables
by the family module's bound side, and the engine-owned fan cache stays borrowed, never
owned. Home: `degenbot-math`'s `concentrated_liquidity` module (sibling of the `v2` and `balancer` leaves,
ADR-035) once a second tier consumer reaches it; the solve tree's `concentrated_liquidity`
module until then.
_Avoid_: a trait hierarchy over pools on the solve path; the kernel as an unowned tree
under `degenbot-pools`; envelope types inside the kernel; "concentrated-liquidity family" naming the kernel.

**Refine bound**:
The walk-side refine-window bound in the kernel's own vocabulary: one affine upper bound on the
path output at input `x`, expressed as the predicate `(a + b·x)/c ≥ best` with `c > 0`, which marks
the inputs a composed envelope bound cannot disprove. The active-set walk intersects its refine
window against these bounds and skips the disproven sub-window without a simulation, so no
envelope type (`Line`, verdict, cap) reaches into the kernel. The concentrated-liquidity family module translates
the envelope's composed lines into refine bounds at the seam (`refine_bounds`).

_Avoid_: an envelope type inside the kernel; re-deriving the bound inside the walk instead of at
the seam.

**Piecewise walker**:
The active-set engine that solves a multi-hop concentrated-liquidity arbitrage by
climbing per-hop ending-range indices piece by piece, refining the terminal window until
the exact interior optimum is found. Runs entirely on captured-state values.
_Avoid_: "the solver", "the mobius intake", naming the engine after its runtime config.

**Hop state**:
The immutable per-hop integer arithmetic view a walker consumes during evaluation
(`IntHopState`-shaped), distinct from the **Hop** (the captured-state adapter observed at
resolve time). A hop state does not carry an origin block.
_Avoid_: using "hop" for the evaluation-time form.

**Word profile**:
The precomputed prefix of per-word-boundary swap steps for a dense concentrated-liquidity range, where the
boundary list, entry state, and fee are fixed — converts a sim query into a partition
search plus one live landing step. Built once per range, shared by the walk across paths.
_Avoid_: "caching table", "profile cache".

**Prepared hop**:
One concentrated-liquidity hop's walk intake: the tick-range sequence paired structurally with its crossing
table, word profiles, and derivation fingerprint — built once per (pool, direction) by the
projection and shared across paths. The composition memo's key is folded from the same
value the walk consumes.
_Avoid_: positional pairing of parallel table slices; per-solve fingerprint re-derivation.

**Composition memo**:
The engine-owned cross-block fingerprint map from exact hop-composition fingerprints to
prior solve outcomes. The fingerprint is the composition's correctness key; an identical
composition always maps to the identical outcome.
_Avoid_: "cache", "solution cache", "walk cache".

**Walk telemetry**:
The solver-internal counters and cost histograms the walker writes while solving (piece
visits, per-section timings, densely-worded-range heights). Read by the run through the
returned outcome's stats field; production defaults to off.
_Avoid_: env-gated mutation reads mixed into solve math side effects.

**WalkTally**:

The per-solve walk telemetry accumulator the active-set walk threads by `&mut`; one value per

`solve_active_set_path` call, merged into the engine's `WalkTelemetry` at exit. Solve-scoped, so

it cannot mix counts across solves or tests.

_Avoid_: "thread-local counters", "the TLS cells", treating it as ambient state.

**WalkTelemetry**:

The engine-owned walk telemetry collector (`Arc`, `Option<&WalkTelemetry>` at the solve entries

beside `WalkMemo`): cumulative wall-time census plus the census-piece log, drained by tooling.

_Avoid_: "the walk statics", "the process globals", a process-global singleton.

**WalkNsCensus**:

The process-cumulative walk wall-time census (nanoseconds) the collector accumulates: the

whole-solve, sim, anchor and prediction timers, the anchor build/compose/argmax sub-timers, plus

the left-edge, right-edge, direction and refinement `(wall, sims, sim-ns)` section triples. Merge adds; a drain swaps it back to zero.

_Avoid_: reading it as a per-solve value; resetting it mid-solve.

## Profit envelope

**Profit envelope**:
A piecewise-linear concave upper bound on a hop's output curve, derived from projection
data the solve already builds, and the point-wise minimum of its hop tangent lines and
per-hop caps. A constant-product-family hop contributes its exact post-fee entry tangent
beside the fee-agnostic rise; a concentrated-liquidity hop contributes its pool-static tangent fan. A
Balancer V2 weighted hop contributes its entry tangent (its output curve is concave in the
input) beside its asymptotic reserve cap. A Solidly/Aerodrome/Camelot stable hop
contributes its entry tangent (its output curve is
concave in the input) beside its reserve cap, and a Balancer V2 stable hop contributes
its entry tangent wherever the closed-form curvature test holds (its output curve is
concave there) beside its reserve cap; the Curve stableswap family keeps the reserve cap
alone, because its leaf's out-curve was measured non-concave (an entry tangent would
under-cut a real leaf).
Extending a single piece's validity window is not a sound bound. The stableswap-family bound derivations and the solve-method decision are recorded in [the stableswap gate-bounds evaluation](docs/architecture/stableswap-gate-bounds-and-solve-method-evaluation.md).

**Envelope gate**:
The pre-solve skip test over the chained path bound: when the bound's best possible gain
is below the profit floor, the path is provably unprofitable and is skipped without a
single simulation. Distinct from the per-hop direction viability gate.

**Envelope verdict**:
The gate's typed outcome: `Bound` (a sound bound) or `Unsupported` (none derivable).
Unsupported paths are solved unscreened, never skipped.
_Avoid_: overloading a bare `None` to mean both unsupported and unprofitable.

**Rise line**:
A hop's entry tangent at zero input, anchoring its envelope. A constant-product (Möbius)
hop carries its exact post-fee tangent `γ·r_out / (fee_denom·r_in)` beside the
fee-agnostic `r_out / r_in` line; taking the point-wise minimum of both stays a rigorous
upper bound and is strictly tighter than either alone.
_Avoid_: using a fee-agnostic rise where the retained fee is known.

**Tangent fan**:
The pool-static set of sampled crossing-entry tangent lines a concentrated-liquidity hop contributes, plus the
asymptotic cap from its last crossing. It is a pure function of the crossing table after
the live in-range price, which anchors a head tangent rebuilt per call, so reuse is keyed
by fan content and scoped to the solve-cycle epoch.
_Avoid_: caching the head tangent; keying a fan to the live price.

**Exact concave composition**:
Composing two hulls into their point-wise lower envelope by selecting only the
y-overlapping outer/inner piece pairs, yielding a canonical hull of at most `K1 + K2`
pieces with no sampling. Degenerate operands (flat or negative-slope lines, ambiguous
seam boundaries, an `I512` compose wall) fall back to the frozen sampled reference.
_Avoid_: composing the full `K1 × K2` product; letting a fallback emit a looser bound than
the sampling it replaces.

**Bound-scan early exit**:
The gate's floor-threaded stop in the concave-max scan: once a hull segment's candidate
strictly clears the caller's skip floor, later segments cannot change the skip verdict and
are not visited. Distinct from the envelope verdict, which the scan still returns.
_Avoid_: running the full scan when the skip floor is already decided.

**Hull-edge seed**:
Seeding the active-set walk's first-piece right-edge bisection from the composed hull's
first growth-rate takeover. Not a production mechanism: the walk's event solver
supersedes the seeded bisection and the measured probe saving is negligible
([evaluation](docs/architecture/hull-edge-seed-evaluation.md)).
_Avoid_: seeding walk bisections from envelope geometry without measuring under the
production event-solver stance.

**Walk memo**:
The engine-owned cross-block composition cache passed into solve entries and advanced
once per block.
_Avoid_: global memo state; memoization gated by the environment.

**Solve runtime config**:
The injected config the solver internals read — data, never the environment — built once
by the engine owner.

**Walk telemetry**:
The copy counters a solve walk always returns alongside its result; heavy captures ride
an optional, caller-supplied capture parameter.

## Construction I/O

**Construction I/O**:
The I/O seam pool construction consumes — database reads and writes plus generic RPC —
held by `Bot` and passed to every builder.

**DbConstruction / RpcConstruction**:
The two construction sub-traits, one seam per concern: DB-facing construction
reads/writes (returning core row types, errors propagated loudly) and RPC-facing generic
calls.

**ContractRead**:
The one call primitive the ABI read family and the construction transports share: one
`eth_call(to, data, block)` returning the raw return bytes. Each `abi::fetch_*` helper is
generic over it, so selectors, calldata and return decoders are written once and read
through an `AlloyProvider`, a shared `Arc<AlloyProvider>`, or a construction transport;
a transport supplies call policy (pacing, retry, canned bytes), nothing else.
_Avoid_: a second fetch family per transport; a transport-specific copy of an
`encode → call → decode` helper.

**NoDb**:
The construction adapter whose methods always return nothing, used when no database is
configured; it doubles as the in-memory test fake.

## Execution strategy

**Execution strategy**:
The searcher-owned layer that turns a solve result into a submitted transaction: payload
composition, declared probe reads, the assess gate, and fee pricing — implemented as an
adapter that both Python and Rust consumers meet at the same seam.

**Payload composer**:
The encode half of an execution strategy: solve result → payload bytes for one execution
contract. Rust consumers implement it; Python consumers supply a callable.

**Probe and assess**:
Declared pre/post balance reads (probe) plus the gate converting deltas into profit and
pass/fail (assess). Fee pricing is the defaulted pricing half of assess, not a separate
seam.

## Executor command layer

The layer turning a solver result into the bytes passed to the on-chain executor's
`execute`.

**Command stream**:
The `bytes` payload `execute()` runs — a sequence of compact opcodes against an address
table; the atomic unit the command grammar emits.
_Avoid_: "payload" (reserved for the solve-result → strategy seam).

**Encode request**:
The per-path intake value the composer consumes: the path, the solver's amounts, and the
operator's declared axes. It is the contract the concentrated-liquidity overfeed-clamp invariant attaches to.
_Avoid_: "command stream" (the bytes it encodes into), "payload", "EncodeOptions".

**Execution context**:
The strategy-session deployment value carrying the executor, authoritative V4
`PoolManager`, and WETH identity to command composition, frame simulation, and V4
descriptor/roster projection. The backrun boot constructs it once.
_Avoid_: rebuilding it per adapter, frame, or candidate; a candidate-derived manager fallback.

**Encode context**:
The low-level encode-intake value shared by command encoding calls in one session.
_Avoid_: using it as the strategy-wide session context or folding it into the
per-path encode request.

**Command grammar**:
The rules that derive a valid command stream for a shape class, including the ordering
invariants the stream must satisfy. Distinct from the stream it emits and from the
composer that executes it.
_Avoid_: "composer" for the model, "encoder" for the model.

**Funding source**:
The declared origin of a stream's entry (seed) capital, chosen per path by the operator:
self-funded, pool flash-loan, PoolManager free take, external-lender flash (Aave), or
ERC-6909 burn-to-settle. Exactly one per stream; inter-hop inputs are not funding
decisions.
_Avoid_: "capital source", "flash source".

**Profit capture**:
The declared destination of a stream's terminal profit: custody, owner, native, ERC-6909
mint, or Balancer Vault.
_Avoid_: "profit taking", "settlement".

**Builder bribe**:
A separately-declared payment to a block builder, orthogonal to profit capture.
_Avoid_: "tip", "fee".

**Ledger**:
The accounting target an operation reads from or writes to: the executor balance, the
PoolManager delta, an ERC-6909 balance, a pool-to-pool handoff, or an external
vault/lender. Credit precedes debit within a ledger.
_Avoid_: "realm", "book", "track".

**Hop coupling**:
How one hop's output reaches the next: direct pool-to-pool, via the executor balance, or
via a ledger delta — including the repayment pivot that settles a borrowed ledger.
_Avoid_: "handover"; conflating with the funding source (the seed).

**Flash source pool**:
The pool whose own swap callback lends the stream's entry capital. May be *in-path*
(also a hop, repaid by the path) or *off-path* (an independent stop whose excess stays
with the executor).
_Avoid_: conflating with the funding-source axis or with external-lender flash.

**Repayment pivot**:
The derived hop or mechanism settling a borrowed ledger, chosen by token roles and hop
coupling — never hand-picked.
_Avoid_: "repay hop" (a pivot may be a settle, not a swap).

**Derivation outcome**:
The tri-state result of turning a shape class into a command stream:

- **Encoded** — a valid stream was derived.
- **Decline** — the derivation declines the family (no producer, or a producer guard
  returned nothing); routine and expected, and the strategy skips the path.
- **Reject** — a stream was built but the ledger validator rejected it; by contract this
  is a latent bug, so it is always fatal — never swallowed or degraded to a skip.

_Avoid_: collapsing decline and reject under "None"; treating a reject like a skip.

**Hop facts**:
The per-protocol data descriptors the plan walker consumes — ledgers touched,
credit/debit, funding and capture role, repayment obligation. A new protocol adds one
hop-facts descriptor and one mechanics module, never per-family plan bodies.
_Avoid_: conflating with "ledger" (a location, not a descriptor).

**Mechanics**:
The shared step-primitive library the walker's shape modules compose; all step
construction goes through it, never hand-built literals.
_Avoid_: "encoders" (the byte side), "ledger" (the validator side).

**Enclosure**:
The callback-nesting structure of a command stream — which operations wrap which, and
the repayment order. The grammar's output, never a user axis.
_Avoid_: "nesting", "wrapping".

**Walker shape family**:
The per-shape modules the plan walker routes between; every shape is a walk over hop
facts and mechanics, pinned by honesty probes and golden byte streams.
_Avoid_: hand-authored per-family plan bodies.

**Terminal form**:
How the trailing hop of a three-hop shape completes: `DirectHandoff` vs `UnlockInternal`.
Set on the terminal hop only.

**Repay mechanism**:
The physical across-hops repayment transport (executor transfer, in-callback transfer,
in-unlock take, downstream flash delivery, downstream take). Data terrain reserved for
future plans.

**Seed delivery**:
How the WETH seed reaches the pool that needs it: an ERC-20 callback prefund or an
in-unlock compact take. Set per hop where it varies.

## Swap simulation

**Swap simulation**:
The one owner of "what would this swap do against current pool state" — every such read
goes through its single entry point; family math stays in the pools crate.
_Avoid_: "quote", "oracle", "gate"; the retired miss-aware/with-fetch twins.

**SwapRequest**:
The simulation request: direction, signed amount (positive = exact output, negative =
exact input, from the user's perspective), and price limit.

**Swap-read block context**:
The block a swap read threads into sparse-miss recovery (it never affects pure computation).
A handle's `simulate` resolves an omitted block to its own `update_block`, so the plain
current-state read names none; an explicit block is the historical read, where the context
genuinely differs from the handle's current stamp.
_Avoid_: passing the handle's `update_block` back to the same handle as an echo.

**Override state**:
The hypothetical per-family state a swap read runs against — `ReservePair` reserves,
`BalanceVector` balances, or `ConcentratedLiquidity` scalars plus tick rows — one
`OverrideState` variant per family (a new family is a new variant, never a new method),
with immutable params borrowed from the registration rather than re-sent. The outcome
carries the end state (reserve-pair reads include their post-swap reserves), so no caller
re-derives it from the deltas.
_Avoid_: "hypothetical pool"; per-family override methods; an override flag beside the state.

**SwapRead**:
The typed outcome (`Computed` / `NotComputable` / `FetchFailed` / `FetchExhausted`).
No silent zero — a former silent-failure mode is always an observable variant. A computed
read carries `specified_met`: whether the requested side was fully met, the verdict a
consumer reads instead of re-deriving it from the deltas.

**SwapRefused**:
The seam's typed refusal, raised with `kind` (a `SwapRefusalKind` member), `word` (the
bitmap word the walk stalled on — fetch arms only, `None` elsewhere), and `amount` (the
signed request the read could not meet: negative = exact input, positive = exact output —
`None` on the fetch arms, which never answered for an amount). Consumers read the typed
attributes; the `(kind, word)` args survive only as the exception's own text. Python's
`swap_refusal_to_error` is the ONE render into the companions' tree — fetch arms become
`MissingLiquidityData`, `NotComputable` becomes `IncompleteSwap` naming the requested
side's amount and zero for the side genuinely unconsumed.
_Avoid_: decoding `exc.args` positionally; comparing a kind by identity (the seam builds a
fresh instance per refusal, so compare by value); fabricating `IncompleteSwap(0, 0)`.

**Caveats**:
The additive flag set on an outcome, whose empty value means the number is exact.
Variants name why a number may be approximate (sparse tick coverage, hooked pool).

## ERC6909 capture

**ERC6909 capture**:
The operator option to take V4 profit as an ERC-6909 claim on the PoolManager instead of
custody WETH, armed per stream with the on-chain floor check.

## Block pump and stage machine

**Block epoch**:
One confirmed block from first delivery to publish plus quiesced settlement — the unit of
per-block work. Rewind reopens an epoch at the reorg block with a fresh sequence,
invalidating pre-rewind solve contexts.
_Avoid_: reintroducing the retired per-block machines' vocabulary (`DrainSink`, `Engine`
as per-block fan-out, `SolveCoordinator`, `DispatchOwner`, `DirtySets`).

**StageMachine**:
The single pure, I/O-free state machine owning every per-block edge condition: header
admission, log routing, reorg window, cursors, WS-completeness, watchdogs, quiesce, and
the publish arm. Stage handlers are thin I/O drivers per stage.

**StageHandlers**:
The pipeline's product/facts seam — the required stage hooks whose outcomes carry the
facts the driver consumes.

**PumpControl**:
The driver-facing control seam — the typed pokes the driver performs on the engine
(dirty-path checks, cursors, block notification, pump end).

**StateView**:
The cheap-read data plane: pool state read through snapshots and views rather than
cloned from the registry.

**degenbot-ingestion**:
The standalone crate owning event ingestion and its watchdogs (log silence, header
staleness); the Python layer never sees raw WS streams.

**Epoch delta ledger**:
The single dirt ledger recording pool-state changes, with exactly one owner.
_Avoid_: the retired subscriber bus (`PoolStateSubscriber`, `notify_pool_state_changed`).

**Driver seam**:
The arb engine's one external interface (`EngineDriver`) — the stage surface both the block
pump and the Python companion cross. Everything else is internal machinery. Distinct from
the host's **Strategy driver** and the submission crate's **Pending-transaction driver**.
_Avoid_: "engine facade" (a facade fronts another surface; there is no second door); using
bare "driver" where a strategy loop, the Python cockpit, or the settlement arm is meant.

**Engine retune**:
The typed operator re-parameterization applied at construction and at runtime.
_Avoid_: "stance" (the fleet-migration concept), "engine config" (the config-schema value
that feeds it).

**Pipeline census**:
The drain path's one owner of the event→metric map (`degenbot-bot/src/instruments.rs`):
every pipeline stage reports happenings as closed **census events** — the ADR-041 stage
table's and the Batch outcome record's own vocabulary — and the census alone decides which
instrument, labels, and buckets answer an event. Metric names, the Grafana/alert couplings
that quote them verbatim, and the None-guard live here and nowhere else; emit sites never
name a metric. The trace-span channel stays separate (ADR-043: one fact per channel).
_Avoid_: naming a metric at an emit site; a per-site `pipeline()` None-guard; a census
event named after the metric it feeds; absorbing trace spans into the census.

## Solve cycle

**Solve cycle**:
The per-block unit that fans affected paths out from the dirt ledger and admits,
resolves, solves, witnesses, drains, and merges them, through one owner over the solve
entrances.
_Avoid_: "solve loop" (the pump's block loop), bare "cycle", "engine cycle".

**Cycle-transient state**:
The state a cycle borrows between blocks — admission verdict, cycle arm, pending new
paths, cursor advance — owned by the cycle rather than smeared across engine fields.

**Epoch context**:
The one value the solve cycle's phases (`shed / resolve / solve / merge / deliver`,
ADR-076) thread: the cycle-transient state plus the status, admission, cache, and
results/config clusters. Test seams are construction-time fields on the cycle, never
methods on its interface.
_Avoid_: `*_for_test` setters on the cycle's public surface; a mutable handle to the
cycle's internal stash crossing the seam.

**Path cost ledger**:
The solve cycle's owner of per-path cost accounting — the pipeline instruments, the
slowest-path retention, the next-cycle bin-packing cost history, and the capture triggers
— behind one record call from the lane walk.
_Avoid_: per-sink fields smeared across the cycle's shared context; accumulators nothing
reads.

**Cycle outcome**:
The typed fact a solve cycle returns: the arm (shed / skipped-empty / solved), the
solved-block coordinate, and the submission counts the stage hooks read.

**Lane walk**:
The one walk function folding each admission bin's items — resolve, clamp, simulate,
envelope stamp — into the solve result pipe. Distinct from the fleet's **Lane** (thread
ownership).
_Avoid_: "dispatch" (the retired grab file), "run_bin".

**Path registry**:
The solve engine's identity module: path/pool relations, the reverse index, signature
dedup, and the path cap. Deliberately shallow — no resolve, no solve.

## Worker fleet

The fleet's execution vocabulary — one meaning per word, closed set.

**Lane**:
A logical ownership lane: who owns which receipts and ledger writes. A lane names
ownership, never a thread.
_Avoid_: "worker pool" for fleet seats; "mode" for binding.

**Seat**:
The runtime a unit of work executes on: pooled seats contend on one shared queue; keyed
solver seats are one-per-bin persistent mailboxes.

**Binding**:
The adapter mapping lanes to threads: pinned, serial, or logical.

**Plan**:
The pure function turning a budget into a binding, the projected tier budget, and the
tier refusal it fell from. No call site decides a tier on its own.

**Budget**:
The seat/share table sum-checked against the core quota, with one owner deriving all
tier projections.

**Census**:
The worker registry: one row per execution resource (thread-name pattern, sizing rule,
count, binding), boot-dumped and exported as a metric.

**Boot registry**:
The one keyed owner of the pooled roles' boot facts, with first-wins canonical
ownership.
_Avoid_: cross-module static reach for a role's boot.

**Intake receipt**:
The submitter's join on a submitted intake unit, held in the backlog until fulfilled —
never dropped.

**Lane-death terminal receipt**:
The typed failure a lane's still-owed paths receive when the lane dies mid-flight: the
outcome ledger stays exact, the posture cordons, the process lives.

**Posture cause**:
Why the fleet cordoned: throttle hysteresis, or a lane death (sticky until a fresh
process).

**runtime_status()**:
The live-process view of the plan, projected budget, and census rows.

**Bot orchestrator**:
`bot_core::bot::Bot` — the per-chain pure-Rust facade that owns the shared
`BotState` handle, the event bus, and the construction-I/O attachment, so the
standalone-Rust bot and the PyBot driver drive the same core.
_Avoid_: confusing it with the `PyBot` adapter (the thin PyO3 shell) or with
`BotState` (the data owner the orchestrator drives).

**Registry core**:
`RegistryCore` — the shared registry struct `BotState` composes: `pools`,
`pool_addresses`, `tokens`, `next_pool_id`, `journal_depth`, and the
ADR-040 `quarantined_pools` set — the one canonical object space every handle
reads through, and the view concentrated-liquidity-orchestration fns receive as their `reg`
parameter.
_Avoid_: "session registry" for it; the `SessionObjectRegistry` is the
identity-only layer, the registry core is the live state.

**Concentrated-liquidity orchestration capability**:
`ConcentratedLiquidityOrchestration` — the one struct owning every concentrated-liquidity-local state surface:
the V3/V4 liquidity event buffers, `v4_pool_ids`, the V4 state-view registry,
the registration gate, the snapshot seed block, the pump delivery cutoff, and
the per-pool event horizons. `BotState` stays the composition root and
delegates its public concentrated-liquidity surface to it one-line-at-a-time; callers that need
only concentrated-liquidity-local state reach the capability directly through `BotState::concentrated_liquidity()` /
`concentrated_liquidity_mut()` over the capability's `pub` method set, capability fns reach the
registry core only through the `RegistryCore` view their callers pass, and
registry-side consumers (the sim-anchor projection, the storage probe) see
V4 registration only through `registered_v4_pools()`.
_Avoid_: reaching into `state.concentrated_liquidity.<field>` from registry-side code or growing
concentrated-liquidity policy outside the capability's method set.
