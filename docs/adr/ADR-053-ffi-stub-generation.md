# ADR-053: FFI Stub Generation — Retire the Hand-Maintained `.pyi` Set?

**Status: accepted.** Recommendation from spike ergo **BLY6CT** (T6). The
hand-maintained stub set stays; the bespoke AST drift gate is slimmed to the
checks a standard tool cannot cover, with `mypy.stubtest` taking over the
surface-consistency checks. `pyo3-stub-gen` is rejected.

## Context

`degenbot._ffi` is the maturin-built Rust extension seam (ADR-013: private).
Python cannot see types on it at runtime — the compiled module shadows the
namespace-package stub dir on import — so type checking and IDE hover run
entirely off hand-maintained stubs, and correctness of those stubs is policed
by a bespoke AST gate.

### Measured cost of the status quo

- **Stubs**: 27 `.pyi` files totaling **4,178 lines** under
  `src/degenbot/_ffi/` (largest: `__init__.pyi` at 65.6 KB / ~1,672 lines).
- **Drift gate**: `tests/rust/test_ffi_stub_drift.py` — **369 lines** of
  AST/introspection code encoding rules R0–R4 (test module docstring):
  - **R0** — every runtime `degenbot._ffi.*` submodule has a stub file.
  - **R1** — every public runtime symbol appears in its stub.
  - **R2** — every stub `__all__` entry exists at runtime (no phantoms).
  - **R3** — every top-level class/function/constant a stub defines exists
    at runtime.
  - **R4** — for a **curated 52-entry class table** (`_CLASS_STUBS`), the
    stub's declared members match the compiled class's own members in both
    directions, minus a 27-name default-dunder allowlist.
- **Churn**: `git log --oneline -- src/degenbot/_ffi` = **126 commits** —
  every surface-changing task (registration, feature-gated module
  additions, renames) must update a stub by hand in the same commit. The
  gate itself needed **two iterations** (DSWX6Z created R0–R3; review
  A527QE found a stub method parked on the wrong class while the gate
  stayed green, and S3FOCH added R4 + the curated table + a negative
  control). The curated-table design is an admission that the AST gate
  only partially sees class members, and it must be hand-extended for
  every new meaningful pyclass.

### Measured shape of the Rust registration surface

`rust/crates/degenbot-python/src/`:

- **70** `#[pyclass]`, **153** `#[pyfunction]`, **0** `#[pyenum]`,
  **74** `#[pymethods]` blocks, **223** `#[getter]`, **31** `#[setter]`,
  across **73** files.
- Registration is **split across 28 functions**: `c_api.rs::register()` plus
  per-domain `add_*_module(m)` / `register_*(m)` fns (e.g.
  `db::add_db_module`, `rpc::provider::add_provider_module`).
  `db`, `abi`, `solady`, `concentrated_liquidity_math`, etc. build **real
  submodules** with `PyModule::new(py, "degenbot._ffi.<sub>")` +
  `add_submodule`.
- Nearly everything is **feature-gated** (`#[cfg(feature = "bot")]`,
  `#[cfg(all(feature = "pathfinding", feature = "db"))]`, …), and many
  pyclasses carry `module = "degenbot._ffi.<sub>"` (16 multi-line
  `#[pyclass(...)]` attribute blocks).
- **The seam is dynamically typed in Rust.** Counting
  `Py<PyAny>` / `Bound<'py, PyAny>` / `PyObject` / `Py<PyDict>` /
  `Bound<'py, PyList>` uses gives **≥210**; return types skew the same way:
  `PyResult<Bound<'py, PyAny>>` (68), `PyResult<Py<PyAny>>` (64),
  `PyResult<PyObject>` (42), `PyResult<Option<Py<PyAny>>>` (26),
  dict/list returns (27 + 14 + 11 + …). Typed primitive returns
  (`u64`/`u128`/`String`/`bool`/`Option<String>`/...) total ≈75.

### What the hand stubs actually contain

The stubs are **better than anything mechanically derivable today**:

- Domain-typed signatures that map dynamic Rust values onto real Python
  types: `get_block(block_number: int) -> BlockData | None`,
  `get_logs(*, from_block, to_block, addresses, topics) -> list[LogData]`
  (`provider.pyi`), with kwargs-only signatures and defaults spelled out.
- They import domain types the Rust side only knows as
  `Bound<'py, PyAny>`: `from degenbot.types.rpc_types import BlockData,
  LogData, ...` (`provider.pyi`), `from degenbot.types.chain import
  HexAddress` (`__init__.pyi`).
- They carry **docstrings** consumed by IDE hover — pyproject.toml
  explicitly ignores ruff's `docstring-in-stub` for precisely this reason
  ("required for IDE to show wrapped Rust code info").

`ty` (the repo's type checker — `just lint-python` runs `ty check src/`)
and ruff (`PYI` rules) both consume these stubs, so they carry real gate
weight, not just documentation.

## Options evaluated

### Option A — `pyo3-stub-gen` (generate `.pyi` from Rust registration code)

Assessment is from the crate's public model combined with this repo's
surface; crates.io/docs.rs were unreachable from the spike sandbox (empty
HTTP responses), and the crate is **absent from every `Cargo.toml` in the
workspace** (zero matches), so nothing below is from a live integration.

Work required to reach coverage:

1. **Annotate the whole surface.** stub-gen's model requires its own
   attributes/derives (`gen_stub_pyclass`, `gen_stub_pyfunction`,
   `gen_stub_pymethods`, …) alongside PyO3's, on **every** exported item.
   That is a parallel annotation sweep over 70 pyclasses / 153 pyfunctions
   / 223+31 accessors in 73 files — i.e. **the hand-stub maintenance burden
   is not retired, it is relocated into Rust macros**, where it chases
   Rust churn instead of stub churn. The one thing it genuinely buys is
   mechanical existence-consistency for items the derive covers.
2. **Type fidelity would regress, badly.** stub-gen infers Python types
   from Rust types. Because the seam is `Py<PyAny>`/`PyObject`-typed
   (≥210 uses above), generated stubs would emit `Any` (or
   `dict[str, Any]` / `list[Any]`) exactly where the hand stubs have full
   domain typing (`BlockData`, `LogData`, `TransactionData`, `HexAddress`).
   stub-gen cannot import the Python-side `degenbot.types.*` domain types
   the Rust code never names. Verdict: a generated stub today is *worse*
   than what it replaces — it converts hand-curated truth into
   machine-curated `Any`.
3. **The add_*_module / real-submodule split is awkward.** stub-gen's
   stub-tree model hangs off one crate-level `stub_info()` and its own
   module declarations; this repo constructs submodules at runtime with
   `PyModule::new` + `add_submodule` inside feature-gated fns spread over
   28 registration functions, and pyclasses name their module via
   `module = "degenbot._ffi.<sub>"`. Mapping that layout onto stub-gen's
   expected module tree is exactly the bespoke-glue work the crate is
   meant to save.
4. **Feature gates.** Generated stubs reflect one compile-time feature
   set. The repo builds many variants (dev-profile `uv sync` features vs
   release `--features pyo3/extension-module`, plus per-feature toggles);
   R1–R3 compare against the *installed* runtime's surface, so a stub
   generated from an all-features build would systematically disagree with
   even a correctly generated per-variant stub fed to `ty`.
5. **Docstrings.** The hand stubs' hover value (pyproject's
   `docstring-in-stub` carve-out) has no straightforward hand-off;
   whatever stub-gen preserves cannot reproduce the prose now living in
   `.pyi` files without that prose being migrated by hand anyway.
6. **Wiring.** A generate step (run stub-gen through a Python one-liner /
   extra build hook) must be added to maturin-driven builds and CI, with
   the usual "committed vs generated" churn loop — while `ty`, not
   `mypy`, does the repo's type enforcement, so the generated stubs pay
   rent only through the drift gate.

**Rejected.** The maintenance burden moves rather than disappears, and the
type fidelity of the artifact drops sharply relative to what
`ty`/ruff/IDEs consume today. Could be revisited *only if* the Rust seam
itself is retyped (returning newtype wrappers with `IntoPyObject`
conversions instead of `PyObject`) — which is a much larger ADR-013-scope
project than stub tooling.

### Option B — `mypy.stubtest` as the consistency checker

`mypy.stubtest` is the standard stub↔runtime consistency checker (ships
with mypy). Run against the installed `degenbot._ffi` with an in-repo
allowlist (its `--allowlist <file>` mechanism), inside the Python lint
gate (`just lint-python-check` / prek), it mechanically replaces the
*mechanics* of R1/R3/R4:

| Drift-gate rule | stubtest covers it? | Remainder for a small custom check |
|---|---|---|
| **R1** runtime symbols present in stub | **Yes** (its core "missing in stub" direction) | — |
| **R2** stub `__all__` is honest | **Partially** (verify against installed stubtest; `__all__` phantom-name checking is not a documented guarantee) | keep the R2 loop (~15 lines of AST) until verified, else delete |
| **R3** stub definitions exist at runtime | **Yes** (its core "not found at runtime" direction); annotation-only imports must be allowlisted per the current R3 exemption semantics | — |
| **R4** class members both directions | **Yes — generalized**: stubtest checks member presence on *every* class, not a curated 52-row table, and resolves inheritance the way the hand-written `vars()` walk approximates | delete the entire R4 machinery, including `_DEFAULT_PY_DUNDERS` and the negative controls; express PyO3's synthetic dunders as stubtest allowlist entries if they fire |

Known integration cost (the honest price of B):

- **New dev dependency.** The repo's checker is `ty`; **mypy appears
  nowhere** in `pyproject.toml`, the justfile, or CI. stubtest comes only
  with mypy, so adopting it means a `dev`-group `mypy` entry plus a gate
  recipe. The Python-side type checker stays `ty` (stubtest is a
  consistency checker, not a replacement type checker), and
  `uv run stubtest degenbot._ffi` needs the fresh `.so` (the
  `verify-build-fresh` receipt gate already covers staleness).
- **Extension-module noise.** stubtest against native modules needs a
  small allowlist for PyO3-synthesized members (object-protocol dunders,
  `__getstate__`/`__setstate__`, etc.) and for "defines in stub but
  annotation-only import" cases. Budget: an allowlist file measured in
  tens of lines, not hundreds — R1/R3/R4 currently reproduce the same
  exemptions by hand in 369 lines of test code.
- **What stubtest cannot do**: it validates *existence*, not *types*.
  It will never catch `get_block(...) -> None` where
  `-> BlockData | None` belongs; the hand stubs' domain typing remains
  the load-bearing artifact. (There is no "verify generated types are
  semantically honest" tool; ty catches downstream misuse, not stub
  lies.)
- **`ty` has no stubtest equivalent today** (no runtime↔stub consistency
  mode), so mypy's checker is un-replaceable here without writing R1–R4
  by hand — which is precisely the status quo being judged.

### Option C — status quo (stubs + bespoke AST gate, unchanged)

Cost as measured above: 4,178 stub lines with hand maintenance on 126
commits' worth of churn, 369 lines of test whose R4 half is a manually
curated 52-row table requiring extension on every meaningful new pyclass,
and a demonstrated near-miss class (A527QE) where the gate stayed green
through drift. The stubs themselves stay regardless (both A and B keep
them); the question C answers badly is only whether the *checker* should
stay hand-built. As a full answer it means the next R5 (whatever shape
the next A527QE takes) is again ~100 lines of bespoke AST code.

## Decision

Keep the hand-maintained stubs (they are the highest-fidelity artifact and
`pyo3-stub-gen` cannot reach that fidelity), and **replace the bespoke AST
gate's R1/R3/R4 with `mypy.stubtest`**, keeping only small hand-rolled
checks for what stubtest does not cover. In one line:

**Hand-maintained `.pyi` stubs stay the surface of record; slim the drift
gate to R0 + R2 (+ any un-verified stubtest gaps) and delegate
R1/R3/R4 to `mypy.stubtest` with an in-repo allowlist; reject
`pyo3-stub-gen`.**

## Migration steps

1. Add `mypy` to the `dev` dependency group (toolchain addition only; the
   repo's type checker remains `ty`).
2. Add `tests/rust/stubtest_allowlist.txt` plus a `just` recipe
   (e.g. `lint-stubtest: uv run stubtest --allowlist
   tests/rust/stubtest_allowlist.txt degenbot._ffi ...`) wired into the
   pre-push / CI Python gate mirroring `lint-python-check`.
3. Run stubtest once; triage its findings. Expected bulk: PyO3-synthetic
   dunders and R3's annotation-only-import exemptions — encode both in
   the allowlist with comments mapping each entry to the rule it serves.
4. Delete from `tests/rust/test_ffi_stub_drift.py`: R1
   (`test_runtime_symbols_are_stubbed`), R3
   (`test_stub_definitions_exist_at_runtime`), and **all** R4 machinery
   (`_DEFAULT_PY_DUNDERS`, `_CLASS_STUBS`, `_stub_class_defs`,
   `_stub_class_members`, `_class_member_drift`,
   `test_stub_class_members_match_runtime`, and the two negative-control
   tests). Keep R4's *idea* — generalized member checking — in stubtest.
5. Keep: `test_every_runtime_submodule_has_a_stub` (R0, ~15 lines) and,
   pending verification in step 3, `test_stub_all_is_honest` (R2).
   Both are small, deterministic, and use no hand-curated tables.
6. Update the drift-gate docstring to enumerate what stubtest now owns vs
   what the file still owns, referencing this ADR.

## What the drift gate keeps doing after cutover

- **R0 (submodule stub coverage)** — retained as the small AST/`sys.modules`
  test it already is. stubtest's traversal of extension
  `add_submodule`-registered submodules cannot be relied on, and the rule
  is 15 lines.
- **R2 (honest `__all__`)** — retained *only if* the stubtest run in the
  migration does not already flag phantom `__all__` entries; otherwise
  deleted with the rest. Decide in step 3, and record the decision in the
  gate docstring.
- **Allowlist curation** — human-owned, not code: new PyO3-synthetic
  members on new pyclasses land in `stubtest_allowlist.txt` with a
  comment, replacing R4's `_CLASS_STUBS` row-adding chore with a one-line
  allowlist entry.
- **Semantic type honesty** — remains out of scope for any automated gate
  (stubtest checks existence, not meaning). The hand stubs stay the
  surface of record; `ty`/ruff enforce how consumers use them, and
  reviews enforce what they promise.
