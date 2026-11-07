# ADR-089: The evm2 pilot — a flag-gated parallel engine behind the simulation seam

## Status

**Implemented (2026-10-30, phase 5 cutover `5bc38246e`).** Phases 0–5
landed: the evm2 engine is the only engine on production paths; the revm
engine, the pilot scaffolding, and every feature flag retired per ADR-087.
Phase 6 is largely landed (2026-10-30, `a1f3f7c50`..`c4103ce8d`): the
offline benchmark harness; the JIT spine (process-shared runner on the
sequence EVMs; production capture moved to the state-layer seam after the
inspector-XOR-runner finding, proven by the capture-seam probe); the AOT
executor (durable artifact store, `aot-prepare-executor` lane, holder
preload); the engine-owned executor-bytecode loader (the Python loader
retired); the worker pool with fleet-sized tuning and compile telemetry on
the existing metrics surface; registration warmup through the inverted
code_warmup port; and frame-replay residency (~2.9x on the replay legs).
The JIT ships ON by default (`simulation.sim_jit`, flipped 2026-10-31 —
below); the concurrency trio (coroutine fan-out, speculative detach, BAL
prefetch) is the remaining open work. The revm dependency remains
lib-level for the `oracle`/`harness`/`capture` lab modules (the Tier-3
truth source until their deliberate migration decision — made by
ADR-091: retire revm completely).

Accepted (2026-10-30).

**The default flip (2026-10-31).** The JIT program (phases 1–6) was complete
but inert: no production build carried `evm2-jit`, so `shared_runner()`
answered `None` in every shipped artifact. Both consumers now carry the
feature and the key that arms it defaults to on — the binding's `simulation`
feature forwards `degenbot-simulation/evm2-jit` (every default and release
extension build), the umbrella's narrowly-named `jit` feature forwards the
same and the umbrella's `default` selects it (the pure-Rust bot gets the same
engine the Python driver gets, not an interpreter-only build with the JIT
buried), and `simulation.sim_jit`'s schema default flips false → true. The
escape hatch is unchanged and is the whole point of the fail-soft: with
`DEGENBOT_SIM_JIT=false` (or a build without the feature — `cargo add
degenbot --no-default-features`) there is no runner, every frame interprets,
and no verdict moves, because a lookup miss interprets anyway. What changes
is that the build-side cost is no longer a diagnostic: every production build
now links **LLVM 23** (the devcontainer's `llvm-config-23`), which makes an
equivalent LLVM toolchain on the release builder a hard requirement for the
next release rather than an optional extra.

**The Linux-only shipment (2026-10-31).** That build-side cost is now paid only
where the JIT can run. Both consumers' `evm2-jit` forward moved out of their
feature lists and into `[target.'cfg(target_os = "linux")'.dependencies]`, so a
Linux build still gets the runner by default, while macOS/Windows builds have no
`evm2-jit`, `evm2-jit-llvm`, or `llvm-sys` node and ship the interpreter — the
fail-soft makes that the same engine uncompiled (`simulation.sim_jit` keeps its
default-true and finds no runner; no verdict moves). The umbrella's `jit`
feature and its `default` entry are gone with it, and LLVM 23 stays a hard
requirement of every Linux builder, release CI included. The rationale (the
JIT's compiled-artifact loader is ELF-only; no x86_64 macOS LLVM 23 exists) and
the release consequences: `docs/llvm-release-ci.md`.

**The registration-warmup retirement (2026-11-02).** The inverted
`code_warmup` port (a fresh registration queued its pool's runtime bytecode and
the per-block handle build handed it to the worker pool) is deleted, and the
worker pool is now warmed only by the lazy path: a lookup miss interprets the
frame, the fan-out width's worth of hotness promotes the program, and the next
frame executes compiled code. The measurement that decided it: a dry-run boot's
crawl queued 13 distinct registration programs and compiled all 13 (0.07–1.09 s
each), and none of them was ever looked up — the four programs that served
every steady-state lookup were pools whose bytecode was never queued (they
reach the engine from persisted rows, which carry no code), so they compiled on
first miss and hit from then on. Paying an `eth_getCode` plus a cold LLVM
compile per fresh registration bought nothing a crawl's own registrations are
ever simulated for; the lazy path already covers the contracts a block actually
executes. The fail-soft contract is unchanged: with `simulation.sim_jit` off
(or a build without the JIT) every frame interprets, and no verdict moves.

## Context

revm is the sole simulation executor (ADR-019): the per-block `BlockSimHandle`
over the `CacheDB<WarmCodeCache<BotStateDb<WrapDatabaseAsync<AlloyDB>>>>`
stack, the inspector-driven swap capture and revert attribution, and the
Tier-3 differential oracles that treat EVM execution as the truth source for
the hand-written swap math. revm 43 is pinned workspace-wide.

reth 2.9.0 replaced revm with `evm2` (alloy-rs/evm2, DaniPopes' revm
successor: a tighter interpreter, claimed up to 2× revm, alloy-primitives
native, runtime feature flags where revm uses compile-time features). evm2 is
unpublished on crates.io — reth pins rev `f93318f120f6300599b21170d6c7bef81ed63d87`
— and carries an explicit "MSRV may be updated at any time" policy against
this workspace's toolchain pin (1.98.1) and certified MSRV (1.97).

The engine/strategy seam (ADR-019 D4/D7) contains the swap: revm is named
inside `degenbot-simulation`'s engine and the `degenbot-arbitrage` strategy's
trait bounds, nowhere else in production. Two constraints shape the approach:

- **Publish contract.** The workspace publishes crates; `cargo publish`
  rewrites every path and git dependency at packaging into a registry
  requirement that must resolve on crates.io. Measured at packaging
  (2026-11-07): a `version` key becomes that requirement — `evm2 = "^0.1.0"`
  against a registry holding only the name reservation — and a version-less
  dependency outside the dev slots is rejected outright. The declaration
  shape that follows from this is clause 2; the population it leaves
  unpackageable is the publish-blocked census (docs/gates.md section 7).
- **Truth-source risk.** The Tier-3 oracles assume the EVM is ground truth.
  Swapping the truth source without a differential gate would devalue every
  oracle that runs on it.

The pilot's first differential run already caught a real divergence class:
revm 43's `CfgEnv` caches a `gas_params` table, so assigning `cfg.spec`
directly leaves the default spec's gas schedule active (an EIP-7623 floor
appeared at Cancun). evm2 derives its gas schedule from the spec at
construction and cannot drift this way.

## Decision

**evm2 enters as an opt-in parallel implementation behind the `evm2` cargo
feature on `degenbot-simulation`, gated at every layer by a differential
parity test against revm, and promoted only by a hard cutover that removes
the flag.**

1. One feature, one home: `degenbot-simulation/evm2` gates the
   `src/sim/evm2/` module and its test targets. No default lane enables it;
   no other crate gains an evm2 dependency during the pilot.
2. The dependency is pinned at a commit, the way reth pins it (amended
   2026-11-07; it initially pinned reth 2.9.0's rev `f93318f`, tracked main
   HEAD from 2026-10-30, and carried `version = "0.1.0"`). Moving the pin
   is a deliberate, reviewed event (MSRV and API churn ride along). No
   `version` key, deliberately: `cargo publish` strips the git source at
   packaging and keeps a `version` as a crates.io requirement that must
   resolve, while a version-less dependency outside the dev slots cannot
   package at all — either way the engine closure is unpackageable while
   the registry holds only upstream's name reservation, so the publish
   oracle withholds those members by name (the publish-blocked census,
   docs/gates.md section 7).
3. Only pure-Rust precompile backends are enabled (no GMP/MCL/aws-lc native
   builds), matching the revm dependency's backend shape and keeping the
   devcontainer build contract intact.
4. The migration mechanism is the differential gate
   (`tests/evm2_pilot.rs`): the same transaction through both engines must
   produce identical status, gas, output, and storage writes. Every layer
   ported to evm2 (state-forwarding DB, warm code cache, async RPC
   fallback, inspectors) extends the gate before it extends the engine.
5. revm stays the Tier-3 truth source and the production engine until the
   gate covers every engine capability the strategy uses, including the
   live fork-truth legs. The cutover is hard: the flag, the revm
   dependency, and this ADR's pilot scaffolding retire together.
6. `evm2-jit` enters the pilot behind the stacked `evm2-jit` feature
   (amended 2026-10-30; this clause initially read "out of scope"). It is a
   fail-soft hybrid: `JitInterpreterRunner` falls back to the interpreter
   when no compiled program exists, so the differential gate covers the JIT
   with no new divergence surface. Its only cost outside the default lanes
   is LLVM 23 (the F45 devcontainer's `llvm-devel`; `evm2-jit-llvm` binds
   the C API directly and probes `llvm-config`/`llvm-config-23`). evm2
   dropped its inkwell dependency at `86fa2648` (2026-10-30), retiring the
   workspace-root inkwell `[patch]` re-pin this clause originally carried.

## Rejected alternatives

**Wait for a crates.io release, then big-bang the swap.** Rejected: the
release has no date, reth's production mileage on the pinned rev is the
best de-risking signal available now, and a big-bang swap would land
without the layer-by-layer parity evidence the gate accumulates.

**Track evm2 HEAD instead of reth's rev.** Initially rejected (reth's rev
carries production validation), then accepted by the D2 amendment
(2026-10-30): the opt-in feature plus the differential gate make HEAD's
churn cheap and its early warning valuable.

**Parallel implementations as a habit.** This project defaults against
parallel implementations. The exception stands because the truth-source
risk is asymmetric: a silent execution divergence corrupts the solver's
accuracy oracle, not just a runtime path. The flag has a recorded deletion
condition (D5), not an open-ended lifetime.

## Feature-parity roadmap

Recorded 2026-10-30 (accepted in review). Each phase extends the
differential gate before the engine (D4); phases 1–3 are engine-internal
(`degenbot-simulation` only), phase 4 is the first cross-crate step.

0. **Seam + gate foundation** (done): pilot EVM, verdict gate, warm-cache
   and `BotStateDb` ports, `AlloyAsyncDb`, log-inspector parity, live fork
   leg, JIT runner with builtins export.
1. **Engine core** — block-env wiring from the pumped block,
   `SimulationOverrideParams` application, the settle verb (`transact` +
   `discard`, never commit) and typed view reads. Gate: override parity,
   view-read parity, run-invisibility.
2. **Composite inspector suite** — swap-event capture (V2/V3/V4), call
   trace (revert attribution), access-list emission, with
   tentative-buffer-on-revert semantics. Gate: three-family LOG parity,
   reverting-subcall trace parity, access-list == settled-journal parity.
3. **Production stack + frame replay** — storage memo, divergence
   observer, false-empty provenance probe, the counting scratch stack, and
   the `EvmFeatures::BALANCE_CHECK`/`BASE_FEE_CHECK` runtime flags
   replacing revm's compile-time optional features. Gate: capture-corpus
   replay parity, RPC read-count parity (behavior-identical, not just
   result-identical), live legs over replayed frames.
4. **Strategy driver cutover** — `degenbot-arbitrage`'s engine trait
   bounds go engine-generic; the 7-call settlement bundle drives an evm2
   session through the escape hatch; facade + Python shell swapped. Gate:
   offline replay→extract→admit→solve parity, full pytest, flake-probe.
5. **Hard cutover** — evm2 only on production paths; revm drops to
   dev-only for the Tier-3 oracles until their migration decision; flags
   and pilot scaffolding retire (ADR-087); JIT moves from `blocking` gate
   mode to the worker pool with compile telemetry. Gate: the pre-push
   manifest plus a multi-window fork-lane soak.
6. **The performance roadmap below.**

Risks called out at recording: evm2's moving MSRV (every `cargo update`
review includes an MSRV check), and the 7-call bundle's
journaled-accumulation contract — the escape hatch needs an
invisibility-BETWEEN-calls proof, not just final-state parity. Phase 3 is
expected to hold the real work (behavior-identical layering); phase 4 is
mostly mechanical.

### Roadmap status (2026-10-30)

Phases 1–4 shipped with their gates green (`degenbot-simulation` feature
`evm2,evm2-jit`: 218 tests; full `just test` per phase):

1. `state_override` port (infallible — evm2's storage insert skips revm's
   backing prefetch) and `Evm2SimSession` (settle verb, typed view reads,
   escape hatch).
2. `Evm2SimInspector` — one inspector over the shared engine-neutral
   buffers (`SwapEventBuffer`/`CallTraceBuffer`/`CollectedSlots`, widened
   to `pub(crate)`); the `Rc<RefCell>` handle pattern is gone via
   `inspector_mut` + `NonStaticAny` downcast.
3. `Evm2ScratchEvm` + `Evm2CountingFrameDb`; the storage memo and
   divergence observer wired into the evm2 `BotStateDb`. The
   balance/base-fee relaxations are `Version` runtime flags — two revm
   compile-time feature-matrix entries are already dead on this side. The
   sequence overlay is evm2's internal accepted state; a base-fee
   rejection restarts the whole sequence (validation-only, so state and
   cold-read accounting are preserved).
4. `Evm2SimSession::run_sequence` — the called-out risk materialized:
   `State::checkpoint`/`rollback` covers the JOURNAL only (commits to the
   accepted overlay escape it, and the warmed cache keeps serving written
   values), so the journaled bundle cannot isolate on the shared EVM. The
   shipped shape builds a THROWAWAY EVM per sequence over the shared
   EVM's own database handle (`initial_mut`): steps commit into the
   throwaway's overlay, reads fall through and warm the shared stack, the
   overlay dies with the throwaway. Per-step writes stream through a
   `commit_with` sink (`account_changes`' iterator over-captures the EVM
   borrow under Rust 2024 rules). The invisibility-BETWEEN-calls proof is
   the gate's copy-step assertions, and post-sequence invisibility is
   asserted on both engines.

Phase 4 completed the hard-swap wiring (chosen over trait genericization
in review): `Evm2BlockSimHandle` (the production handle twin, OSAKA +
nonce-check off + composite inspector, driving the async RPC fallback
through the sync `BlockingAsyncDb` mirror of `WrapDatabaseAsync`),
`simulate_path_on_evm2` in `degenbot-arbitrage` (the 7-call seam's evm2
twin, policy verbatim), the dispatch's compile-time engine arms (one
engine per build; the parity harness runs both builds and compares), and
the `degenbot-python` `evm2` facade feature. The end-to-end offline
parity run (`dispatch_end_to_end_revert_fixture_matches_fixed_expectations`)
passes under both feature builds. One documented diagnostic-field
divergence: halt-frame gas (revm's error path reports a zeroed tracker,
evm2 reports the actual burn). Phase 3's false-empty provenance probe
remains unported (noted in `bot_state_db`), and the pytest probes against
an evm2-built extension are a lane decision ahead of phase 5.

## Post-cutover performance roadmap

Recorded 2026-10-30. These presuppose the evm2 engine is the one running
(the gate's end-state); they are the cutover's prize, not a reason to skip
its discipline. Ordered by expected impact on the measured hot path.

1. **AOT-compile the injected executor at build time.** The executor
   bytecode is committed in-repo and runs on every simulated candidate;
   `evm2-jit-build` compiles it to native code at `cargo build` time into
   an AOT artifact preloaded into the `ArtifactStore` at handle-build.
   Zero first-touch stall, zero worker-pool dependency, on the hottest
   contract in the system.
2. **JIT the tracked pool set at registration, persist across blocks.**
   Pool bytecode is immutable and the tracked set is a few hundred
   contracts; registration (already a lifecycle event) submits bytecode to
   the `JitBackend` worker pool. The warm code cache avoids RPC fetches;
   the JIT artifact store avoids interpretation. Blocks advance, the
   compiled set doesn't.
3. **Coroutine fan-out for cold misses.** The serial fan-out's structural
   weakness is a cold storage slot parking the whole queue on RPC
   (`WrapDatabaseAsync` blocks a thread). evm2's `AsyncDatabase` +
   `transact_async` yields the fiber instead: candidate k's read flies
   while candidate k+1 runs. The shared per-block `CacheDB` discipline is
   unchanged; cold misses overlap execution instead of serializing it.
4. **Speculative parallel candidate evaluation.** `ExecutedTx::detach` /
   `discard` / `commit` plus `StateChangeSource`/`Sink` streaming makes
   every candidate's read-set explicit and mergeable: per-worker EVMs over
   the shared warm read layer, detached pending states collected, only the
   winner applied. The EIP-7928 BAL support supplies the conflict-detection
   input for free.
5. **BAL-driven next-block prefetch.** The read-set stream doubles as a
   prefetch oracle: slots this block's candidates touched are next block's.
   Feed them into the storage memo as async prefetch at block start,
   hiding RPC latency inside the mempool idle gap.
6. **Frame replay on persistent JIT programs.** Pending-tx replays
   re-execute the same mempool transactions across consecutive blocks;
   their programs compile once and stay hot on the pending-tx strategy's
   time-to-bid critical path.

Sequencing: 1 and 2 first (always-on, low risk), then measure — the
`rpc_cache_fanout` shape settles whether the per-block budget is
execution-bound (4 next) or RPC-latency-bound (3 next).

## Consequences

- `rust/Cargo.lock` gains evm2 and its dependency tree; the default build
  neither compiles nor downloads them (optional dep).
- The revm-side pin inside the gate documents the `gas_params` caching
  footgun (`set_spec_and_mainnet_gas_params`, not `cfg.spec = ...`); the
  same trap applies anywhere the engine sets a spec directly.
- CI lanes are unchanged; the pilot runs on demand via
  `just test-rust-nextest -p degenbot-simulation --features degenbot-simulation/evm2`.
- Downstream crates (`degenbot-arbitrage`, facade, Python shell) see no
  surface change during the pilot; the strategy's `ExecuteEvm` trait bounds
  keep naming revm until the cutover.
