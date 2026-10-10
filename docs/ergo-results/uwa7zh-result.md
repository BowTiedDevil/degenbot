# UWA7ZH (PC6, epic GZPPS3): the `Py*Row` mirrors are already retired — consumer census, ADR-023 D2 close-out, acceptance pin

STATUS: **GREEN on a verification + close-out chunk. NO source deletion was made, and none was
possible without changing DB read behaviour.** The seven mirror classes named by ADR-023 D2 are
gone from Python already (retired by earlier landed chunks); the residue is Rust `#[pyclass]`es
over `degenbot-db` typed records with live Python consumers. Constraint (e) applies: the last
consumers still read these rows, so the retirement is complete and further deletion would be a
behaviour change, not a retirement. Reported below instead of forced.

Baseline cited: `docs/ergo-results/hqylek-result.md` run 2 (green). Landed pre-existing chunks not
re-opened: `937153a29`, `4a03f5e04`, `231334a8f`, `ad035c48a`, `345a8c118`, `5de576784`, `d18c49b8d`.

## Step 1 — consumer census FIRST (`rg "Erc20TokenRow|PoolRead" src/ tests/`)

No Python mirror class remains. The seven names of ADR-023 D2 are `Erc20TokenRow` + the six
`db/pool_read.py` classes (`LiquidityPoolRow`, `PoolKindRow`, `ExchangeRow`, `PoolManagerRow`,
`LiquidityPositionRow`, `InitializationMapRow`). Census, read-only in the foreground:

```
rg -c "Erc20TokenRow|PoolRead" src/              # 3 files / 8 hits, all non-class residue (below)
rg   "PoolRead"              src/ tests/        # EMPTY (exit 1) — the db/pool_read.py classes are gone
rg -n "^class \w*Row"        src/                # only the generated .pyi stubs declare row classes
rg -l "Erc20TokenRow"        src/                # _ffi/__init__.pyi, db/__init__.py, builders/erc20_builder.py
```

| name | Python mirror class in `src/**/*.py` today | retired by | where it lives now |
|---|---|---|---|
| `Erc20TokenRow` | **0** | `37de68602` (DB read + write-back through the `PyBotIo` seam) | Rust `#[pyclass]` `PyErc20TokenRow` over `degenbot_db::rows::Erc20TokenRow` (`bot/py_bot_io.rs`) |
| `LiquidityPoolRow` | **0** | `a3ce2edde` (sync builders' DB reads through the seam) | `#[pyclass]` `PyLiquidityPoolRow` over `degenbot_db::rows::LiquidityPoolRow` (`db/pool_read.rs`) |
| `PoolKindRow` | **0** | `a3ce2edde` (pyclass), `2ed00aa54` (pyclass deleted) | deleted with its `PyBotIo` method — the `degenbot-db` typed record survives for the Rust core |
| `ExchangeRow` | **0** | `a3ce2edde` | `#[pyclass]` `PyExchangeRow` over `degenbot_db::rows::ExchangeRow` |
| `PoolManagerRow` | **0** | `a3ce2edde` | `#[pyclass]` `PyPoolManagerRow` over `degenbot_db::rows::PoolManagerRow` |
| `LiquidityPositionRow` | **0** | `a3ce2edde` / `2ed00aa54` | deleted with its `PyBotIo` method |
| `InitializationMapRow` | **0** | `a3ce2edde` / `2ed00aa54` | deleted with its `PyBotIo` method |

(the historic Python classes were removed by those commits; the seven names appear in `src/` today
only as the generated stub, the `degenbot.db` re-export and one type-only builder annotation.)

**Consumer count (defined: a distinct site under `src/` that reads a field off one of the seven
records; generated `.pyi` and the re-export module excluded): BEFORE = 27, AFTER = 27.** No
consumer was removed by this chunk, because all of them are live:

| consumer | fields read | sites |
|---|---|---|
| `builders/type_resolution.py` | `pool_row.exchange_id`, `pool_row.kind`, `exchange_row.factory` | 3 |
| `builders/erc20_builder.py` | `token_from_db.name` / `.symbol` / `.decimals` | 9 |
| `updater/pool_updater_configs.py` | `exchange.factory` / `.chain_id` / `.name` / `.id`, `pool_row.exchange_id` | 15 |

The two other `Erc20TokenRow` residues are a generated stub (the record's declared FFI face) and
the `degenbot.db` re-export — the sanctioned public path for the type (ADR-013 keeps
`degenbot._ffi` private; `tests/test_db_public_mirror.py::test_retired_private_db_imports_are_absent_from_companions`
pins that companions must not reach into it directly).

**Historical attribution of the seven retirements (all pre-existing, none mine):** the Python
`db/pool_read.py` classes became Rust pyclasses when the sync builders' DB reads moved behind the
`PyBotIo` seam (`a3ce2edde`); `Erc20TokenRow` likewise (`37de68602`); and the three whose last
consumer had already gone were deleted outright with their `PyBotIo` methods in VK3YDM-S5
(`2ed00aa54`, "retire dead PyBotIo facade methods" — -1182 lines).

## Step 2 — deletion decision (constraint (e): STOP and report)

**No deletion was made.** Every remaining record is still the DB read shape and is read by a live
consumer (census above), so removing it would change DB read behaviour — exactly the condition the
brief says to stop and report on. There is also no Python mirror left to delete: the four survivors
are `#[pyclass]`es owned by the shell over `degenbot-db` typed records, which is the residue the
acceptance explicitly allows ("one typed record owned by `degenbot-db`, not a Python mirror").
`degenbot-db` already owns them in both directions (the rows are constructed by the crate's
`from_row` decoders and read back through `degenbot_db::rows::*`).

## Shapes changed (diff)

No production code shape changed. This chunk lands:

| file | intent |
|---|---|
| `docs/adr/ADR-023-construction-io-retirement-disposition.md` | D2 closed out: records the roster of seven, the current home of each, the three already deleted, the four that stay (with the field-reading consumers named), and why deletion would be a behaviour change. A3's re-home pointer gains "(closed 2026-10-10, see D2)". |
| `tests/database/test_pyrow_mirror_retirement.py` (NEW) | PC6's acceptance as a named test (repo idiom: acceptance criteria live in named tests). `test_no_python_class_mirrors_a_retired_db_row` AST-scans `src/degenbot/**/*.py` and fails if any of the seven names reappears as a Python class (`.pyi` excluded — that is the allowed FFI residue). `test_surviving_records_are_rust_owned_pyclasses` pins that each of the four survivors announces a `degenbot._ffi`/`degenbot._ffi.db` module (a `PyO3` class, not a Python mirror). A reintroduced mirror fails loudly. |

## Counts delta (baseline table shape, re-observed @ HEAD `d18c49b8d` + this diff)

| # | metric | expected | observed | delta | attribution |
|---|---|---|---|---|---|
| C1 | Python mirror classes of the seven ADR-023 D2 names | 7 deleted | **0 remain** (7/7 already retired) | **0 this chunk** | pre-existing: `a3ce2edde`, `37de68602`, `2ed00aa54` |
| C2 | consumer sites reading a mirror record under `src/` | shrink with the mirrors | **27 before → 27 after** | **0** | none removed — all live (constraint (e)) |
| C3 | `rg "Erc20TokenRow" src/` hits | 0, or one typed record | **8 hits / 3 files**: generated stub (4), `db/__init__.py` re-export (2), `builders/erc20_builder.py` TYPE_CHECKING import + annotation (2) | unchanged | accepted residue (typed record + sanctioned re-export) |
| C4 | `rg "PoolRead" src/ tests/` | 0 | **0** (exit 1) | 0 | already retired |
| C5 | Rust shell `#[pyclass]`es carrying a row name (the four survivors) | 4 (the DB read shape) | **4** | 0 | kept — live consumers |

## Acceptance

- `rg "Erc20TokenRow" src/` is not empty, but the residue is exactly the typed record owned by
  `degenbot-db`: the generated stub declaring it, the `degenbot.db` re-export that is the
  sanctioned public path for it, and one type-only annotation in the builder that reads it. There
  is **no Python mirror class**. PASS on the acceptance's parenthetical branch.
- `rg "PoolRead" src/ tests/` — EMPTY (exit 1). The six `db/pool_read.py` classes are not Python
  classes anywhere.
- ADR-023 D2 carries its closed-out note; A3 points at it.
- New pinning test green; no mirror can be reintroduced without failing it.

## Gate exits (captured DIRECTLY, `cmd; echo $?`, no pipes)

| gate | exit |
|---|---|
| `cargo test --locked --manifest-path rust/Cargo.toml -p degenbot-db` (task scoped) | **0** — 172 lib + 8 + 2 + 6 + 3 + 8 + 5 + 1 + 5 + 3 + 3 + 19 + 8 + 1 + 4 + 2 tests, every suite `ok` |
| `uv run --no-sync pytest tests/database tests/builders` (task scoped) | **0** — 141 passed / 1 skipped |
| `uv run --no-sync pytest tests/database/test_pyrow_mirror_retirement.py` | **0** — 2 passed |
| `just test-rust-nextest -p degenbot-db` | **0** — 4724 passed / 29 skipped |
| `just test-rust-nextest` (FULL workspace, manager-mandated) | **0** — 4724 passed / 29 skipped |
| `just lint-rust-check` | **0** |
| `just lint-python-check` (ruff `src/ examples/` + ty `src/`) | **0** — "All checks passed!" twice |
| `uv run --no-sync pytest tests/test_ffi_boundary.py` | **0** — 8 passed |
| `just gen-stubs --check` | **0** — "committed stubs match generator output" |
| `just lint-comment-hygiene` | **0** |
| `just test-python` (FULL Python suite) | **0** — 3132 passed / 50 skipped |
| `just verify-build-fresh` | **0** — build 2212 / `c61d981f8d5ef09e`, "extension is fresh" |
| `uv run --no-sync ruff format --check -- tests/database/test_pyrow_mirror_retirement.py` | **0** — "1 file already formatted" |
| `uv run --no-sync ruff check tests/database/test_pyrow_mirror_retirement.py` | **0** |
| `uv run --no-sync ty check --no-progress tests/database/test_pyrow_mirror_retirement.py` | **0** |
| `rustfmt --check --edition 2021 --config skip_children=true -- <touched .rs>` | **N/A — no `.rs` file touched** |
| `just test-rust-nextest -E 'package(degenbot-db)'` | **2** — the recipe interpolates `{{args}}` unquoted, so a `-E` filter is a shell syntax error, not a code failure; superseded by the `cargo test -p degenbot-db` line above |

Stub-diff self-check: **N/A** — no Python-visible surface moved (`.pyi` files byte-identical;
`gen-stubs --check` confirms), so no regen was required.

Logs: `/tmp/uwa7zh-cargo-db.log`, `/tmp/uwa7zh-pytest-scoped2.log`, `/tmp/uwa7zh-newtest.log`,
`/tmp/uwa7zh-nextest-db.log`, `/tmp/uwa7zh-nextest-full.log`, `/tmp/uwa7zh-lint-rust.log`,
`/tmp/uwa7zh-lint-python.log`, `/tmp/uwa7zh-ffi.log`, `/tmp/uwa7zh-genstubs.log`,
`/tmp/uwa7zh-comment-hygiene.log`, `/tmp/uwa7zh-pytest-full.log`.

Nothing detached; every load-bearing command ran in the foreground inside the harness bound.

## Deliberately kept, and why

1. **`Erc20TokenRow`** — read by `Erc20Builder.build_many` (`io.fetch_erc20_token` → `.name`/
   `.symbol`/`.decimals`, DB-first metadata) and exposed through `degenbot.db` as the sanctioned
   public path. Deleting the pyclass changes the DB-first token-metadata read.
2. **`LiquidityPoolRow` / `ExchangeRow` / `PoolManagerRow`** — read by
   `builders/type_resolution.py` (the DB `kind` fast path of pool classification) and
   `updater/pool_updater_configs.py` (exchange scope + `exchange_id` preconditions). These are the
   DB read shape, owned by `degenbot-db`; the pyclasses are its FFI face.
3. **The `degenbot.db` re-export** — the ADR-013 import boundary forbids companions reaching into
   `degenbot._ffi` directly, and `tests/test_db_public_mirror.py` pins the mirror set; removing it
   would move a public import path, not retire a mirror.

## PENDING (manager re-runs)

1. **MANAGER LADDER: PENDING** — `just pre-push` was not run by this worker. The manager-mandated
   gates (FULL `just test-rust-nextest`, `just lint-rust-check`, `just lint-python-check`,
   `just gen-stubs --check`, `pytest tests/test_ffi_boundary.py`, `just lint-comment-hygiene`, both
   fmt authorities) WERE run and are green above.
2. **Deletion remains un-done by design.** If the epic wants the four surviving records gone too, it
   needs the `type_resolution` DB fast path and the updater configs moved core-side first
   (behaviour-changing; out of this chunk's scope per constraint (e)).

## Tree / foreign edits

Files written by this chunk (2): `docs/adr/ADR-023-construction-io-retirement-disposition.md`,
`tests/database/test_pyrow_mirror_retirement.py` (new). No foreign (unowned) modifications were
present in the working tree during the run; the tree was clean at start. No commit; no ergo
task-state change; `.ergo/plans.jsonl` untouched.

## Fix round (manager finding) — the drift gate was INERT

**Finding.** The acceptance gate `test_no_python_class_mirrors_a_retired_db_row` passed with a
planted mirror in the tree. The manager's negative probe:

```
$ printf 'class Erc20TokenRow:\n    pass\n' > src/degenbot/_probe_tmp.py   # since removed
$ uv run --no-sync pytest tests/database/test_pyrow_mirror_retirement.py -x -q
2 passed in 4.78s   (PROBE_EXIT=0 — the planted mirror was NOT detected)
```

The test was inert: it asserted the absence of a condition it never actually tested.

**Root cause — a mis-anchored repo root, not a filename or import-graph filter.** The module
derived `REPO_ROOT = Path(__file__).resolve().parents[1]`. From
`tests/database/test_pyrow_mirror_retirement.py`, `parents[1]` is `tests/`, so `SRC_ROOT` resolved
to `tests/src/degenbot` — a directory that does not exist. `Path.rglob("*.py")` on a missing
directory yields nothing *silently*, no error and no warning, so the AST walk never ran over real
source, `defined` was always the empty `set()`, and `defined & RETIRED_MIRROR_NAMES == set()`
held **vacuously**. The `_probe_tmp.py` underscore prefix and "brand-new module" angles were red
herrings: the walk never reached `src/degenbot/` at all, so *no* planted name could ever be seen.
The `test_surviving_records_are_rust_owned_pyclasses` half passed for unrelated reasons (it reads
the live `degenbot.db` surface, not the filesystem).

**Fix (in `tests/database/test_pyrow_mirror_retirement.py`, the root-cause location):**
1. `REPO_ROOT = Path(__file__).resolve().parents[2]` — the actual repo root.
2. The helper now returns a `class name -> [relative file paths]` map (`_python_class_locations`),
   so a reintroduced name is reported with the file that defines it, not just the bare name.
3. **Anti-vacuity guards**, so a future mis-anchor/glob break fails loudly instead of silently
   passing: the scan root must exist (`is_dir()`), the walk must have scanned at least one module
   (else "the gate would be inert: scan root"), and the test asserts the discovered class set is
   non-empty (`"scan is inert"`). The `.pyi` exclusion (stubs are the allowed FFI residue) and the
   four-survivor `PyO3` pin are unchanged; the test's claim was strengthened, never weakened.

**Red path, demonstrated with the exact repro (fresh module under `src/degenbot/`):**

```
$ printf 'class Erc20TokenRow:\n    pass\n' > src/degenbot/_probe_reappearing_mirror.py
$ uv run --no-sync pytest tests/database/test_pyrow_mirror_retirement.py -x -q; echo "PROBE_EXIT=$?"

    def test_no_python_class_mirrors_a_retired_db_row() -> None:
        defined = _python_class_locations(SRC_ROOT)
        # Positive control: a vacuous scan (bad root, wrong glob, empty tree) must
        # fail loudly here rather than let the retirement claim pass on nothing.
        assert defined, f"no Python classes found under {SRC_ROOT}; scan is inert"
        reintroduced = {name: defined[name] for name in sorted(defined) if name in RETIRED_MIRROR_NAMES}
>       assert not reintroduced, f"Python row mirrors reintroduced under src/: {reintroduced}"
E       AssertionError: Python row mirrors reintroduced under src/: {'Erc20TokenRow': ['_probe_reappearing_mirror.py']}
E       assert not {'Erc20TokenRow': ['_probe_reappearing_mirror.py']}
tests/database/test_pyrow_mirror_retirement.py:77: AssertionError
FAILED tests/database/test_pyrow_mirror_retirement.py::test_no_python_class_mirrors_a_retired_db_row
1 failed, 1 passed in 4.66s
PROBE_EXIT=2
```

The failure names **both the file and the class**. Probe then removed and re-run: `2 passed in
4.75s`, exit `0`. (I planted an explicitly `_`-prefixed module name to rule out the underscore-skip
hypothesis conclusively — the gate catches it.)

**Gate exits for the fix round (captured DIRECTLY, `cmd; echo $?`, no pipes):**

| gate | exit |
|---|---|
| `uv run --no-sync pytest tests/database/test_pyrow_mirror_retirement.py` (post-fix) | **0** — 2 passed |
| planted-mirror repro (`-x -q`) | **2** — RED, names `{'Erc20TokenRow': ['_probe_reappearing_mirror.py']}` |
| `uv run --no-sync pytest tests/database/test_pyrow_mirror_retirement.py` (probe removed) | **0** — 2 passed |
| `just lint-python-check` (ruff `src/ examples/` + ty `src/`) | **0** — "All checks passed!" twice |
| `uv run --no-sync pytest tests/database tests/builders` | **0** — 141 passed / 1 skipped |
| `uv run --no-sync ruff format --check -- tests/database/test_pyrow_mirror_retirement.py` | **0** — "1 file already formatted" |
| `just lint-comment-hygiene` | **0** |



## Manager review — verdict PASS (2026-10-10, after the fix round)

Independently verified the finding AND the fix with the manager's own planted-mirror repro: the
pre-fix gate passed vacuously on `class Erc20TokenRow` in a fresh `src/degenbot/_pm_probe.py`
(PROBE_EXIT=0 — inert), the fixed gate caught it BY NAME AND FILE
(`{'Erc20TokenRow': ['_pm_probe.py']}`, exit 2) and went green on removal (GREEN_EXIT=0). Root
cause confirmed: `parents[1]` anchored the scan at `tests/`, so `rglob` silently walked a
non-existent `tests/src/degenbot` and the assertion held vacuously — the exact inert-check failure
mode the negative-probe idiom exists to catch. The anti-vacuity guards (root is_dir, module
count, non-empty discovered set) now make a mis-anchor fail loudly. Manager battery
`/tmp/pc6-gates/`: `just pre-push` exit=0, `just gen-stubs --check` exit=0,
`just lint-comment-hygiene` exit=0.
