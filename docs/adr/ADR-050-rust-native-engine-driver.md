# ADR-050: A public `EngineDriver` is the Rust driver seam — the engine stays crate-private behind the one stage seam

**Status: accepted** (2026-09-14; RSP-3a design phase, epic `RGZG4S`, task `OJI4FH`; decides Gap G1 / task `5XOGRK`; supervisor review accepted the driver-over-EngineStages layering and the PyArbEngine re-parent). Implementation is tracked by the RSP task chain `5XOGRK` (the driver), `XFEJUG` (registration pipeline), `L4E7RI` (consume/dispatch), and `IUGFLH` (the ownership sweep that ratifies the lift). The facade does not exist yet; the crate sources remain the last word on what the code does today.

## Context

The pure-Rust settlement bot (`rust/examples/settlement_bot`) is the parity twin of the Python driver (`examples/eth_settlement_arbitrage_v2_v3_v4_rust.py` + `src/degenbot/runner/*`). Slice 1 (config/CLI/DB boot) is committed, and it stops at a hard wall the audit ledger records as **Gap G1** (`docs/architecture/rust-settlement-bot-parity.md`, rows 6–8 and downstream 16):

- `arb_engine::ArbitrageEngine` is `pub(crate)` machinery. The only external driver of the pump/solve lifecycle is the PyO3 wrapper `PyArbEngine` (via `PumpState`) in `degenbot-python`.
- `BlockPump::subscribe` / `resume_from_subscribe` are public but require `Arc<dyn StageHandlers>` + `Arc<dyn PumpControl>` + `ReorgCoordinator` — i.e. the engine itself.
- `EngineStages` is public but is the **stage/observer seam**, not a driver: it has no `subscribe`/`resume`/`stop`, no snapshot-seed access, no verify-config pokes, no registration lifecycles, and no result-batch producer.

Two recorded doctrines meet head-on:

- **CONTEXT.md / ADR-049** record the `pub(crate)` choice as deliberate (epic `5TBT7L` Q2b): the engine's interface is **one stage seam**, `EngineStages`; the engine recedes to composition machinery behind it, and the one-impl-block census gate (`just check-engine-impl-blocks`) keeps a second door from re-opening. CONTEXT.md's glossary even says _avoid_ the phrase "engine facade" — "a facade fronts ANOTHER still-pub surface; the whole point is there is no second door."
- **AGENTS.md** says a pure-Rust MEV bot must be buildable from `cargo add degenbot` — "Rust is the engine; Python is a driver shell, not a co-implementation."

Meanwhile the startup ritual and phase machine live **twice** in shape if not yet in code: `EngineRegistry.start()` (`src/degenbot/arbitrage/engine_registry.py`) owns S-read → subscribe → verify-config and deliberately stops before `resume()`; `BotRunner` (`src/degenbot/runner/bot_runner.py`) owns the `_Phase` FSM (`New → Started → Running → Closed`), the "attach consumer before resume" ordering, and the "stop the pump first, then cancel the consumer" teardown. A Rust driver that reimplemented this would be a **twin implementation of one concept** — exactly the ADR-046 failure mode (`on_pump_ended` had an inherent twin that silently skipped a required log; two implementations, only one correct). Task `IUGFLH` (RSP-9) already frames this as a **LIFT candidate**: the sequencing is correct for every driver, so the core should own it once.

## Decision

### D1 — The public Rust driver is `degenbot_bot::arb_engine::EngineDriver`, built ON the one stage seam; `ArbitrageEngine` stays `pub(crate)`

`EngineDriver` composes the already-public `Arc<EngineStages>` (the engine's one seam) with the pump session state that currently lives in the PyO3 `PumpState`: the shared `Arc<Bot>`, the `ReorgCoordinator`, the shutdown flag, the subscribe state, the pump `JoinHandle`, the verify provider, and the result/block channel ends.

This is **not** the "engine facade" CONTEXT.md forbids. That note rejects a facade that fronts a _still-pub_ engine — a cosmetic wrapper over a second door. `EngineDriver` fronts nothing that is publicly reachable by another path: the engine type remains `pub(crate)`, and every `EngineDriver` member that touches engine state crosses `EngineStages`. The crate keeps **one door to the engine** (`EngineStages`) and gains **one driver seam above it** (`EngineDriver`), which is a layer, not a second door. The type is named a *driver*, matching the ledger's G1 wording and the driver/engine vocabulary (`docs/architecture/rust-owned-bot.md`).

The Python `PyArbEngine` becomes a **second adapter** onto the same `EngineDriver` (D7) — so both the pure-Rust consumer and the FFI consumer share exactly one implementation of the ritual.

### D2 — `EngineDriver` owns the pre-pump ritual; the backfill has exactly one owner

Constructor / adoption:

```rust
impl EngineDriver {
    /// Standalone-Rust construction: adopts the shared Bot, builds the stage seam.
    pub fn new(bot: Arc<Bot>, cfg: &Arc<BotConfig>) -> Self;

    /// Adapter adoption (the PyO3 wrapper already built its stages).
    pub fn from_stages(bot: Arc<Bot>, stages: Arc<EngineStages>) -> Self;

    pub fn core(&self) -> Arc<StateLock<BotState>>;
    pub fn bot(&self) -> &Arc<Bot>;
    pub fn stages(&self) -> &Arc<EngineStages>;   // observer/registration escape hatch
}
```

`new` calls `EngineStages::with_core_cfg(bot.state_arc(), cfg, bot.active_delta())`, builds `ReorgCoordinator::new(bot)`, creates the result/block channel pair and installs the result channel on the stage seam — the constructor work `PyArbEngine::new` does today, moved down.

The startup ritual (mirrors `EngineRegistry.start()` exactly):

```rust
impl EngineDriver {
    /// S-read → subscribe(W) → verify-config; stops BEFORE resume().
    pub async fn start(
        &mut self,
        node_http: &str,
        node_ws: &str,
        verify_state_view: Option<&str>,
    ) -> Result<u64, DriverError>;          // returns W

    pub async fn subscribe(&mut self, node_ws: &str) -> Result<u64, DriverError>;
    pub async fn resume(&mut self) -> Result<(), DriverError>;
    pub fn stop(&self) -> Result<(), DriverError>;

    pub fn snapshot_seed_block(&self) -> Option<u64>;
    pub fn set_snapshot_seed_block(&self, block: Option<u64>);
    pub fn set_verify_rpc_url(&self, node_http: &str);
    pub fn set_verify_state_view(&self, addr: &str);
}
```

- **S (snapshot seed) ownership.** S is a shared-`BotState` fact, not driver state. The DB path sets it in `Bot::load_snapshot_from_db`; the non-DB path calls `set_snapshot_seed_block(S)` where the **consumer** computes `S = min(newest_block)` across its snapshots (the facade only stores it). `start` reads S before `subscribe` so the phase transition uses `EnginePhase::after_subscribe(current, core_has_snapshot)` and lands at `SnapshotLoaded` (the construction-time-load path).
- **subscribe.** Gate with `EnginePhase::allow_subscribe`; call `BlockPump::subscribe(node_ws, bot, stages.clone(), stages.clone(), reorg.clone(), shutdown.clone())`; store `SubscribeState`; advance the phase; return W. A second subscribe while subscribed is a typed phase/state error.
- **verify-config.** `set_verify_rpc_url` builds the verify provider; `set_verify_state_view` stores the V4 `StateView` address. Both are the pokes `EngineRegistry.start` performs after subscribe.
- **resume — the auto-backfill is driver-owned.** `resume` gates on `EnginePhase::require(SnapshotLoaded)`, rejects an already-`Resumed` phase, takes the `SubscribeState`, and **awaits `BlockPump::backfill_with_drain(W, stream)` synchronously** (applying `S+1..W` inclusive under `BotState::process_backfill_logs`, zero result batches), then spawns the live loop (`pump.run_with_stream(combined, W)`), stores the handle, and advances to `Resumed`. The consumer **never** calls `backfill_from_snapshot` — ownership is the driver's, because the backfill's synchrony (the consumer's registration draining the per-pool backfill buffer must not race the backfill) is a core invariant, and leaving the call to each consumer would re-create the twin and the race.
- **Ordering.** `start()` stops short of `resume()`. The consumer attaches its result consumer between `start()` and `resume()`, then calls `resume()` as the single gate after which batches flow — the `BotRunner.run` invariant ("create the consumer BEFORE resume — closes the stale-backlog window").

### D3 — The result-batch stream is `ResultBatch` over the existing unbounded channel

`ResultBatch` is already public. The driver exposes the channel end once:

```rust
pub fn take_result_receiver(&mut self) -> Option<UnboundedReceiver<ResultBatch>>;
pub fn take_block_receiver(&mut self) -> Option<UnboundedReceiver<BlockNotification>>;
pub fn latest_results(&self) -> (HashMap<u64, SolvePathResult>, u64);
```

The consumer takes the receiver **before** `resume()`; `stop()` closes the channel so a pending `recv().await` returns `None` exactly once (the incident-2026-08-20 #2 end-of-stream contract owned by the delivery lifecycle). The PyO3 `__anext__` adapts the same receiver unchanged, so the Python async-iterator shape survives byte-for-byte.

### D4 — Registration, lifecycles, retune, cap — delegate to the one stage seam

```rust
pub fn register_and_solve_path(&self, hops: Vec<PoolHop>) -> Result<(u64, bool), PathRegistrationError>;
pub fn register_path(&self, hops: Vec<PoolHop>) -> Result<(u64, bool), PathRegistrationError>;
pub fn deregister_path(&self, path_id: u64) -> bool;
pub fn set_path_cap(&self, cap: Option<usize>);
pub fn path_count(&self) -> usize;
pub fn path_dedups(&self) -> u64;
pub fn path_info_for(&self, path_id: u64) -> Option<Result<PathInfo, PathInfoBuildError>>;
pub fn apply_retune(&self, retune: &EngineRetune);
pub fn set_inline_simulator(&self, sim: Arc<dyn InlineSimulator>);

pub async fn run_v3_registration_lifecycle(&self, address: &str, snapshot_block: Option<u64>) -> Result<(), VerifyError>;
pub async fn run_v4_registration_lifecycle(&self, pool_manager: &str, pool_id_hex: &str, snapshot_block: Option<u64>) -> Result<(), VerifyError>;
pub fn run_v3_registration_lifecycle_sync(&self, address: &str, snapshot_block: Option<u64>) -> Result<(), VerifyError>;
pub fn run_v4_registration_lifecycle_sync(&self, pool_manager: &str, pool_id_hex: &str, snapshot_block: Option<u64>) -> Result<(), VerifyError>;
```

- **`PathRegistryFull` mapping.** In Rust the refusal is the typed `PathRegistrationError`; `RegistryFull { cap, registered }` stays the benign stop signal and `Invalid(_)` stays the caller bug. The facade returns the enum verbatim — no `String` flattening. The PyO3 layer keeps its existing `map_path_registration_err` (→ `PathRegistryFullError` / `ValueError`), unchanged.
- **Lifecycles.** The `run_v3/v4_registration_lifecycle` + `_sync` bodies move **down** from `PumpState` (D7). The core owns the quarantine → seed-verify → drain+pin → post-drain-verify → `set_live` choreography (ADR-022 D1); the async/blocking twin is the async form + a runtime `block_on`, replacing the PyO3 `future_into_py` shape. `VerifyError` (mismatch = fatal; RPC = retryable) is the typed Rust error. The **retry policy is not lifted** here: the values and the retry-with-backoff loop stay consumer-side (RSP-9 `IUGFLH` row).
- **inline sim.** The core member is `set_inline_simulator(Arc<dyn InlineSimulator>)` (already on `EngineStages`). The Python `install_inline_simulator` stays in `degenbot-python`: it builds the `InlineSimHook` from the session's `PySimulateContext` and registers the escalation port, then calls the core member. A pure-Rust consumer builds its own `InlineSimulator` and calls it directly.

### D5 — `PumpPhase` (formerly `EnginePhase`) is the pump-protocol phase; `Stopped` is a driver terminal latch; phase violations are typed

The engine-session protocol truth stays the existing FSM (`Created → Subscribed → SnapshotLoaded → Backfilled → Resumed`); the driver reproduces today's transitions exactly (`after_subscribe` landing at `SnapshotLoaded`; `resume` landing at `Resumed`). No new discriminant is added to it.

**Pump-protocol naming.** The enum is now `PumpPhase`: it is a protocol-phase machine for `subscribe`/`load_snapshot`/`backfill`/`resume` ordering, NOT the operator-facing lifecycle. The one operator lifecycle (`Registered → Enabled → Running → Stopped | Halted | Disabled`) is owned by `strategy_host::StrategyHost`; a consumer reads the pump machinery read-only through `EngineDriver::snapshot() -> DriverSnapshot { phase, is_stopped, pump_handle_armed }` and never derives operator legality from it.

The driver adds a **terminal stopped latch** (`stopped: AtomicBool`), because the engine enum cannot express teardown. Every driver member except `stop()` rejects once stopped. `stop()` is deliberately any-phase and idempotent — the `BotRunner.shutdown` contract (the SIGINT/partial-startup teardown depends on it).

`EnginePhase::require`'s `String` error is wrapped at the driver boundary in a typed `PhaseError { method, current, required }`; `DriverError` is the closed enum (`Phase(PhaseError)`, `Subscribe`, `Resume`, `Registration(PathRegistrationError)`, `Verify(VerifyError)`, …). A consumer matches on the variant; no string matching. The existing `EnginePhase::require` surface is unchanged for the engine-internal callers.

### D6 — `stop()` mirrors `BotRunner.shutdown`/`__aexit__` ordering

`stop()` sets the shutdown `AtomicBool`, takes and `abort()`s the pump handle, `block_on`s the join so the WS subscription futures (and the `Arc<dyn StageHandlers>` clones) drop before return, clears the subscribe state (re-allowing a later `subscribe`), and sets the stopped latch. Idempotent — a second call is a no-op.

The **ordering contract** is stated once here and consumed by every driver: **stop the pump first** → the delivery lifecycle closes the result/block channels → a pending consumer `recv().await` sees the natural end → the consumer ends cleanly → *then* the consumer task is cancelled/joined. Cancelling the consumer first leaves the pump holding the WS task on the shared runtime, blocking process exit up to `BACKFILL_TIMEOUT_SECS` (60s) — the exact bug `BotRunner.__aexit__` documents. The driver owns the stop; the consumer owns its own task cancellation.

### D7 — The Python `PyArbEngine` / `PumpState` re-parents onto `EngineDriver`

The PyO3 wrapper holds `Arc<EngineDriver>` and delegates `start`/`subscribe`/`resume`/`stop`, `snapshot_seed_block` (get/set), `set_verify_rpc_url`/`set_verify_state_view`, the four lifecycle entry points, `set_path_cap`, `set_inline_simulator`, and the result channel. `PumpState`'s session fields (bot, reorg coordinator, shutdown, subscribe state, pump handle, verify provider) collapse into `EngineDriver`; what remains of `PumpState` is the block-stream GIL adapter co-owned with `PyBot`, or it dissolves entirely.

**Why re-parent rather than leave Python as-is:** the alternative is two implementations of the ritual — `PumpState` (Python) and `EngineDriver` (Rust) — which is the ADR-046 twin-drift failure with a Python/Rust accent. Re-parenting makes the FFI and the pure-Rust drivers share exactly one correct implementation, keeps the Python-visible signatures byte-identical (ADR-049's discipline), and lets the conformance tests live once in `degenbot-bot`. The Python wrapper keeps only the things that are genuinely Python: GIL detachment, `future_into_py` adaptation, and `block_on` over the shared runtime.

### D8 — `release_python_state` stays driver policy, not a facade member

`BotRunner`'s post-registration trim (`release_python_state` + drop the Python pool/token caches) is Python-companion scaffolding — a Python-object-lifetime concern with no Rust-core counterpart (`rust-owned-bot.md` records the Rust engine as canonical state). It stays in the Python driver. The facade does not grow an "release" member.

### D9 — RSP task mapping

| Facade member group | Consuming task |
|---|---|
| constructor/adoption, `start`/`subscribe`/`resume`/`stop`, snapshot seed, verify-config, result stream, `latest_results`, `set_inline_simulator`, `apply_retune` | **`5XOGRK`** (the driver + the parity-example rewire) |
| `register_and_solve_path`/`register_path`/`deregister_path`, `set_path_cap`, `path_count`/`path_dedups`, `run_v3/v4_registration_lifecycle(+_sync)`, `snapshot_seed_block`, `PathRegistrationError::RegistryFull` | **`XFEJUG`** (G3 discovery + registration pipeline) |
| `take_result_receiver`, `latest_results`, `path_info_for`, `take_block_receiver`, the `stop()` teardown ordering | **`L4E7RI`** (G4 result consumption, fan-out, dispatch, submission) |
| the D2/D7 LIFT decision (ritual sequencing, phase machine, VerifyClaims dance, key maps) | **`IUGFLH`** (RSP-9 ownership sweep) |

The once-per-pool key maps and the `VerifyClaims` TOCTOU dance remain `XFEJUG`'s to lift or keep per the sweep; this ADR only fixes the **driver seam** they attach to.

### D10 — 0.7 kill list untouched

This decision changes only `degenbot-bot` and `degenbot-python`. It deletes nothing on the AGENTS.md 0.7 kill list: `src/degenbot/migrations/`, the `alembic`/`sqlalchemy` dependencies, `DatabaseSessionManager`, the SQLAlchemy models package, `ALEMBIC_HEAD`, the `ensure_schema` Alembic branch, and the `query_only` pragma all remain exactly as they are.

## Retired shape

The **Python-only pump ritual** is retired: `PumpState`'s `subscribe`/`resume`/`stop`/`snapshot_seed`/`set_verify_*`/`run_v3/v4_registration_lifecycle` bodies become thin delegations to `EngineDriver`. The **"a pure-Rust consumer must re-derive the handshake"** assumption is retired with Gap G1. The engine type stays `pub(crate)`; the one-impl-block census is untouched.

## Considered options rejected

- **(a) Make `ArbitrageEngine` public with a curated API.** Rejected. It re-opens the two-door engine ADR-049 D1 closed: a `pub` engine reachable directly *alongside* the `EngineStages` seam. The cost is exactly the ADR-046 twin-drift class — two consumer-visible shapes of "drive the engine" — plus every internal refactor (the `SolveCycle`/`DeliveryPolicy`/registry internals) becoming a public breaking change, and the census invariant losing its meaning. A curated `pub` API is still a second door; the engine is not the interface.
- **(b) Keep the ritual in Python and reimplement it in the Rust example.** Rejected: two implementations of one concept, only one likely to be fixed when a backfill-synchrony-class bug lands. The Rust example is the parity twin, not a second copy of the driver.
- **(c) Extend `EngineStages` itself with `subscribe`/`resume`/`stop`.** Rejected on ADR-046 D1's vocabulary test: `EngineStages` is the **stage/observer** seam (the pump's hooks + the driver's reads), while `subscribe`/`resume`/`stop` are **pump-session sequencing** that needs `Bot` + `ReorgCoordinator` + runtime/task ownership — state the stage surface deliberately does not hold. Folding them in re-mixes the two conversations the ADR-046 split exists to separate.
- **(d) A Python-only extension of `PyArbEngine`.** Rejected: it does nothing for the `cargo add degenbot` consumer, which is the whole point.
- **(e) A new `degenbot-engine-driver` crate.** Rejected: there is no second consumer crate and the pump/session types already live in `degenbot-bot`; a new crate is sample-of-one packaging. `EngineDriver` lives in `arb_engine` beside `EngineStages`.

## Consequences

- A pure-Rust consumer can drive the full settle lifecycle — `start` → attach the `ResultBatch` consumer → `resume` (auto-backfill) → `register_and_solve_path` / lifecycles → consume batches → `stop` — with zero Python. Ledger rows 6–8 and downstream 16 unblock; the parity example retracts its Gap-G1 tail.
- The startup ritual and phase machine have **one** implementation. `EngineRegistry.start()` and `BotRunner` become Python-side policy (config, SIGINT, pool-cache trim) over a Rust-owned sequencing contract.
- The Python-visible API is byte-identical: same class name, same method signatures; only the Rust ownership behind `PyArbEngine` changes.
- The degenbot umbrella re-exports `EngineDriver` (via `degenbot_bot::arb_engine` / `degenbot::bot`), so `cargo add degenbot` reaches it without naming a sub-crate.
- Risks to manage in implementation: the `PumpState`→core move is a behavioral cutover over the verify machinery (the async/blocking twin, the provider ownership, the GIL-detach shape); `degenbot-bot`'s test suite and the pyo3 parity tests are the characterization net; telemetry labels (ADR-043) must stay byte-identical.
- `IUGFLH` records the supersedure pointer for the previously "stays-python" sequencing doctrine so CONTEXT.md can be updated by the implementing task.

## Open questions deferred to implementation

- Whether `EngineDriver` should expose blocking `subscribe`/`resume` wrappers beside the async ones, or stay async-only with the consumer owning the runtime.
- Whether `PumpState` shrinks to a block-stream adapter or dissolves entirely into `PyBot` + `EngineDriver`.
- The exact `DriverError` variant set (how much of the underlying `String`/`VerifyError` surface is preserved vs wrapped).
- Whether the verify provider is constructed by the driver (`set_verify_rpc_url`) or injected as a typed handle.
- Whether `EngineDriver` should own a convenience `run_until_stopped` loop or leave the main loop entirely to the consumer (the ledger's L4E7RI surface).

## Related

- **ADR-046** (StageHandlers / PumpControl split) — the two-shapes twin-drift lesson this decision applies; the vocabulary test that rejects option (c).
- **ADR-049** (engine stage driver seam) — the one-door engine invariant and the `pub(crate)`/census discipline this decision preserves.
- **ADR-019** (in-process revm, strategy/engine separation) — the "Rust-canonical, not Python re-derivation" lineage behind the LIFT.
- **ADR-022** (registration verify lifecycle is core-owned) — the `run_v3/v4_registration_lifecycle` choreography the driver exposes.
- **ADR-006** (Bot as the per-chain orchestrator) — the pump/engine ownership the session state composes.
- **ADR-041** (block-epoch pipeline) — the stage machine whose driver surface this completes.
- **ADR-043** (observability standard) — the telemetry labels the move must preserve.
- **ADR-055** (pending-transaction strategy seams) — Phase C records the ADR-018
  engine-family trigger as *pulled by decision* with the dynamic strategy host
  (parameterized stage payloads, `EngineDriver<S>` over a second settled-block
  strategy). Scheduled; do not expand the driver shape for a hypothetical strategy.
- Epic `RGZG4S` (Rust settlement-bot parity), task `OJI4FH` (this ADR), `5XOGRK` (Gap G1), `XFEJUG`, `L4E7RI`, `IUGFLH` (RSP-9), `23DLCY` (the running parity gate), `YFIOSF` (Gap G2), `KPLWUM` (Gap G5).
- The task chain behind the preserved invariant: epic `5TBT7L`, tasks `2NLZE3` / `3WI4EO` / `RS64JJ` / `5AFSXM` / `RPEBMX` / `MHLURV` / `XURYVA`.
- `docs/architecture/rust-settlement-bot-parity.md` (the G1 ledger rows), `docs/architecture/rust-owned-bot.md` (the Rust-owned design), `CONTEXT.md` ("Engine seam deepening", "Engine retune", "Driver seam").
