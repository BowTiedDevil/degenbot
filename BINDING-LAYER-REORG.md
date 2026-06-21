# Binding-Layer Reorganization — Polars-Style Binding Crate Migration

**Status:** done. Tracked by ergo epic `UG6FKN` ("Polars-style binding-crate reorganization") + the follow-up ergo `DPSVCH` ("Lift binding layer to `crates/degenbot-python/`").
**Related:** ADR-005 (Polars-Inspired Three-Layer Architecture), ADR-003 (Bot as state owner),
`rust/AGENTS.md` (three-layer pattern + module organization). Reference project: `polars-python`
(`~/.cache/checkouts/github.com/pola-rs/polars/crates/polars-python/src/`).

> **Scope boundary.** This migration concerns the **organization of the binding
> crate** (the `degenbot_rs` cdylib — now at `rust/crates/degenbot-python/src/`) — file layout, `#[pymethods]` splitting, shared converters,
> registration, cargo features. It is **distinct from** the existing epic `XQ5UX6`
> ("Polars three-layer migration"), which tracks the **stateful companion** migrations
> (Python `V2/V3/V4/Curve/Balancer` companions over `Py*` wrappers — ADR-005 slices 1–15).
> No behavior changes; every step is behavior-preserving until step 7 (cargo features, additive).

## Context

degenbot recently split the Rust cores into seven `pyo3`-free workspace crates
(`degenbot-core`, `-cl-math`, `-abi`, `-rpc`, `-decoders`, `-uniswap`, `-bot`) plus the root
`degenbot_rs` cdylib binding layer at `rust/src/`. The crate split is done; the **binding layer**
still has the flat `*_py.rs` layout from when `pyo3` permeated a single crate. It is already
straining under its own growth:

- `py_binding.rs` — **3012 lines**, one `#[pyclass]` (`PyUniswapArbEngine`) with 100+ methods.
- `rust/src/subscription.rs` — a **mis-named duplicate** of `crates/degenbot-rpc/src/subscription.rs`
  (violates the project's own `*_py.rs` rule documented in `rust/AGENTS.md`).
- 21 flat files in one directory; shared converters (`alloy_py`, `py_cache`, `py_converters`,
  `json_converters`) are mixed in with `#[pyfunction]` wrappers and `#[pyclass]` impls.

Polars' `polars-python/src/` is the working reference for a binding crate of this shape: 109 files
across 14 domain subdirectories (`dataframe/`, `expr/`, `series/`, `lazyframe/`, `conversion/`,
`functions/`, `io/`, …), one `#[pymethods] impl` block per concerns-file, a shared `conversion/`
subdir, a tiny `lib.rs` plus a dedicated `c_api/mod.rs` registration site, cargo features
gating every module, and a `prelude.rs` taming the `use` boilerplate. The techniques below are
extracted directly from how Polars does each of those.

## The seven techniques

1. **Domain subdirs, not flat `*_py.rs`.** Drop the `_py` filename convention inside the binding
   crate — every file is "py" by default there. Mirror the underlying core crate's structure:
   `rust/src/bot/` ↔ `crates/degenbot-bot/`, `rust/src/abi/` ↔ `crates/degenbot-abi/`, etc. This
   is what `polars-python/src/{dataframe,series,expr,…}/` does — only one `*_py.rs` file in the
   whole crate (an incidental typo, `interop/arrow/to_py.rs`).

2. **Split `#[pymethods]` impls across files for one `#[pyclass]`.** PyO3 permits multiple
   `#[pymethods] impl PyX { … }` blocks per type. Declare the struct once in the domain subdir's
   `mod.rs`; drop one `#[pymethods]` block per concern into sibling files. Polars splits
   `PyDataFrame` across `dataframe/{general,io,construction,map,export,serde}.rs`; `PyExpr` across
   17 files; `PyLazyFrame` across 10. degenbot's `py_binding.rs` (`PyUniswapArbEngine`) gets the
   same treatment — split per the **existing** `uniswap_engine/` core layout
   (`crates/degenbot-bot/src/optimizers/uniswap_engine/{diagnostic,event_routing,solver_dispatch,
   lifecycle,snapshot_verify,result_channel}.rs`); just mirror those names in the binding layer.

3. **Shared `conversion/` subdir for non-`#[pyfunction]` converters.** Polars has a 5-file
   `conversion/` subdir (`any_value.rs`, `categorical.rs`, `chunked_array.rs`, `datetime.rs`)
   holding PyO3-dependent converters used by many wrappers but exposing no `#[pyfunction]`
   themselves, plus a root `Wrap<T>` newtype (`pub type PySchema = Wrap<polars_core::schema::Schema>;`).
   degenbot's `alloy_py.rs`/`py_cache.rs`/`json_converters.rs`/`py_converters.rs` are exactly this
   category. Move them into `conversion/{alloy,cache,json,rpc_types}.rs`.

4. **Dedicated `c_api.rs` for `#[pymodule]` registration.** Polars collects all 159
   `m.add_function(wrap_pyfunction!(...))` / `m.add_class::<...>()` lines in `c_api/mod.rs`,
   gated behind `#[cfg(feature = "c_api")]`; `lib.rs` is a thin module-declaration + re-export
   file. Pull the registration body out of `rust/src/lib.rs` into `rust/src/c_api.rs`; `lib.rs`
   keeps just the module tree, re-exports, and a one-line `#[pymodule] fn degenbot(...) { c_api::register(m) }`.

5. **Binding-crate cargo features mirroring core features.** Polars gates everything at the
   binding layer (`#[cfg(feature = "pymethods")] mod general;`, even
   `#[cfg(all(feature = "meta", feature = "pymethods"))]`), matching the core crate's features
   one-to-one. Add `degenbot_rs` features `bot`/`rpc`/`abi`/`cl-math`/`uniswap`/`decoders`/`async`
   gating `dep:` on each core path dep + `#[cfg(feature="...")]` on the matching module. This is
   the action that most directly prepares for ADR-005's deferral (binding crate becoming its own
   workspace member, `degenbot-python`).

6. **Resolve the duplicate-file naming the migration left behind.** `rust/src/subscription.rs`
   shouldn't exist — `crates/degenbot-rpc/src/subscription.rs` is the pure core; the binding
   sibling must carry the `Py` convention (per ADR-005 naming). Audit all root files for the same
   collision pattern; pick one rule and apply it uniformly.

7. **Binding-crate `prelude.rs` for `use` boilerplate.** Polars' `prelude.rs` is 3 lines:
   `pub use polars::prelude::*; pub use crate::conversion::*; pub(crate) use crate::py_modules;`.
   Every wrapper file opens with `use crate::prelude::*;` + `use super::*;` instead of ~15 lines
   of `pyo3::prelude::*` + `crate::alloy_py::*` + `crate::py_converters::*` + `degenbot_core::*`
   repeats. Add `rust/src/prelude.rs`; migrate wrapper `use` blocks incrementally.

## Migration sequence (with dependencies)

Each step is a behavior-preserving rename/split except step 7, which is additive (new features,
default-on, no surface change). Order is chosen so that earlier steps make later steps cleaner
and so that the big mechanical move (step 6) happens last when the file identities are settled.

```
Step 1 — Dedup naming (subscription.rs → subscription_py.rs; audit)   [no deps]
Step 2 — Extract #[pymodule] registration into c_api.rs              [no deps]
Step 3 — Add conversion/ subdir (alloy_py, py_cache, json, py_conv)  [no deps]
Step 4 — Add prelude.rs + migrate use blocks                          [deps: 3]
Step 5 — Split py_binding.rs per uniswap_engine/ core layout          [no deps]
Step 6 — Move files into per-domain subdirs (bot/, abi/, …)           [deps: 1,2,3,4,5]
Step 7 — Binding-crate cargo features (gates workspace-member split)  [deps: 6]
```

**Rationale for the dependency edges.** Steps 1–3 and 5 are independent and parallelizable —
they touch disjoint files. Step 4 depends on 3 because the prelude re-exports `conversion::*` —
land it after the conversion subdir exists so its paths are stable. Step 6 is the umbrella
rename commit and must land last among the structural steps to avoid fighting in-flight splits;
it depends on 1, 2, 3, 4, 5 so the file identities it moves are already final. Step 7 (features)
is additive but logically needs the clean module tree from step 6 to gate against.

## Target layout

```
rust/src/
  lib.rs              # thin: module tree, re-exports, one-line #[pymodule]
  c_api.rs            # all add_function/add_class (was inline in lib.rs)
  prelude.rs          # pub use degenbot_core::prelude::*; pyo3; conversion;
  error.rs            # binding-layer errors (if not all in degenbot-core)
  conversion/
    mod.rs
    alloy.rs          # was alloy_py.rs (PyU256/PyI256 + extractors)
    cache.rs          # was py_cache.rs (PyOnceLock refs)
    json.rs           # was json_converters.rs
    rpc_types.rs      # was py_converters.rs (block/log/dict → Py)
  bot/                # wraps degenbot-bot
    mod.rs            # PyBot struct (was py_bot.rs)
    pool.rs           # was py_liquidity_pool.rs
    token.rs          # was py_erc20_token.rs
    dex_identity.rs   # was py_dex_identity.rs
    engine/           # was py_binding.rs (split)
      mod.rs          # PyUniswapArbEngine struct + From/Clone/new
      register.rs     # register_pool/register_path/start/stop/freeze
      solve.rs        # solve_dirty/process_logs/latest_results
      verify.rs       # verify_* + VerifyRpc + #[create_exception] types
      snapshot.rs     # snapshot_* (mirrors core snapshot_verify.rs)
      result_channel.rs
      errors.rs       # #[create_exception] types
  abi/                # wraps degenbot-abi
    decoder.rs        # was abi_decoder_py.rs
    encoder.rs        # was abi_encoder_py.rs
  cl_math/            # wraps degenbot-cl-math
    tick_math.rs      # was tick_math_py.rs
    cl_lib.rs         # was cl_lib_py.rs
  rpc/                # wraps degenbot-rpc
    provider.rs       # was provider_py.rs
    contract.rs       # was contract_py.rs
    subscription.rs   # was subscription_py.rs (post step-1 rename)
    async_provider.rs # was async_provider.rs
    async_contract.rs # was async_contract.rs
  uniswap/            # wraps degenbot-uniswap (free fns, no classes)
    address.rs        # was address_utils_py.rs
```

## Validation (whole migration)

- `just test-rust` — Rust tests pass.
- `just test-python` and `just test-rust-python` — Python tests reaching the extension pass.
- `just lint-rust` — `clippy::pedantic` clean (the root `Cargo.toml` sets it to `warn`).
- `just check-no-pyo3-in-cores` — confirms no `pyo3` leaked into the seven core crates (this
  migration must not regress it; the binding-crate changes are confined to `rust/src/`).
- `uv run python -c "import degenbot_rs"` — extension imports cleanly.
- Manual: a representative end-to-end smoke (build a V2/V3 pool, run an arb solve) to confirm
  no shift in Python-facing behavior.

## Non-goals

- No change to the public Python API surface (function names, class names, signatures).
- No change to the Rust core crates (`crates/degenbot-*`) — binding-crate only.
- No stateful-companion work (that's epic `XQ5UX6`).

## Completed (post-execution)

- **Binding-crate workspace-member split** — originally listed above as a non-goal
  ("step 7 *prepares* for it but does not execute it"). Executed by ergo `DPSVCH`: the
  `degenbot_rs` binding layer now lives at `rust/crates/degenbot-python/` as a peer
  workspace member of the seven pyo3-free cores, and `rust/Cargo.toml` is a pure
  virtual manifest (workspace + profiles only). The standalone-`degenbot-core`
  Rust-consumer smoke test and the `degenbot` umbrella + `cargo add degenbot`
  re-export remain deferred under ergo `KWTAXJ` (ADR-005 slice 13).
