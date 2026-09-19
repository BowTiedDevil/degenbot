# ADR-057: The strategy host — one process, many drivers over one operator account

**Status: accepted** (2026-09-19). Records the landed dynamic host (commits
184b800fe, 8015f23f3, b9c8d8954, 73d902f9d, 971917465, 6e057be05). Basis:
ADR-055 D5's scheduled Phase C, the Phase B substrate (ADR-056 and the
hub/registry slices), and the executed design
`.scratch/strategy-arch-survey/phase-c-design.md`. The crate sources remain the
last word on what the code does today.

## Context

Phase B left a single-strategy process with a healthy shared substrate:
`degenbot-eventhub` owns per-process intake fan-out, a boot-snapshot
`RouteRegistry` answers pool membership, and the gated serving seam is retired
(ADR-056). No layer owned governance over more than one strategy in one
process:

- `EngineDriver` minted and owned its own hub, and the engine's `ResultBatch` /
  `BlockNotification` were once-only receiver slots — a second driver could not
  attach to the same hub.
- Each driver scanned and reserved operator-account nonces privately. Two
  drivers on one EOA is a correctness defect: two independent reservation
  tables can sign the same nonce.
- There was no runtime admission for a strategy: `strategy.name` selected
  exactly one arm at boot, and nothing could register, enable, or disable at
  runtime.
- Submission outcomes (landed / stale / orphaned) had no per-strategy record
  and no typed delivery to the owning driver.

ADR-055 D5 recorded the dynamic host as scheduled, not speculative. Phase B had
made the mechanics cheap; this decision adds the governance on top and lands
it.

## Decision

### D1 — `StrategyHost` owns the shared services; drivers attach

`StrategyHost` (`degenbot-bot/src/strategy_host.rs`) is the per-process owner of
the hub, the `RouteRegistry`, and the `NonceAuthority`. It is generic over
strategy types and never names a strategy family; it loans the shared handles
out by `Arc`, registers drivers by name, and drives their lifecycle. A
**driver** is a strategy family's runnable loop (`BackrunDriver` today); the
**host** decides which drivers may run. Vocabulary is host / driver / strategy;
"sidecar" names the standalone two-process deployment, not a host member.

### D2 — The driver lifecycle is a frozen-tombstone FSM

`DriverState`: `Registered → Enabled → Running → {Halted, Disabled}`. Operator
verbs are `register`, `enable`, `disable`, `list`; driver-originated moves are
`start` and `halt`. `Halted` (a self-halt) and `Disabled` (operator) are
terminal tombstones with no exit edge — no auto-restart. Every illegal move
answers with a typed `FsmDecline`, never a panic; `enable` on an unknown name is
`HostError::UnknownStrategy`, and on a known-but-unconfigured facet is
`HostError::UnconfiguredStrategy`. A halted driver stores its `halt_detail`; a
terminal state is a frozen record that `list` reports but nothing resurrects.

`start_driving` boots every enabled driver that registered a
`DriverSpawnFactory`, hands it the driver's `LaneNamespace`, and returns a
`DriverTask`; a driver-task panic is folded by the lane boundary into a
`DriverExit::Halted` tombstone rather than reaching the host process. A facet
may legitimately register no factory — the settlement pump arm is its own
driver — and is skipped rather than failed.

### D3 — The hub is hoisted; the driver attaches via a bound pair

`Hub::add_named_unbounded_source` is `&mut self`, so the engine's named channels
must be registered before the hub is shared. `StrategyHost::mint(registry,
nonce, register: impl FnOnce(&mut Hub) -> T)` mints the hub, hands the closure
the exclusive `&mut Hub`, wraps it, and returns `(StrategyHost, HostHub<T>)`
where `HostHub<T> { hub, attachment: T }`. `EngineDriver::from_stages_with_hub`
consumes that pair; `from_stages` keeps its signature and mints a private hub
through the same `assemble` body, so the standalone and Python paths are
byte-identical.

The pair exists so channels minted on one hub cannot be attached to another by
accident: a mis-attach would be a silent dead stream. A caller can still pass a
forged closure, so the guarantee is a documented convention on `mint`, not a
type-level one.

### D4 — NonceAuthority: sign-time issuance, lowest-free, contiguity, typed declines

The `NonceAuthority` (`degenbot-bot/src/nonce_authority.rs`) is the host's single
owner of the operator account's nonce space.

- **Sign-time.** The only issuance path is `lease(strategy)`; there is no
  background allocator. A driver simulates first and stamps the signed
  transaction against the leased nonce, so the authority is queried at the last
  moment before signing.
- **Lowest-free.** `lease` returns the lowest nonce not currently outstanding
  at or above the confirmed chain nonce, which makes the outstanding set a
  **contiguous prefix** above the chain nonce; a freed gap self-heals at the
  next sign, with no artificial self-send to fill it.
- **At most one outstanding lease per strategy (v1).** A second `lease` for a
  strategy holding one declines `StrategyLeaseOutstanding`, matching both
  existing arms' behavior.
- **Typed declines, never panic.** `DeclineKind::{StrategyLeaseOutstanding,
  BelowChainNonce, UnknownLease, Exhausted}`. `BelowChainNonce` is the
  fail-closed guard on the contiguity invariant: a tracked reservation below
  the confirmed chain nonce is a contradiction to release, not sign around.
- **`release_lease` vs `release_strategy`.** The repackage path releases only
  the strategy's own stale lease (`release_lease`); `release_strategy` clears
  leases *and* broadcasts and is reserved for enable / disable / halt, because a
  broadcast may be genuinely in flight.
- **Reorg-safe head.** `set_confirmed_reorg(confirmed)` is the only head entry
  point: a forward move advances the confirmed nonce, a backward move (a reorg
  rewind) restores every broadcast confirmed inside the rewound window
  `[confirmed, old)` to the outstanding set and returns a typed `ReorgAdvisory`
  per revoked lease. `set_confirmed` retains confirmed broadcasts in an
  owner-keyed `landed` index so the restore is possible; the index is never
  pruned and survives a tombstone, because a released strategy's transaction
  may still be on the wire.

### D5 — Submission ledger, notifications, and HeadPolicy actions

`SubmissionLedger` (`degenbot-submission/src/submission_ledger.rs`) is the
per-strategy record of what happened to each signed submission:
`SubmissionRecord { nonce, target, bundle_hash, built_at_head, state }` with the
closed FSM `Signed → Broadcast → {Landed, Stale, Orphaned}` (terminal).

- **Reconciliation is nonce-level.** Given the chain's confirmed nonce and the
  authority's outstanding set, `reconcile` closes each record to `Landed` (nonce
  below confirmed), `Stale` (nonce left the outstanding set without landing), or
  `Orphaned` (the immediate predecessor was vacated while this record stayed
  outstanding). The orphan test is deliberately immediate-predecessor only:
  with a gap at `n-1`, only `n` is the natural filler. Outcomes certify the
  nonce slot, not the exact bytes — a record signed but never broadcast can
  reach `Landed` through other account traffic.
- **Notifications are typed and addressed.** One `Notification` per state
  change, keyed by nonce and addressed to the owning strategy; ordering is
  deterministic (strategy name, then nonce). `Orphaned { fillable_nonce }`
  converts to a `RepackageRequest` advisory — a recommendation to re-stamp at
  the vacated predecessor, never an automatic submission.
- **The host owns delivery via a trait.** `degenbot-bot` cannot name the ledger
  (the submission crate depends on `degenbot-bot`), so `StrategyHost` holds
  `Arc<dyn HeadReconciler>`; `SubmissionLedger` implements it, mapping reconcile
  outcomes into the host's `StrategyNotice` vocabulary (`Landed`, `Stale`,
  `Orphaned`, and the reorg rewind's `LeaseRevoked`).
  `StrategyHost::on_head(confirmed)` refreshes the authority through
  `set_confirmed_reorg`, asks the reconciler to reconcile against the refreshed
  outstanding set, and delivers each notice only to the owning strategy.
- **`HeadPolicy`** is the default v1 reaction to a typed notice:
  `Restamped { nonce }` on `Orphaned` / `LeaseRevoked`, `Reevaluate` on `Stale`
  (the lane's decide stage re-bids or drops), `Retired` on `Landed`. The re-bid
  — economics re-check and fresh sign — stays with the caller.
- **One nonce seam for both submission paths.** `NonceSource` is the explicit
  switch in `dispatch_and_submit`: `Dispatcher { start }` keeps a standalone
  driver's private reservation table, `Authority(lane)` stamps through the host
  authority and records the signed / broadcast transitions. `NonceLane` binds a
  strategy to the authority and ledger, and `stamp()` is its sign-time entry,
  with the default repackage-on-decline loop.

### D6 — A driver's run artifacts live under its own namespace

`LaneNamespace::under(state_root, id)` is `<state_root>/<strategy>` with
`session/` and `quarantine/` subdirectories. A hosted driver receives its
namespace at the driving edge; the standalone sidecar passes `None` and keeps
the process-global journal root. This is what lets two drivers on one host write
run artifacts without colliding; the host stays free of the submission crate's
file vocabulary.

### D7 — Deferred, deliberately

- **Shadow-feedback between drivers.** v1 is exclusion-only: reconciliation
  records the nonce-level shadow (the strategy's exact bytes may not be the
  transaction that consumed the slot) as `ShadowPosture::ObservedNotActedOn`
  and makes no decision from it. Feeding the shadow back to a driver's decide
  stage — win / accept-the-shadow, repackage, or wait — is a post-v1 revisit
  triggered by forensics.
- **More than one outstanding lease per strategy.** v1 binds at most one,
  matching both arms today; multi-outstanding candidates are a post-v1 revisit
  that depends on the shadow-feedback design.
- **Process-level submission arbitration.** Drivers submit independently;
  contention at the relay is observed through existing submission records and
  telemetry, with no in-process arbiter until live byte evidence says drivers
  collide meaningfully.
- **Lane hot registration.** The host can register only strategies the config
  named at boot; registering a driver the config never named is out of v1.
- **The ADR-018 engine-family generalization.** The host landed over the
  existing `EngineDriver`; parameterizing settled-block stage payloads and
  per-family fleet globals stays on-demand (ADR-055 D5). The host was the
  load-bearing half; the generalization is not pulled by a sample of one.

## Consequences

- One process can run settlement arbitrage (via its pump arm) and backrun (as a
  hosted driver) over one operator account, with one hub, one boot-snapshot
  registry, and one nonce authority. Adding a driver is one strategy impl, one
  spawn factory, one config facet, and a registration.
- The operator gains `enable_strategy` / `disable_strategy` / `strategies` on
  the engine adapter; unknown and unconfigured names raise typed errors
  (`UnknownStrategyError`, `UnconfiguredStrategyError`, base
  `StrategyHostError`).
- The standalone sidecar deployment is not removed and stays green:
  `from_stages` still mints a private hub, `BackrunContext.lane_root = None`
  keeps the process-global root, and the settlement-only Python boot is
  observably unchanged.
- Residuals recorded with the landings: the Python boot still hands the host an
  empty route registry (the hosted driver's discovery fan waits on boot-DB
  wiring), and the live per-head feed drives `on_head` from the Python
  settlement consumer's accepted-header clock, with the guard
  (`has_hosted_activity`) short-circuiting a settlement-only boot so it pays no
  new RPC.

## Related

- **ADR-055** — pending-transaction strategy seams; D5 named this host as
  scheduled. The host is its Phase C governance layer.
- **ADR-050** — the `EngineDriver` driver seam; the host composes drivers, and
  the hub hoist extends `EngineDriver` construction without changing ownership.
- **ADR-049** / **ADR-046** — the one-door engine and the stage / handler split
  the driver attaches through.
- **ADR-056** — the retired gated serving seam; `RouteRegistry` is the
  membership oracle the host loans.
- **ADR-026** — settlement / backrun terminology; host / driver / strategy is
  the vocabulary this host fixes.
- **ADR-043** — the observability standard; the policy fold emits structured
  events on the `degenbot.strategy.head` target.
- `docs/architecture/strategy-seams.md`,
  `docs/architecture/phase-b-architecture.html`, `CONTEXT.md`.
