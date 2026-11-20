# rust/vendor — the vendored evm2 family (temporary)

This tree is a **source copy** of [`alloy-rs/evm2`](https://github.com/alloy-rs/evm2)
at rev `0a8a935c4ffe274a23cf8480f7a3a07015f3030e`, package-renamed into the
`degenbot-evm2*` family so the engine can be **published to crates.io**. The
source is upstream's, except the two `evm2` files the [Downstream
patches](#downstream-patches) section enumerates; every `Cargo.toml` here is
rewritten for the rename.

## Why it exists

crates.io accepts only packages whose dependencies already exist on crates.io.
`evm2` does not: upstream reserved the name (`0.0.0-reserved`) but has never
uploaded code, so it lives only on GitHub. Any of our crates that runs the
engine therefore cannot be uploaded at all — and `cargo add degenbot` cannot
resolve. Vendoring the family under names we own fixes both today.

## How it is wired

- **Names.** The packages are `degenbot-evm2`, `degenbot-evm2-jit`, … Every
  dependency key stays upstream's (`evm2`, `evm2-jit`, …) via `package =`
  renames, so all `use evm2::…` and `X.workspace = true` spellings — here and
  in our crates — are unchanged. The single wiring point is the family block in
  `rust/Cargo.toml`'s `[workspace.dependencies]`.
- **Source plus named patches.** `Cargo.toml` is rewritten: package name,
  lockstep `version`/`rust-version`/`repository` with the workspace, upstream's
  `edition` (2024) / `license` / `authors` / metadata kept, dependency specs
  materialized from upstream's workspace table, and test targets off
  (`test = false`). Every other file is upstream's byte for byte, except the
  two files the [Downstream patches](#downstream-patches) section enumerates —
  no other difference from a checkout of that rev is sanctioned. Upstream's
  in-crate test modules stay in the source as upstream wrote them, but their
  dev-deps are not declared and nothing compiles them here:
  the workspace-wide lanes select the family only as a dependency
  (`scripts/vendor_members.sh` supplies the excludes). Our own parity suites
  are the behavioral gate.
- **Default features** drop upstream's native precompile backends
  (`gmp`, `bn254-mcl`, `p256-aws-lc-rs`): our build contract forbids GMP/MCL/
  aws-lc native builds. The features remain declared and unused. The family's
  `nightly` feature likewise never fires here: the workspace-wide lanes do not
  select the family as a primary package, and our toolchain is stable.
- **Layout mirrors upstream's** (`crates/…`) so the `include_str!` fixture paths
  in test modules resolve inside this tree (`rust/vendor/data/` holds the two
  fixtures they read).
- `rustfmt.toml` here sets `disable_all_formatting`, so the source stays
  byte-identical to what was copied (upstream's files, and the two patched
  ones), and `just check-vendor-tracked` asserts every file here is in the git
  index (the copy is only complete if the whole tree is tracked).
- **The editor checks the family the same way the lanes do.** rust-analyzer's
  default check compiles upstream's in-crate test modules as primary packages
  and floods the editor with diagnostics from the undeclared dev-deps above.
  `scripts/rust_analyzer_check.sh` runs the lane contract instead and is wired
  as `rust-analyzer.check.overrideCommand` (`.vscode/settings.json`); it
  derives its excludes from `scripts/vendor_members.sh` like every lane.

## Downstream patches

evm2 has no async-fiber counterpart of its untracked account-info read, and the
coroutine sequence driver needs one: `degenbot-simulation`'s
`Evm2SimSession::run_sequence_async` reads a native balance as a sequence step,
and on an `AsyncDb`-backed stack that read is a database round trip only an EVM
fiber can suspend on. The patch is one method:

- `crates/evm2/src/evm/mod.rs` — `Evm::read_account_info_async`, the minimal
  `#[cfg(feature = "async")]` wrapper that runs the synchronous
  `state.account_info_untracked` body on the same reusable fiber
  `transact_async` uses. Additive: nothing upstream calls it, and it changes no
  existing behavior.
- `crates/evm2/src/evm/async.rs` — the `AsyncDatabase` doc comment lists the
  new entrypoint beside `transact_async`. Documentation only.

Its behavioral coverage is ours, not upstream's:
`rust/crates/engine/degenbot-simulation/tests/session_async.rs` pins the cold
read (the step suspends and resumes), the accepted-overlay read (a value-moving
step earlier in the sequence is visible to a later balance read), and the
per-step parity of both sequence drivers.

The two files are the whole patch surface. A third file showing up in `diff -r`
against a checkout of the pinned rev means either a mistaken edit or a patch
that must be enumerated here — and covered by a test — before it ships.

To re-vendor at another rev: check it out in a clone of `alloy-rs/evm2`, copy
`crates/evm2` (without `tests/` and `examples/`), `crates/jit` (without
`tests/`) and `crates/macros` over this tree, and carry over any of the root
files (`LICENSE-*`, `rustfmt.toml`, the `data/` fixtures the in-crate test
modules read) that changed. Re-write the ten manifests per the bullet above,
re-apply the two patch files, then update the rev in the manifest headers, this
file, and the family comment in `rust/Cargo.toml` — `diff -r` against the
checkout is the check that nothing else moved. The read-through is every
consumer-visible API change the new rev makes, recorded in the section below.

## Upstream API changes between `e681662b` and `0a8a935c`

Nine upstream commits are in this range. The vendored crates' manifests did
not change upstream, so the package rewrites and dependency versions stayed as
they were and `rust/Cargo.lock` did not need a dependency update. The
consumer-visible changes and their dispositions here are:

- **`Host::execute_call` is a new provided method** (upstream #1081). It runs a
  `CALL`/`CALLCODE`/`DELEGATECALL`/`STATICCALL` message from outside an
  interpreter frame with the same charges as the opcode, sharing the account
  access and gas path (`load_acc_and_calc_gas`) with the call opcodes.
  Implementations should not override it. No consumer here implements `Host`,
  so no call site changed, and opcode behavior is unchanged.

- **`BlockExecutionContext.system_calls` is required** (upstream #1091,
  breaking). The field is `BlockSystemCalls` instead of
  `Option<BlockSystemCalls>`, `BlockExecutionContext` no longer derives
  `Default`, and `post_execution_system_call_state_changes` drops its `context`
  parameter. No consumer here names `evm2::block`, so no call site changed.

- **Inspectors can read the transaction-wide refund counter** (upstream #878).
  `Evm::tx_refunded(frame_refunded)` is new, and the EVM tracks the refunds of
  the frames below the running one plus the pre-execution EIP-7702
  authorization refund while an inspector is installed. A transaction
  dispatched from inside a running one — such as from an inspector hook — now
  starts with its own refund counter and no enclosing frame, and gives both
  back on exit. Additive and unused here: `Evm2SimInspector` reads the
  per-frame gas counters of `MessageResult` and `InterpreterState`, which are
  unchanged, and no consumer dispatches a transaction from an inspector hook.
  Upstream's struct-log and JavaScript tracers live in its `evm2-inspectors`
  crate, outside this tree.

- **Boxed components passed to the EVM stay downcastable** (upstream #1093,
  #1098). `boxed_dyn_database`, `boxed_inspector`, and
  `boxed_precompile_provider` reuse a `Box<dyn …>` argument instead of boxing
  it a second time, so database downcasts through `State`, `Evm::clear_inspector_as`,
  and `Evm::precompiles_as` see the inner concrete type instead of `None`. Our
  downcast call sites (`engine-lab`'s `capture_seam_probe` over
  `Evm2SimInspector`) pass the concrete type and are unchanged; every inspector
  hook and precompile call saves an allocation and a pointer chase.

- **Gas and accounting fixes require no consumer edits:** the EIP-2929
  `SSTORE` cold-access guard compares the remaining gas against
  `GasId::ColdStorageCost` (was `ColdStorageAdditionalCost`) before skipping
  the cold surcharge, in both the interpreter and the JIT builtin (upstream
  #1093); a nested create checks its creator's balance and bumps its nonce
  through one account handle, and reads the creator's info only when a value
  moves (upstream #1093); the EIP-7702 handler records its pre-execution refund
  for inspectors and clears it when it exits (upstream #878). The
  gas-decomposition and parity suites remain the behavioral gate.

- **Pooled interpreter frames are bounded** (upstream #1094). Idle frames drop
  memory beyond 64 KiB and their return-data buffer between top-level messages
  (`InterpreterPool::trim`, run when no frame is current). Internal: frame and
  memory APIs are unchanged.

- **Test-only changes are outside the compiled surface:** the BN254 entry
  points delegate to `*_with::<O>` generics and the substrate backend module
  compiles alongside `mcl`, so upstream's `#[cfg(test)]` differential suite can
  pick a backend (upstream #1092), and KZG point evaluation gains mutated test
  vectors. The differential module is `#[cfg(test)]` and reads the `rand`
  dev-dependency, which this tree does not declare, so nothing here compiles
  it; precompile behavior is unchanged.

Upstream's `evm2-inspectors` changes (`CallTraceStep`/`CallTrace` reshaping,
tracer refund reads) and bench-only changes are outside this vendored tree. The
two downstream patch files still apply: upstream's `evm/mod.rs` changes do not
touch `read_account_info_async`'s region, and `evm/async.rs` is unchanged in
this range.

## Upstream API changes between `53fe0ef` and `e681662b`

Eighteen upstream commits are in this range. The vendored crates' manifests did
not change upstream, so the package rewrites and dependency versions stayed as
they were and `rust/Cargo.lock` did not need a dependency update. The
consumer-visible changes and their dispositions here are:

- **Call instruction input is borrowed from the caller's memory.** `MessageInput`
  is now an owned-or-borrowed representation instead of a byte string. It no
  longer dereferences to `[u8]`; inspectors use `as_bytes(interp.memory())`,
  `to_bytes(interp.memory())`, or `as_owned`. The two call-hook sites in
  `degenbot-simulation`'s `Evm2SimInspector` now pass the running caller's
  memory for the selector and captured calldata. A `call` or `call_with` hook
  that writes caller memory without replacing the input now changes what the
  child reads; our inspector does not write memory, so its capture semantics
  are unchanged. The child frame resolves the input once and
  `CALLDATALOAD`/`CALLDATASIZE`/`CALLDATACOPY` read that slice, removing the
  per-call input copy from inspected execution.
- **`initial_call_depth` moved from `Version` to `Evm`.** Runtime users call
  `Evm::{initial_call_depth,set_initial_call_depth}`; `Host` has a required
  `initial_call_depth` method, and `Version::{initial_call_depth,max_call_depth}`
  are gone. No consumer here referenced the removed `Version` items or
  implements `Host`, so no call site changed. The fixture harnesses continue to
  mutate the surviving `Version` fields through `Version::new` plus assignment.
- **JIT lookup requests borrow bytecode.** `LookupRequest<'a>` now holds a
  `LookupCode<'a>` (`Bytecode` or raw `Bytes`) rather than an owned `Bytes`, and
  lookup hit paths avoid cloning frame bytecode. The `evm2-jit` test helper in
  `degenbot-simulation::sim::evm2::jit` now returns `LookupRequest<'static>`
  with `LookupCode::Bytes` over static one-opcode code. `should_compile` takes
  `&[u8]`; no consumer here calls it directly. Artifact storage traits and
  runtime tuning/config shapes are unchanged. `ABI_VERSION` is 8 (was 7), and
  the AOT store already keys manifests and filenames by that value, so
  stale-ABI artifacts are not selected.
- **EIP-8037 EIP-7702 accounting no longer refunds state gas never charged.**
  `GasSettlement::state_refund` and the corresponding authorization accounting
  fields/prices were removed. No consumer here names those APIs. The correction
  concerns Amsterdam state gas; production sequences run at Osaka and retain
  their existing accounting. The gas-decomposition and parity suites remain
  the behavioral gate.
- **Additive execution/state APIs are available but not adopted by this sync:**
  `Evm::take_precompiles` (pair with `set_boxed_precompiles`),
  `Evm::message_input`, `State::cool_loaded`, `State::reload_storage_slot`, and
  `Interpreter::active_bytecode`. They enable provider reuse, explicit
  warmth/slot refreshes, and borrow-based bytecode access respectively; using
  them to change candidate or sequence lifecycle policy is separate work, not a
  mechanical migration. `BlockState` now derives `Clone`.
- **Behavior and reliability improvements require no consumer edits:** JIT
  workers catch compile panics and skip memory analysis for unsupported host
  opcodes; transient storage drains in one pass and the JIT `CODECOPY` builtin
  reads active bytecode without cloning it. EIP-7702 authority recovery uses
  the installed crypto provider (identical results with the default provider
  used here). Moving precompiles preserves warm addresses, and the `fn_align`
  feature gate is restricted to x86-64. The async database traits, database
  traits, inspector hook signatures, `MessageResult`, and `InstrStop` are
  unchanged.
- Upstream's `evm2-inspectors` and bench-only changes are outside this vendored
  tree. The two downstream patch files still apply: upstream's `evm/async.rs`
  change is confined to its test module, and the `evm/mod.rs` source delta
  applies around `read_account_info_async` without changing that method.

## Upstream API changes between `baf0d38` and `53fe0ef`

Consumer-visible Rust API changes forced by the sync, and what each forces here.
The family's consumers in this workspace are `degenbot-simulation` (the engine),
`degenbot-arbitrage` and with it the `degenbot` facade, `degenbot-python`'s
optional in-process probes, and the dev-only `evm2`-backed fixture harnesses in
`degenbot-math` / `degenbot-pools` / `degenbot-strategy` /
`degenbot-submission`:

- **`evm2::Version` gains the public field `tx_total_gas_cap: u64`** — EIP-8037's
  total transaction gas cap is configurable now, where it was hard-coded to
  `MAX_TX_TOTAL_GAS_LIMIT_AMSTERDAM` (`2^32 - 1`) inside `tx_gas_caps()`;
  `Version::new` seeds that same constant, so every default is unchanged, and
  `u64::MAX` disables the cap. A `Version { … }` struct literal no longer
  compiles; every consumer here builds the version with `Version::new(spec)` and
  assigns fields, so no call site changed. The field is inert for this
  workspace: the production spec is `SpecId::OSAKA` and EIP-8037 activates at
  Amsterdam.
- **`evm2::evm::inspector::OpcodeSet::remove` is gone** — upstream dropped its
  only caller. Nothing here names `OpcodeSet`; no consumer changed.
- **`evm2::evm::state::AccountHandle::original()` is new** — the
  transaction-boundary baseline alongside `get()`. Additive and unused here.
- **Inspector config reloads are picked up after any `hooked` instruction**, not
  only from inside `call_with`/`create_with`. The dispatch now runs the
  instruction to completion and yields `InstrStop::Reload` after it (so the pc
  advances past it once, on the new table), which also drops upstream's
  "an instruction that runs a message must be `hooked`" assertion; a `hooked`
  instruction that runs its child message through the host can change the
  config too. Our composite inspector (`degenbot-simulation`'s
  `Evm2SimInspector`) does not override `inspector_config`, so it observes every
  step, never requests a reload, and its capture semantics are unchanged.
- Internal-only changes with no API and nothing a consumer can observe: dispatch
  alignment (a nightly-only `fn_align` gate, plus a 64-byte loop and handler
  alignment), raw-stack length scaling on the x86-64 tail-call backend, a shared
  `do_return` body between the JIT builtins, and JIT builtin-symbol enumeration
  always going through `builtin_symbol_names`. `rust/Cargo.lock` did not change —
  this rev moves no dependency, and no vendored manifest needed a dependency
  edit.

## The switch (this tree is temporary)

The day upstream publishes `evm2` to crates.io:

1. Point the family block in `rust/Cargo.toml`'s `[workspace.dependencies]` at
   the registry release (drop the `package =` renames and `path =` keys).
2. Delete `rust/vendor/`.
3. Drop the ten names from `EXPECTED_NAMES` in
   `rust/crates/facade/degenbot/tests/architecture_gates.rs`, the catalog row in
   `docs/capabilities.md`, this tree's exclusions in
   `scripts/hooks/comment-hygiene.sh` and `prek.toml`, the
   `scripts/vendor_members.sh` call sites, and
   `scripts/rust_analyzer_check.sh` with its `.vscode/settings.json` wiring.

Recorded as the deletion condition of ADR-096. Deleting the tree is the
complete retirement — no gate keeps it gone afterwards.

## License

Upstream is `MIT OR Apache-2.0`; the texts are in `LICENSE-MIT` and
`LICENSE-APACHE` here. Individual crate manifests declare the same SPDX.
