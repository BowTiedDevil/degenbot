# Adding a strategy family — the landed-seam runbook

The implementer's conductor for adding the next strategy family to the
Phase-C substrate. It is written from the code that exists, not from the design
docs: every signature is copied from the tree, and every claim a future reader
could dispute carries a `file:line` anchor.

Cross-links: [ADR-054](../adr/ADR-054-strategy-plumbing-surface.md) (the frame
evidence seams), [ADR-057](../adr/ADR-057-strategy-host.md) (the strategy host),
and the substrate map in
[strategy-seams.md](strategy-seams.md). The proof-gate mock that exercises this
runbook's host path lives at
`rust/crates/degenbot-submission/tests/mock_third_family.rs`.

## 0. Pick the reaction kind first

A strategy picks exactly one reaction kind (ADR-055; `CONTEXT.md`
"Strategy reaction kinds").

- **Pending-transaction**: implement `PendingTxReaction`
  (`rust/crates/degenbot-strategy/src/pending_tx.rs:68`). Its stages are
  `admit` (`:80`), `discover` (`:99`), `evaluate` (`:111`), `compose` (`:121`),
  `decide` (`:134`), threaded by the neutral artifacts `ComposedIntent` (`:45`)
  and `Decided` (`:56`). `BackrunStrategy` is the reference.
- **Settled-block**: react to sealed blocks through the block pump /
  `StageHandlers` seam. Settlement arbitrage is the only one, and its product
  types are deliberately settlement-shaped; the ADR-018 generalization is
  on-demand (ADR-054, `strategy-seams.md` "Adding a settled-block strategy").

The host is reaction-kind-agnostic: it registers a *driver* (a runnable loop), not
a strategy trait object. `StrategyHost` never names a family
(`strategy_host.rs:1-30`).

**Evidence / Keeps / Retires**

- Evidence: reaction kind is an existing `CONTEXT.md` term, and
  `PendingTxReaction` is the only landed family trait
  (`pending_tx.rs:68`).
- Keeps: the two-kind split and the `admit → discover → evaluate → compose →
  decide` stage vocabulary.
- Retires: describing the strategy unit as a "lane" or "sidecar"
  (`strategy-seams.md:1-10`).

## 0.1 State provisioning — the boot-resolved `StrategyKit`

Pool-state provisioning is a plane capability (ADR-061), not machinery a
strategy builds. The boot resolves one `StrategyKit` per strategy
(`StrategyKit::resolve`,
`rust/crates/degenbot-strategy/src/strategy_kit.rs`) and hands it to the spawn
factory; a strategy composes `kit.provision.ingress` (the one `Db → Chain` V3
tick-map ingress) and `kit.discovery` (frozen registry + startup graph), and
never an ingress it constructed itself.

Tick maps enter the planning sandbox only through the sealed `TickMapSeed`
boundary (`rust/crates/degenbot-bot/src/bot_core/planning.rs`): the `Db` and
`Chain` provenance constructors are crate-private to `bot_core` — the ingress
mints them — while `TickMapSeed::journal` is the public exact-replay
constructor the backrun journal admission owns. A strategy cannot fabricate a
sparse ladder.

Verification is a typed policy on the provisioning cell: `VerifyLevel`
(default `bootstrap`) with `strict` / `off` alternatives. Integrity is distinct
from sampling and **unconditional** — the Tracked self-contradiction abort and
the two-stamp liquidity clock run under every level, so `off` never means
"proceed on a self-contradictory map".

## 1. Admission — the `StrategyHost` FSM

A family is admitted as a driver FSM instance. The operator-facing verbs
(`rust/crates/degenbot-bot/src/strategy_host.rs`):

```rust
pub fn register(&mut self, id: impl Into<StrategyId>, facet: FacetStatus)
    -> Result<(), HostError>;                                  // :707
pub fn enable(&mut self, id: &StrategyId) -> Result<DriverPose, HostError>; // :735
pub fn start(&mut self, id: &StrategyId) -> Result<DriverPose, HostError>;  // :756
pub fn halt(&mut self, id: &StrategyId, detail: impl Into<String>)
    -> Result<(), HostError>;                                  // :774
pub fn disable(&mut self, id: &StrategyId) -> Result<(), HostError>;        // :796
pub fn state_of(&self, id: &StrategyId) -> Option<DriverPose>;              // :974
```

`DriverPose` (`:41`) is the authoritative lifecycle:

```text
Registered ──enable()──► Enabled ──start()──► Running ──stop()──► Stopped
                                                  │
                                                  └──halt()──► Halted
Registered/Enabled/Running/Stopped ──disable()──► Disabled
```

- Operator verbs are `register`, `enable`, `disable`; driver-originated moves are
  `start` and `halt`. The transition functions are `on_enable` (`:92`),
  `on_start` (`:105`), `on_halt` (`:118`), `on_stop` (`:132`), `on_disable`
  (`:145`), each returning a typed `FsmDecline` (`:68`) rather than panicking.
- `Halted` and `Disabled` are terminal tombstones: `is_terminal` (`:61`),
  `disable` refuses them (`DisableRejectsTerminal`), and nothing restarts a name
  but a fresh process.
- `Stopped` is **not** terminal: it is where a loop that returned cleanly lands,
  and the record is still disableable. The fold that produces it is
  `record_driver_exit` (`:926`) reached only through `drive_and_fold` (`:954`)
  or `HostSupervisor` (`:993`), so no caller re-implements the await-then-fold
  ritual.
- Enabling an unknown name is `HostError::UnknownStrategy`; a registered name
  with no booted facet is `HostError::UnconfiguredStrategy`
  (`FacetStatus` `:260`, `HostError` `:304`).

**Evidence / Keeps / Retires**

- Evidence: `DriverPose` at `strategy_host.rs:41`; the Python-facing vocabulary
  is mapped in `rust/crates/degenbot-python/src/bot/engine/strategy.rs:30-40`.
- Keeps: the operator state names `registered`/`enabled`/`running`/`stopped`/
  `halted`/`disabled`, and the frozen-tombstone rule.
- Retires: `DriverState` as a type name (it survives only in ADR-057 prose);
  `DriverExit::Stopped` as an untyped no-op; auto-restart of a tombstone.

## 2. The runtime edge — spawn factory and driving

A family that owns a loop attaches a once-only factory and lets the host boot it:

```rust
pub fn attach_spawn(&mut self, id: &StrategyId, spawn: DriverSpawnFactory)
    -> Result<(), HostError>;                                  // :845
pub fn start_driving(&mut self) -> Result<Vec<DriverTask>, HostError>; // :879
pub async fn drive_and_fold(&mut self, task: DriverTask) -> Result<(), HostError>; // :954
```

`DriverSpawnFactory` (`:486`) is a `FnOnce(Option<LaneNamespace>) -> DriverFuture`
(must be `Send`); `DriverFuture` (`:388`) is a non-`Send` pinned future, because a
lane's replay stack is single-threaded. `start_driving` polls each enabled,
factory-bearing driver under the **ambient multi-thread runtime**
(`degenbot_core::runtime::get_runtime`) via `spawn_blocking` + `block_on`, then
moves the record to `Running` (`:879-925`). An enabled driver with no factory is
skipped, not failed — the settlement pump arm is its own driver.

**Evidence / Keeps / Retires**

- Evidence: factory/host split at `strategy_host.rs:845-925`; the ambient-runtime
  reason is pinned by
  `rust/crates/degenbot-submission/tests/hosted_driver_ambient_runtime.rs`.
- Keeps: "the host decides which drivers run; a driver owns its loop".
- Retires: booting a driver inline on a dedicated current-thread runtime (it
  fails `WrapDatabaseAsync::new`).

## 3. The nonce lane — sign-time issuance over one authority

There is **no** `NonceAuthority::lane_for`. The landed seam binds a lane with
`NonceLane::new` (`rust/crates/degenbot-submission/src/submission_ledger.rs:362`):

```rust
pub fn new(authority: Arc<NonceAuthority>, ledger: Arc<SubmissionLedger>,
           strategy: impl Into<StrategyId>) -> Self;
pub fn stamp(&self) -> Result<NonceLease, DeclineKind>;        // :404
pub fn record_signed(&self, lease: &NonceLease, target: TargetId,
                     bundle_hash: B256, built_at_head: u64) -> Result<(), LedgerDecline>; // :428
pub fn release(&self, lease: &NonceLease) -> Result<u64, DeclineKind>; // :450
pub fn release_broadcast(&self, nonce: u64) -> Result<u64, DeclineKind>; // :462
pub fn observe_chain_nonce(&self, confirmed: u64);             // :474
```

`stamp()` is the only issuance entry. It calls `NonceAuthority::lease`
(`rust/crates/degenbot-bot/src/nonce_authority.rs:295`), which returns the
**lowest free nonce at or above the confirmed chain nonce** so leases and
broadcasts stay a contiguous prefix. One outstanding lease per strategy
(`DeclineKind::StrategyLeaseOutstanding`); a tracked reservation below the
confirmed nonce is `DeclineKind::BelowChainNonce` (`nonce_authority.rs:69`).
`release_lease` (`:351`) frees only the lane's own stale lease;
`release_strategy` (`:365`) clears leases **and** broadcasts and belongs to the
host's halt/disable edges; `release_broadcast` (`:400`) is the monitor-expiry escape
hatch that re-opens a broadcast that will never land. Head moves enter the
authority through `set_confirmed_reorg` (`:247`) only — a forward move lands
confirmed broadcasts, a rewind restores reorged broadcasts and returns
`ReorgAdvisory`s.

**Evidence / Keeps / Retires**

- Evidence: `NonceLane::new`/`stamp` at `submission_ledger.rs:362,404`; the
  contiguity proptest in `nonce_authority.rs` tests; the two-lane probe in
  `submission_ledger.rs` `two_lanes_one_outstanding_each_never_share_a_nonce`.
- Keeps: `stamp()` as the one sign-time entry; lowest-free contiguous
  reservation; typed declines.
- Retires: `NonceSource` (the deleted `Dispatcher { start } | Authority(lane)`
  switch — see `tests/nonce_issuer_unified.rs`); a private reservation table;
  Python-computed operator nonces.

## 4. The ledger attach and typed head notices

The per-strategy ledger is the head feed's submission-truth arm. The boot
attaches it to the host with a trait object, because `degenbot-bot` cannot name
the submission crate:

```rust
pub fn attach_reconciler(&mut self, reconciler: Arc<dyn HeadReconciler>); // strategy_host.rs:623
pub fn subscribe_head(&self, id: &StrategyId, sink: UnboundedSender<HeadNotice>)
    -> Result<(), HostError>;                                  // :632
pub fn on_head(&self, confirmed: u64) -> Vec<HeadNotice>;      // :673
```

`SubmissionLedger` implements `HeadReconciler` (`submission_ledger.rs:541`);
`reconcile` (`:673`) closes each non-terminal record against `(confirmed,
outstanding)`:

| Record position | Classification | `StrategyNotice` |
|---|---|---|
| `nonce < confirmed` | Landed | `Landed` |
| `nonce >= confirmed` and not outstanding | Stale | `Stale` |
| outstanding, immediate predecessor neither confirmed nor outstanding | Orphaned | `Orphaned { fillable_nonce }` |

`on_head` is the one head entry: it refreshes the authority reorg-safely, asks
the reconciler to reconcile, and delivers each typed `StrategyNotice`
(`strategy_host.rs:397`) only to the owning strategy
(`a_head_update_delivers_each_notice_only_to_its_owner`). The default reaction is
`HeadPolicy::on_notice` (`submission_ledger.rs:527`) → `PolicyAction` (`:483`):
re-stamp on `Orphaned`/`LeaseRevoked`, re-evaluate on `Stale`, retire on
`Landed`. A family feeds its own delivered notices through the policy; the
re-bid itself stays with the caller.

**Evidence / Keeps / Retires**

- Evidence: `impl HeadReconciler for SubmissionLedger` at
  `submission_ledger.rs:541`; the host delivery test at `strategy_host.rs` tests;
  the in-process two-strategy integration
  `two_strategies_drive_lowest_free_repackage_and_orphan_fill`.
- Keeps: one writer (`on_head`) with two value-identical consumers (return vec +
  per-strategy sinks).
- Retires: a driver calling `reconcile`/`set_confirmed_reorg` on its own and
  discarding the notices (the round-2 F2 residual).

## 5. Hub subscription — the typed reaction

There is **no** `ReactionKind::EachPoll`. The typed reaction vocabulary is the
hub class plus its declared overflow policy
(`rust/crates/degenbot-eventhub/src/`):

- `HubClass` (`event.rs`): `NewHead | PoolEvent | PendingTx`.
- `OverflowPolicy` (`policy.rs`): `DropOldestCounted { name } | LatestOnly |
  UnboundedFlagged { name }` — declared once per source at
  `Hub::register_source`, visible via `Hub::policy_of`.
- `Hub::subscribe(class) -> Result<Subscription, HubError>` and the head clock
  `Hub::subscribe_head() -> Result<HeadSubscription, HubError>` (`hub.rs`).

The per-poll tick a strategy awaits is `HeadSubscription::changed()`
(`head.rs`); `head()`/`borrow_and_update()` read the latest. A family whose hub
tick arrives **without** a mempool stream subscribes to the head class and never
registers `HubClass::PendingTx`; the mock pins exactly that
(`tests/mock_third_family.rs`). Pending-transaction strategies consume the
`PendingTx` drop-oldest ring the feed registers on the hub.

**Evidence / Keeps / Retires**

- Evidence: the three `HubClass` variants and three `OverflowPolicy` variants;
  the `NewHead`/`LatestOnly` registration in `Hub::register_head_source`.
- Keeps: registration declares strictness once; consumers receive a policy-typed
  subscription; the hub is transport-pure.
- Retires: a per-call policy choice; an untyped "reaction kind" enum; treating
  the head tick as requiring a mempool source.

## 6. The three driver partitions (boot / loop / policy)

A family's driver is a facade over three partitions, by invariant
(`rust/crates/degenbot-strategy/src/backrun_driver.rs:1-42`, the three-way split):

- `driver_boot` (`driver_boot.rs`) — the boot handoff and resolvers. It never
  owns process boot. Entry points: `backrun_boot(...)` (`:227`),
  `backrun_spawn_factory(...)` (`:264`), `resolve_backrun_registry` (`:139`),
  `resolve_backrun_host_registry` (`:186`), `resolve_backrun_node_join` (`:78`).
- `driver_loop` (`driver_loop.rs`) — the loop and every runtime surface it
  touches: `BackrunDriver::start(...)` (`:1114`) returns a `DriverHandle`
  (`:1047`) driving `LoopPhase` (`:924`). The loop consumes the host-minted hub,
  registry, and node join without owning them.
- `driver_policy` (`driver_policy.rs`) — the bid's economics: price reads,
  relay fan-out, bundle target. Pure reads/derivations, never lifecycle moves.

The facade `backrun_driver.rs` is 42 lines: module doc + `mod` declarations +
`pub use` re-exports, so `degenbot_strategy::backrun_driver::<item>` never
moves. A new family mirrors this shape: a boot module that resolves its
artifacts and mints a spawn factory, a loop module owning only its own runtime
state, and a policy module for its economics.

**Evidence / Keeps / Retires**

- Evidence: the facade is `backrun_driver.rs:1-42`; the partition headers state
  each module's invariant surface.
- Keeps: `backrun_boot`/`backrun_spawn_factory`/resolver names and the
  standalone-vs-hosted shared resolver.
- Retires: `lane_root` (now `namespace_root`), `LaneBoot` (now `LoopBoot`),
  `DriverLifecycle`/`LifecycleDecline`/`LifecycleShared` (now
  `LoopLifecycle`/`LoopDecline`/`LoopShared`) — all renamed by the vocabulary
  sweep without changing crate-public reach.

## 7. Config — one facet per family

A family's operator surface is a typed schema facet, declared once
(`rust/crates/degenbot-config/src/schema.rs`):

```text
strategy StrategyConfig {
    settlement StrategySettlementConfig {}                      // :363
    mevblocker_backrun StrategyMevblockerBackrunConfig { ... }  // :369
    peer_backrun StrategyPeerBackrunConfig { ... }              // :407
}
```

Each facet's `active` key is its activation; there is no single-arm
`strategy.name` selector (it is deliberately undeclared, pinned by
`strategy_arm_selector_is_retired`). To add a family: add a `StrategyName`
variant in `degenbot-strategy/src/strategy_plane.rs`, add a `StrategyXConfig`
facet struct with its keys, add the `strategy.<name>` row, and extend the schema
tests (`strategy_facets_are_declared_as_typed_sections`,
`strategy_activation_keys_parse_and_collapse_to_defaults`). The boot sets
`FacetStatus::{Configured, Unconfigured}` from the facet so `enable` fails loudly
on a name the config never configured.

**Evidence / Keeps / Retires**

- Evidence: the facets are declared exactly once
  (`strategy_facets_are_declared_as_typed_sections`); the retired
  `strategy.name` selector is pinned undeclared
  (`strategy_arm_selector_is_retired`).
- Keeps: one declaration site; no strategy-scoped ad-hoc env reads outside the
  schema.
- Retires: parallel env-only strategy selection; the single-arm selector.

## 8. Python thinness rules

The PyO3 layer translates, never decides
(`rust/crates/degenbot-python/src/bot/engine/strategy.rs:1-20`):

- Every admission/configured-ness/FSM decision is the Rust host's. A typed
  refusal maps to a typed exception (`map_host_error`, `:42`).
- The cockpit session phase is the host's `SessionPhase` table
  (`strategy_host.rs:169`), exposed as `session_phase_next` (`:59`); Python's
  `_Phase` translates the verdict and never authors legality (the session-phase consolidation
  surface).
- The Python driver shell's engine surface is declared once as
  `ENGINE_SEAM_MEMBERS` (`tests/fakes/engine.py:30`) and parity-bound against the
  real engine and the `.pyi` stub by
  `tests/arbitrage/test_engine_fake_parity.py`. A new Python surface is added
  only when a consuming site needs it; the fake and stub move in lockstep.

**Evidence / Keeps / Retires**

- Evidence: module doc at `strategy.rs:1-20`; `FakeEngine` (`engine.py:115`) and
  the parity test.
- Keeps: "Rust is the engine; Python is a driver shell".
- Retires: Python-derived FSM legality (`_Phase` if-chains); five divergent
  private engine doubles (one shared fake now).

## 9. The test-surface pattern

Pin every seam a family depends on at its own level:

- **Name-level pins** (`include_str!` textual scans): `tests/nonce_issuer_unified.rs`
  asserts `NonceSource` never reappears and every signing path stamps through a
  `NonceLane`.
- **Observability-name pins**: `tests/trace_default.rs` and
  `tests/trace_explicit.rs` pin emitted trace names.
- **Behavioral integration**: module `#[cfg(test)]` tests own the FSM tables and
  the host/ledger round-trips (`strategy_host.rs` tests,
  `submission_ledger.rs` `two_strategies_drive_lowest_free_repackage_and_orphan_fill`).
- **The proof-gate mock**: `tests/mock_third_family.rs` registers, enables, runs,
  and stops a mock family through the real host/seam path with no production
  edits, exercising admission declines, lane contiguity, ledger classification,
  the head tick without a mempool, and the session-stop surface.

**Evidence / Keeps / Retires**

- Evidence: `nonce_issuer_unified.rs`; `trace_default.rs`/`trace_explicit.rs`.
- Keeps: one fake bound to the real surface; textual + behavioral pins.
- Retires: unbound private doubles; a seam pinned in only one language.

## 10. Worked checklist

1. Pick the reaction kind (§0).
2. Add the config facet + `StrategyName` variant + schema test (§7).
3. Implement the family body (`PendingTxReaction` or settled-block stage).
4. Add the driver partitions: a boot resolver + spawn factory (§6).
5. Bind the lane: `NonceLane::new(host.nonce().clone(), ledger.clone(),
   StrategyId::new("<name>"))` (§3).
6. Register the facet and attach the spawn in the boot (§1, §2).
7. Subscribe head notices and feed them through `HeadPolicy` (§4).
8. Pin the seams, including a mock-family integration test (§9), and update the
   engine fake parity if the Python surface moved (§8).

## Appendix — seam inventory

| Seam | File:line | Signature |
|---|---|---|
| Host register | `strategy_host.rs:707` | `register(id, FacetStatus) -> Result<(), HostError>` |
| Host enable | `strategy_host.rs:735` | `enable(&id) -> Result<DriverPose, HostError>` |
| Host start | `strategy_host.rs:756` | `start(&id) -> Result<DriverPose, HostError>` |
| Host halt | `strategy_host.rs:774` | `halt(&id, detail) -> Result<(), HostError>` |
| Host disable | `strategy_host.rs:796` | `disable(&id) -> Result<(), HostError>` |
| Host attach spawn | `strategy_host.rs:845` | `attach_spawn(&id, DriverSpawnFactory)` |
| Host driving edge | `strategy_host.rs:879` | `start_driving() -> Vec<DriverTask>` |
| Host stop fold | `strategy_host.rs:954` | `async drive_and_fold(DriverTask)` |
| Host reconciler attach | `strategy_host.rs:623` | `attach_reconciler(Arc<dyn HeadReconciler>)` |
| Host head entry | `strategy_host.rs:673` | `on_head(confirmed) -> Vec<HeadNotice>` |
| Session phase table | `strategy_host.rs:169` | `SessionPhase::{on_start,on_run,on_query,on_shutdown}` |
| Authority lease | `nonce_authority.rs:295` | `lease(&StrategyId) -> Result<NonceLease, DeclineKind>` |
| Authority reorg head | `nonce_authority.rs:247` | `set_confirmed_reorg(confirmed) -> Vec<ReorgAdvisory>` |
| Lane bind | `submission_ledger.rs:362` | `NonceLane::new(authority, ledger, strategy)` |
| Lane stamp | `submission_ledger.rs:404` | `stamp() -> Result<NonceLease, DeclineKind>` |
| Ledger reconcile | `submission_ledger.rs:673` | `reconcile(confirmed, outstanding) -> Vec<Notification>` |
| Head policy | `submission_ledger.rs:527` | `on_notice(&HeadNotice) -> Result<PolicyAction, DeclineKind>` |
| Hub subscribe | `degenbot-eventhub/src/hub.rs` | `subscribe(HubClass) -> Result<Subscription, HubError>` |
| Hub head subscribe | `degenbot-eventhub/src/hub.rs` | `subscribe_head() -> Result<HeadSubscription, HubError>` |
| Driver boot | `backrun_driver/driver_boot.rs:227` | `backrun_boot(...) -> BackrunBoot` |
| Driver factory | `backrun_driver/driver_boot.rs:264` | `backrun_spawn_factory(...) -> DriverSpawnFactory` |
| Driver loop | `backrun_driver/driver_loop.rs:1114` | `BackrunDriver::start(...) -> DriverHandle` |
| Config strategy section | `degenbot-config/src/schema.rs:362` | `strategy StrategyConfig { settlement, mevblocker_backrun, peer_backrun }` |
| Engine fake parity | `tests/arbitrage/test_engine_fake_parity.py` | binds `ENGINE_SEAM_MEMBERS` to real engine + stub + fake |

## Discrepancies between the brief and the landed seams (Fistle log)

The proof gate found no step that required a production edit; the mock compiles
and passes against the public API as-is. Four names in the task brief do not
exist in the tree; the runbook documents the landed equivalent rather than
inventing them:

| Brief name | Landed shape | Disposition |
|---|---|---|
| `ReactionKind` / `EachPoll` | `HubClass` + `OverflowPolicy` typed at `Hub::register_source`; per-poll tick is `HeadSubscription::changed` | Documented as the typed reaction (§5); no production change needed. |
| `NonceAuthority::lane_for` | `NonceLane::new(authority, ledger, strategy)` (`submission_ledger.rs:362`) | Documented as the lane constructor (§3). |
| `DriverState` | `DriverPose` (`strategy_host.rs:41`) | Documented as `DriverPose` (§1); `DriverState` is retired prose. |
| `persist_observables` (test) | Observability-name pins `tests/trace_default.rs`, `tests/trace_explicit.rs` | Documented as the observability pin pattern (§9). |

If a future family needs a typed `ReactionKind` enum or a `persist_observables`
pin, that is a new shape, not an existing seam; the patch belongs in its own
change with the mock updated to exercise it.
