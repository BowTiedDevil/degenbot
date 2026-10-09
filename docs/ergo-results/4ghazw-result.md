# 4GHAZW — P3: GateDeps::offline_with_runtime closes the field-patching habit

STATUS: **DONE.** Both scoped gates are green on the current tree; nothing PENDING.

C3.1 made `GateDeps`' fields crate-private. The two offline tests that needed a
non-default runtime patched `deps.runtime` after `GateDeps::offline()`. This adds the
missing constructor and migrates both sites, so no caller patches a field.

## What changed

- `rust/crates/engine/degenbot-solvers/src/profit_envelope/gate.rs`
  - Added `pub fn offline_with_runtime(runtime: SolveRuntimeConfig) -> Self`, placed
    right after `offline()`, documented as the offline/test stance with a custom
    runtime. Body: `Self { runtime, ..Self::default() }`.
- `rust/crates/engine/degenbot-solvers/src/cl/tests/refinements.rs`
  - The two `offline()` + `.runtime = ...` sites (`:456-464`) become
    `GateDeps::offline_with_runtime(cfg_even)` / `GateDeps::offline_with_runtime(cfg_mass)`.
  - Removed the explainer comment about the missing constructor.

## Gates (foreground)

- `cargo test -p degenbot-solvers` — all suites green (177 + 20 + 7 + 7 + 4 + 3 +
  2 + 1 + 1 + 1 + 1 passed, 0 failed, 3 ignored; doc-tests 0).
- `cargo clippy -p degenbot-solvers --all-targets` — clean, no warnings
  (rustc/cargo 1.98.1, the pinned toolchain).
- `cargo fmt -p degenbot-solvers` — no diff produced.
