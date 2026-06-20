# Plan 103: Split the Rust monolith into scoped workspace crates

## Overview

Break the single `degenbot_rs` crate (42,870 LoC, ~75 files, one `Cargo.toml`)
into a Cargo workspace of six scoped crates along the three-layer seams the
codebase already follows logically but does not enforce at the crate boundary.
The goal is compile speed (incremental per-crate caching, parallel leaf-crate
codegen, a lighter `cdylib` that only relinks when bindings change) and a
sharper enforcement of the "no `pyo3` in the Rust core" rule from
`rust/AGENTS.md`.

## Problem

### Deletion test

If you deleted `rust/src/tick_math.rs` (the root module): nothing would break.
It is dead code — a stale duplicate of `cl_lib/tick_math.rs` that has no
internal importer. The crate root declares `pub mod tick_math;` and never
re-exports anything from it; `tick_math_py.rs:17` consumes
`cl_lib::tick_math::{...}`; `lib.rs:63` re-exports `cl_lib::tick_math::{...}`.
The cl_lib version is strictly more complete (`max_usable_tick` /
`min_usable_tick` / `compress` exist only there). This file is the canary for
the broader problem: in a 43k-line single crate, dead and duplicated code goes
undetected because nothing enforces the intended layering.

If you deleted the *crate boundary* itself (i.e. collapsed the whole thing into
one crate — which is the status quo): you'd get the current slow rebuilds and
no protection against `pyo3` leaking into pure-Rust cores. The split earns its
keep by making the seams Cargo-enforceable.

### Specific friction

| Friction | Where | Why it hurts |
|----------|-------|--------------|
| Monolithic recompile | `rust/Cargo.toml` (single crate) | A one-line tick-math tweak recompiles 42,870 LoC including the 3,458-line `uniswap_engine/py_binding.rs` PyO3 surface; no per-seam incremental cache |
| No parallel leaf codegen | `rust/src/` (flat) | Cargo cannot compile `cl_lib/`, `abi_types/`, `provider.rs` concurrently — they are one codegen unit |
| `cdylib` relinked on every logic change | `Cargo.toml` `[lib] crate-type = ["rlib","cdylib"]` | Pure logic edits trigger the slow `lto="thin"` + `codegen-units=1` + `strip` release profile that only the Python extension needs |
| `pyo3` not compile-enforced out of cores | `rust/AGENTS.md` "Module Naming Convention" | The "no `pyo3` in `foo.rs`" rule is a lint-by-convention; `errors.rs` already bakes `impl From<*> for PyErr` (lines 60, 76, 124, 143, 233, 309) into the shared error types every core depends on |
| Dead/duplicated code undetected | `rust/src/tick_math.rs` (root) | 403-line stale duplicate of `cl_lib/tick_math.rs`; only discoverable by grep, no compiler help |
| Symmetric coupling hidden as one crate | `bot_core/` ↔ `optimizers/` | ~30 mutual `use crate::bot_core` ↔ `use crate::optimizers` references look like one module but are logically two peer layers (ADR-003) |

## Solution

A six-crate workspace, split along the three-layer seams. Every arrow points
down; `degenbot-py` is the only crate whose *core* depends on `pyo3`.

```
degenbot-core        ← errors (sans PyErr), hex_utils, address_utils, runtime, dex_identity
  ↑
degenbot-cl-math     ← cl_lib/* (+ reconciled root tick_math)    [alloy::primitives only]
degenbot-abi         ← abi_types, abi_decoder, abi_encoder, signature_parser
degenbot-rpc         ← provider, subscription, contract (+ async cores)
  ↑
degenbot-bot         ← bot_core/ + optimizers/ merged (state + solvers, by ADR-003)
  ↑
degenbot_rs (root)   ← lib.rs #[pymodule] + all *_py.rs + errors→PyErr impls + extension-module
```

The `cdylib` stays at the workspace **root** (`rust/Cargo.toml` is both workspace
root and the `degenbot_rs` package) rather than moving into `crates/`. This is the
"degenbot-py" binding layer conceptually; it keeps `manifest-path = rust/Cargo.toml`
(`pyproject.toml:222`) unchanged and avoids relocating 9k LoC of PyO3 surface +
the integration tests. Only the pure cores move into `crates/`.

### Crate boundaries

| Crate | LoC (≈) | Contents | Depends on |
|---|---|---|---|
| `degenbot-core` | ~900 | `errors.rs` (without `From<*> for PyErr`), `hex_utils`, `address_utils`, `runtime`, `dex_identity` | (leaf) |
| `degenbot-cl-math` | ~3.2k | `cl_lib/` (bit_math, full_math, functions, liquidity_math, sqrt_price_math, swap_math, tick_math, unsafe_math); root `tick_math.rs` deleted (dead duplicate) | `core` |
| `degenbot-abi` | ~3.4k | `abi_types/`, `abi_decoder.rs` (pure `decode_rust`), `abi_encoder.rs`, `signature_parser.rs` | `core` |
| `degenbot-rpc` | ~6.7k | `provider.rs`, `subscription.rs` (core `drain_raw`), `contract.rs` (core `FunctionSignature`); `async_provider`/`async_contract` cores | `core`, `abi` |
| `degenbot-bot` | ~17k | `bot_core/` + `optimizers/` merged into one crate — the coupled state+solver blob (ADR-003 refuses to over-abstract this seam) | `core`, `cl-math`, `rpc` |
| `degenbot_rs` (root cdylib) | ~9k | `lib.rs` (`#[pymodule]`), `cl_lib_py`, `tick_math_py`, `address_utils_py`, `provider_py`, `contract_py`, `subscription_py`, `py_converters`, `py_cache`, `alloy_py`, `json_converters`, `bot_core::py_*` wrappers, `optimizers::uniswap_engine::py_binding`, the `From<Error> for PyErr` block | all five + `pyo3/extension-module` |

### Design decisions

- **Keep `bot_core` + `optimizers` in one crate (`degenbot-bot`), do not split state from solver.** ADR-003 explicitly refuses to extract the `LiquidityMap` generic ("no abstraction against sample of one"). The ~30 mutual references are genuine domain coupling (state needs `IntHopState`/`IntV3TickRangeSequence`/decoders; solver needs `BotState`/`V3PoolState`/`TickInfo`/`PoolStateSubscriber`). Splitting would require an artificial shared-trait crate the ADRs reject.
- **`errors.rs` lives in `degenbot-core`; the `From<*> for PyErr` impls stay in core behind a non-default `pyo3` feature.** The current file bakes pyo3 into the shared error types every core transitively depends on, defeating the no-pyo3-in-core rule. The conversions stay *in core* (not relocated to the binding crate) because of the **orphan rule**: `PyErr` (pyo3) and the error types (degenbot-core) are both foreign to the root cdylib, so `impl From<CoreError> for PyErr` cannot legally live in the root crate. The standard pyo3 pattern — feature-gated conversions co-located with the type — sidesteps this cleanly: core is pyo3-free under default features (the red-check invariant), and the root cdylib + Python-extension build enables the `pyo3` feature. Pure-Rust consumers build core with default features and pull no pyo3.
- **Do not rename `py_*.rs` → `*_py.rs` in this plan.** The `py_` prefix is the established convention for the `bot_core` stateful-wrapper family (`py_bot`, `py_liquidity_pool`, `py_erc20_token`, `py_dex_identity`) and the conversion modules (`py_converters`, `py_cache`) — called out in the `rust/AGENTS.md` convention table. Unifying the prefix is a large mechanical churn (touches `lib.rs`, all `mod` declarations, imports) that does not improve compile speed, the primary goal of this plan. Defer to a separate naming-consistency pass if desired.
- **`degenbot-py` is the only `cdylib`; pure cores get a faster dev profile.** The slow `lto="thin"` / `codegen-units=1` / `strip` release profile applies only where Python extension packaging demands it. Logic-only edits never relink the Python extension.
- **Workspace root keeps `manifest-path = rust/Cargo.toml`.** `pyproject.toml:222` already points there; maturin auto-detects the single `cdylib` member. No Python-side change.
- **`abi_types/cached.rs:595` → `abi_decoder::decode_rust` is test-only.** Belongs in `degenbot-abi` together — no cross-crate cycle once both are in one crate.
- **Root `tick_math.rs` is deleted, not moved.** It is a stale duplicate of `cl_lib/tick_math.rs`; nobody imports it (`crate::tick_math::` returns zero hits). The cl_lib version is strictly more complete. Its unique in-file tests are subsumed by `cl_lib/tick_math.rs`'s own proptest (`roundtrip_any_valid_tick`).

## Files Involved

**Primary:**
- `rust/Cargo.toml` — single package → `[workspace]` manifest + member resolver
- `rust/crates/degenbot-{core,cl-math,abi,rpc,bot,py}/Cargo.toml` — new per-crate manifests
- `rust/src/lib.rs` → `rust/crates/degenbot-py/src/lib.rs` — module registration unchanged, imports re-pointed
- `rust/src/errors.rs` → split: error types to `degenbot-core`, `From<*> for PyErr` to `degenbot-py/src/error_conversions.rs`
- `rust/src/tick_math.rs` — **deleted** (dead duplicate of `cl_lib/tick_math.rs`)

**Secondary (mechanical `use crate::` → `use degenbot_*::` rewrites):**
- Every file in `rust/src/` moves under `rust/crates/<crate>/src/`; imports re-point to external crate paths

**No change needed:**
- `pyproject.toml:222` (`manifest-path = rust/Cargo.toml`) — workspace root stays the manifest; maturin resolves the single cdylib member
- `justfile` rust targets — already use `--manifest-path rust/Cargo.toml`, which works for the workspace + member targeting via `-p`
- `rust/tests/python_integration.rs`, `rust/tests/concurrency_stress.rs` — move under workspace or stay as a `degenbot-py` test target; maturin feature `auto-initialize` moves to `degenbot-py`
- `rust/CONTEXT.md`, `rust/AGENTS.md` — terminology unchanged (the three-layer pattern is unchanged; only crate *enforcement* is added)

## Implementation Order

Each slice leaves `just test-rust` + `just lint-rust` green and ships independently.

### Slice 0: Remove dead `tick_math.rs` (canary)

Red/green TDD on the existing `cl_lib/tick_math` proptest as the guard.

1. Confirm `grep -rn "crate::tick_math::" rust/src` returns zero hits (already verified).
2. Remove `pub mod tick_math;` from `rust/src/lib.rs:55`.
3. Delete `rust/src/tick_math.rs`.
4. Run: `just test-rust` — expect green (the cl_lib proptest still covers the fns).
5. Run: `just lint-rust` — expect green.

### Slice 1: Scaffold the workspace (no moves yet)

1. Convert `rust/Cargo.toml` to `[workspace]` with `members = ["crates/*"]`, `resolver = "2"`.
2. Create a single member `crates/degenbot_rs` that re-exports from `src/` (temporarily) — i.e. move the existing single-package `Cargo.toml` body into `crates/degenbot_rs/Cargo.toml` pointing at `src = "../../../src"` (or move `src/` under the member).
3. Run: `just test-rust` + `just dev` — expect green. The build now goes through the workspace indirection but is still one crate.
4. This slice proves the maturin/workspace plumbing before any splitting.

### Slice 2: Extract `degenbot-core`

1. Move `hex_utils.rs`, `address_utils.rs`, `runtime.rs`, `errors.rs` into `crates/degenbot-core/src/`. (`dex_identity.rs` stays in `bot_core/` — it is bot-domain data, not foundational core; moves in slice 6.)
2. `degenbot-core` deps: `thiserror`, `alloy` (primitives), `tokio` (for `runtime`), **optional** `pyo3` behind a non-default `pyo3` feature.
3. In `errors.rs`, gate the `use pyo3::{...}` import and all six `impl From<*> for PyErr` blocks behind `#[cfg(feature = "pyo3")]`. The inter-error conversions (`TickMathError→ClMathError`, `AbiDecodeError→ContractError`, `ContractError→ProviderError`) stay unconditional (pure Rust). The orphan rule forbids relocating these impls to the root cdylib (both `PyErr` and the error types are foreign there), so they legitimately live with the types under a feature gate.
4. Root `Cargo.toml` adds `degenbot-core = { path = "crates/degenbot-core", features = ["pyo3"] }` and registers the member.
5. Root `lib.rs`: replace the four `pub mod errors;`/`pub mod hex_utils;`/`pub mod address_utils;`/`pub mod runtime;` declarations with `pub use degenbot_core::{errors, hex_utils, address_utils, runtime};` — every `crate::errors::` / `crate::hex_utils::` call site in non-core code keeps resolving through the re-export, zero edits to call sites.
6. Red: `cargo tree -p degenbot-core` (default features) must not contain `pyo3` — failing-first CI check. Added as a `just` target.
7. Run: `just test-rust` + `just lint-rust` — expect green.

### Slice 3: Extract `degenbot-cl-math`

1. Move `cl_lib/` into `crates/degenbot-cl-math/src/`.
2. `degenbot-cl-math` depends only on `degenbot-core` (`errors::ClMathError`/`TickMathError`) + `alloy::primitives`. No pyo3, no tokio, no RPC.
3. Update `use crate::cl_lib::` → `use degenbot_cl_math::` across `degenbot_rs`.
4. Red: `cargo test -p degenbot-cl-math` runs the roundtrip proptest **without** the `auto-initialize` feature / Python interpreter.
5. Run: `just test-rust` — expect green.

### Slice 4: Extract `degenbot-abi`

1. Move `abi_types/`, `abi_decoder.rs` (pure `decode_rust`/`encode_rust`/`decode_for_types` only — see wrapper split below), `abi_encoder.rs` (pure core only), `signature_parser.rs` into `crates/degenbot-abi/src/`.
2. **Wrapper split:** `abi_decoder.rs` and `abi_encoder.rs` each mixed a pure-Rust core with `#[pyfunction]` wrappers + Python-conversion helpers that depend on `alloy_py` (a root binding-layer module). Since `alloy_py` lives in the root cdylib, the wrappers cannot move into `degenbot-abi` without creating a root→abi→root cycle. Split each file: pure core (`decode_rust`/`encode_rust`/etc. + the pure unit/proptest tests) → `degenbot-abi`; the `#[pyfunction]` wrappers + `abi_value_to_python`/`map_decode_error` + the pyo3-touching `test_abi_value_to_python_roundtrip` → new root files `abi_decoder_py.rs` / `abi_encoder_py.rs`.
3. The `cached.rs:595` → `abi_decoder::decode_rust` reference is now intra-crate — no cycle. `abi_types::value` uses `crate::hex_utils::decode_hex` as a private delegate — re-pointed to `degenbot_core::hex_utils`.
4. Root `lib.rs` re-exports `pub use degenbot_abi::{abi_types, abi_decoder, abi_encoder, signature_parser}` so all existing `crate::abi_types::` / `crate::abi_decoder::` call sites (`contract`, `contract_py`, `alloy_py`, `bot_core::v2_encoding`) keep resolving with zero edits.
5. Run: `just test-rust` — expect green. Roundtrip encode→decode proptest runs in-crate.

### Slice 5: Extract `degenbot-rpc`

1. Move `provider.rs`, `contract.rs` (both pure, zero pyo3) into `crates/degenbot-rpc/src/`. `provider` deps: `degenbot_core::{errors, address_utils}`. `contract` deps: `degenbot_abi::{abi_types, signature_parser, abi_encoder, abi_decoder}` + `degenbot_core::errors` + `provider`.
2. **Split `subscription.rs`** along the existing `drain_raw` seam: the pure core (`SubscriptionHandle`, `RawSubItem`, `RawDrainResult`, `drain_raw`, the `pump_*` drivers, `spawn_subscription_arc`, `impl AlloyProvider::subscribe_*`, the pure unit tests) → `degenbot-rpc`; the GIL-bound `DrainResult` / `convert_item` / `drain_buffer` (which construct Python objects via `py_converters`) → absorbed into root `subscription_py.rs` (the `use crate::subscription::{drain_buffer, DrainResult}` import becomes local).
3. The `provider_py` / `contract_py` / `subscription_py` / `async_provider` / `async_contract` wrappers stay in root (they need `py_cache` / `py_converters` / `alloy_py`); root `lib.rs` re-exports `pub use degenbot_rpc::{provider, contract, subscription}`.
4. **Widen `pub(crate)` → `pub`** on rpc-core items consumed across the crate boundary (`provider_arc`, `build_provider`, `from_provider`, `RawDrainResult`, `drain_raw`). These were `pub(crate)` when everything was one crate; the core is now a library whose public surface is the binding layer.
5. **`test-utils` feature** for `AlloyProvider::from_provider` (a test-only constructor used by `bot_core::block_pump` tests). In a library crate, `#[cfg(test)]` is not visible to *consumers'* tests — so the constructor is re-gated behind a non-default `test-utils` feature, and the root enables it only in `[dev-dependencies]` (production builds pull degenbot-rpc without it).
6. Run: `just test-rust` — expect green. Provider retry/log-fetch + subscription double-buffer tests run without the Python interpreter.

### Slice 6: Consolidate `degenbot-bot` (bot_core + optimizers)

1. Move `bot_core/` and `optimizers/` together into `crates/degenbot-bot/src/`. ADR-003 keeps them in one crate (the state/solver seam is genuine domain coupling — ~30 mutual `crate::bot_core` ↔ `crate::optimizers` refs, now intra-crate and UNTOUCHED).
2. **Split the pyo3 wrappers out to root.** `py_bot.rs` / `py_liquidity_pool.rs` / `py_erc20_token.rs` / `py_dex_identity.rs` (from `bot_core/`) and `py_binding.rs` (from `optimizers/uniswap_engine/`) stay in the binding layer (root flat modules) because they use `crate::alloy_py` / `crate::runtime` — root-only glue. In each wrapper file, `crate::bot_core::` → `degenbot_bot::bot_core::`, `crate::optimizers::` → `degenbot_bot::optimizers::`, `crate::runtime`/`provider`/`errors`/`address_utils`/`hex_utils` → the `degenbot_core`/`degenbot_rpc` paths; `crate::alloy_py` stays (root). One ordering subtlety: `crate::bot_core::py_bot::` → `crate::py_bot::` (the wrappers reference each other now as root flat modules) must run BEFORE the general `crate::bot_core::` → `degenbot_bot::bot_core::` rewrite.
3. `pub(crate)` → `pub` widening across the crate (86 sites, same as slice 5): `BotState` fields/methods, `log_dispatcher` registry, etc. The crate is now a library whose public surface IS the binding layer.
4. `pub(super)` → `pub` (the crate's `uniswap_engine` submodules widened from `pub(super)` to `pub` because the binding layer is now outside the `uniswap_engine` parent).
5. Make `snapshot_verify` + `engine_handle` + `engine_subscriber` `pub mod` — `py_binding` (now root) needs their types cross-crate; a private `mod`'s `pub` items aren't reachable from a downstream crate (dead-code lint fires). Making the modules `pub` exports their `pub` items.
6. Two `UniswapEngine` struct fields (`core`, `path_pools` + the `MixedPath.pools` field) widened to `pub` — `py_binding` accesses them directly (they were sibling-private before).
7. Root `lib.rs`: re-export `pub use degenbot_bot::{bot_core, optimizers};`; declare the 5 flat wrapper modules; rewire the `#[pymodule]` refs (`bot_core::py_bot::PyBot` → `py_bot::PyBot`, `optimizers::uniswap_engine::PyUniswapArbEngine` → `py_binding::PyUniswapArbEngine`, the 4 exception types → `py_binding::*`).
8. **Test-module `use super::*` pitfall:** py_binding's `#[cfg(test)] mod tests` had `use super::*;` (referring to py_binding's own scope). The blanket sed rewrote it to the engine glob (wrong). Restored `use super::*;` (child modules can see private parent items via glob) + added explicit `use degenbot_bot::...::snapshot_verify::VerifyError;` for the engine verification types the tests assert on.
9. The `#[pyclass]`/`#[pyfunction]`/`create_exception!` items DO NOT get a `pyo3` feature gate (unlike slice 2's errors) — they live in the root binding layer, not in `degenbot-bot`. The pure `degenbot-bot` crate (default features) is fully pyo3-free (verified by `cargo tree`).
10. Red: `cargo test -p degenbot-bot` runs `uniswap_engine/tests.rs` (2k LoC) + the full bot/solver suite in-crate without a Python interpreter.
11. Run: `just test-rust` — expect green.

### Slice 7: Finalize the root cdylib as the binding layer

1. The root `degenbot_rs` package (the workspace root + cdylib) is the binding layer; no relocation of `lib.rs` or `*_py.rs` files is needed (they stayed at the root throughout slices 2–6).
2. No `_py_errors.rs` relocation is needed — the `From<*> for PyErr` impls legitimately live in `degenbot-core::errors` behind the `pyo3` feature (orphan-rule decision, slice 2). The root enables the feature via its `degenbot-core` dependency.
3. Confirm `crate-type = ["rlib","cdylib"]`, `extension-module` feature, `auto-initialize` feature, and the `#[ctor::ctor]` Python-init-before-threads (lib.rs) all live on the root package.
4. Red: `just test-rust` + `just test-rust-python` + `just dev` produce the importable `degenbot_rs` Python extension; full suite green.
5. Run: `just test-all` + `just lint` — expect green.

### Slice 8: Validate and clean up

1. Run `just lint` + `just test-all`.
2. Confirm `cargo tree -p degenbot-core`, `-p degenbot-cl-math`, `-p degenbot-abi`, `-p degenbot-rpc`, `-p degenbot-bot` contain no `pyo3` (the no-pyo3-in-core rule is now compiler-enforced).
3. Add a release/dev profile split: `lto`/`codegen-units=1`/`strip` on `degenbot-py` only; faster dev profile on pure cores.
4. Update `rust/AGENTS.md` "Module Organization" table to reflect the crate boundaries.
5. Mark this plan complete and move to `plans/completed/`.

## Testing

### Per-slice test runs

Each slice runs `just test-rust` (Rust unit + `auto-initialize` integration) and `just lint-rust` (clippy `-D warnings`). Slices 1 and 7 additionally run `just dev` (maturin develop) and `just test-rust-python` (PyO3-wrapped Python tests) to validate the maturin/workspace plumbing.

### New unit tests

- **Slice 1/2**: a workspace integrity test — a `just` target or CI step that runs `cargo tree -p <core-crate> | grep pyo3` and fails if it matches. Enforces the no-pyo3-in-core rule at CI level.
- **Slice 3**: the existing `cl_lib/tick_math` proptest (`roundtrip_any_valid_tick`) now runs standalone via `cargo test -p degenbot-cl-math` without `auto-initialize` — assert this in CI.
- **Slice 6**: `uniswap_engine/tests.rs` (existing, 2k LoC) gates the bot_core↔optimizers consolidation.

No new test *files* are required — the refactor is structural, and the existing test suite (unit proptests + `tests/python_integration.rs` + `tests/concurrency_stress.rs`) covers the behavior.

### Integration tests

- `rust/tests/python_integration.rs` and `rust/tests/concurrency_stress.rs` (require `auto-initialize`) move to `degenbot-py`'s test target. These validate the full PyO3 surface round-trips correctly after the split.

## Benefits

- **Locality**: related code in one crate — CL math ports together, ABI types/decoder/encoder together, the coupled state+solver blob together rather than split across `bot_core`/`optimizers` with 30 mutual `use crate::` qualifiers.
- **Seam**: the three-layer pattern (pure Rust core / PyO3 binding / Python module) moves from lint-by-convention to compiler-enforced — `degenbot-core` through `degenbot-bot` cannot depend on `pyo3` because they don't list it as a dependency.
- **Leverage**: a tick-math change rebuilds ~3.2k LoC (`degenbot-cl-math`) + dependents, not 42k. An ABI change doesn't recompile the solver. Cargo parallelizes the independent leaf crates (`cl-math`, `abi`, `rpc`).
- **Depth**: the `cdylib` crate (`degenbot-py`) becomes a shallow binding seam — `#[pymodule]` + type conversion only — over a deep `degenbot-bot` core, matching the pattern `rust/AGENTS.md` prescribes.
- **Standalone-core goal (ADR-005)**: pure cores are now usable in a non-Python Rust context, which ADR-005 explicitly states as a goal but the single-crate layout prevented.

## Risks

- **`pub(crate)` / `pub(super)` / private-field widening is the recurring lesson of stateful slices.** `bot_core`/`optimizers`/`rpc`/`abi_types` all used `pub(crate)` for items that the binding layer (now a separate crate) consumes. Mitigation: blanket `pub(crate)`→`pub` + `pub(super)`→`pub` sed per crate, then let clippy surface the genuinely-vestigial dead code. For struct *fields* the wrapper touches directly (`UniswapEngine.core`/`.path_pools`, `MixedPath.pools`), widen case-by-case.
- **Private modules vs. downstream reachability:** a `mod snapshot_verify;` (private) whose `pub` items are used by a wrapper now living in a downstream crate triggers dead-code errors (the items aren't exported, so unused-within-crate). Mitigation: make such modules `pub mod` when the binding layer drives them.
- **`errors.rs` ↔ `PyErr` (slice 2):** the `From<*> for PyErr` impls cannot live in the root cdylib (orphan rule: both `PyErr` and the error types would be foreign). Mitigation: feature-gated `pyo3` feature on `degenbot-core`, impls co-located with the types; root enables the feature; pure-core builds (default features) pull no pyo3.
- **Maturin workspace + `cdylib` auto-detection** is mature in maturin 1.x but needs `manifest-path` at the workspace root and a single cdylib member. Mitigation: slice 1 validates the plumbing with a single no-op member crate before any splitting, and slice 7 runs `just dev` early.
- **`profile.release` semantics change under workspaces** — currently crate-wide, applies per-package after. Mitigation: slice 8 explicitly splits the profile (slow LTO profile on `degenbot-py`, fast dev profile on pure cores) and benchmarks a representative edit-rebuild.
- **`lib.rs` crate-root re-exports** (`pub use address_utils::{...}`, `pub use cl_lib::tick_math::{...}`) are a de-facto API for any non-Python consumer. Mitigation: keeping the cdylib at the workspace root (slice-7 design decision) means these re-exports never move — Python-visible names don't shift; no non-Python consumer exists today.
- **The `auto-initialize` feature and `#[ctor::ctor]` Python-init-before-threads** (lib.rs:101) must land in `degenbot-py`, not a core crate. Mitigation: slice 7 moves both; the concurrency-stress test gates correctness.
- **Larger mechanical `use` rewrite surface** (~every file) risks import typos. Mitigation: `just lint-rust` (`cargo clippy --deny warnings`) catches unused/wrong imports on every slice; one slice per crate boundary keeps the diff reviewable.

## Relationship to Other Plans

- **ADR-003** (Bot as state owner) / **ADR-005** (Polars-inspired three-layer FFI) / **ADR-006** (per-chain bot orchestrator): this plan *enforces* these ADRs' layering at the crate boundary. The `degenbot-py`↔`degenbot-bot` seam is the ADR-005 three-layer specialization; keeping `bot_core`+`optimizers` together honors ADR-003's refusal to over-abstract the state/solver seam.
- **Plan 100** (botcore state layer / engine dissolution): complementary — it reorganized `bot_core` internals; this plan re-scopes crate boundaries without re-touching internal structure.
- **Independent** of the dead-code `tick_math.rs` removal (slice 0), which is a standalone cleanup that proceeds regardless of whether this plan is adopted.

## Status

[x] Slice 0: remove dead `tick_math.rs`
[x] Slice 1: scaffold the workspace (single no-op member)
[x] Slice 2: extract `degenbot-core`
[x] Slice 3: extract `degenbot-cl-math`
[x] Slice 4: extract `degenbot-abi`
[x] Slice 5: extract `degenbot-rpc`
[x] Slice 6: consolidate `degenbot-bot` (bot_core + optimizers)
[x] Slice 7: finalize root cdylib as binding layer
[x] Slice 8: validate and clean up
