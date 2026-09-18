# Strategy seams — the substrate map and how to add a strategy

Companion to ADR-054 (frame evidence seams), ADR-055 (pending-transaction
strategy seams), ADR-019 (strategy-vs-engine), ADR-025 (execution strategy),
ADR-018 (engine-family trigger, pulled by decision at ADR-055 Phase C).

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

- **Pending-transaction strategies** (`PendingTxStrategy` trait,
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

1. Write one `PendingTxStrategy` impl in `degenbot-submission`: your
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

## Adding a settled-block strategy — Phase C territory

The pump/stage seam and its payload typing are settlement-specific today by
design (a sample of one). When a second settled-block strategy exists, the
ADR-018-named extraction runs: parameterized stage payloads, a generic
`EngineDriver`, per-family fleet globals. Don't pre-build it for a
hypothetical strategy; Phase B/C track the real path to it.

## Where the seams are (exact owners)

| Seam | Lives at |
|---|---|
| `MarketContext` (frame-surviving caches) | `degenbot-submission/src/market_context.rs` |
| `PendingTxStrategy` + artifacts | `degenbot-submission/src/pending_tx.rs` |
| `BackrunStrategy` (reference lane) | `degenbot-submission/src/backrun_strategy.rs` |
| Pending-tx driver (replay/extract/stages/gate) | `degenbot-submission/src/frame_pipeline.rs` |
| `SubmissionTarget` + `dispatch_and_submit` | `degenbot-submission/src/submit.rs` |
| Journal extraction (V2/V3/V4) | `degenbot-simulation/src/sim/evm/journal_pools.rs` |
| Sandbox + `ExplicitPoolState` | `degenbot-bot/src/bot_core/planning.rs` |
| Strategy facet | `degenbot-config/src/schema.rs` (`strategy.name`) |
| Settled-block seam today | `degenbot-bot/src/bot_core/stage_handlers.rs` |