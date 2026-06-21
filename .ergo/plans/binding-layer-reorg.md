# Resolve duplicate-file naming left by the core-crate migration

## Goal
- Eliminate the `rust/src/subscription.rs` ↔ `crates/degenbot-rpc/src/subscription.rs`
  name collision (and any other duplicates found by audit) so the binding layer reliably
  signals "this is the PyO3 side" by file name.
- Unblocks the umbrella rename in step 6 — file identities must be stable before the
  per-domain subdir move.

## Context
- ADR-005's naming rule: the `Py` prefix is kept on the PyO3 wrapper struct AND as a
  Python-exposed name; the bare noun is reserved for the Rust core. By extension, a
  binding-side file should not collide in name with a core-crate file.
- `rust/AGENTS.md` already states the GIL-bound half of subscription moved to
  `subscription_py.rs` in slice 5 — but `rust/src/subscription.rs` still exists and is
  756 lines (GIL-bound, imports `crate::py_converters::*`).
- Other files to audit for the same pattern: anything in `rust/src/` whose stem matches a
  file in `crates/degenbot-*/src/`. (`provider_py.rs` vs `provider.rs` already follows the
  convention — check the rest.)

## Acceptance Criteria
- `rust/src/subscription.rs` is renamed to `subscription_py.rs` (interim — matches the
  current `*_py.rs` convention; the per-domain subdir move in step 6 will relocate it to
  `rpc/subscription.rs`).
- All `use crate::subscription::` / `mod subscription` references in `rust/src/` updated.
- Audit report (in the task completion note): the full list of `rust/src/*.rs` files
  checked, which (if any) besides `subscription.rs` collided, and how each was resolved.
- No remaining file in `rust/src/` whose stem matches a file in `crates/degenbot-*/src/`
  unless it carries a distinguishing suffix.

## Validation Gates
- `just test-rust`
- `just test-rust-python`
- `uv run python -c "import degenbot_rs; print('ok')"`
- `just check-no-pyo3-in-cores`

---
# Extract #[pymodule] registration into rust/src/c_api.rs

## Goal
- Pull the `#[pymodule] fn degenbot` body out of `rust/src/lib.rs` into a dedicated
  `rust/src/c_api.rs` so `lib.rs` is a thin module-tree + re-export file.
- Establish the single place future `#[pyfunction]`/`#[pyclass]` registrations land —
  matches `polars-python/src/c_api/mod.rs`.

## Context
- Today `rust/src/lib.rs` (217 lines) mixes three jobs: module docstring, ~80 lines of
  `pub use degenbot_*::…` re-exports, and a growing `#[pymodule]` body of
  `m.add_function(wrap_pyfunction!(...))?` + `m.add_class::<...>()?` lines.
- Polars puts all 159 registration calls in `c_api/mod.rs`, gated `#[cfg(feature="c_api")]`;
  the binding crate's `lib.rs` is tiny (module declarations + types).

## Acceptance Criteria
- New file `rust/src/c_api.rs` exporting `pub fn register(m: &Bound<PyModule>) -> PyResult<()>`
  containing every existing `m.add_function(...)` and `m.add_class::<...>()` call, in the
  same order as today.
- `rust/src/lib.rs` `#[pymodule] fn degenbot(_py, m) -> PyResult<()>` body reduced to a single
  call: `c_api::register(m)` (plus any module-level `m.add(...)` for submodules/exceptions that
  must run before/after — preserved with their relative order; anything that genuinely must be
  in the module init stays in `lib.rs` with a comment explaining why).
- No change to which functions/classes are registered or to their Python-visible names.
- `lib.rs` keeps the existing `pub use degenbot_*::…` re-exports unchanged (call sites depend
  on them).

## Validation Gates
- `just test-rust`
- `just test-rust-python`
- `uv run python -c "import degenbot_rs; from degenbot_rs import PyBot; print('ok')"`
- Diff sanity: the set of `add_function`/`add_class` calls in `c_api.rs` equals the set that
  was in `lib.rs` before (count + names match).

---
# Add conversion/ subdir for shared PyO3-dependent converters

## Goal
- Carve `alloy_py.rs`, `py_cache.rs`, `json_converters.rs`, `py_converters.rs` out of the
  flat `rust/src/` layout into a shared `rust/src/conversion/` subdir.
- Establish the `polars-python/src/conversion/` analogue: PyO3-dependent glue used by many
  wrappers, exposing no `#[pyfunction]` of its own.

## Context
- `rust/AGENTS.md` already names this as a category: "PyO3-dependent converters (no
  `#[pyfunction]`, but creates Python objects)" — examples `py_converters.rs`, `alloy_py.rs`,
  `py_cache.rs`. The current flat layout buries them among `#[pyfunction]` wrappers.
- Polars: `conversion/{any_value,categorical,chunked_array,datetime}.rs` + a root
  `Wrap<T>` newtype (`pub type PySchema = Wrap<polars_core::schema::Schema>;`).
- Current sizes for sizing: `py_converters.rs` 697, `alloy_py.rs` 335, `json_converters.rs`
  196, `py_cache.rs` 194. No logic changes — purely move + re-export.

## Acceptance Criteria
- New `rust/src/conversion/` dir with:
  - `mod.rs` declaring + `pub use`-ing each submodule
  - `alloy.rs`     (was `alloy_py.rs`)
  - `cache.rs`     (was `py_cache.rs`)
  - `json.rs`      (was `json_converters.rs`)
  - `rpc_types.rs` (was `py_converters.rs`)
- All existing `use crate::alloy_py::…`, `use crate::py_cache::…`,
  `use crate::json_converters::…`, `use crate::py_converters::…` call sites updated to
  `use crate::conversion::{alloy,cache,json,rpc_types}::…` (or via `conversion::` re-exports
  in `mod.rs` if cleaner).
- No behavior change; no logic edits inside the moved files.

## Validation Gates
- `just test-rust`
- `just test-rust-python`
- `just lint-rust`
- `uv run python -c "import degenbot_rs; print('ok')"`

---
# Add rust/src/prelude.rs and migrate wrapper use blocks

## Goal
- Add a binding-crate `prelude.rs` mirroring `polars-python/src/prelude.rs` so wrapper files
  open with `use crate::prelude::*;` instead of ~10–15 lines of repeated imports.
- Migrate the `use` blocks of every `*_py.rs` wrapper to use the prelude.

## Context
- Polars prelude (3 lines): `pub use polars::prelude::*; pub use crate::conversion::*; pub(crate) use crate::py_modules;`.
- Today each degenbot wrapper file repeats `use pyo3::prelude::*;`, `use crate::alloy_py::*;`
  (now `crate::conversion::alloy` post step 3), `use crate::py_converters::*;`, `use degenbot_core::*`,
  `use crate::py_cache::*;` etc.
- Depends on step 3 (`conversion/` subdir): the prelude re-exports `crate::conversion::*`, so
  it must land after the conversion paths are stable.

## Acceptance Criteria
- New `rust/src/prelude.rs` re-exporting at minimum: `pyo3::prelude::*`, the conversion
  submodule surface (`crate::conversion::*`), and `degenbot_core` surface used across wrappers.
- Keep it small — no `pub use *` of rarely-used items; aim for the smallest set that covers
  every wrapper's repeated imports (justify any item that's added but used in only one wrapper
  — those should stay as local `use`s).
- Every `*_py.rs` wrapper file's top-of-file `use` block reduced to `use crate::prelude::*;`
  (plus `use super::*;` and any file-specific imports); no behavior change.
- Gated/duplicate imports removed (no file imports a prelude item *and* the prelude).

## Validation Gates
- `just lint-rust` (clean)
- `just test-rust`
- `just test-rust-python`
- Manual spot-check: pick 3 wrapper files and confirm the `use` block is now ≤ ~3 lines plus
  file-specific imports.

---
# Split py_binding.rs per the uniswap_engine/ core layout

## Goal
- Break `rust/src/py_binding.rs` (3012 lines, one `#[pyclass] PyUniswapArbEngine` with 100+
  methods + several `#[create_exception]` types + helper impls) into a per-concern file set
  under `rust/src/bot/engine/`, mirroring the existing
  `crates/degenbot-bot/src/optimizers/uniswap_engine/` split.
- Use PyO3's support for multiple `#[pymethods] impl PyX { … }` blocks per type — declare
  the struct once, drop one `#[pymethods]` impl per concern.

## Context
- This is the highest-judgment step in the migration; the others are mechanical renames.
- Polars precedent: `PyExpr` split across 17 files (`expr/{array,binary,string,list,datetime,
  rolling,meta,…}.rs`), each holding one `#[pymethods] impl PyExpr { … }` block. `PyDataFrame`
  split across `dataframe/{general,io,construction,map,export,serde}.rs`. Core struct declared
  once in `mod.rs` alongside `From`/`Clone`/`new` (non-py helpers only).
- degenbot's core already factored `uniswap_engine` into
  `{diagnostic,event_routing,solver_dispatch,lifecycle,snapshot_verify,result_channel}.rs` —
  mirror those names in the binding layer so the split is "obvious from the core".

## Acceptance Criteria
- New `rust/src/bot/engine/` directory (note: created flat in `rust/src/bot/engine/` here;
  step 6 will relocate the surrounding `bot/` files alongside — keep `engine/` self-contained
  so step 6 moves it as a unit):
  - `mod.rs` — the `#[pyclass] struct PyUniswapArbEngine` declaration, `From`/`Clone`/helper
    (non-py) impl blocks, and `pub use`/`mod` declarations for the sibling files.
  - `register.rs` — `#[pymethods]` block: `register_pool`, `register_path`, `start`, `stop`,
    `freeze` (registration + lifecycle-entry surface).
  - `solve.rs` — `#[pymethods]` block: `solve_dirty`, `process_logs`, `latest_results`, any
    result-retrieval surface.
  - `verify.rs` — `#[pymethods]` block: all `verify_*` methods + the `VerifyRpc` /
    `EngineVerifyRpc` impls + the `verify_*_rejected` exception types they raise.
  - `snapshot.rs` — `#[pymethods]` block: snapshot_* surface (mirrors core `snapshot_verify.rs`).
  - `result_channel.rs` — `#[pymethods]` block: result-channel surface (mirrors core
    `result_channel.rs`).
  - `errors.rs` — all `#[create_exception]` types currently in `py_binding.rs`.
- `rust/src/py_binding.rs` deleted; `lib.rs` / `c_api.rs` registration rewritten to point at
  `crate::bot::engine::PyUniswapArbEngine`.
- Any cross-file helper used by multiple concern-files kept in `mod.rs` as `pub(crate)`.

## Validation Gates
- `just test-rust`
- `just test-rust-python` — especially the engine / arb-solve test suites.
- `just lint-rust`
- Manual: every `#[pyfunction]`/`#[pymethods]` symbol previously exposed is still exposed
  (diff the `#[pymodule]` registration set before/after; must be identical).
- Confirm `#[pymethods]` is not duplicated for the same method (PyO3 errors at build time, but
  also grep to verify).

---
# Move binding-layer files into per-domain subdirs

## Goal
- Replace the flat `rust/src/*.rs` layout with per-domain subdirs mirroring the seven core
  crates, per technique 1 — the umbrella mechanical rename that supersedes the `*_py.rs`
  filename convention inside the binding crate.

## Context
- Target layout lives in `BINDING-LAYER-REORG.md` ("Target layout" section). Source mapping:
  `rust/src/bot/` ↔ `crates/degenbot-bot/`, `rust/src/abi/` ↔ `crates/degenbot-abi/`, etc.
- Depends on steps 1–5 so that file identities are final (subscription_py renamed, c_api
  extracted, conversion subdir in place, prelude landing, py_binding already split into
  `bot/engine/`) — moving anything earlier fights in-flight renames.
- The `*_py.rs` filename convention is **dropped inside the binding crate**: every file there
  is "py" by default; the crate boundary (workspace split / `just check-no-pyo3-in-cores`)
  is the separator that matters. Polars precedent: only one `*_py.rs` file in all of
  `polars-python/src/`.

## Acceptance Criteria
- Files relocated, renamed per the target layout in `BINDING-LAYER-REORG.md`:
  - `py_bot.rs` → `bot/mod.rs`, `py_liquidity_pool.rs` → `bot/pool.rs`,
    `py_erc20_token.rs` → `bot/token.rs`, `py_dex_identity.rs` → `bot/dex_identity.rs`.
  - `bot/engine/` (from step 5) moved alongside as a subdir.
  - `abi_decoder_py.rs` → `abi/decoder.rs`, `abi_encoder_py.rs` → `abi/encoder.rs`.
  - `tick_math_py.rs` → `cl_math/tick_math.rs`, `cl_lib_py.rs` → `cl_math/cl_lib.rs`.
  - `provider_py.rs` → `rpc/provider.rs`, `contract_py.rs` → `rpc/contract.rs`,
    `subscription_py.rs` → `rpc/subscription.rs`, `async_provider.rs` → `rpc/async_provider.rs`,
    `async_contract.rs` → `rpc/async_contract.rs`.
  - `address_utils_py.rs` → `uniswap/address.rs`.
  - `lib.rs`, `c_api.rs`, `prelude.rs`, `conversion/` stay at root.
- Every `use crate::…` and `mod …` reference updated across `rust/src/`.
- `c_api.rs` `add_function(wrap_pyfunction!(<path>, m))` paths updated to the new module paths;
  `add_class::<…>` paths likewise.
- `rust/AGENTS.md` "Module Organization" / "Module Naming Convention" table updated to reflect
  the new layout (drop the blanket `*_py.rs` rule for the root cdylib; note that the rule now
  applies only to any future standalone binding crate that mixes pyo3 with shared converters).
- `rust/CONTEXT.md` PyO3-handle terms updated if they reference file paths.

## Validation Gates
- `just test-rust`
- `just test-rust-python`
- `just lint-rust`
- `just check-no-pyo3-in-cores`
- `uv run python -c "import degenbot_rs; from degenbot_rs import PyBot, PyLiquidityPool, PyUniswapArbEngine; print('ok')"`
- Manual: confirm no `*_py.rs` files remain in `rust/src/` except inside `conversion/` if there
  are any genuine conversion-only modules named that way (there shouldn't be).

---
# Add binding-crate cargo features mirroring core crates

## Goal
- Add `degenbot_rs` (the root cdylib binding layer) cargo features gating each `dep:` path
  dependency and matching `#[cfg(feature="...")]` on the corresponding binding module, so a
  consumer can build the extension with a subset of surfaces.
- This is the action that most directly prepares for the ADR-005 deferral (binding crate
  becomes its own workspace member, `degenbot-python`).

## Context
- Polars gates `polars-python` with features (`pymethods`, `c_api`, `sql`, `catalog`, …)
  mirroring `polars-core`/`-plan`/etc. one-to-one; `#[cfg(feature="pymethods")] mod general;`.
- degenbot's core crates today carry only a `pyo3`/`test-utils` feature each (per
  `rust/AGENTS.md`); the root `degenbot_rs` cdylib is monolithic — no binding-layer features.
- This is **additive**: default features preserve today's behavior exactly; the work is
  inserting `cfg` gates + feature deps without changing what ships by default.

## Acceptance Criteria
- `[features]` in `rust/Cargo.toml` defines (default all-on):
  ```
  default = ["bot", "rpc", "abi", "cl-math", "uniswap"]
  bot      = ["dep:degenbot-bot"]
  rpc      = ["dep:degenbot-rpc"]
  abi      = ["dep:degenbot-abi"]
  cl-math  = ["dep:degenbot-cl-math"]
  uniswap  = ["dep:degenbot-uniswap"]
  decoders = ["dep:degenbot-decoders"]
  async    = []    # gates rpc/async_provider.rs + rpc/async_contract.rs (no new dep)
  ```
- Each path dep moved under `[features]` accordingly (already-path-deps remain path deps,
  just optionally enabled).
- `#[cfg(feature = "bot")] pub mod bot;` (etc.) in `lib.rs`, and each domain `mod.rs` gates
  its submodule declarations where the feature would not be on.
- `c_api.rs` registration of classes/functions gated so a build with a feature off doesn't
  reference symbols that aren't compiled (e.g. `#[cfg(feature = "bot")] m.add_class::<PyBot>()?;`).
- **Default build is byte-equivalent in surface** to today: `cargo build` with defaults
  produces the same `#[pyfunction]`/`#[pyclass]` set.
- An audit note in the completion body: any implicit cross-feature coupling discovered
  (e.g. "engine wrapper imports subscribe from rpc even though engine uses an injected pump")
  and a recommendation per finding.

## Validation Gates
- `just test-rust` (default features)
- `just test-rust-python` (default features)
- `just lint-rust`
- `cargo build -p degenbot_rs --no-default-features --features bot` succeeds (and a couple
  of other minimal feature combinations).
- `cargo build -p degenbot_rs --no-default-features` fails cleanly at build (no missing-symbol
  panics at runtime) — confirm the error messages point at the gated symbols.
- `uv run python -c "import degenbot_rs; print('ok')"` (default features, byte-equivalent).
