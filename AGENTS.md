# AGENTS.md

**Respond to user messages using ASD-STE100 Simplified Technical English only.**

## Architectural Vision

**Long-term goal: a set of first-class standalone Rust crates that together form a complete, functional MEV bot — no Python required.**

degenbot is migrating from a pure-Python library to a Rust core composed of standalone crates. The end state has two equally first-class consumers:

1. **Pure-Rust MEV bot.** Someone should be able to `cargo add degenbot` (the umbrella crate re-exporting the cores) and build a fully functional MEV bot using Rust components ONLY — event decoding, pool state, solvers, pump loop, swap encoding, the lot. No Python in the build, no Python at runtime.
2. **Python-driven MEV bot.** Someone in Python should be able to build a functional MEV bot using the Python interface as a **driver** over the same Rust core, via a thin PyO3 layer that translates Python calls into Rust calls.

The two consumers share one Rust core, and that core must eventually own **everything** a functional MEV bot needs: pool/token state, swap math, event decoding, solvers, the pump loop, swap encoding, *and* the infrastructure currently still Python-only — the database (persistence, not just ORM calls), RPC interaction, pub-sub, price oracles, DB-aware pool and lending-market updaters, simulation, and transaction submission. There is no piece of bot functionality that lives in Python indefinitely. The end state is a Rust core that can do **every action the bot requires**, driven either by a Rust consumer directly or by a Python interface shell that instructs the Rust core to do them. The framing is: **Rust is the engine; Python is a driver shell, not a co-implementation.**

**Today many components are still pure Python** (database via SQLAlchemy, RPC via web3.py, publisher/subscriber, price oracles, the DB-aware pool and lending-market updaters, simulation, submission). These are *all* on the migration path — none of them is a permanent Python responsibility. They are migrated **one at a time**: each port moves a piece of responsibility into a Rust core crate and converts the corresponding Python from an implementation into a **delegating shell**. The first and canonical migration is the **Polars-inspired three-layer architecture** (ADR-005), whereby a user drives a Rust-owned `Bot` through a PyO3 wrapper that translates Python calls to the Rust core:

| Layer | Where it lives | Holds |
|-------|-----------------|-------|
| **Rust core** | `rust/crates/degenbot-{core,-cl-math,-curve-math,-balancer-math,-abi,-decoders,-uniswap,-rpc,-bot}` — **zero `pyo3`** (enforced by `just check-no-pyo3-in-cores`) | data + state-machine logic + pure math + protocols (DexIdentity, encoders, decoders) |
| **PyO3 wrapper** | `rust/crates/degenbot-python/src/<domain>/**` | `#[pyclass]`/`#[pyfunction]` only — arg extraction → GIL release → core call → result wrap. **No business logic.** |
| **Python companion** | `src/degenbot/**` | user-facing API, docstrings, I/O orchestration, immutable config dual-tracking, `Fraction`-based display |

The **standalone-Rust-core constraint** is first-class: anything a standalone Rust consumer (`examples/standalone_consumer.rs`, `cargo add degenbot`) would need to build an MEV bot must live in a core crate from day one — never "move it later," which strands it across the future crate boundary.

**Directive for all refactoring and feature work:** every change must align with this direction. When evaluating a module against the architecture, apply the triage rubric in [`docs/migration-guides/three-layer-transition.md`](docs/migration-guides/three-layer-transition.md) and choose one of four dispositions (`done` / `partial` / `port-now` / `stays-python`). Do not introduce a Python mirror of Rust-owned state, do not add `pyo3` to a core crate (outside a feature gate), do not strand standalone-usable logic on the Python side, and do not build a backwards-compatibility layer for retired implementations.

**Canonical references:**
- [ADR-005](docs/adr/ADR-005-polars-inspired-three-layer-architecture.md) — the three-layer FFI decision (read this before any FFI/state-ownership work)
- [ADR-003](docs/adr/ADR-003-botcore-state-layer.md) — `Bot` as the single Rust state owner
- [`docs/architecture/rust-owned-bot.md`](docs/architecture/rust-owned-bot.md) — component map + pump/engine lifecycle ("Rust is the engine, Python is the cockpit")
- [`docs/migration-guides/three-layer-transition.md`](docs/migration-guides/three-layer-transition.md) — the rubric for evaluating a Python module and moving its responsibility to Rust

## Planning
Use `ergo` for all feature planning. Discover usage with `ergo --help` and `ergo quickstart`. Include detailed implementation and planning notes in the body of each task.

## Refactoring & Feature Development
Use Red/Green TDD while refactoring and implementing new features.

## Writing Discoverable Code
Agents (and humans) navigate by plain-text search, not hover or jump-to-definition. Make code resolvable in one search.

- **Exported symbols get descriptive names.** Prefer 2–4 words with at least one domain term (`calculate_tokens_out_from_v2_pool`, not `calc_out`). The shortest globally-unique name wins; uniqueness is tested by search.
- **Do not disambiguate via module path only.** `rust/crates/degenbot-uniswap/src/state.rs` communicates less at the call site than `uniswap_v2_state_pool_token_balances`. Use path conventions where they are rigid and absolute (e.g. `Input`/`Output` in encoder modules), but prefer the name itself to carry meaning.
- **One concept, one spelling.** Reuse existing vocabulary; avoid introducing synonyms (`lending_pool` vs `money_market`). When behavior changes, rename in the same commit.
- **Filenames are search queries.** Avoid bare-role names (`utils.rs`, `types.rs`, `helpers.rs`). Prefix with the domain (`uniswap_v3_tick_math.rs`, `erc20_token_registry.rs`). `lib.rs`/`mod.rs` are acceptable only as thin re-export entry points.
- **Doc comments at every export.** One line stating the sharpest thing the signature cannot show (units, timezone, ownership, ordering). Include the plain-word phrase someone would search ("token balance" not just `token_bal`).
- **Keep strings whole.** Do not build event names, error codes, or log prefixes with template interpolation; write full literals so log greps return to the source. Error messages start with a unique literal prefix (`"Pool state missing token0"`, not `"{context}: missing token"`).
- **Colocate tests.** Rust unit tests live in the same file; integration tests live under `tests/` or `rust/crates/<crate>/tests/` with a name matching the module they cover.
- **Mark dead ends.** Use `#[deprecated]` / `TODO` / `DEPRECATED` with a pointer to the replacement path.

### Commit checklist for discoverability
1. Can one search for each new exported name find its definition?
2. Would swapping two same-typed arguments fail the build? (Brand IDs, use newtypes, or add distinct parameter types.)
3. Is the one critical thing a caller must know written at the definition site?
4. Do all error/log strings appear verbatim in source?
5. Did behavior change without a corresponding rename?
6. When code moved, was it removed from the old location?

## Commands
Uses `just` (see justfile) and `uv` as the package runner. Key commands:

### Python
- `just test-python` - Run Python tests
- `just test-rust-python` - Run Rust-wrapped Python tests

### Rust
- `just test-rust` - Run Rust tests
- `just lint-rust` - Run Rust linter (clippy)

**Important**: The Rust extension is rebuilt automatically by **uv** (not maturin) whenever you run an `uv run ...` command. There is no import-time rebuild hook: maturin's editable install is a one-time build-and-place, and the `.so` is loaded straight from `src/degenbot/_ffi.abi3.so`. What keeps it fresh is the `[tool.uv] cache-keys` table in `pyproject.toml`, which watches `rust/**/Cargo.toml` and `rust/crates/*/src/**/*.rs`; when any of those is newer than the installed build, uv marks the package "installed, but not fresh" and rebuilds via maturin on the next `uv run` sync.

Prerequisite: the editable install's `.pth` must point at the live repo (`/workspaces/degenbot/src`). The devcontainer guarantees this — `UV_PROJECT_ENVIRONMENT` points at a container-local venv and `post-create.sh` runs `uv sync` to seed the editable install. Do NOT manually rebuild with `cargo build` (it produces an `.rlib`, not the abi3 `.so` uv loads) or recreate the virtual environment after making Rust code changes.

Recovery: if the `.so` ever goes stale (e.g. a venv copied from another machine whose `.pth` points at a dead path), force a clean rebuild:
`uv sync --reinstall-package degenbot`

### Combined
- `just test-all` - Run all tests (Rust + Python)
- `just lint` - Run lint and type checks (Rust + Python)
- `just format` - Run formatters (Rust + Python)

## Profiling (hotpath)

The `degenbot-bot` drain path (BlockPump → SolveCoordinator → EngineHandle) is instrumented with `#[hotpath::measure]` attributes and `hotpath::measure_block!` phase probes; the guard lifecycle lives in `rust/crates/degenbot-bot/src/profiling.rs`. hotpath is a non-optional dependency of `degenbot-bot` with `default-features = false`, so the macros resolve to **no-op stubs unless the `hotpath` Cargo feature is enabled** — zero compile-time or runtime cost in default builds, and the no-pyo3-in-cores invariant is unaffected (hotpath pulls no pyo3).

**Dev:** the `[tool.maturin] features` list in `pyproject.toml` compiles `degenbot-bot/hotpath` into every dev `uv sync` build, so the dev `.so` always has it. Profiling is then toggled at runtime by an env var — **no rebuild when you want to profile**:

```bash
DEGENBOT_HOTPATH=1 \
HOTPATH_SHUTDOWN_MS=300000 \
HOTPATH_OUTPUT_PATH=hp.json \
HOTPATH_OUTPUT_FORMAT=json \
HOTPATH_REPORT=functions-timing,threads \
uv run python examples/eth_backrun_v2_v3_v4_rust.py
```

`HOTPATH_SHUTDOWN_MS` forces a clean timed report from the long-running bot (the guard otherwise only drops at pump exit). For a live TUI view instead of a static report: `cargo install hotpath --features=tui` then `hotpath console` in another terminal while the bot runs.

`DEGENBOT_HOTPATH=1` is an **opt-in runtime gate** (not a build gate): without it no guard is constructed, so the singleton-guard invariant can't be tripped by default runs, tests, or a Python process hosting multiple bots. Set it to construct the guard; leave it unset to run uninstrumented.

**CI/CD:** release wheels exclude hotpath. The PyPI `maturin-action` passes `--features pyo3/extension-module`, which **overrides** the dev `[tool.maturin] features` list (verified empirically: the cargo invocation shows only `pyo3/extension-module`, no hotpath), so the shipped wheel's macros are no-op `lib_off` stubs with zero runtime penalty. CI's `just build-rust-extension` (`cargo build --features extension-module`) is already hotpath-free for the same reason.

**Extending:** the pattern for new instrumentation is `#[hotpath::measure]` on a function (`impl_type = "Type"` for inherent methods, `label = "..."` for trait impls), or `hotpath::measure_block!("phase_name", { ... })` for sub-function phases. They're no-ops unless `hotpath` Cargo feature + `DEGENBOT_HOTPATH=1` are both on, so sprinkle liberally — same discipline as `log::debug!`. To widen coverage to a library crate, add the crate as a non-optional dep with `default-features = false` and gate the real `hotpath/hotpath` feature behind a Cargo feature on that crate (see `degenbot-bot/Cargo.toml` for the pattern).

## Git Commits
Commit messages must follow the project convention enforced by `commitlint`. Git hooks are managed by [`prek`](https://prek.j178.dev/) and declared in [`prek.toml`](prek.toml). Run `just setup-git-hooks` once after cloning to install the hooks and the editor template. The hooks are:

- **`pre-commit`** — fast, non-mutating checks that catch a bad commit the instant it is made rather than at push time: the file-scoped Markdown lint and `# noqa: PLC0415` guard, plus the whole-crate linters (Rust fmt/clippy/no-pyo3, Python fmt/lint) run check-only over the staged tree. All use the `*-check` just recipes (no `--fix`), so staged files can never be dirtied; pre-commit stashes unstaged changes first, so a failure is always a real defect introduced by the commit being made.
- **`commit-msg`** — commitlint against `.commitlintrc.yml`.
- **`pre-push`** — a **commit-message re-lint** of the outgoing push range (safety net for `git commit --no-verify`; see `scripts/hooks/commitlint-push.sh`) plus the **slower build + test suite** (Rust build/test, Python build/test) that gates the push. Push can no longer fail on a fast lint — those ran at commit time — so a push block is always a build or test failure, not a formatting/lint slip. Bypass with `git push --no-verify` (CI still runs), or re-run a subset with `prek run --hook-stage pre-push --skip rust-test` etc.

Run hooks on demand with `uv run prek run` / `uv run prek run --all-files`. For manual commit-message range checks: `just lint-commits` (default: unpushed commits) or `just lint-commits main..HEAD`.

## Backwards Compatibility
- Unless directed otherwise, design standalone features without a backwards compatibility layer.

## Dual-Path Coverage Tests (ADR-005 standalone claim, mechanically enforced)

ADR-005 makes a first-class, testable claim: **Rust and Python are two consumers of one Rust core, both first-class.** Three tiers of tests enforce this mechanically — they live under `rust/crates/degenbot/tests/` (Rust consumer side) + `tests/standalone_parity/` (Python consumer side). **When you add or change a capability that crosses the FFI boundary, extend the corresponding tier.**

- **Tier 0 — standalone promotion.** `examples/standalone_consumer.rs` is gated by `just test-standalone` (a dependency of `just test-rust`, so CI + the pre-push hook run it). It proves a `cargo add degenbot` consumer reaches `BotState`/`DexIdentity`/calc math with no Python in the build graph. When you add a new standalone-reachable capability, add a slice to this example.
- **Tier 1 — reachability (static).** `rust/crates/degenbot/tests/reachability.rs` diffs *what the PyO3 binding (`degenbot-python/src`) reaches* against *what the `degenbot` umbrella re-exports (`src/lib.rs`)*. A `#[pyfunction]` delegating to `degenbot_foo::bar` where the umbrella forgot `pub use degenbot_foo` fails CI. When you add a new core crate the binding consumes, either `pub use` it from the umbrella **or** add it to `INTENTIONALLY_NOT_STANDALONE` with the migration-task / rubric disposition that owns the eventual standalone port (the self-cleaning guard (`allowlist_entries_are_still_actually_consumed`) forces the allowlist to only shrink — a crate that lands its `pub use` but stays on the allowlist fails).
- **Tier 2 — behavioral dual-driver parity (the real “both access the same functionality”).** A fixture driven through *both* consumers (`BotState` for Rust, `PyBot` for Python) must produce identical results. The seed corpus covers V2 `calculate_tokens_out`+`calculate_tokens_in`, V3 CL swap, V4 CL swap (sign-flipped path). **When you port a capability into the Rust core that a Python driver reaches (a new calc, an encoder, a decoder, a state-mutation path), add a dual-driver pair** — same canonical fixture, same expected output, in both `rust/crates/degenbot/tests/parity_<domain>.rs` and `tests/standalone_parity/test_<domain>_dual_driver.py`. Prefer a hand-derived closed-form oracle (V2 shape) so each side independently re-derives the expected constant; where no closed form exists (V3/V4 CL math), use a recorded constant + monotonicity/symmetry sanity checks, and note the weaker oracle in the test header (a non-circular re-derivation is its own TDD task).

**V3/V4 fixture drift — RESOLVED (HRT356).** The V3/V4 expected constants were *copied* between the Rust and Python files with no mechanical link: a fixture edit that updated one constant and forgot the other left both tests green but testing *different* fixtures. The fix is a single shared fixture file (JSON under `tests/standalone_parity/fixtures/`) read by both sides for inputs *and* expected outputs; new dual-driver pairs should use this shared-fixture shape rather than copied constants. Both V3 (`v3_swap.json`) and V4 (`v4_swap.json`) now load from the shared file on both sides; a deliberately-wrong edit fails BOTH sides (RED-verified).

**The two halves gate independently but both must pass.** CI's `rust-test` runs `just test-rust` (the Rust half); CI's `python-test` runs `just test-python` (the Python half). The pre-push hook runs both. Divergence between the halves = a lossy FFI seam (arg extraction, rounding, direction flag) that the static reachability gate cannot detect.

- **Tier 3 — on-chain accuracy oracle (the shared-bug-breaker; ADR-020).** Tier 2's CL-math oracle is a *Rust twin* (`v3_simulate_swap`/`v4_simulate_swap` re-deriving the same algorithm the engine uses), so an implementation bug shared by the engine and its twin is invisible to Tier 2. Tier 3 breaks that class by deploying the **canonical foundry-compiled reference contract** (V3 `UniswapV3Pool` / V4 `PoolManager`) as real bytecode in an in-process revm `CacheDB`, seeding its storage slot-for-slot from a `V3PoolState`/`V4State` via the `v3_storage_slots`/`v4_storage_slots` encoders, driving the swap, and asserting the Rust math + solver === the on-chain `Swap` event **byte-for-byte** (amount0/amount1/sqrtPriceX96/liquidity/tick). **When you port a new CL-math pool-state computation or multi-step solver crossing calc, add a Tier-3 oracle slice** — a pinned mainnet regression + a proptest over (pool state incl. SPARSE tick topologies spanning uninitialized word boundaries, amount, direction). The seed corpus: `just test-tier3-smoke` (forge→revm loop), `just test-tier3-step` (V3/V4 `computeSwapStep`), `just test-tier3-swap` (V3 `Pool.swap` deploy+seed foundation — the byte-exact swap + proptest is the remaining slice of `2LTKVO`), `just test-tier3-pancake` (the PancakeSwap V3 fork — real `PancakeV3Pool` bytecode from the Etherscan-verified source; proves byte-exact math AND that the 9-field `Swap` event variant decodes only via `v3_pancakeswap_swap_decoder`); the umbrella is `just test-tier3`. The oracle bytecode is COMMITTED under `tier3-oracle/artifacts/`, so the oracle tests run in the default `cargo test` path with NO toolchain at runtime, guarded two ways against stale bytecode: `tier3_harness_artifacts.rs` (toolchain-free) hashes the tracked harness sources against `artifacts/manifest.json`, and `just verify-tier3-artifacts` (the authoritative compile-vs-use check, in the CI `tier3-oracle` job) recompiles every harness with the real solc/forge toolchain and asserts the committed bytecode equals a fresh build. After editing a `tier3-oracle/src*/**/*.sol` harness (or bumping a pinned vendored lib), run `just rebuild-tier3-artifacts` and commit the refreshed `artifacts/`. **Mechanical enforcement is deferred** (per ADR-020) until the pattern is proven across V3 + V4 + proptest fuzz — tracked as `BQ43DK`'s reachability-style guard sub-item. Until then the AGENTS.md requirement is **prospective**: new CL-math or solver-crossing capabilities MUST add a Tier-3 oracle; existing families migrate per the `SH6HAK` cutover epic.

## V4 Fee-1 Over-Prediction Harness (ergo UO3JM4)

The live mainnet bot surfaced a Möbius-solver bug on fee-1-style V4 pools: the solver's V4-hop `hop_outputs[i]` (from the int-solve `compute_crossing`/`int_simulate_v3_swap` crossing path) **over-predicts `v4_simulate_swap` by a few wei** on fee-1/tiny pools, so the composer's `V4_TAKE(predicted)` overdrafts the output token → ERC20 "transfer amount exceeds balance" → the path reverts in simulation. It is **not** specific to fee-1 — it manifests across low-fee / `tick_spacing=1` V4 pools; the path shape is `V3(fee=30) → V4(fee=1-like) → {V3(fee=25) | V2}` (`zfo` alternates: true/false/true). A reconstruction harness + a live watch-and-capture toolchain exists so any future session can reproduce and pin a recurrence.

**Diagnostic vocabulary in `logs/bot_run.log` (the live `eth_backrun_v2_v3_v4_rust.py` bot):**
- `[sim-revert-swap] path_id=… hop=1 family=V4 emitter=<canonical PoolManager> actual_out=… predicted=… matched=false` — THE event: `predicted > actual_out` is the over-prediction. The emitter is always the canonical PoolManager, so **this line carries no `pool_id`**; resolve the pool offline (see `dump_pool_state.py`).
- `[solver-st] path_id=… hops=[V3:…;V4:…;V2:…]` — per-hop `sq`, `liq`, `fee`, `zfo` at solve time. **The `fee=` value is a display rounding artifact** (`fee_bps = 10000 − gamma_numer/100`); it is NOT the true pool fee (e.g. the fee-1 pool prints `fee=1` but is on-chain lp_fee **50**/1e6=0.005%). Always treat the on-chain value / `getSlot0` as authoritative.
- `[debug-v4-solve] pool details pool_id=… tick=… liquidity=… sqrt_price_x96=… protocol_fee=… n_ranges=…` — the solve-time V4 scalars + pool_id (the DB does not hold current V4 scalars).
- `[sim-diag] {"path_id":…,"solve_block":…,"optimal_input":…,"hop_outputs":[…],…}` — solve block + the solver's hop output vector.

The **DB is static** — the running bot does not write it. So the DB holds identity + tick_data (positions) but NOT live scalars (sqrt_price/liquidity/tick/protocol_fee); those come from the log lines above. Capture-time and investigate-time DB reads return identical data, so `dump_pool_state.py` can be run at any point after an event.

**Pieces (all committed):**
- `rust/crates/degenbot/examples/fee1_v3v4v3_solver_fixture.rs` — the repro harness (modeled 1:1 on `path13308_solver_fixture.rs`). Loads a fixture, reconstructs the three pools into `BotState`, runs the production Möbius solver, and asserts the fix target **byte-exact**: solver V4-hop output == `v4_simulate_swap` (the tier-3-proven on-chain oracle) == recorded `[sim-revert-swap] actual`. Run standalone: `cargo run -p degenbot --example fee1_v3v4v3_solver_fixture`. Exit 0 = PASS, exit 1 = RED (fix violated), exit 2 = state not computable. The comparison is **input-matched**: it feeds the solver's own `consumed_inputs[hop]` into `v4_simulate_swap` rather than the recorded input, so it never compares at different amounts.
- `tests/fixtures/fee1_v3v4v3_block25600000.json` — the default fixture: real recovered pools (V3 Uniswap `0x4e68ccd3…` WETH/USDT 0.30%; V4 USDC/USDT `pool_id 0x76f75965…` fee 50 spacing 1; V3 Pancake `0x19ac5f80…` USDC/WETH 0.25%) with DB tick_data (hop0 561 ticks, hop2 6, V4 single `[−2,+3]` position) + live scalars. The heuristic historical repro (9586 pred / 9585 actual) does **not** trip at the *current* optimal input (it needs a specific crossing/input), so this fixture currently PASSES — that is correct, not a broken harness.
- `scripts/capture_fee1_v3v4v3_fixture.py` — regenerate a fixture for a **specific solve block**: fills `TARGET` + pool identity + recorded solve, reads DB tick_data + `cast` scalars at TARGET, writes the fixture JSON consumed by the harness.
- `scripts/watch_fee1_overdraw.py` — **background watch** for live recurrences. Tails `logs/bot_run.log` (read-only — never truncates the bot log), detects new `matched=false` V4 over-predictions (`family=V4` + `actual < predicted`), and writes a compact snapshot to `logs/fee1_snapshots/<ts>_path<p>_hop<h>_block<b>/event.json` containing the event, the path's `[solver-st]`/(`[sim-diag]`/`[sim-fail]`/`[debug-v4-solve]` context (the live scalars the DB lacks), and solve_block. Detection scans only newly-appended lines (no duplicate snapshots).
- `scripts/dump_pool_state.py <pool_hash_or_address>` — pull the static DB identity + tick_data for a V4 pool (by `pool_hash`, 0x+64) or a V3 pool (by address, 0x+40; dispatches Uniswap/Pancake/Sushi by `pools.kind`). Emits JSON in the fixture `PoolJson` shape. Use it to resolve the `pool_id` for a captured event by matching the path's hop token pairs (`[sim-fail]` hops field) + solver-st/sq values.

**Workflow for a fresh recurrence:** (1) leave `scripts/watch_fee1_overdraw.py` running and let it capture the event snapshot; (2) from `event.json`, read the `[sim-fail]`/`[solver-st]` hop token pairs and solve for the V4 `pool_id` via `dump_pool_state.py`; (3) set the capture script's `TARGET` to `solve_block` + the recovered identity/recorded solve and run it to build the exact-block fixture; (4) run the harness — it must go RED at the failing input and PASS after the fix; (5) extend the appropriate Tier-3 oracle slice per ADR-020 for the new CL-math crossing case the recurrence exposes.

## Architecture & Domain Knowledge
**Start with the [Architectural Vision](#architectural-vision) above** — it states the long-term goal and the canonical references for the three-layer architecture. This section is the index into the remaining focused docs; read the relevant one before naming, editing, or extending a module.
- **[ADR records](docs/adr/)** — ADR-001 I/O-free pools, ADR-002 pool-type registry singleton, ADR-003 Bot as state owner, ADR-004 CL tickmap typed boundary, ADR-005 Polars-inspired three-layer FFI, ADR-006 per-chain bot orchestrator, ADR-007 pool unregister seam, ADR-008 block state machine, ADR-009 single-source-of-truth versioning, ADR-010 Alembic retention + Rust schema cutover, ADR-011 Auto-healed Alembic retirement (dump-and-restore cutover), ADR-012 spec-bound pool admission, ADR-013 FFI seam is private, ADR-014 pool-state deepening layer, ADR-015 solver-seam relocation (resolve→solve boundary), ADR-016 ReorgPoolState trait (pool-owned reorg rollback; refines ADR-014 D3), ADR-017 forward-apply pool-state traits, ADR-018 tracked debt — bot_core↔solvers fusion (extraction trigger: second engine family), ADR-019 in-process revm as sole simulation executor; strategy-vs-engine separation, ADR-020 Tier-3 on-chain accuracy oracle (revm + canonical reference bytecode)
- **[`docs/architecture/`](docs/architecture/)** — long-form architecture

### Schema ownership & Alembic retention (see [ADR-010](docs/adr/ADR-010-alembic-retention-and-rust-schema-cutover.md))

The database schema is **Alembic-owned during the 0.6.x point releases** and becomes **Rust-owned** in a 0.7 release. The cutover mechanism (`degenbot database cutover` + the `ensure_schema` `RustOwned` branch) is built and opt-in during 0.6.x so `pip` users can upgrade a stale database through the final Alembic revision and then cutover at a time of their choosing. Dropping the Alembic dependency and deleting the migration scripts is gated to 0.7 (ergo task `JFFQV2`).

**Forbidden-until-0.7 kill list.** No change before the 0.7 retirement task may delete or stub any of:

- `src/degenbot/migrations/` (the Alembic migration scripts) — deletion is gated on the `heal` operation shipping and being proven (epic `TGIP5N`, tasks T2-T5; see ADR-011) **and** the 0.7.0 release decision (T6 / `OXKANZ`), not just on the 0.7.0 version bump;
- the `alembic` and `sqlalchemy` entries in `pyproject.toml`;
- `DatabaseSessionManager` and the SQLAlchemy `src/degenbot/database/models/` package;
- the `ALEMBIC_HEAD` constant in `rust/crates/degenbot-db/src/schema.rs`;
- the `alembic_version`-reading branch of `rust/crates/degenbot-db/src/migrate.rs::ensure_schema`;
- the `PRAGMA query_only=on` setting on the `AlembicCurrent` path in `DegenbotDb::open`.

**An import falling out of use is not permission to delete it.** If a 0.6.x task makes an Alembic/SQLAlchemy symbol unused, leave it in place and note the orphaned symbol in the task completion summary; removal is the 0.7 retirement task's exclusive responsibility.
- **[`docs/migration-guides/`](docs/migration-guides/)** — completed refactors and the rubric for evaluating a Python module against the three-layer architecture
