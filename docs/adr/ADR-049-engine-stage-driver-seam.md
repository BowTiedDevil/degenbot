# ADR-049: The engine's interface is one stage seam — the engine recedes to composition machinery

**Status: accepted** (2026-09-14; architecture-review #11 candidate 2, grilling decisions Q1–Q9 all accepted; ergo epic `5TBT7L`, tasks `2NLZE3` / `3WI4EO` / `RS64JJ` / `5AFSXM` / `RPEBMX` / `MHLURV` / `XURYVA`; commits `c43941f34`..`1e35449b8`). The one-impl-block census gate lands red at `c43941f34` (T1) and goes green, wired into the lint chain, at `1e35449b8` (T6).

## Context

Architecture review #11, candidate 2, found the arb engine with **two doors** and no single file that showed the interface:

- The Python driver reached the raw engine through `Arc<parking_lot::Mutex<ArbitrageEngine>>` — its PyO3 wrapper held the engine value and called inherent engine members directly.
- The block pump crossed the `StageHandlers` / `PumpControl` traits (ADR-046) — a different interface again — and the engine's own `EngineStages` implementation sat between the two.

Behind both doors the engine's inherent surface had smeared into **twelve `impl ArbitrageEngine` blocks across eight files**: the retune packing in `mod.rs`, path lifecycle in `lifecycle.rs`, delivery in `delivery_policy.rs`, solve/drain/intake across `event_routing.rs` and `engine_stages.rs`, diagnostics, the block cursor, the seat host. The shape made the interface undiscoverable — a caller had to know which themed file a method happened to live in — and concentrated churn on a shared grab file (`engine_stages.rs` / `mod.rs`). The inherent twins (one operation existing as both an engine method and a machine method or stage hook) had already produced real drift: ADR-046's `on_pump_ended` twin silently skipped the loud-close log the delivery-liveness contract requires.

The vocabulary test that resolves the mess: **the engine is not the interface**. The stage surface is.

## Decision

### D1 — `ArbitrageEngine` is `pub(crate)` machinery: exactly one inherent impl block

`ArbitrageEngine` becomes `pub(crate)`; it never leaves `degenbot-bot`. Its entire inherent surface is ONE `impl ArbitrageEngine` block in `arb_engine/mod.rs`, holding only real composition work: the constructors (`new` / `with_core` / `with_core_cfg`), the `apply_retune` body, the phase state, the `core()` handoff, `path_pools()`, the `require_phase*` gates, and the cfg(test) pool-registration helpers.

The standing invariant is the census gate `just check-engine-impl-blocks`, wired into `lint-rust-check`, the prek pre-commit tier, and CI. It asserts exactly ONE `impl ArbitrageEngine {` block, in `arb_engine/mod.rs`; a failure prints the per-file census. The same gate runs the **symmetric degenbot-python symbolic isolation census**: `ArbitrageEngine` appears in `degenbot-python/src` exactly once — the pyclass compat string `name = "ArbitrageEngine",`. That string is a deliberate Python-API compatibility exemption (the wrapper type's real name is `PyArbEngine`); the rejected escaped-name hack is recorded below.

### D2 — `EngineStages` is the ONE external seam

Every external consumer crosses the stage surface:

- the **block pump**, through its `StageHandlers` (eight product hooks) and `PumpControl` (seven driver pokes) trait implementations on `EngineStages` (ADR-046);
- the **PyO3 driver**: `PyArbEngine` holds `Arc<EngineStages>`, and observation, control, registration, and the `core()` handoff all re-source onto it. Construction crosses via `EngineStages::with_core_cfg` / `with_core` — the driver never names the engine type;
- the **umbrella `standalone_consumer` example**, the Rust consumer's entry, uses the same seam.

Registration, observation, solve control, and the core handoff have no second path. The seam is the interface; the engine recedes to composition machinery.

### D3 — operator re-tuning is a typed `EngineRetune` value

The config-derived operating knobs — event-buffer max age (expiry enable), the admission trio (target depth / retention blocks / enable), path cap, the profit window, force-deferred — are one typed value, `arb_engine/retune.rs::EngineRetune`. It is packed ONCE from the caller's `BotConfig` (`from_config`; the KAHU5W/J4HN66 construction-stance discipline) and applied through the ONE knob-write body `ArbitrageEngine::apply_retune`: once at construction and per runtime operator retune via `EngineStages::apply_retune`. It is the engine's twin of the fleet's centralized posture feeders + wake-on-retune. A channel install (`set_result_channel`) and `set_inline_simulator` are wiring, not a retune, and stay discrete.

Naming discipline: not "stance" (the fleet-migration stance of ADR-042, and the KAHU5W per-construction construction-stance values) and not "posture" (the fleet's cordon concept) — see CONTEXT.md's "Engine retune" entry.

### D4 — machine-direct free functions are the internal style

The retired shape is the **thematic inherent impl block** — the "smear." Members that are not composition work no longer live as inherent `ArbitrageEngine` methods scattered by theme. They are **free functions over `&mut ArbitrageEngine` / `&ArbitrageEngine`**, owned by the machine they concern (the cycle, the registry, delivery, the cursor) and called from `EngineStages` and the tests. `EngineStages::run_solve_cycle` drives the machine directly; the `delivery_policy` / `lifecycle` members became machine-direct free functions; `event_routing.rs` is deleted wholesale.

The point is structural, not aesthetic: the free-function style is enforced by the one-impl-block gate, so a member that is not composition work has nowhere to hide as an inherent engine method. The interface is one file; the machines own their bodies.

**Test-harness discipline.** White-box tests reach the machines and free functions directly; they never re-add an inherent engine member. The T4 shims (`run_test_cycle`, `merge_detached_for_test`, `finalize_for_test`, `process_updates`) and the YI5NGB boot-stamp probe live in the cfg(test) `arb_engine::test_harness` module as free functions over the engine value.

## Retired shape

The **two-door engine** is retired: a `pub` `ArbitrageEngine` the driver can hold and call directly, alongside the `EngineStages` seam. The compile is the guard — the engine type is `pub(crate)` and the census forbids any inherent member outside the one composition block. Inherent engine twins of stage hooks and pump pokes (ADR-046) retire with it.

## Considered options rejected

- **(a) Keep the thematic inherent impl blocks (the smear).** Rejected: it is the one-layer-out rebuild — the interface stays undiscoverable (a caller must know the theme file), churn stays concentrated on a shared grab file, and the gate has nothing to assert. The free-function style (D4) plus the census gate is the structural fix.
- **(b) Two-tier `pub` engine** — keep registration + core directly drivable on a `pub` engine while pushing everything else through the seam. Rejected: a second door invites the next direct grab; "there is no second door" is the whole point. The driver's registration path goes through `EngineStages` today.
- **(c) `solve_dirty` moving into `SolveCycle` instead of onto the stage surface.** Rejected: it smuggles a stance-gated core write (the pre-cycle buffered-event expiry) into the cycle machine's interface. That expiry belongs under the stage hook's held engine lock (`EngineStages::run_solve_cycle`); the cycle stays a pure per-epoch machine.
- **The escaped pyclass-name hack.** A draft renamed the wrapper's Python symbol via an escaped literal to dodge the census. Rejected in orchestrator review: the plain `name = "ArbitrageEngine",` compat string is the honest API-compat exemption; the census exempts exactly that one symbol rather than obscuring it.

## Consequences

- The driver's Rust signatures change shape (`Arc<EngineStages>` instead of the raw engine mutex) but the Python-visible API is byte-identical: the class name stays `ArbitrageEngine`, and no Python method signature moved.
- Rust consumers (`standalone_consumer`) construct through `EngineStages::with_core_cfg` and drive the same seam.
- The census is a standing invariant: a future inherent member outside `mod.rs` fails `lint-rust-check` / prek / CI until it is either composition work (moved to `mod.rs`) or a machine-direct free function.
- Hard cutover, no feature flag (AGENTS.md); the `arb_engine` suite, kept green every slice, is the characterization net.
- ADR-043 telemetry labels are byte-identical; the golden snapshots passed unchanged across T4.

## Related

- **ADR-045** (solve cycle extraction) — the `SolveCycle` / `PathRegistry` machines the stage surface now drives directly; candidate #2 was its named follow-up.
- **ADR-046** (StageHandlers / PumpControl split) — the two trait surfaces on `EngineStages` the pump crosses.
- **ADR-032** (`#[pyclass]` naming) — the Python-visible-name convention underpinning the compat-string exemption.
- **ADR-041** (block-epoch pipeline) — `EngineStages` is the stage machine's engine-side implementation.
- The task chain: epic `5TBT7L`, tasks `2NLZE3` (T1 red gate), `3WI4EO` (T2 retune), `RS64JJ` (T3 caller cutover), `5AFSXM` (T4 solve_dirty), `RPEBMX` (T5 pyo3), `MHLURV` (T6 collapse + gate), `XURYVA` (T7 records).
