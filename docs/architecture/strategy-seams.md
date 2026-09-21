# Strategy seams — the substrate map and how to add a strategy

Companion to ADR-054 (frame evidence seams), ADR-055 (pending-transaction
strategy seams), ADR-019 (strategy-vs-engine), ADR-025 (execution strategy),
ADR-018 (engine-family trigger, pulled by decision at ADR-055 Phase C), and
ADR-057 (the strategy host).

## The runbook

The step-by-step companion to this map is
[adding-a-strategy.md](adding-a-strategy.md): it carries the real
signatures, the admission/lane/ledger/hub wiring, the driver
partitions, the config-facet additions, and the test-surface pattern.

## The shared substrate

| Layer | Owner | Notes |
|---|---|---|
| Pool state + identity values | `degenbot-pools` | `PoolEntry`, family states, `slot_layout` (the one location table) |
| Solver math + envelopes | `degenbot-solvers` | value-only; no chain, no async |
| Path enumeration primitive | `degenbot-pathfinding` | leaf `PathGraph` DFS; both discovery styles build on it |
| Simulation engine | `degenbot-simulation` | `BlockSimHandle`, `ScratchEvm`, replay seam, journal extraction (V2/V3/V4) |
| Command grammar | `degenbot-executor` | `encode_cmd_stream`, `EncodeRequest` |
| RPC spine | `degenbot-rpc` | provider, multicall3, head watch, pending-tx feeds, fee oracle |
| State owner + admission | `degenbot-bot::bot_core` | live registry `BotState` (pump-fed) and the planning sandbox (`planning::Workspace` + `ExplicitPoolState`) |
| Submission machinery | `degenbot-submission` | signer, dispatcher, monitor, gap quarantine, finality-liveness FSM |

## The two reaction kinds

A strategy picks exactly ONE:

- **Pending-transaction strategies** (`PendingTxReaction` trait,
  `degenbot-submission/src/pending_tx.rs`) react to observed mempool
  transactions. The pending-transaction driver owns the substrate loop:
  simulate the pending tx (`ScratchEvm::replay`) → recover pool post-states
  (`extract_pool_post_states`) → admit → discover → evaluate → compose →
  bundle-sim gate → decide → submit. `BackrunStrategy`
  (`backrun_strategy.rs`) is the reference implementation.
- **Settled-block strategies** react to sealed blocks via the block pump /
  `StageMachine` seam; settlement arbitrage is the only one today. Their
  product types are deliberately settlement-shaped; generalization is
  Phase C work (ADR-055 D5).

## Adding a pending-transaction strategy — the checklist

1. Write one `PendingTxReaction` impl in `degenbot-submission`: your
   `admit` selection (from recovered pool post-states), `discover`
   (anchored DFS over the connector index, or otherwise), `evaluate`
   pricing, `compose` payload policy, `decide` gate. The runtime caches
   (`MarketContext`), replay, journal extraction, sandbox admission, the
   pathfinding primitive, the sim executor, liveness, and the submission
   channel are provided.
2. V4 works out of the box: the sandbox admits V4 explicit state
   (`ExplicitPoolState::V4`), the journal extracts V4 post-states for pools
   whose identities your descriptors carry (`V4PoolSet`).
3. Declare your strategy facet in the typed config (`strategy.name` exists;
   add per-strategy tables as needed — one declaration site).
4. Choose your `SubmissionTarget` (`Bundle` / `Public`).
5. Register your launcher/console row so the operator can select the
   strategy explicitly.

**Loud-abort rule** (ADR-055 D4): if your strategy asks the substrate for a
pool family it cannot serve, that surfaces as a loud typed failure
immediately — silent skips are reserved for transient I/O failure, never
for capability gaps.

## The strategy host (Phase C, landed)

The dynamic strategy host landed 2026-09-19
([ADR-057](adr/ADR-057-strategy-host.md)): `StrategyHost`
(`degenbot-bot/src/strategy_host.rs`) owns the hub, the boot-snapshot
`RouteRegistry`, and the `NonceAuthority`
(`degenbot-bot/src/nonce_authority.rs`), and drivers attach through the bound
`HostHub` pair (`EngineDriver::from_stages_with_hub`,
`degenbot-bot/src/arb_engine/driver.rs`). The per-strategy submission ledger,
`NonceLane`, and the default `HeadPolicy` live in
`degenbot-submission/src/submission_ledger.rs`; the backrun lane is a
registrable driver (`degenbot-submission/src/backrun_driver.rs`); and the
operator drives admission through the engine adapter's `enable_strategy` /
`disable_strategy` / `strategies` verbs. The driver FSM is
`Registered → Enabled → Running → {Halted, Disabled}` with terminal tombstones,
and nonces are leased at sign time (lowest-free, contiguous above the confirmed
chain nonce).

Forward-looking statements reserved to the deferred work: shadow-feedback
between drivers (reconciliation observes the nonce-level shadow and acts on
nothing) and more than one outstanding nonce lease per strategy. Both are
post-v1 revisits, as are process-level submission arbitration and lane hot
registration.

## Adding a settled-block strategy — on-demand engine generalization

The pump/stage seam and its payload typing are settlement-specific today by
design (a sample of one). When a second settled-block strategy exists, the
ADR-018-named extraction runs: parameterized stage payloads, a generic
`EngineDriver`, per-family fleet globals. The strategy host is landed and can
run such a driver; the payload generalization is not pulled by a sample of one,
so it stays on-demand.

**Phase B landed (2026-09-19):** `degenbot-eventhub` owns per-process intake
fan-out with declared overflow policies (`OverflowPolicy`); the backrun feed
ring and head watch subscribe through it (B1/B2), both engine→driver channels
are hub-registered `UnboundedFlagged` sources with observable depth via
`Hub::named_pending` (B3), the gated serving seam was retired (B4, ADR-056),
and the boot-snapshot `RouteRegistry` answers pool membership for strategies
(B5). A second pending transaction strategy now implements
`PendingTxReaction` and subscribes; no intake wiring. The Phase C runtime host
now lands on top of it ([ADR-057](adr/ADR-057-strategy-host.md)).

## Where the seams are (exact owners)

| Seam | Lives at |
|---|---|
| `MarketContext` (frame-surviving caches) | `degenbot-submission/src/market_context.rs` |
| `PendingTxReaction` + artifacts | `degenbot-submission/src/pending_tx.rs` |
| `BackrunStrategy` (reference lane) | `degenbot-submission/src/backrun_strategy.rs` |
| Pending-tx driver (replay/extract/stages/gate) | `degenbot-submission/src/frame_pipeline.rs` |
| `SubmissionTarget` + `dispatch_and_submit` | `degenbot-submission/src/submit.rs` |
| Journal extraction (V2/V3/V4) | `degenbot-simulation/src/sim/evm/journal_pools.rs` |
| Sandbox + `ExplicitPoolState` | `degenbot-bot/src/bot_core/planning.rs` |
| Strategy facet | `degenbot-config/src/schema.rs` (`strategy.name`) |
| Strategy host (hub + registry + authority + FSM) | `degenbot-bot/src/strategy_host.rs` |
| Nonce authority | `degenbot-bot/src/nonce_authority.rs` |
| Hub hoist / driver attach | `degenbot-bot/src/arb_engine/driver.rs` |
| Submission ledger + `NonceLane` + `HeadPolicy` | `degenbot-submission/src/submission_ledger.rs` |
| Registrable backrun driver | `degenbot-submission/src/backrun_driver.rs` |
| Settled-block seam today | `degenbot-bot/src/bot_core/stage_handlers.rs` |