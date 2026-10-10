# 7ZUFGV (PC3, epic GZPPS3): the registration lifecycle becomes one value behind one pymethod pair

STATUS: **PC3 structural work DONE.** The four-layer registration-lifecycle wrapper ladder
collapses to one value type, one implementation, and one `run_registration_lifecycle` pymethod
pair. Stub drift green with the diff read and reviewed below; the negative probe is recorded with
its RED exit. The manager's whole-workspace ladder is **PENDING** (not mine to run).

Chunk: `quarantine -> seed-verify -> drain+pin -> post-drain-verify -> set_live` used to need four
files and fourteen near-identical renames; it now needs one value.

Baseline cited: `docs/ergo-results/hqylek-result.md` run 2 (green), plus landed PC1 `937153a29`
and PC2 `4a03f5e04`.

---

## 1. Shapes changed (read from the diff by this worker)

### New: `rust/crates/shells/degenbot-python/src/bot/registration_lifecycle.rs` (untracked)

- **`#[pyclass(name = "RegistrationLifecycle", module = "degenbot._ffi", frozen)]`**, Rust type
  `PyRegistrationLifecycle`, fields `{ family, address | (pool_manager, pool_id_hex),
  snapshot_block, mode, policy }`. `#[new]` is
  `(family, *, mode, address=None, pool_manager=None, pool_id_hex=None, snapshot_block=None,
  policy=None)`; it validates the family/mode/target/policy combination and raises `ValueError`
  with a shape-specific message. Read-only getters for every field.
- **One implementation**: `pub(crate) fn run_registration_lifecycle(py, &Arc<EngineDriver>,
  &PyRegistrationLifecycle)` — the single home of the address / pool-id parse (the historical
  `Invalid V3 address:` / `Invalid pool_manager:` / `Invalid pool_id:` strings), the
  `mode` switch (`async` -> `future_into_py`; `blocking` -> `py.detach(block_on)`; 
  `blocking_with_retry` -> the same under the core dance), `RetryPolicy::to_core()`, and the error
  mapping (`map_driver_lifecycle_err` for the async/blocking arms,
  `map_verify_lifecycle_error` for the retry arm).
- **One pymethod pair**, both in this module: `impl PyArbEngine { fn run_registration_lifecycle(
  &self, py, lifecycle) }` and `impl PyBot { fn run_registration_lifecycle(&self, py, lifecycle) }`.
  Each is a single delegation to the shared implementation.
- Three Rust unit tests: an unparseable V3 address and a malformed V4 pool id each raise the typed
  `ValueError` through the value (`target()`), and an unknown family/mode is refused at construction.

### Deleted layers

- `bot/pump.rs`: all six `pub(crate) fn run_*_registration*` wrappers deleted (168 lines); the file
  now holds only `start` and `session_end_future`. Its imports shrink to `map_driver_err`.
- `bot/engine/verify.rs`: all six `#[pymethods] impl PyArbEngine` lifecycle wrappers deleted (119
  lines); the file keeps `verify_v3_pool` / `verify_v4_pool` / `set_verify_*`.
- `bot/mod.rs`: the two `#[pymethods] impl PyBot` lifecycle wrappers deleted (44 lines changed);
  `pub(crate) mod registration_lifecycle;` declared.
- `bot/engine/mod.rs`: `driver` field widened to `pub(crate)` (the pair reaches the shared driver).
- `bot/mod.rs`: `pump_state` widened to `pub(crate)` (the `Bot`-arm pymethod reaches the same).
- `lib.rs`: one `#[pymodule_export] use crate::bot::registration_lifecycle::PyRegistrationLifecycle;`.

### Python surface

- `src/degenbot/arbitrage/engine_registry.py`: `run_v3_verify_lifecycle_sync_with_retry` +
  `run_v4_verify_lifecycle_sync_with_retry` collapse to ONE
  `run_registration_lifecycle(lifecycle)` pass-through; `register_v3_pool` / `register_v4_pool`
  build the `async` value; a read-only `verify_snapshot_block` property exposes the seed the
  startup ritual stashed. The module keeps no per-family, per-mode verb.
- `src/degenbot/runner/build_paths.py`: `_verify_v3_pool` / `_verify_v4_pool` state the
  `blocking_with_retry` value at the call site — the mode the old method name used to encode.
- `tests/fakes/engine.py`: the six doubled methods become one `run_registration_lifecycle`
  (mirroring the seam: an awaitable for `mode="async"`, inline `None` otherwise);
  `ENGINE_SEAM_MEMBERS` gains `run_registration_lifecycle` and loses the six names.
- `tests/arbitrage/{test_engine_registry_two_step_verify,test_engine_registry_claims_are_core_owned,
  test_verification_retry_config,test_registration_pipeline}.py` updated to the one-call shape;
  new `tests/arbitrage/test_registration_lifecycle_value.py` pins the value's shape contract and the
  target-refusal probe.

### Choreography unchanged (constraint 1, checked against the deleted bodies)

Every deleted wrapper was parse + `Arc::clone` + (`RetryPolicy::to_core`) + `py.detach` +
`map_*`; the new implementation issues exactly the same core calls
(`EngineDriver::run_v3/v4_registration_lifecycle{,_sync,_with_retry_sync}`) with the same arguments
and the same per-mode error mapper. No core file was touched: ADR-022 D1 (core ownership) is not
reopened.

---

## 2. Stub diff review self-check (constraint 2b/2c)

`just gen-stubs` regenerated `src/degenbot/_ffi/__init__.pyi` (+100 / -69, the only stub file that
changed). I read the full diff. It contains exactly:

| removed (8 defs) | added (2 defs + 1 class) |
|---|---|
| `ArbitrageEngine.run_v3_registration_lifecycle` | `ArbitrageEngine.run_registration_lifecycle(lifecycle)` |
| `ArbitrageEngine.run_v3_registration_lifecycle_sync` | `Bot.run_registration_lifecycle(lifecycle)` |
| `ArbitrageEngine.run_v3_registration_lifecycle_with_retry_sync` | `class RegistrationLifecycle` (`__new__` + 7 read-only properties) |
| `ArbitrageEngine.run_v4_registration_lifecycle` | |
| `ArbitrageEngine.run_v4_registration_lifecycle_sync` | |
| `ArbitrageEngine.run_v4_registration_lifecycle_with_retry_sync` | |
| `Bot.run_v3_registration_lifecycle` | |
| `Bot.run_v4_registration_lifecycle` | |

Plus the `__all__` entry `"RegistrationLifecycle"`. The remaining hunks are doc-comment prose
carried verbatim from the new Rust `///` text. **No other member, signature, or `__all__` entry
moved** — that is the whole surface change, and it is the intended one.

---

## 3. Negative-probe demonstration record (required: red shown once, then reverted)

Probe: an invalid target must raise the one mapped typed error *through the value*.

GREEN (final tree, foreground, exit captured directly):

```bash
uv run --no-sync pytest tests/arbitrage/test_registration_lifecycle_value.py -q; echo PROBE_GREEN_EXIT=$?
# 6 passed in 5.07s
# PROBE_GREEN_EXIT=0
```

FLIPPED (expectation changed to `VerificationRpcError`, one line + one import; nothing else):

```bash
uv run --no-sync pytest 'tests/arbitrage/test_registration_lifecycle_value.py::test_an_unparseable_v3_address_raises_the_typed_target_refusal' -q -p no:randomly; echo PROBE_RED_EXIT=$?
# >       with pytest.raises(VerificationRpcError, match="Invalid V3 address"):  # FLIPPED PROBE
# E           ValueError: Invalid V3 address: invalid string length
# tests/arbitrage/test_registration_lifecycle_value.py:86: ValueError
# 1 failed in 3.42s
# PROBE_RED_EXIT=1
```

Load-bearing proven: the value refuses the unusable target with the historical `ValueError`
vocabulary (`Invalid V3 address: invalid string length`), not the flipped typed
`VerificationRpcError`. Reverted byte-for-byte; `rg 'FLIPPED' tests/arbitrage/` -> no matches, and
the reverted file is green (the 521-test run in §4).

The V4 twin is pinned in the same file (`Invalid pool_id` for a malformed hex), and the Rust unit
tests `a_bad_v4_pool_id_raises_the_value_error` / `an_unparseable_v3_address_raises_the_value_error`
pin the same two refusals one layer down.

---

## 4. Counts delta

Baseline source: `docs/ergo-results/hqylek-result.md` (PC0, run 2, 2026-10-10).

| # | shrink target | PC0 baseline | PC3 observed | delta |
|---|---|---|---|---|
| C4 | registration-lifecycle wrappers in `rust/crates/shells/degenbot-python/src/bot/pump.rs` | **6** | **0** | -6 |
| C4b | the same ladder elsewhere in the shell (`bot/engine/verify.rs` + `bot/mod.rs` pymethods) | **8** (6 + 2) | **0** | -8 |
| C4c | the collapsed shape | (unknown) | **1 implementation + 1 pymethod pair** (2 pymethods) | new |

```bash
rg -n 'pub\(crate\) fn run_.*registration' rust/crates/shells/degenbot-python/src/bot/pump.rs   # -> no matches
rg -n 'run_v[34]_registration_lifecycle|run_v[34]_verify_lifecycle' rust/crates/shells -g '*.rs' # -> no matches
rg -n 'registration_lifecycle' rust/crates/shells   # -> one module + its two callers
```

`rg 'registration_lifecycle' rust/crates/shells` now resolves to:
`bot/registration_lifecycle.rs` (the value, the one `run_registration_lifecycle` implementation, the
two pymethod blocks, the six core entry-point calls), the `pub(crate) mod` declaration in
`bot/mod.rs`, the `lib.rs` export, the `errmap.rs` import of the core error type, and one
`pump.rs` doc cross-reference. `bot/engine/verify.rs` and `bot/mod.rs` hold **zero** lifecycle
wrappers.

Whole-tree diff: 13 tracked files changed, +261 / -605; plus 2 new files
(`registration_lifecycle.rs` 521 lines incl. docs+3 unit tests; `test_registration_lifecycle_value.py`
90 lines).

---

## 5. Scoped gates (foreground; exits captured directly — no pipes on the captures)

| gate | command | result |
|---|---|---|
| Rust workspace tests (superset of `-p degenbot_rs -p degenbot-bot`) | `just test-rust-nextest` | **GREEN** — `4717 tests run: 4717 passed, 29 skipped`, `NEXTEST_EXIT=0` |
| Rust lint (Clippy via recipe, `--deny warnings`) | `just lint-rust-check` | **GREEN** — `CLIPPY_EXIT=0` |
| Commit-time formatter authority (constraint 3) | `rustfmt --check --edition 2021 --config skip_children=true -- <6 touched .rs files>` | **GREEN** — `RUSTFMT_CHECK_EXIT=0` |
| Stub drift | `just gen-stubs --check` | **GREEN** — `✓ committed stubs match generator output`, `GEN_STUBS_CHECK_EXIT=0` |
| Build receipt (rust-rebuild) | `just rebuild-if-stale` | **GREEN** — rebuilt via maturin, then `extension is fresh` (`2197 b556ca427883b954`), `REBUILD_EXIT=0` |
| Python scoped (brief) | `uv run --no-sync pytest tests/arbitrage -q` | **GREEN** — `521 passed in 8.96s`, `PYTEST_ARBITRAGE_EXIT=0` |
| Seam suites (extra) | `uv run --no-sync pytest tests/ffi tests/exceptions -q` | **GREEN** — `95 passed`, `PYTEST_FFI_EXCEPTIONS_EXIT=0` |
| Python lint (touched src) | `uv run --no-sync ruff check` + `ruff format --check` on the two touched `src/` files | **GREEN** — `RUFF_CHECK_EXIT=0`, `RUFF_FORMAT_EXIT=0` |
| Python types (touched src) | `uv run --no-sync ty check --no-progress <the two touched src files>` | **GREEN** — `TY_EXIT=0` |
| Negative-probe flip | (expectation deliberately flipped) | **RED as designed** — `PROBE_RED_EXIT=1`; reverted |

Notes carried forward:

- **The brief's narrow Rust gate was a strict subset.** The brief named
  `cargo test -p degenbot_rs -p degenbot-bot`; AGENTS makes narrow Rust runs go through the recipe,
  so I ran `just test-rust-nextest` (whole workspace). That was load-bearing: **run 1 of that gate
  was RED** on `degenbot::architecture_gates::one_engine_impl_block` — a facade test that a
  two-crate run never reaches. My first cut named `ArbitrageEngine` in the new module's doc prose,
  and the gate allows that string exactly once in the shell crate (as the pyclass compat string).
  The prose now says "the arbitrage engine"; the gate is green. Exit recorded: run 1
  `NEXTEST_EXIT=100` (`4716 passed, 1 failed`), run 2+ `NEXTEST_EXIT=0`.
- Doc-tests are not run by nextest; `just test-rust` (cargo) remains the manager's lane.
- No gate detached past the harness bound except the first `just test-rust-nextest` (superseded by
  the two later full runs above); nothing is left running.

---

## 6. Manager ladder — PENDING

**PENDING (manager's to run — not this worker's):** the whole-workspace ladder — `just pre-push` /
`just test` (`just test-rust-nextest` + `just test-python`), `just test-rust` (doc-tests),
`just fmt-check`, and the PC3-scoped suites beyond `tests/arbitrage` that consume
`tests/fakes/engine.py` and `build_paths`. This worker ran the scoped gates in §5 only.

---

## 7. Tree / foreign edits

- **Written by this chunk:** `rust/crates/shells/degenbot-python/src/bot/registration_lifecycle.rs`
  (new), `.../bot/{pump.rs,mod.rs}`, `.../bot/engine/{mod.rs,verify.rs}`, `.../src/lib.rs`,
  `src/degenbot/_ffi/__init__.pyi` (generated), `src/degenbot/arbitrage/engine_registry.py`,
  `src/degenbot/runner/build_paths.py`, `tests/fakes/engine.py`,
  `tests/arbitrage/{test_engine_registry_claims_are_core_owned,test_engine_registry_two_step_verify,
  test_registration_pipeline,test_verification_retry_config}.py`,
  `tests/arbitrage/test_registration_lifecycle_value.py` (new), and this file. No `.ergo/plans.jsonl`
  entry, no ergo task-state change, no commit. The probe flip was reverted byte-for-byte.
- **Foreign tree activity:** none observed. The tree was clean at HEAD `3e88f7b727cfb6f0cb59aa74a986c2ea3a3b94e6`
  at start and the files the brief calls foreign (`docs/adr/*`, `docs/getting-started.md`,
  `docs/index.md`, `docs/ergo-results/3r6ley-result.md`, the wave-5 Aave example/cassette/golden
  paths) are not present as modifications and were not touched, staged, or reverted.
- **Process note (disclosed):** one intermediate edit destroyed `bot/mod.rs` — a full-file rewrite
  built from a `read` that truncates large files at 50 KB. The file was restored with
  `git checkout --` (the tree was clean at HEAD, so nothing foreign was lost) and the three intended
  edits were re-applied with targeted `edit` calls. The final `bot/mod.rs` diff is exactly the
  intended 44-line change; no subsequent full-file rewrite touched a file over the read cap.

No command was left detached; every gate above ran in the foreground with its exit captured directly.


---
## 9. Manager review — verdict PASS (2026-10-10, after fix round 2)

Adversarial review against the diff: (1) the collapse is real — one `run_registration_lifecycle`
implementation + the one pymethod pair, old wrapper names `rg` = 0; (2) the census gate
(`architecture_gates::one_engine_impl_block`) was satisfied legitimately — `ArbitrageEngine` is
named exactly once in the shells crate and NO gate file was touched; (3) the stub delta is exactly
the intended surface (8 defs removed / 2 added / `RegistrationLifecycle` value class). Two fix
rounds were dispatched: round 1 caught nothing new, round 2 fixed the ADR-013 barrier crossing
(manager's ladder caught it — scoped runs had missed `tests/test_ffi_boundary.py`; the fix routes
the import through the `degenbot.arbitrage` barrier, gate untouched, verified independently).

Manager battery (`/tmp/pc3-gates2/`): `just pre-push` **exit=0** + `just gen-stubs --check`
**exit=0** + per-file `rustfmt --check` **exit=0**. Green against the PC0 baseline (run 2).

Lesson filed for the epic: worker briefs must name `tests/test_ffi_boundary.py` in scoped gates when
a chunk changes Python-side FFI imports — the barrier gate is not in `tests/arbitrage`.

## 8. Fix round 2 (manager finding)

### The gate failure

The manager's full ladder was RED on the ADR-013 FFI-boundary gate — a lane the round-1 scoped runs
did not cover. Round 1 had imported the new value straight from the private extension in the two
call-site modules, so `test_no_ffi_imports_outside_barriers_and_explicit_seams` failed with 2
violations:

```text
FAILED tests/test_ffi_boundary.py::test_no_ffi_imports_outside_barriers_and_explicit_seams
Found 2 private `_ffi` import(s) outside approved seams:
  - src/degenbot/arbitrage/engine_registry.py:32: from degenbot._ffi import RegistrationLifecycle
  - src/degenbot/runner/build_paths.py:22: from degenbot._ffi import RegistrationLifecycle
```

### The fix (barrier re-export — the gate's own prescription)

`RegistrationLifecycle` now reaches both modules through the **`degenbot.arbitrage` package
barrier**, the same route the sibling FFI symbols already use in those two modules
(`engine_registry.py` already takes `ArbitrageEngine` from it; `build_paths.py` already takes
`RetryPolicy` from it). Two import routes changed; no gate logic was weakened, no leaf exception was
added, no `.rs` was touched.

| file | before | after |
|---|---|---|
| `src/degenbot/arbitrage/__init__.py` | re-exported `ArbitrageEngine`, `RetryPolicy`, `session_phase_next`, `solve_balancer_weighted_basket` | adds `RegistrationLifecycle` to the `from degenbot._ffi import (...)` list and to `__all__` |
| `src/degenbot/arbitrage/engine_registry.py` | `from degenbot._ffi import RegistrationLifecycle` + `from degenbot.arbitrage import ArbitrageEngine` | one line: `from degenbot.arbitrage import ArbitrageEngine, RegistrationLifecycle` |
| `src/degenbot/runner/build_paths.py` | `from degenbot._ffi import RegistrationLifecycle` + `from degenbot.arbitrage import RetryPolicy` | one line: `from degenbot.arbitrage import RegistrationLifecycle, RetryPolicy` |

Both call sites are unchanged in behavior — the name binds to the same pyclass; only the module it
is imported from moved behind the barrier. `degenbot.arbitrage.__init__` is an ordinary
`__init__.py` barrier, so it is skipped by the gate's own scan (as all package barriers are) and the
private import lives there legitimately.

### Gates (foreground; exits captured directly — no pipes on the captures)

| gate | command | result |
|---|---|---|
| FFI boundary (the RED gate) | `uv run --no-sync pytest tests/test_ffi_boundary.py; echo EXIT=$?` | **GREEN** — `8 passed in 5.16s`, `EXIT=0` |
| Python scoped (arbitrage + exceptions) | `uv run --no-sync pytest tests/arbitrage tests/exceptions; echo EXIT=$?` | **GREEN** — `564 passed in 9.18s`, `EXIT=0` |
| Stub drift | `just gen-stubs --check; echo EXIT=$?` | **GREEN** — `✓ committed stubs match generator output`, `EXIT=0` (a pure-Python re-export moved no stub) |
| rustfmt (only if `.rs` touched) | — | **N/A** — no `.rs` file was touched in this round |

The §2 stub-diff review stands unchanged: `RegistrationLifecycle` was already exported by `_ffi`, and
adding a Python package re-export does not change the generated extension surface.
