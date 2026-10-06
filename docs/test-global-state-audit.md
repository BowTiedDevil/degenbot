# Test-touched process-global state — audit

Epic `64ZQLA` (host-shape independence) task `I4EJ4N`. Systematic pass over
every piece of process-global state that the Rust test suites touch, after two
2026-09 failures that only fired on a raw 24-core host while staying green in
the devcontainer:

- `ccc148275` — parity test binned at `solve_worker_count()` (host cores)
  against hermetically booted fleet seats → 22 bins on 6 seats → fail-loud
  abort at the T2 grant;
- `50b76dd24` — the state-lock dump test asserted the global `ACTIVE_READS`
  table renders `"(none)"` while parallel strangers registered foreign holds
  inside its diagnostic window.

The class of bug: **a test's observable outcome depends on (a) another test's
window into a shared global, or (b) machine-derived sizing**. Container
parallelism happened to keep both latent.

## Method and coverage

`rg` over `rust/crates/**` for `static`/`LazyLock`/`OnceLock` declarations
(219 total sites scanned; ~90 are mutable-relevant), plus env reads
(`degenbot-config/tests/no_stray_env_reads.rs` already enforces the env-read
inventory), plus `available_parallelism`/cgroup/`Instant::now` reachability.
Each mutable, test-touchable site is classified:

- `safe-serialized` — every concurrent consumer sits behind one gate that
  actually covers all walkers of the value.
- `racy-by-parallelism` — parallel test threads may observe/mutate within a
  test's window; outcome depends on scheduling.
- `host-shape-coupled` — the value or a threshold derives from
  `available_parallelism`, cgroup files, core counts, or clocks.

Dispositions: `leave` (safe or already mitigated; reason recorded), `gate`
(add serialization), `inject` (parameterize the seam), or `property`
(covered by a suite under this epic).

## Findings

### degenbot-bot

| Site | Kind | Classification | Disposition |
|---|---|---|---|
| `bot_core/state_lock.rs` `DIAG`/`WARN_THRESHOLD_MS`/`TRACE_BACKTRACES` — global flag flips inside the state-lock diag tests | shared register-enable window | racy-by-parallelism (mitigated) | **leave** — `50b76dd24` made the only stranger-sensitive assertion own-row-tolerant; residual foreign effects are warning logs only. The `test_serial` gate covers all `state_lock` diag tests against each other. |
| `bot_core/state_lock.rs` `ACTIVE_READS`/`ACTIVE_WRITES`/`SLOW_READ_DROPS` | shared hold registry | racy-by-parallelism (mitigated) | **leave** — same ownership pattern (own-key assertions; tolerant fn is the accepted depth). |
| `failure_policy.rs:166` `OVERRIDES` — `installed_override_governs_action` installs `verify_mismatch -> Observe` into a process-global `OnceLock` | shared decision-table override | racy-by-parallelism (latent, benign today) | **leave** — the only overridden bucket (`verify_mismatch`) is asserted by no other parallel reader (`matrix_rows_match_adr_040_table` asserts `severity`, not `action`, for it), and the `FIRST` OnceLock keeps the install single-shot. **Standing rule** recorded here: any future test asserting `action()`/`bucket()` on a bucket another test may override must assert its own bucket only, or the override must ride a typed config. |
| `failure_policy.rs:300` `COOLDOWNS` | keyed cooldown registry | safe-serialized | **leave** — tests use independent keys (`cooldown_keys_are_independent`); registry keyed, no cross-key reads. |
| `allocator_ctrl.rs` `AUTO_ENABLED`/`INIT_DONE`/`VERSION_OK` | global stance + one-shot logging | safe-serialized (by ignore) | **leave** — the two env/singleton-touching tests are `#[ignore]` with a "run alone" contract; the default-track tests never toggle the flag. |
| `arb_engine/{fleet_solve,fleet_sim,fleet_registration}_executor.rs` `FLEET_{SOLVE,SIM,REGISTRATION}_BOOT` + the lazy `*_EXECUTOR` `OnceLock`s | construction boot descriptor — install-then-read static + `fallback_boot` absence hatch (scope note: the LIVE half of the old `SOLVE_FLEET_HOSTED` row; that symbol is DELETED from the codebase — zero hits — and the old `engine_stages.rs:507` flip citation was a stale-path phantom: `engine_stages.rs` lives under `arb_engine/`, and the live flip is `arb_engine/tests.rs:7431`) | **racy-by-parallelism → FIXED (YI5NGB)** | **inject — fixed (YI5NGB)**: each engine now OWNS its `FleetBoot::from_config(cfg)` as a construction value — packed as the stamped `BootStamp {boot, engine_id, cfg_hash}` field in `with_core_cfg` (mod.rs:592, the KAHU5W sibling of `streaming_delivery`/`runtime_cfg`) — and the per-role `OnceLock<BootStamp>` statics are the identified COURIER to the single process-wide fleet materialization. The lazy `get_or_init` window per role (`global_fleet_solve_executor` fleet_solve_executor.rs:458, sim :382, registration :415) is CLOSED BY CONSTRUCTION: `fallback_boot` and its ambient-derivation arm are DELETED — a stamp-less fetch aborts `expect(...)`-loud (never a silent boot nobody chose), and any post-first-write construction with a DIFFERENT-cfg boot is recorded in the fleet-boot-audit ledger (prod: count + one warn log per winner/rider pair; tests: ILLEGAL — ledger `panic!`). Census rows + `work-fleet-*` thread names are keyed per ROLE (the five `fleet_solver_slots`/`fleet_simdriver_slots`/`fleet_resolve_slots`/`fleet_merge_slots`/`fleet_pool_state_updater_slots` rows, upserted by resource string in worker_census.rs from `role.census_resource()` at role.rs:115–119; per-role name patterns `work-fleet-solver-{n}`/`work-fleet-sim-{n}`/`work-fleet-resolve-{n}`/`work-fleet-merge-{n}`/`work-fleet-poolupd-{n}` at role.rs:129–140) — every module/resource name here is the tree's verbatim spelling: `fleet_solve_executor.rs`, `fleet_sim_executor.rs`, `fleet_registration_executor.rs` (underscored file names; there is no hyphenated `fleet-registration` module in the tree). They stay single-population: ONE process fleet remains the invariant (only the three materializers call `FleetHost::boot`; per-engine fleets were REJECTED on exactly these census/GOQWCL grounds — `logs/boots-design.md` §4 Option A). The stance statics formerly in this row's scope: the RESOLVE_PAR flip → its own row (next row below); STREAMING/INLINE_SIM/MIN_PROFIT/PROJECTION-MEMO are out of this row (live non-construction consumers; future task). |
| `arb_engine/solver_dispatch.rs` `RESOLVE_PAR_STANCE` (the old row's phantom `engine_stages.rs:507` pointed at a flip of this static's predecessor; the LIVE flip was `arb_engine/tests.rs:7431` / restore :7487) | construction stance for the chunked-parallel resolve arm — install-then-read AtomicBool, ONE test-driven A/B flip site | **racy-by-parallelism → FIXED (YI5NGB)** | **inject — fixed (YI5NGB)**: the static is DELETED; the stance becomes an `ArbitrageEngine` instance field `resolve_par_stance` packed from `cfg.solve.solve_resolve_par` at construction (the KAHU5W sibling of `streaming_delivery` and `detached_solving`); the A/B test (`resolve_chunk_parity_parallel_matches_serial_and_reuses_cache_walks`, tests.rs:7370) drives BOTH arms through the test-only `set_resolve_parallel_for_test` instance mutator — the two runs keep byte-identical coverage (profit/hop-shape/projection-delta parity + sharded-cache walk-once), the process-global flip/restore pair and its parallel-order dependence are GONE. |
| `arb_engine/sim_slots.rs` `CAP`/`SLOTS` | first-init defaults | safe-serialized | **leave** — no test installs; all consumers accept the first-initialized default, which is the prod default. |
| `metrics.rs:165` `GLOBAL`, `otel.rs:85` `HANDLE`, `instruments.rs:961` `PIPELINE` | init-once telemetry singletons | safe-serialized | **leave** — no parallel-asserting tests found; init is idempotent; extra instrumentation from strangers is unobservable in assertions today. |
| `arb_engine/solver_dispatch.rs:1814`, `solve_executor.rs:118,134` — `solve_worker_count()` as prod sizing input under the legacy (non-fleet) stance | host-shape-coupled prod sizing | host-shape-coupled | **property** — covered by `CVURM7`/`TTANQJ` (the quota→slot authorities) and structurally by `ccc148275` (bins bind at the executor's own seat count). |

### degenbot-core

| Site | Kind | Classification | Disposition |
|---|---|---|---|
| `cpu_budget.rs:219,277` `SOLVE_WORKERS`/`AMBIENT_WORKERS` `OnceLock`s — cached host-derived counts | host-derived cache | host-shape-coupled | **property + inject** — the pure `*_from_with_roots`/`solve_worker_count_from` seams exist and are the sanctioned test surface (`CVURM7`); no test may assert a host-derived count. |
| `worker_census.rs` `CENSUS`/`BOOT_DUMPED`/`EXPORT_HOOK` | append registry + one-shot dump | safe-serialized | **leave** — registry is mutex-guarded; tests assert *contains/sorted* (strangers benign); the boot dump is logging only. |
| `runtime.rs:46` `RUNTIME` (`OnceLock<Runtime>`), `IO_RT_SEQ` | init-once runtime singleton | host-shape-coupled (io worker count) | **leave** — sized from the ambient budget once; no test asserts its width. |

### degenbot-workers, degenbot-simulation, other crates

| Site | Kind | Classification | Disposition |
|---|---|---|---|
| `degenbot-workers/gauges.rs` `DASHBOARD_HOOK` | init-once hook | safe-serialized | **leave** — hook install is idempotent, no test asserts a specific hook. |
| `degenbot-simulation/sim/evm/serving.rs` `SERVE_ENABLED` + `SERVE_TEST_GUARD`; `divergence_probe.rs` `PROBE_ENABLED`/`TALLY` + `TALLY_TEST_GUARD` | global stance + shared tally | safe-serialized | **leave** — the test-guard mutex is exactly the pattern this audit prescribes for stance-shaped globals. |
| `degenbot-simulation/sim/evm/sim_metrics.rs` counters | metric counters | safe-serialized | **leave** — write-only in the suite; no test re-reads absolute values. |
| `degenbot-pools`/`degenbot-math` `tests/alloc_tracking.rs` `#[global_allocator]` shim + `ACTIVE`/`BYTES`/`ALLOCS` | allocator-wide counters | safe-serialized | **leave** — opt-in `DEGENBOT_ALLOC_TRACK=1`; the measuring harness is a single test binary with a single measuring `#[test]` driving all phases on one thread (the pool file's second test is the gate-off pass-through). |
| `degenbot-abi` `abi_types/cached.rs` `CACHE_TEST_MUTEX` | cache test gate | safe-serialized | **leave** — the canonical test-guard-mutex pattern. |
| `degenbot-uniswap` `deployments.rs` `TABLE`; `degenbot-python` `dex_identity` `PRESETS`, `conversion/rpc_types.rs` field sets, `diagnostics/thread_registry.rs`/`gil_probe.rs` | read-only tables / thread-scoped diagnostics | safe-serialized | **leave** — `LazyLock`-initialized read-only data; diagnostics are thread-scoped or monotone. |
| `degenbot-config` `holder.rs` `CFG`/`DEFAULT` `OnceLock`s | init-once typed config | host-shape-coupled (config source) | **leave** — no test installs a config (verified across `degenbot-config/tests`); config enters tests as values, not via the holder. `cpu_budget` reads it once (cached) — see the core rows above. |

## Python test suite (`tests/**`)

The Python side of the same class. Method: an AST sweep over every
module-level assignment in `tests/**/*.py` whose value is a mutable literal
or constructor (`dict`/`list`/`set` literals and comprehensions,
`dict()`/`list()`/`set()`, `threading.Lock` kin) — 77 sites at sweep time —
followed by a whole-module walk of each site for runtime mutation (mutating
method calls, subscript assignment, augmented assignment). Env writes are
swept separately (`os.environ` outside `monkeypatch`), as is host-shape
reach (`sched_getaffinity`, cgroup reads, `cpu_count`).

| Site | Kind | Classification | Disposition |
|---|---|---|---|
| `tests/arbitrage/test_arbitrage_session.py` `_POOL_ID_LOCK`/`_POOL_IDS` — module-level id cache mutated by `_pool_id_for`, never reset | cross-test shared dict (pool ids depended on test execution order under pytest-randomly/xdist) | racy-by-parallelism | **inject — fixed (arb-session determinism pass)**: the cache moved into the `_FakeCtxBot` double that needs it; ids are per-bot-instance and `test_pool_ids_resolve_identically_per_fresh_bot` pins order-independence. The re-sweep reports zero runtime-mutated module-level mutable assignments in `tests/**`. |
| the remaining module-level `dict`/`list`/`set`/comprehension sites — 76 at sweep time, 72 after the pass consolidated the constant tables into `tests/helpers/` — constant fixture tables: ABI fragments, golden address tables, expected-vector matrices, `__all__` exports | import-time constants, read-only by usage (the mutation walk finds zero writers) | safe-serialized | **leave** — each module is re-imported per xdist worker and no site is written after import, so no test can observe another test's window. A future runtime-mutated site should fail this sweep; the row above is the worked example of the fix shape. |
| env writes in `tests/**` | process env | safe-serialized | **leave** — all writes ride `monkeypatch` (auto-undone; `tests/helpers/rpc_env.py` is the one canonical chain-1 RPC env installer behind the six autouse fixtures), or live inside child-subprocess script templates (`_CHILD`/`_FLEET_DRIVER`/`_LEGACY_CHILD`/`_BASE_LEVEL_PROBE` in `test_runtime_status`/`test_registration_intake_station`/`test_boot_refusal`/`test_env_read_timing`), where the child controls its own env by construction — the documented pass/skip-wobble hermeticity fix. No parent-level bare `os.environ` write exists. |
| host-shape reach: `os.sched_getaffinity`/cgroup mirrors in `test_registration_intake_station.py`/`test_runtime_status.py`; `os.sched_setaffinity(0, {0})` in `test_boot_refusal.py` | host-derived sizing / simulated small host | host-shape-coupled | **property** — the affinity/quota reads are parent-side *prediction* mirrors (`_fractional_quota_cpus`) that adapt the test's own expectation (xfail gating) and never assert an absolute host count; the child subprocess stays the sizing authority. `test_boot_refusal` simulates the small host inside the child; the parent's affinity is untouched. No Python test asserts a `cpu_count()`-derived value. |

## Systemic guards already in place

- `degenbot-config/tests/no_stray_env_reads.rs` — inventoried env-read sweep.
- `just check-no-pyo3-in-cores` / `just check-no-inner-allow` — contain the
  surface property tests and fixes must respect.
- Property tasks `CVURM7`/`TTANQJ`/`JXCAR4` under this epic pin all
  host-derived sizing formulas to their pure seams.

## Items requiring action (summary)

1. ~~`SOLVE_FLEET_HOSTED` + fleet-boot `OnceLock` TOCTOU~~ —
   **FIXED (YI5NGB)**: the fleet boots are construction-stamped (the
   engine owns its boot; the per-role lazy materialization window is
   closed by construction and red-tested; mixed-cfg rides are
   ledgered); the phantom `engine_stages.rs:507` citation and the
   dead `SOLVE_FLEET_HOSTED` symbol are dropped; the last
   test-owned stance flip (`RESOLVE_PAR_STANCE`, live at
   `arb_engine/tests.rs:7431`) retired to a construction-packed
   instance field. See the rewritten rows above and the
   point-of-record note below.
2. Standing rule for the failure-policy override store — recorded in the
   table above; no code change.

## Point of record

The 2026-09-10 architecture review that flagged this residue
(Candidate 3, 'one boot surface'), its adversarial verification, and
the honest bounding of the live scope (the `FLEET_*_BOOT` OnceLock
census; `SOLVE_FLEET_HOSTED` already retired; the
`engine_stages.rs:507` citation exposed as a stale-path phantom) are
recorded at the repository root:
[`architecture-review-20260910-095446-adversarial.md`](../architecture-review-20260910-095446-adversarial.md)
(§'Candidate 3 — one boot surface'; Candidate-3 residual-scope cites
at its tail). The reviewed report itself:
[`architecture-review-20260910-095446.html`](../architecture-review-20260910-095446.html).
Both documents are LINK-ONLY records — edit THIS file's rows, never
the review artifacts.
