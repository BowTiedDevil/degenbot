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
_Avoid_: describing this bot's mechanism as "backrun"; "victim transaction" framing.

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

**Pending-transaction driver**:
The `degenbot-strategy` driver owning the strategy-neutral loop around a strategy's
stages: pending-tx replay (ADR-054 seam 1), journal extraction (seam 2), the bundle-sim
gate, timings/tracing, liveness, and submission.

**MarketContext**:
The process-lifetime shared caches for pending-transaction strategies — the connector
index + DFS graph, token id/address joins, and the warm code cache
(`degenbot-strategy/src/market_context.rs`). Substrate, never strategy identity.
_Avoid_: "StrategyRuntime" (retired).

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

**HeadPolicy**:
The default per-driver reaction to a typed head notice: re-stamp on an orphaned
submission or a revoked lease, re-evaluate at the decide stage on a stale one, retire on
a landed one.
_Avoid_: "retry policy", "rebroadcast policy".

## System layers

**Rust core**:
The pyo3-free crates under `rust/crates/degenbot-*` owning all state, math, I/O
orchestration, simulation, and submission — everything a standalone pure-Rust MEV bot
needs.

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
The cockpit's owner of the registration memo: the hop-identity key, the typed
stable-vs-transient build refusal, and the bounded outcome vocabulary behind the metric
tags.
_Avoid_: "skip gate", "class-name refusal set".

**Session state**:
The cockpit's one owner of a session's coordination state (dispatcher, sim context,
current block, provider, credentials).
_Avoid_: "session dict", "cockpit config".

**Session watch**:
The cockpit's one owner of a session's end-state: the typed watch-set transitions
(``_WatchSet`` / ``on_task_done``), the end-verdict ranking, and teardown.
_Avoid_: "await loop", "fail-fast wrapper".

## Pool registration lifecycle

Canonical phases for the CL (V3/V4) registration verify lifecycle:
`Quarantined → drain + verify → Live`.

**Registration lifecycle**:
The per-pool state a registered CL pool occupies: a Sparse pool is always `Live`; a
Tracked pool is `Quarantined` until its verification passes.

**Quarantined**:
A registered CL pool whose live events are deferred to the pump buffer until registration
verification completes. A Quarantined pool is not solvable.

**Live**:
The steady-state direct-apply contract, and the only solvable state.

**Tracked** (a `PoolTickCoverage`):
A pool whose snapshot provided complete tick data, so solver results are trustworthy.
Registers `Quarantined`; must pass verification before `Live`.

**Sparse** (a `PoolTickCoverage`):
A pool for which no complete tick data exists, so solver results may be inaccurate.
Registers `Live` immediately and is never verified.

**Known bitmap word**:
A tick-bitmap word whose whole tick set has been established by a checked source (sparse
fetch, full-sync replace, snapshot tick keys, or explicit update input). Exists only on
Sparse pools; the on-disk bitmap itself is derived from the pool's tick rows.

**Checked-empty word**:
A checked bitmap word that holds no initialized ticks, kept present-but-zero. A word
*absent* from a bitmap snapshot is indeterminate on a Sparse pool (fetch before use) and
known-empty on a Tracked pool (the map is complete).

**Snapshot seed**:
The registration-time (pinned) tick data captured for a Tracked pool, verified exactly
once against on-chain state at the snapshot block.

**Last complete block**:
The highest block the pump has fully delivered, tombstoned by the first `removed:false`
log of the next block. A registration's state application may not advance past it.

**Verify lifecycle**:
The per-pool choreography — quarantine, seed verification, drain, post-drain
verification, live — plus its block-resolution and config-gating policy, owned by the
Rust core.

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
**State tripwire**, which remains live. Do not revive this term for new gates.

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

**Concentrated liquidity (CL)**:
A family holding slot0 scalars plus per-tick data, with partial-prior block deltas
(V3, V4).

**Hop**:
The solver's snapshot-and-classifier adapter observed at resolve time: it captures pool
state (a selective projection for CL) so the solve runs lock-free off a copied value, and
its variants let the solver pick the algorithm from path composition. Not a pool concept
and not math-leaf vocabulary.

**Solve anchor**:
The block at-or-above the pool-state head that a block's solve, verification, and
simulation run against. A hop whose price clock runs ahead of the anchor is *future* and
never legitimate.

**Pool-state head**:
The maximum `update_block` across all registered pools — the state clock. During a
backfill/drain desync it can run ahead of the pump's header clock; the solve anchor takes
the max of the two.

## Piecewise CL solving

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
The precomputed prefix of per-word-boundary swap steps for a dense CL range, where the
boundary list, entry state, and fee are fixed — converts a sim query into a partition
search plus one live landing step. Built once per range, shared by the walk across paths.
_Avoid_: "caching table", "profile cache".

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

## Profit envelope

**Profit envelope**:
A piecewise-linear concave upper bound on a hop's output curve, derived from projection
data the solve already builds. Extending a single piece's validity window is not a sound
bound.

**Envelope gate**:
The pre-solve skip test over the chained path bound: when the bound's best possible gain
is below the profit floor, the path is provably unprofitable and is skipped without a
single simulation. Distinct from the per-hop direction viability gate.

**Envelope verdict**:
The gate's typed outcome: `Bound` (a sound bound) or `Unsupported` (none derivable).
Unsupported paths are solved unscreened, never skipped.
_Avoid_: overloading a bare `None` to mean both unsupported and unprofitable.

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
operator's declared axes. It is the contract the CL overfeed-clamp invariant attaches to.
_Avoid_: "command stream" (the bytes it encodes into), "payload", "EncodeOptions".

**Encode context**:
The session-scoped bundle of deployment addresses shared by every encode request in a
session.
_Avoid_: folding it into the encode request (session scope restated per path).

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

**SwapRead**:
The typed outcome (`Computed` / `NotComputable` / `FetchFailed` / `FetchExhausted`).
No silent zero — a former silent-failure mode is always an observable variant.

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

## Solve cycle

**Solve cycle**:
The per-block unit that fans affected paths out from the dirt ledger and admits,
resolves, solves, witnesses, drains, and merges them, through one owner over the solve
entrances.
_Avoid_: "solve loop" (the pump's block loop), bare "cycle", "engine cycle".

**Cycle-transient state**:
The state a cycle borrows between blocks — admission verdict, cycle arm, pending new
paths, cursor advance — owned by the cycle rather than smeared across engine fields.

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
