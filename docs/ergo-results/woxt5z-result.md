# WOXT5Z (PC12, epic GZPPS3): kill hardcoded per-tick block stamps — 0 is a legal stamp

STATUS: **GREEN.** The three manager-listed fill sites are fixed, the two Python model
defaults are gone, the pinned 0-assertion is replaced by a real-block assertion, the
stamp-contract docs carry the new wording, and the residual production sweep is cited. Tree
left dirty for the manager's whole-tree ladder (no commits, no `.ergo` state changes).

**FIX ROUND (manager review findings 1 + 2).** The 8 residual production fills the first pass
left are now CLOSED where a real block is obtainable in scope: the 6 DB-row readers stamp the
pool's committed `liquidity_update_block` (threaded in), and the 2 reorg-rollback fills stamp
the restored liquidity clock (judgment — the rewind target was rejected as semantically wrong;
see the fix-round section). No silent-0 production fill remains; the only `0` in the new fill
path is the documented "no committed rows" branch of `PoolUpdateState::snapshot_block`.

Design ruling honored (2026-10-10): block `0` is a LEGAL stamp (a chain can launch with a
deployed V3/V4 pool, tokens, and a position at genesis). No `Option<u64>`, no magic values, no
sentinel semantics were introduced. Every fill that had a real block in scope now uses it.

Baseline cited: `docs/ergo-results/hqylek-result.md` shrink-counts table (C2 lists the
`tick_data` Python→Rust conversion sites; the two-tuple seam is one of the bodies that table
counted — it now threads a real block instead of a hardcoded 0).

## Inventory — every hardcoded block-0 site, classified

Sweep form: `rg -n --type rust '\bblock: 0\b' rust/crates` (the `\b` boundary excludes the
pool-level `update_block` / `tick_data_block` / `max_update_block` clocks, which the task body
placed out of scope) plus `rg 'block: BlockNumber = 0' src/`. Totals: **314** `\bblock: 0\b`
hits in `rust/crates`, of which **247** are under `tests/`/`benches/`/`examples/` dirs and the
rest split into `#[cfg(test)]` modules and 8 production sites (below).

### Production silent fills — FIXED (real block in scope)

| # | file:line (post-fix) | site | real block threaded from |
|---|---|---|---|
| 1 | `rust/crates/shells/degenbot-python/src/bot/pool.rs:3093` | `TickData::from_py_pairs` filled `Self::info(gross, net, 0)` | new `block: u64` param; callers pass `block_number` |
| 2 | `rust/crates/shells/degenbot-python/src/bot/engine/solve.rs:401` | `sync_v3_pool_states` 2-tuple backfill | `block_number` (the seam's sync block) |
| 3 | `rust/crates/shells/degenbot-python/src/bot/engine/solve.rs:453` | `sync_v4_pool_states` 2-tuple backfill | `block_number` (feeds `V4StateSync::update_block`) |
| 4 | `rust/crates/facade/degenbot/src/investigation/reconstruct.rs:24` | `tick_map` built `TickInfo { block: 0 }` | `p.liquidity_update_block` (the fixture's bulk-sync block) |
| 5 | `rust/crates/foundation/degenbot-substrate/src/tick_assembly.rs:512` | `convert_liquidity_at_tick` pinned `0` for the Db arm | `resolve_tick_map_arm`'s `block` (the Db sync block `S`) |
| 6 | `rust/crates/integrations/degenbot-pool-updater/src/verify.rs:125` | `shared_map` built `degenbot_pools::TickInfo { block: 0 }` | `value.block` (the math `LiquidityAtTick` already carries it) |

Sites 5 and 6 are the manager's item-4 rule ("any PRODUCTION silent fill with a real value in
scope gets fixed") applied to production sites the pre-survey inventory did not enumerate. Both
had the real block one frame up (`resolve_tick_map_arm(block)` and the updater's stamped
`LiquidityAtTick.block`) and were silently discarding it.

### Defaults REMOVED (the two Python models)

| file:line | before | after |
|---|---|---|
| `src/degenbot/uniswap/concentrated/types.py:26` | `class BitmapAtWord: ... block: BlockNumber = 0` | `block: BlockNumber` (required) |
| `src/degenbot/uniswap/concentrated/types.py:38` | `class LiquidityAtTick: ... block: BlockNumber = 0` | `block: BlockNumber` (required) |

Every construction site now supplies its real value:
- production: `cl_companion.py:101/:119` (already carried `block`), `snapshot_readers.py`
  (see next row);
- tests: `test_v3_rust_swap_parity.py`, `test_v4_rust_swap_parity.py`,
  `pool_companion/test_io_free_construction.py`, `test_snapshot_facade_semantics.py`,
  `v3/test_v3_calc_delegation.py`, `v4/test_v4_calc_delegation.py`,
  `v4/test_v4_snapshot_backfill.py`, `v3/conftest.py` — all already passed `block` explicitly;
- defaults-dependent tests updated (explicit legal `0`, not sentinel use):
  `tests/uniswap/v3/test_uniswap_v3_dataclasses.py`,
  `tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py`.

### The DB-snapshot path (required — otherwise removing the defaults breaks the public seam)

`_liquidity_map_from_raw` does `LiquidityAtTick(**entry)` / `BitmapAtWord(**entry)`. The
Rust-backed DB seam (`rust/crates/shells/degenbot-python/src/db/snapshot.rs::build_liquidity_map_py`,
documented at `src/degenbot/_ffi/db.pyi:132`) emits **no** per-tick/per-word `block` — the DB
schema stores only `liquidity_gross`/`liquidity_net` (`degenbot-db/src/schema_head.sql`, and
`degenbot-db/src/snapshot.rs`'s `BitmapAtWord`/`LiquidityAtTick` have no `block` field). With
`block` now required, `DatabaseSnapshotBase._liquidity_map_for_key` stamps the snapshot's
bulk-sync block onto each DB row via `entry.setdefault("block", self._seam_newest_block())`
before wrapping. File-snapshot rows already carry their own `block` and are unchanged. This is
the contract's "bulk-sync block", a real value — not a `0` fill.

### Explicit fixture values — KEPT (legal under the ruling; not churned)

| file | hits | note |
|---|---|---|
| `rust/crates/foundation/degenbot-substrate/src/cl_orchestration.rs:3095/:3186` | 2 | inside `#[cfg(test)]` registration helpers |
| `rust/crates/foundation/degenbot-substrate/src/planning.rs:672/:680` | 2 | `#[cfg(test)]` `v4_ticks()` builder |
| `rust/crates/foundation/degenbot-substrate/src/swap_simulation.rs:1397/:1668/:1691/:1851` | 4 | `#[cfg(test)] mod tests` (cfg at :1346) |
| `rust/crates/foundation/degenbot-substrate/src/log_dispatcher.rs:836/:910/:1072` | 3 | `#[cfg(test)] mod tests` (cfg at :764) |
| `rust/crates/foundation/degenbot-substrate/src/tick_assembly/tests.rs:62/:155/:165/:187/:197/:457/:619/:627` | 8 | test file |
| `rust/crates/.../**/tests/*.rs`, `benches/`, `examples/` and all `#[cfg(test)]` builders | 306 | explicit fixtures (legal 0) |
| `rust/crates/foundation/degenbot-pools/src/lib.rs:198`, `tick_bitmap.rs:737`, `v3_state.rs`, `v4_state.rs`, `v3/v4_storage_slots.rs`, `degenbot-math/.../liquidity_mapping.rs:720`, `bot/mod.rs:3181…` | — | all inside `#[cfg(test)]` modules |

The manager's item-4 list is fully accounted for above; none is a production silent fill.

### Residual production fills — CLOSED in the fix round (real block in scope)

The manager's two review findings: the earlier "no real block in scope" reading was wrong —
the ruling's scope test is the CODE REGION, and a real block was one call away in both.

#### Finding 1 — `degenbot-db/src/liquidity_updater.rs` DB-row readers (6 fills)

The DB schema stores no per-tick/per-word block, but the stamp contract's **bulk-sync block**
— the block the pool's committed map is exact at, i.e. its `liquidity_update_block` — is in
scope at every read. New `PoolUpdateState::snapshot_block()`
(`rust/crates/foundation/degenbot-db/src/liquidity_updater.rs:94-108`) returns it; `0` means
"no liquidity persisted yet" (no marker ⇒ no committed rows), a legal stamp, never a sentinel.

| file:line (post-fix) | site | block threaded from |
|---|---|---|
| `degenbot-db/src/liquidity_updater.rs:423` | `fetch_v3_liquidity_map_on_conn` word fill | new `block: u64` param |
| `degenbot-db/src/liquidity_updater.rs:449` | `fetch_v3_liquidity_map_on_conn` tick fill | new `block: u64` param |
| `degenbot-db/src/liquidity_updater.rs:503` | `fetch_v4_liquidity_map_on_conn` word fill | new `block: u64` param |
| `degenbot-db/src/liquidity_updater.rs:529` | `fetch_v4_liquidity_map_on_conn` tick fill | new `block: u64` param |
| `degenbot-db/src/liquidity_updater.rs:1696` | `compute_liquidity_delta_on_tables` word fill | `state.snapshot_block()` (line 1676) |
| `degenbot-db/src/liquidity_updater.rs:1722` | `compute_liquidity_delta_on_tables` tick fill | `state.snapshot_block()` (line 1676) |

`fetch_v3_liquidity_map(_on_conn)` / `fetch_v4_liquidity_map(_on_conn)` gained the `block: u64`
param; every caller passes its fetched `state`'s `snapshot_block()` — the DB map's OWN sync
block (the same clock the `tick_assembly` Db arm stamps via `convert_liquidity_at_tick`), never
the verify's comparison `block_number` (that names the chain read, not the map's mutation
clock, so stamping it would falsify):

- internal compute: `compute_v3/v4_liquidity_update_on_conn` (`liquidity_updater.rs:598/:711`);
- `degenbot-pool-updater/src/verify.rs:249/:286/:370/:410` (`verify_all_pools_committed_on_conn`, `collect_pre_commit_checks`);
- `degenbot-python/src/pool/mod.rs:265/:343` (`verify_v3/v4_liquidity_map`);
- `degenbot-cli-core/src/pool.rs:429/:463` (`fetch_verify_state`).

#### Finding 2 — reorg rollback fills (`v3_state.rs`, `v4_state.rs`) — judgment

Verified against the code: `state_history.rs`'s `TickBefore` carries only
`liquidity_gross_before`/`liquidity_net_before` — NOT the prior block — so a restored tick's
exact last-mutation block is genuinely unrecoverable from the journal. The rewind TARGET is
semantically WRONG as a stamp (it names a mutation this rollback just UNDID). So neither the
manager's "fill the rewind target" nor a "leave the 0" was taken: the real value IS in scope —
the rewind has ALREADY reset the pool's liquidity clock (`tick_data_block`) to its pre-target
value, and that is the block the restored map is exact at (its bulk-sync block — the same clock
Finding 1 uses; the DB's `liquidity_update_block` mirrors it). The restored rows are stamped
with it:

| file:line (post-fix) | site | block |
|---|---|---|
| `degenbot-pools/src/v3_state.rs:1105` | `restore_before_block` restored `TickInfo` | `restored_block = self.tick_data_block` (captured at `:1093`, post-rewind) |
| `degenbot-pools/src/v4_state.rs:810` | V4 twin | `restored_block = self.tick_data_block` (captured at `:798`) |

`0` is legal but falsifies a value that IS recoverable, so leaving it was not taken. The
per-tick EXACT block stays unrecoverable: the schema-level closure is to add a
`block_before: u64` to `TickBefore` (captured in `registry.rs`'s four `journaled_priors`
builders alongside `liquidity_gross_before`) and restore it here — a state-history type change,
left for a follow-on (see PENDING). Also documented: the mid-flight "empty-rows" branch of
`snapshot_block` is the only remaining `0` in a fill path, and it is provably fill-free.

No non-test production `block: 0` fill remains (post-fix sweep: `rg -n 'block: 0'
rust/crates/foundation/degenbot-db/src/liquidity_updater.rs` → none; the reorg sites now read
`block: restored_block`).

## Shapes from the diff

Rust — `pool.rs`, `from_py_pairs` (the pinned seam):

```rust
    pub(crate) fn from_py_pairs(
        container: &Bound<'_, PyAny>,
        block: u64,
    ) -> PyResult<HashMap<i32, TickInfo>> {
        let parsed: std::collections::HashMap<i32, (u128, i128)> =
            container.extract().map_err(|_| Self::malformed())?;
        Ok(parsed
            .into_iter()
            .map(|(tick, (gross, net))| (tick, Self::info(gross, net, block)))
            .collect())
    }
```

Rust — `tick_assembly.rs`, the Db arm now threads its sync block:

```rust
    let arm = match liquidity_map_to_tick_info(map, tick_spacing, block).map_err(E::from)? {
...
fn convert_liquidity_at_tick(
    tick_data: HashMap<i32, LiquidityAtTick>,
    block: u64,
) -> HashMap<i32, TickInfo> {
    ...
                TickInfo {
                    liquidity_gross: lat.liquidity_gross.to::<alloy::primitives::U128>(),
                    liquidity_net: lat.liquidity_net,
                    block,
                },
```

Rust — `verify.rs` `shared_map` keeps the math row's own stamp:

```rust
            degenbot_pools::TickInfo {
                liquidity_gross: gross,
                liquidity_net: net,
                block: value.block,
            },
```

Rust — `reconstruct.rs` `tick_map(data, block)`; each caller passes
`p.liquidity_update_block.expect("v3|v4 update_block")`.

Python — `types.py` (both defaults gone):

```python
class BitmapAtWord(pydantic.BaseModel, frozen=True):
    """... ``0`` is a legal stamp (a chain can launch at genesis), never a sentinel."""
    bitmap: ValidatedUint256
    block: BlockNumber
```

Python — `snapshot_readers.py`, the DB seam stamps the bulk-sync block:

```python
        source_block = self._seam_newest_block()
        for entry in raw["tick_bitmap"].values():
            entry.setdefault("block", source_block)
        for entry in raw["tick_data"].values():
            entry.setdefault("block", source_block)
        return _liquidity_map_from_raw(raw)
```

Stamp-contract docs updated (new wording, zero sentinel language):
- `rust/crates/foundation/degenbot-pools/src/lib.rs:163` `TickInfo.block`
- `rust/crates/shells/degenbot-python/src/bot/pool.rs:3015` the 2-tuple seam doc
- `src/degenbot/uniswap/concentrated/types.py` both model docstrings

Pinned test flipped: `tick_data_two_tuple_backfill_fills_block_from_context` (asserted `0`) →
`tick_data_two_tuple_backfill_fills_the_real_sync_block` (passes `21_000_000`, asserts it lands).
`rg 'fills_block_from_context' rust/` → NONE.

## Negative probe (non-negotiable) — record verbatim

Flip: `rust/crates/shells/degenbot-python/src/bot/pool.rs` `from_py_pairs` closure
`Self::info(gross, net, block)` → `Self::info(gross, net, block * 0)` (hardcode the fill back to
0 while keeping the param live to avoid an unused-variable compile error), then
`just test-rust-nextest` (exits captured directly):

```
        FAIL [   0.014s] (4609/4735) degenbot_rs bot::pool::tests::tick_data_two_tuple_backfill_fills_the_real_sync_block
  stderr ───
    thread 'bot::pool::tests::tick_data_two_tuple_backfill_fills_the_real_sync_block' (25879) panicked at crates/shells/degenbot-python/src/bot/pool.rs:3161:13:
    assertion `left == right` failed
      left: 0
     right: 21000000
    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
error: recipe `test-rust-nextest` failed with exit code 100
```

The failure names the missing real block (`right: 21000000`). Reverted byte-for-byte
(`git diff -- pool.rs` shows only the intended `block` fill); `just test-rust-nextest` re-run
green: `4735 tests run: 4735 passed, 29 skipped`.

## Gate exits (captured directly — `cmd; echo EXIT:$?`, never through a pipe)

| gate | exit |
|---|---|
| `just lint-rust-check` | 0 (run twice: after edits, and after final rustfmt) |
| `just test-rust-nextest` | 0 (`4735 passed, 29 skipped`); probe run 100; post-revert 0 |
| `just rebuild-if-stale` | 0 (`extension is fresh`) |
| `uv run --no-sync pytest tests/pool_companion tests/uniswap tests/database/test_snapshot_seam_parity.py tests/test_v3_rust_swap_parity.py tests/test_v4_rust_swap_parity.py -q` | 0 (`213 passed, 1 skipped`) |
| `just test-python` | 0 (`3136 passed, 50 skipped`) |
| `just lint-python-check` (ruff + ty) | 0 |
| `just gen-stubs --check` | 0 (`committed stubs match generator output`) |
| `just lint-comment-hygiene` | 0 |
| `rustfmt --check --edition 2021 --config skip_children=true -- <touched .rs>` | 0 |
| `uv run --no-sync ruff format --check -- <touched .py>` | 0 |

The scoped pytest list was widened by `tests/database/test_snapshot_seam_parity.py` because the
DB snapshot seam (`snapshot_readers.py`) was touched.

## Fix-round negative probe (record verbatim)

Flip: `degenbot-db/src/liquidity_updater.rs:423` — the `fetch_v3_liquidity_map_on_conn` word
fill `block,` → `block: 0,` (hardcode the newly-fixed fill back to 0), then the scoped test:

```
    FAIL [   0.009s] (1/1) degenbot-db liquidity_updater::tests::fetch_v3_liquidity_map_stamps_rows_with_the_snapshot_block
  stderr ───
    thread 'liquidity_updater::tests::fetch_v3_liquidity_map_stamps_rows_with_the_snapshot_block' (102093) panicked at crates/foundation/degenbot-db/src/liquidity_updater.rs:2359:9:
    assertion `left == right` failed: reconstituted word row must carry the snapshot block, not 0
      left: 0
     right: 21000000
    note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
error: test run failed
EXIT:100
```

The failure names the missing real block (`right: 21000000`). Reverted byte-for-byte
(`rg -c 'block: 0' …/liquidity_updater.rs` → no matches); scoped test re-run green, and the
full ladder below ran on the reverted tree.

## Fix-round gate exits (captured directly — `cmd; echo EXIT:$?`)

| gate | exit |
|---|---|
| `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` (rustfmt authority) | 0 |
| `just lint-rust-check` | 0 |
| `just lint-comment-hygiene` | 0 |
| `just gen-stubs --check` | 0 — NO stub delta (`✓ committed stubs match generator output`) |
| `just lint-python-check` (ruff + ty) | 0 (`All checks passed!`) |
| `cargo check --locked --workspace --all-targets` | 0 |
| `just test-rust-nextest` | 0 (`4740 tests run: 4740 passed, 29 skipped`); probe FAIL(100); post-revert 0 |
| scoped `cargo nextest -p degenbot-db -p degenbot-pools -E 'test(stamps) \| test(liquidity_map) \| test(restore_before_block)'` | 0 (`38 passed, 522 skipped`) |

The fix round touched Rust-only files, so no Python fmt/lint delta (the previous agent's Python
files are untouched this round). No commits, no `.ergo` changes.

## Stub delta

None. `just gen-stubs --check` is green with no regen. Removing the two pydantic defaults does
not move `src/degenbot/_ffi/*.pyi` — those stubs are generated from the Rust PyO3 surface, and
none of the touched Rust signatures is a `#[pyfunction]` (the `from_py_pairs` signature change
is a `pub(crate)` internal).

## Deliberately left

- All test-fixture `block: 0` sites (247 under test dirs + the `#[cfg(test)]` in-source builders)
  — explicit legal values under the ruling; not churned.
- Pool-level clocks (`update_block`, `tick_data_block`, `initial_state_block`,
  `max_update_block`) — different clocks, out of scope per the task body.
- `TickInfo` / `LiquidityAtTick` / `BitmapAtWord` type shapes — no restructuring, no `Option`.

## PENDING (manager decides)

1. **Per-tick EXACT stamp after a reorg rollback.** The restored rows now carry the pool's
   restored liquidity clock — the block the map is exact at — but a tick's exact
   last-mutation block stays unrecoverable (the journal prior records only gross/net).
   Schema-level closure: add `block_before: u64` to `TickBefore`
   (`rust/crates/foundation/degenbot-pools/src/state_history.rs`), capture it in
   `registry.rs`'s four `journaled_priors` builders alongside `liquidity_gross_before`, and
   restore it in the two `restore_before_block` fills in place of `restored_block`. A
   state-history type + journal-format change, deliberately left out of a fill-site fix round.
2. **Verify-seam stamp choice.** The verify seams stamp `state.snapshot_block()` (the map's own
   committed sync block), not the verify's `block_number`. That is the honest "block at which
   the tick was last mutated" and keeps ONE convention with Finding 1; if the manager prefers
   the comparison block (the `tick_assembly` caller-supplied convention), it is a one-line
   change per seam.
3. Removing the two Python model defaults is a breaking change to the public constructor surface
   of `BitmapAtWord`/`LiquidityAtTick` (a call that omitted `block` no longer validates). The
   task body's preferred fix, and no in-repo caller relied on the default except the two tests
   updated here — flagging in case a deprecation decision is wanted.
4. Manager whole-tree ladder (the fix round re-ran it — see the gate table).

## Commit attribution note (manager, 2026-10-10)

Commit `0c09b1aa3` ("refactor(ticks): kill hardcoded block stamps...") contains THREE
independently-authored streams — a manager staging error swept in two foreign lanes' uncommitted
work alongside this task's diff. The tree stayed coherent (battery green: pre-push 0, 302 passed,
stubs 0, fmt both 0; the tree-wide comment-hygiene red is from a third foreign lane's in-flight
comment sweep in `src/degenbot/logging.py` / `tests/test_rust_logging_bridge.py`, left dirty).

1. **This task (PC12/WOXT5Z)** — the tick-stamp work recorded above (fills, defaults, contract docs).
2. **Foreign: DB-only snapshot enforcement (ADR-005)** — file-based snapshot sources deleted
   (`MonolithicJsonFileSnapshotBase` et al. in `snapshot_readers.py`, `v3_snapshot.py`,
   `v4_snapshot.py`), 8 fixture JSONs under `tests/uniswap/v3/snapshot/` retired,
   `test_uniswap_v3_snapshot.py` reshaped. Not authored by this task's workers.
3. **Foreign: ambient-config + logging lanes** — `.devcontainer/devcontainer.json`
   (MEVBLOCKER backrun endpoint env) and `tests/conftest.py` (log-listener drain-order fixture).
   Not authored by this task's workers.

If your lane authored (2) or (3): your work is committed at `0c09b1aa3`, unmodified. History is
not rewritten (house rule). The three still-dirty foreign files were left untouched.
