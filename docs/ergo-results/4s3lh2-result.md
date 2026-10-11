# 4S3LH2 / BW3 — word-value deletion wave: COMPLETION round (STOP-and-report)

**Epic** `U2XZSD`. **Lane** BW3. **Wave** word-bitmap VALUE retirement + schema
migration + auto-heal tolerance for old DBs.

**Round verdict: STOP.** The value machinery's *engine-logic* consumers are gone, but a
**large mirror/export/FFI + test-golden/fixture consumer surface remains** — far beyond the
dirty set this round was dispatched with — and the previous agent's Rust-core deletion was
**not gate-clean** (two committed gate suites were RED before this round touched anything).
Per the task body ("if any consumer remains beyond export/mirror paths, STOP and report") and
the dispatched brief ("If a logic consumer of VALUES remains, finish the deletion or STOP and
report"), this round **finished the bounded, mechanical part of the deletion that the previous
agent left broken** (Rust test/golden/fixture surface + the never-running migration test) and
**STOPs on the Python/FFI mirror redesign**, which is a public-API reshape whose target shape
is not pinned by the task body or the BW0 matrix and cannot be completed + gate-verified inside
one one-shot under the ~120 s foreground bound.

No commits. Foreign files untouched (see §6). `.ergo/plans.jsonl` untouched.

---

## 1. Attribution of the inherited dirty set (content-decided)

| File(s) | Attribution | Reason (diff content) |
|---|---|---|
| `degenbot-db/src/{lib,liquidity_updater,migrate,migrations,read,schema,snapshot}.rs`, `schema_head.sql`, `rows/{liquidity,mod}.rs`, `tests/{auto_heal_matrix,parity}.rs`, **NEW** `tests/word_bitmap_retirement.rs` | **BW3** | `BitmapAtWord`/`INITIALIZATION_MAPS` reads+writes, schema head tables, `RUST_SCHEMA_VERSION` 2→3 + `drop_word_bitmap_tables` step. |
| `degenbot-math/src/cl/liquidity_mapping.rs` | **BW3** | `BitmapAtWord` + `flip_tick` deletion (‑328 lines). |
| `degenbot-substrate/src/{planning,pool_ingress,tick_assembly}.rs` + `tick_assembly/tests.rs` | **BW3** | `TickMapSeed.bitmaps` removal, derive-at-call-site verifier. |
| `degenbot-pool-updater/src/{run,verify}.rs` | **BW3** | `tick_bitmap` plumbing removal. |
| `degenbot-bot/src/bot_core/construction_io/{adapters,handle,mod,tests}.rs`, `pool_builder/{builder,tests}.rs` | **BW3** (not foreign — brief said "check content") | Diff removes `InitializationMapRow`/`ManagedPoolInitializationMapRow` fetch methods and the bitmap half of `chain_arm(...)` — pure bitmap-word/init-table content. Brief's "if NOT about bitmap words ⇒ foreign" test: it IS about bitmap words ⇒ BW3/wave. |
| `degenbot-strategy/tests/{sequence_deficit_probe,v3_anchor_tick_window,v4_lane_admission}.rs` | **BW3** (not foreign) | Diff removes `ApplyBitmapAtWord` seeding + `upsert_v3/v4_initialization_maps(...)`. Unambiguously bitmap/init content. Brief's "decide by content" rule ⇒ BW3. |
| `degenbot-cli-core/src/pool.rs`, `degenbot-python/src/db/snapshot.rs`, `degenbot-python/src/pool/mod.rs`, `degenbot-python/src/bot/mod.rs` | **BW3** | `tick_bitmap` de-plumbing in verify state / snapshot dict / FFI verify / test fixtures. |
| `degenbot-python/src/lib.rs` | **FOREIGN** | Diff is ONLY the `config_seam` pub-mod + `_ffi` pymodule_export (SERRGX). No bitmap content. |
| `degenbot-config/src/holder.rs`, `degenbot-python/src/config.rs`, **NEW** `degenbot-python/src/config_seam.rs`, **NEW** `tests/test_config_holder_seam.py`, `tests/conftest.py` | **FOREIGN** | Config-holder seam lane (SERRGX): `OnceLock`→`RwLock` reinstall seam, `governed_holder` fixture. No bitmap content. |

No file landed in **uncertain**.

**Missed by the dispatched dirty set (found this round, all BW3):**
`degenbot-db/tests/liquidity_updater_parity.rs`, `degenbot-pool-updater/tests/{sql_golden_replay,
verification_gate_probes,wave2_capture_replay}.rs`, `facade/degenbot/examples/
generate_evm_oracle_captures.rs`, `facade/degenbot/benches/updater_replay_bench.rs`, the
`tests/fixtures/sql_goldens/**` + `tests/fixtures/cassettes/wave2/w2_drained_pool_zero_mint.json`
goldens, and the whole `src/degenbot` Python mirror (§4).

---

## 2. Zero-consumer precondition re-verified

**Engine/solver logic readers of the VALUES: 0.** Rust `BitmapAtWord` occurrences now **1**
(a doc comment in `db/snapshot.rs`). The retired tables exist in Rust source only in the
migration step, the new retirement/auto-heal tests, and two comment mentions.

**But the value-consumer surface that remains is large** (the reason for STOP):

- **Rust test/golden/fixture surface** (proven RED at round start, §3): `liquidity_updater_parity.rs`
  read `initialization_maps`; four `DUMP_TABLES` lists + the wave-2 generator/bench seeded or
  dumped the two tables; the committed `tests/fixtures/sql_goldens/**` goldens still carried
  them.
- **Python + FFI mirror (untouched, 15 `src/degenbot` files + 22 test files):** `BitmapAtWord`
  type (`concentrated/types.py`), `LiquidityMap.tick_bitmap` + `_drain_tick_bitmap`
  (`concentrated/snapshot_readers.py`), `UniswapV3/V4PoolState.tick_bitmap` + `InitializedTickMap`
  (`v3_types.py`/`v4_types.py`), `ConcentratedLiquidityCompanion.tick_bitmap` + the
  `word not in self.tick_bitmap` gate (`cl_companion.py`), `v3/v4_snapshot.tick_bitmap()`,
  `bitmap_at_word=TickDataTypes(...)` wiring (`bot/_bot.py`, `builders/tick_data_fetcher.py`), and
  the FFI derivation/`tick_bitmap` getter (`degenbot-python/src/bot/pool.rs`
  `cl_snapshot_fields` + `PyConcentratedLiquidityView::tick_bitmap`).

**LOAD-BEARING machinery intact (proven by green crate suites, §5):** `known_bitmap_words`,
word-position math (`get_tick_word_and_bit_position`), on-chain staged word fetch
(`fetch_tick_bitmap` consuming a *chain* bitmap to enumerate ticks — untouched), `tick_fingerprint`,
the solver walk (`compute_tick_ranges`/`gen_ticks_iter`), the sparse `MissingTickWord` gates.

`.bitmap` logic readers outside passthroughs: **0**. The FFI `tick_bitmap` getter is a *derived*
passthrough (built from `tick_data` keys); it is exactly the mirror targeted for retirement, not
a stored value.

---

## 3. What the previous agent left broken (found and fixed this round)

The previous agent's one-shot died awaiting a whole-workspace nextest run; **that run was never
completed and the Rust test/golden surface was left RED.** Verified at round start:

```
cargo test -p degenbot-db --test liquidity_updater_parity
  → 3 failed: `no such table: initialization_maps` / `managed_pool_initialization_maps`
cargo test -p degenbot-db --test word_bitmap_retirement
  → 2 failed: FOREIGN KEY constraint failed  (the NEW migration test NEVER RAN — the
    fixture inserted liquidity_positions(1,…) with no parent pools row)
cargo test -p degenbot-pool-updater --test sql_golden_replay
  → statement count drifted: left 19, right 21
```

### Migration mechanism — house pattern (confirmed, one-liner)

The repo's house migration mechanism is the **hand-rolled forward-only registry**
`RUST_MIGRATIONS: &[MigrationStep]` applied at open by `apply_rust_migrations` under the
ADR-052 D2 forward version-lock (`ensure_schema_at_open_with` → `apply_forward_lock`), stamped
in `_degenbot_db_schema_version`; the bump ritual is "bump `RUST_SCHEMA_VERSION`, append a
`MigrationStep`, extend the release fixture matrix". The previous agent's shape **IS the house
pattern**: `RUST_SCHEMA_VERSION = 3` + step `{version: 3, name: "drop_word_bitmap_tables",
sql: "DROP TABLE IF EXISTS initialization_maps; DROP TABLE IF EXISTS
managed_pool_initialization_maps;"}` — `RUST_MIGRATIONS.last().version == RUST_SCHEMA_VERSION`
holds, the step is contiguous/ascending, and each step runs in its own transaction. No
reshaping needed.

### Fixes landed (all verified green)

1. `word_bitmap_retirement.rs` — seed the parent chain (`erc20_tokens`→`exchanges`→`pools`/
   `pool_managers`→`managed_pools`) the FK-bearing inserts reference. Test now **2/2 pass**.
2. `liquidity_updater_parity.rs` — drop the `initialization_maps` oracle field, the two
   `init-maps` dumps + assertions, and `to_initmap_tuple`. **3/3 pass.**
3. `sql_golden_replay.rs` / `verification_gate_probes.rs` / `wave2_capture_replay.rs` /
   `generate_evm_oracle_captures.rs` — remove `initialization_maps` +
   `managed_pool_initialization_maps` from every `DUMP_TABLES`; drop the two
   `row_count("initialization_maps")` assertions; recompute `EXPECTED_LEDGER_STATEMENTS`
   **21 → 19** (BW3 removed the last two `initialization_maps` statements: its full-map SELECT
   and its upsert) with a dated comment.
4. `updater_replay_bench.rs` — drop the bitmap-word accumulation + `INSERT INTO
   initialization_maps` seed (+ stale doc/import).
5. Regenerated goldens (byte-identical via the repo's own writers):
   `REGENERATE_SQL_GOLDENS=1 cargo test -p degenbot-pool-updater --test sql_golden_replay`
   (pool_update_chunk ledger+dump) and
   `cargo run … --example generate_evm_oracle_captures` (wave-2 ledger+dump **and** the
   drained-pool cassette — see §5 note). `just gen-stubs` regenerated `_ffi/{__init__,pool}.pyi`.
6. Doc-comment hygiene in `db/liquidity_updater.rs`, `db/rows/liquidity.rs`,
   `python/src/pool/mod.rs`.

### Migration test + probes (verbatim)

**Old-DB test — GREEN** (`cargo test -p degenbot-db --test word_bitmap_retirement`, EXIT=0):
`old_db_with_populated_word_tables_migrates_cleanly` + `old_db_without_word_tables_migrates_cleanly`
both `ok`; the fixture is a version-2 Rust-owned file with both tables POPULATED, opens to
`RustOwned { schema_version: 3 }`, the tables are dropped, and the live `liquidity_positions`
row survives.

**Probe (a) — revert the auto-heal tolerance ⇒ the old-DB test fails loudly** (reverted
byte-for-byte):
```
# DROP TABLE IF EXISTS …  →  DROP TABLE …          (migrations.rs step 3)
test old_db_without_word_tables_migrates_cleanly ... FAILED
panicked at …/word_bitmap_retirement.rs:139:
  old DB opens through migration: MigrationStepFailed { version: 3,
  name: "drop_word_bitmap_tables", at: 2, cause: SqliteFailure(…,
  Some("no such table: initialization_maps")) }
```
**Probe (b) — reintroduce a value consumer ⇒ the gate fails loudly** (reverted
byte-for-byte):
```
# re-add "initialization_maps" to DUMP_TABLES in sql_golden_replay.rs
test pool_chunk_ledger_and_db_dump_match_the_committed_goldens ... FAILED
panicked at …/sql_golden_replay.rs:207:
  dump the touched tables: Sqlite(… "no such table: initialization_maps")
```
Both probes reverted; the re-run is green again.

---

## 4. Remaining work (the STOP reason)

**Python/FFI mirror retirement** — a public-API reshape, not a mechanical delete:
`tick_bitmap` today is `dict[int, BitmapAtWord]` in `PoolState`/`LiquidityMap`/the FFI view and
a `dict[int, (U256, u64)]` in `PyConcentratedLiquidityView`. BW0 says the VALUES are a derived
projection (DEAD) and the KEYS are LOAD-BEARING, but **the post-retirement Python/FFI shape is
not pinned** (word-key `set[int]`? `frozenset`? keep the type name? does `PoolState` drop the
field or carry a key set? does the FFI `update_tick_data` keep its `dict` signature?). That
decision must precede the edit. Scope: 15 `src/degenbot` files + 22 test files + the FFI in
`bot/pool.rs` + a stub regen, and `tests/uniswap/v3/conftest.py`'s recorded-`tick_bitmap`
fixtures.

**Test-fixture data still referencing the tables** (outside the Rust source): the frozen
`tests/fixtures/liquidity_updater_v{3,4}_initial.db` (stamped v2, tables populated — migrated
away on open, so harmless) and `fixtures/generate_liquidity_updater_parity.py` (still emits
`initialization_maps`; not gate-enforced).

**Frozen release fixture matrix must be regenerated at v3 (bump-ritual step).** The committed
`tests/fixtures/{parity,pathfinding,aave_parity}.db` are stamped **v2** with the tables present.
Opening them in place (`parity.rs`/`pathfinding_parity.rs` do) now runs the forward migration
step 3, which **writes the committed binary fixtures in place** — the tests still pass, but the
tree dirties on every run. Confirmed: after `cargo test -p degenbot-db --test parity -- opens_rust_owned_db_as_current_and_writes_nothing`,
`git status` shows `tests/fixtures/parity.db` modified (stamp 2→3, tables dropped). This round
**restored** those fixtures to their committed bytes and did **not** regenerate them; the
generators (`generate_parity.py`, which still `INSERT`s `initialization_maps`, plus
`generate_pathfinding.py`/`generate_aave_parity.py`) must be updated and the matrix regenerated
at `RUST_SCHEMA_VERSION == 3` before the manager commits a clean tree. (The migrations.rs doc's
bump ritual names this step explicitly: "extend the release fixture matrix".)

---

## 5. Gate evidence (direct exits)

| Gate | Result | Exit |
|---|---|---|
| `just rebuild-if-stale` | fresh (installed == receipt `4c25a6a7f471bb36`) | 0 |
| `just dev` (extension rebuild — pytest needed it; `verify_build_fresh` had flagged stale) | built + installed | 0 |
| `cargo test -p degenbot-db` | 175 unit + all integration suites **ok** (incl. new `word_bitmap_retirement` 2/2, `liquidity_updater_parity` 3/3, `parity` 19/19, `auto_heal_matrix` 6/6) | 0 |
| `cargo test -p degenbot-math -p degenbot-substrate -p degenbot-strategy` | all **ok** (substrate 359) | 0 |
| `cargo test -p degenbot-bot` | 731 unit + integration **ok** | 0 |
| `cargo test -p degenbot-pool-updater` | all suites **ok** (ledger/dump/probes/wave2) | 0 |
| `cargo check -p degenbot --all-targets --features degenbot/sql-ledger` (benches/examples) | compiles | 0 |
| `just lint-rust-check` | **FAIL** `one_engine_impl_block` — FOREIGN `config_seam.rs` doc mentions `ArbitrageEngine::with_core` (not BW3) | 101 |
| `just lint-python-check` (ruff **and** ty) | All checks passed | 0 |
| `just gen-stubs --check` | ✓ committed stubs match (after `just gen-stubs`; delta: `_ffi/__init__.pyi` (foreign config seam exports), `_ffi/pool.pyi` (BW3 doc edit)) | 0 |
| `cargo fmt --all --check` | **FAIL** — only FOREIGN files remain: `degenbot-config/src/holder.rs`, `degenbot-python/src/{config,config_seam}.rs`. BW3 crates were fmt-normalized this round. | 1 |
| `scripts/hooks/comment-hygiene.sh` | **FAIL** — only FOREIGN `SERRGX` citations (`config_seam.rs`, `config.rs`, `holder.rs`, `conftest.py`, `test_config_holder_seam.py`); 0 BW3 hits | 1 |
| `pytest tests/database tests/pool_companion` | **FAIL 5** / passed 104 — Python mirror still seeds/reads `initialization_maps` / `tick_bitmap` (the §4 work) | 1 |
| `just test-rust-nextest` | **PENDING** (not run) | — |
| `pytest tests/uniswap tests/pool_companion` (full) | **PENDING** (only `tests/database`+`pool_companion` run) | — |

**Coverage note (accepted per BW0 §5 / Q4):** the wave-2 drained-pool cassette legitimately
lost one Multicall3 `eth_call` at block 1003 — the verifier now derives words from `tick_data`
(a drained pool ⇒ empty word set ⇒ no `tickBitmap` read). This is the documented
"derive-words-at-call-site" capability cost, not a regression. The statement ledger for the
pool-update corpus lost exactly the two `initialization_maps` statements.

---

## 6. Foreign tree activity observed (NOT touched, NOT staged, NOT fixed)

The config-holder seam lane (SERRGX) is live in this workspace and is **breaking three
whole-tree gates** on its own files: `cargo fmt --check` (`holder.rs`, `config.rs`,
`config_seam.rs`), `lint-rust-check` (`config_seam.rs`'s `ArbitrageEngine` mention), and
`comment-hygiene.sh` (bare `SERRGX` token). Plus the untracked `config_seam.rs` /
`test_config_holder_seam.py` and `tests/conftest.py`'s `governed_holder` fixture. A concurrent
`cargo test -p degenbot_rs` process held the build lock intermittently during this round. The
regenerated `_ffi/__init__.pyi` reflects that lane's new `_ffi` exports (generated artifact —
required for `gen-stubs --check`).

---

## 7. Sign-off

- **Attribution:** §1 — the dispatched foreign set held except `lib.rs` (foreign) and the three
  `degenbot-strategy` tests + `construction_io`/`pool_builder` files, which are **BW3 by
  content** (bitmap/init tables).
- **Deletion deltas:** Rust `BitmapAtWord` 22→1 occurrences (doc only); `degenbot-math`
  bitmap apply path deleted; DB tables dropped from head schema + step-3 migration;
  `EXPECTED_LEDGER_STATEMENTS` 21→19; goldens regenerated (pool_update_chunk + wave-2 dumps
  and ledgers; one wave-2 cassette call).
- **Migration mechanism:** house pattern confirmed = `RUST_MIGRATIONS` forward-only registry,
  step 3 `drop_word_bitmap_tables`, `RUST_SCHEMA_VERSION` 3.
- **Old-DB test:** GREEN (2/2); **probe (a)** reverting `IF EXISTS` ⇒ `MigrationStepFailed`
  (recorded); **probe (b)** re-adding a value consumer ⇒ `no such table: initialization_maps`
  (recorded); both reverted.
- **Survivors:** word-key sets, word-position math, staged fetch + `tick_fingerprint`, solver
  walk, sparse gates — all crate suites green.
- **Result doc:** `docs/ergo-results/4s3lh2-result.md`.
- **PENDING:** `just test-rust-nextest`; full `pytest tests/uniswap tests/pool_companion`; the
  Python/FFI mirror retirement (§4) — needs the target-shape decision before dispatch.
- **No commits; foreign files untouched.**


---

# Round 3 (COMPLETION) — the Python mirror / FFI / fixture-golden reshape

**Scope completed:** the Python value-mirror + FFI wire shape + test/golden
fixtures. No commits. Foreign config-holder lane untouched.

## R3.1 The chosen keys-only shape (pinned)

`BitmapAtWord` is **deleted**. Every former `dict[int, BitmapAtWord]` /
`(bitmap, block)` word-value map becomes a **`set[int]` of bitmap WORD
positions** — the shape the Rust authority already uses
(`known_bitmap_words: HashSet<i32>`). Rationale: the only load-bearing
consumers are set-membership (`word not in tick_bitmap`, sparse
`LiquidityMapWordMissing` gating, `known_bitmap_words`), the FFI can emit a
`PySet` directly, and no consumer needs order or a per-word value. The public
NAME `tick_bitmap` is kept on the companion / `PoolState` / `LiquidityMap` /
the FFI view — only its element type moved from a value map to a word set, so
the sparse gate reads `word not in lp.tick_bitmap` unchanged.

## R3.2 Reshape deltas (file:line)

| File | Change |
|---|---|
| `src/degenbot/uniswap/concentrated/types.py` | `BitmapAtWord` class DELETED (‑20 l); `ValidatedUint256` import dropped. `LiquidityAtTick` (incl. `block`) untouched. |
| `src/degenbot/uniswap/concentrated/snapshot_readers.py:23` | `LiquidityMap.tick_bitmap: set[int]`. `_liquidity_map_from_raw:57` builds the word set from `raw.get("tick_bitmap", ())` (Rust emits no key). `_liquidity_map_for_key:159` word-`setdefault("block")` loop deleted. `_drain_tick_bitmap:258` returns/clears a `set[int]`. |
| `src/degenbot/uniswap/v3_snapshot.py:116,138` / `v4_snapshot.py:128,159` | `tick_bitmap(...) -> set[int] \| None`; `update(..., tick_bitmap: set[int])`; `BitmapAtWord` TYPE_CHECKING import gone. |
| `src/degenbot/uniswap/v3_types.py:61,73` / `v4_types.py:30,79` | `PoolState.tick_bitmap: set[BitmapWord]`; `InitializedTickMap = set[BitmapWord]`. |
| `src/degenbot/uniswap/cl_companion.py:87` | `tick_bitmap` property returns `{int(w) for w in view.tick_bitmap}` (‑value decode gone); `update_tick_data(tick_bitmap: set[int] \| dict[int, Any], …)` passes the caller's checked words straight through (`:194`); sparse gate `:273` unchanged. |
| `src/degenbot/uniswap/v3_liquidity_pool.py:61` | `BitmapAtWordAsDict` TypedDict DELETED; docstrings → `tick_bitmap`. |
| `src/degenbot/uniswap/v4_liquidity_pool.py:8,25,157` | docstrings → `tick_bitmap` word set. |
| `src/degenbot/builders/tick_data_fetcher.py:28` | `TickDataTypes.bitmap_at_word` field DELETED. |
| `src/degenbot/bot/_bot.py:59,708,748` | `BitmapAtWord` import + both `bitmap_at_word=` kwargs gone. |
| `rust/.../degenbot-python/src/bot/pool.rs:3252,2871,3343` | `cl_snapshot_fields` emits `Vec<i32>` of words (a `BTreeSet`, stable order) instead of OR-ing bitmap words; `PyConcentratedLiquidityView.tick_bitmap` getter returns a `PySet`; `update_tick_data` accepts **any iterable of word ints** (`try_iter`) instead of requiring a `PyDict`. |
| `rust/.../degenbot-python/src/db/snapshot.rs` | doc comments corrected (`DB emits no tick_bitmap key`). |
| `rust/.../degenbot-math/src/cl/liquidity_mapping.rs:232` | stale `#[expect(clippy::panic, clippy::cast_possible_truncation)]` trimmed (round-1 `flip_tick` deletion left them unfulfilled → `lint-rust-check` RED). |
| `rust/.../degenbot-db/tests/word_bitmap_retirement.rs:22` | stale `clippy::panic` expect trimmed (same). |
| fixtures/tests | see R3.3. |

**Stub delta (`just gen-stubs`):** `src/degenbot/_ffi/pool.pyi`
(`tick_bitmap` doc/signature), `src/degenbot/_ffi/db.pyi` (`get_liquidity_map_*`
doc), `src/degenbot/_ffi/__init__.pyi` (re-exported doc). `--check` green after
regen.

## R3.3 Test / fixture reshape

- `tests/uniswap/v3/conftest.py` — dropped the `BitmapAtWord` construction + the
  ignored `tick_bitmap=` kwarg to `make_v3_pool`.
- `tests/helpers/v3_pool_factory.py` (‑`tick_bitmap` param, ‑`BitmapAtWord`
  `__all__` export), `tests/helpers/v4_pool_factory.py` (‑param).
- `tests/uniswap/v3/test_uniswap_v3_dataclasses.py`, `test_uniswap_v3_liquidity_pool.py`
  — `test_tick_bitmap`/`test_tick_bitmap_equality` (pure value-type tests) DELETED;
  `lp.tick_bitmap.keys()` → `lp.tick_bitmap`.
- `tests/uniswap/test_snapshot_facade_semantics.py` — facade drain/merge now
  compared against word sets (`{1}`, `{1,3}`, `set()`).
- `tests/database/test_snapshot_seam_parity.py:71` — the DB-backed seam emits an
  **empty** word set (`assert result["tick_bitmap"] == set()`): the DB stores no
  words now that `initialization_maps` is dropped.
- `tests/database/test_liquidity_updater_seam.py` — the `initialization_maps`
  oracle field + both dump queries/assertions removed.
- `tests/database/test_pybot_io_pool_db.py:47` — the `initialization_maps` seed
  INSERT removed (it referenced a dropped table).
- `tests/pool_companion/test_io_free_construction.py:419,528` — value map →
  `pool.tick_bitmap == {0, -1}`.
- `tests/arbitrage/test_solvers/test_shared_state_topology.py` — `tick_bitmap`
  snapshots compared as word sets; `{3: (0,50)}`-style value args → word sets.
- `tests/builders/test_tick_data_fetcher.py:74` — `bitmap_at_word=` gone.
- `tests/test_ffi_checked_words.py` — `_snapshot` returns the word set;
  `(0, block)` value assertions → word-membership; the T3 gate test dropped its
  `initialization_maps` seed/UPDATE (dropped table).
- `tests/golden/recorded_pool.py` + `tests/golden/data/uniswap/{v3/wbtc_weth/17600000,v4/eth_usdc/25706703}.json`
  — `tick_bitmap` **trimmed** from `{word: {bitmap, block}}` to a sorted word
  list (`[16]`, `[-79]`); replay feeds the word set into `update_tick_data`.
- `tests/test_cl_companion_sparse_gate.py` — unchanged (the sparse gate still
  reads `word not in lp.tick_bitmap`, green).

## R3.4 Probes (verbatim, both reverted byte-for-byte)

**(a) Reintroduce a Python word-VALUE consumer** — `cl_companion.tick_bitmap`
restored to `.items()` over the FFI set:
```
$ uv run --no-sync pytest tests/pool_companion/test_io_free_construction.py
E       AttributeError: 'set' object has no attribute 'items'
src/degenbot/uniswap/cl_companion.py:101: AttributeError
FAILED ...::TestV4PoolIOFreeConstructor::test_io_free_with_tick_data
1 failed, 32 passed
```
Reverted → 33 passed.

**(b) Break the key-set plumbing** — removed
`core.mark_bitmap_words_known_by_pool_id(...)` from the FFI `update_tick_data`
(so checked words are never recorded), rebuilt, re-ran the sparse suites:
```
$ uv run --no-sync pytest tests/test_ffi_checked_words.py tests/test_cl_companion_sparse_gate.py tests/test_v3_sparse_fetch_seam.py
E       AssertionError: V4 checked-empty word must survive: set()
tests/test_ffi_checked_words.py:144: AssertionError
FAILED tests/test_ffi_checked_words.py::test_sparse_checked_word_survives_in_snapshot
FAILED tests/test_ffi_checked_words.py::test_sparse_checked_zero_word_prevents_refetch
FAILED tests/test_ffi_checked_words.py::test_v4_sparse_checked_zero_word_survives_in_snapshot
3 failed, 10 passed
```
The sparse refetch / `MissingTickWord` gating goes RED when the key set stops
being recorded. Reverted → 13 passed.

## R3.5 Gate evidence (fresh, direct exits)

| Gate | Result | Exit |
|---|---|---|
| `just rebuild-if-stale` (first action; known receipt race — installed 2279 → rebuilt 2280) | fresh | 0 |
| `just rebuild-if-stale` (after Rust edits) | fresh, receipt `2287` | 0 |
| `just lint-rust-check` (workspace clippy `--deny warnings`) | **PASS** (after trimming two stale BW3 `#[expect]`s; the round‑2 FOREIGN `config_seam.rs` failure is gone) | 0 |
| `just lint-python-check` (ruff **and** ty) | All checks passed | 0 |
| `just fmt-check` (`cargo fmt --all -- --check`) | clean (incl. the previously-FOREIGN files) | 0 |
| `just fmt-check-python` (`ruff format --check src/`) | 203 files formatted | 0 |
| `pytest tests/database tests/uniswap tests/pool_companion` | **240 passed, 1 skipped** | 0 |
| `pytest tests/golden` | 141 passed, 3 skipped | 0 |
| `pytest tests/builders tests/arbitrage/test_solvers/test_shared_state_topology.py tests/test_ffi_checked_words.py` | 138 passed, 1 skipped | 0 |
| `pytest tests/test_cl_companion_sparse_gate.py tests/test_v3_sparse_fetch_seam.py` | green (13 with the two above) | 0 |
| `just gen-stubs --check` | **FAIL before regen** (Python surface moved) → `just gen-stubs` → `--check` PASS | 1 → 0 |
| `just lint-comment-hygiene` | **FAIL** — every hit is the FOREIGN SERRGX config-holder lane (`config_seam.rs`, `config.rs`, `holder.rs`, `lib.rs`, `conftest.py`, `test_config_holder_seam.py`); **0 BW3 hits** | 1 |
| `just test-rust-nextest` | **PENDING** — detached past the 120 s bound (task `ed35f94e`) | — |

**BW3 regression fixed (found red this round):** `lint-rust-check` was RED on
two BW3 files whose `#[expect(clippy::panic, clippy::cast_possible_truncation)]`
became unfulfilled when round‑1 deleted `flip_tick` — this was masked in round 2
by the FOREIGN `config_seam.rs` failure aborting the same build. Trimmed.

## R3.6 Counts delta

| Symbol family | Before (round 2 end) | After R3 |
|---|---|---|
| Rust `BitmapAtWord` occurrences | 1 (doc comment) | **0** |
| Python `BitmapAtWord` occurrences (src + tests) | 25 | **0** |
| Python `dict[int, BitmapAtWord]` shapes | 4 | **0** |
| FFI word wire shape | `Vec<(i32,(U256,u64))>` → `dict[int,(bitmap,block)]` | `PySet[int]` of word positions |
| FFI `update_tick_data` arg | `dict` (keys read, values ignored) | any iterable of word ints |
| `TickDataTypes.bitmap_at_word` | 1 field + 2 call sites + 1 stub | **0** |
| Golden `tick_bitmap` payload | `{word:{bitmap,block}}` | sorted word list |

**Survivors proof:** `known_bitmap_words` (Rust `HashSet<i32>` — unchanged),
word-position math, the on-chain staged word fetch + `tick_fingerprint`, the
solver walk, and the sparse `MissingTickWord`/`LiquidityMapWordMissing` gates
all green; probe (b) proves the key set is still load-bearing.

## R3.7 Tree notes

- BW3 dirty set as expected; foreign config-holder files untouched
  (`config_seam.rs`, `test_config_holder_seam.py`, `config.rs`, `holder.rs`,
  `lib.rs`, `tests/conftest.py`).
- `rust/crates/foundation/degenbot-db/tests/fixtures/{parity,aave_parity}.db`
  were re-dirtied by the (still-running) `just test-rust-nextest` — those
  v2-stamped fixtures migrate in place on open (round‑2 §4). Restore with
  `git checkout -- rust/crates/foundation/degenbot-db/tests/fixtures/parity.db
  rust/crates/foundation/degenbot-db/tests/fixtures/aave_parity.db` after the
  gate completes; the generators still need the v3 regeneration (round‑2 §4).
- `rust/crates/foundation/degenbot-db/tests/fixtures/liquidity_updater_v{3,4}_initial.db`
  and `*_expected.json` still carry `initialization_maps` residue (not
  gate-enforced; harmless — the tables are dropped on open).

## R3.8 Sign-off

- **Reshape:** value map → word `set[int]` across the Python mirror + FFI;
  `BitmapAtWord` deleted on both sides.
- **Probes:** (a) value consumer → `AttributeError`; (b) key-set plumbing broken
  → 3 sparse checked-word tests RED; both reverted byte-for-byte.
- **Gates:** lint-rust 0, lint-python 0, fmt 0, fmt-python 0, pytest gate set 0,
  gen-stubs 0 (after regen), comment-hygiene 1 (FOREIGN only).
- **PENDING:** `just test-rust-nextest` (detached > 120 s).


---

## R4 — manager note (the fix round + final gates)

The manager's independent gate pass found the reshape's TEST code lagging: 9 compile errors in
`degenbot_rs` lib tests still importing deleted shapes (`ApplyBitmapAtWord` x2, `LiquidityMap.tick_bitmap`,
`ComputedLiquidityUpdate.tick_bitmap`, `ApplyLiquidityAtTick` tuple mismatches). A fresh fix round
updated the test code (compile-clean verified: `cargo check -p degenbot_rs --all-targets` = 0 errors)
and stopped at a caution boundary (foreign lane rebuilding the tree). The manager then ran the full
battery: nextest **4768 passed / 29 skipped**, lint-rust 0, lint-python 0, pytest 240+ passed,
gen-stubs 0. (An earlier "poison test file" scare was a manager-side path typo in a brief —
`tests/test_snapshot_facade_semantics.py` lives under `tests/uniswap/` and was green throughout.)
Epic U2XZSD closes on this record.
