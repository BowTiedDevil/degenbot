# 4QBFGW (PC1, epic GZPPS3): the tick-data snapshot seed gets one boundary home

STATUS: **PC1 structural work STANDS (prior worker); the doc-prose stub-drift regression is FIXED
by this worker.** `just gen-stubs --check` is GREEN with ZERO stub regeneration. The manager's
whole-workspace ladder is PENDING (not mine to run).

Chunk: one boundary home for the registration-lifecycle tick-data seed value
(`dict[tick, (gross, net, block)]` <-> `HashMap<i32, TickInfo>`).

---

## 1. What PC1 changed (structural — from the diff, read by this worker)

The prior worker collapsed seven conversion sites onto a single adapter. Files and shapes:

### `rust/crates/shells/degenbot-python/src/bot/pool.rs`

- **New `pub(crate) struct TickData;`** (a namespaced adapter, no state), owning the wire contract:
  - `const MALFORMED: &str = "tick_data must be {tick: (liquidity_gross, liquidity_net, block)}"`
    — the ONE error string every seed site reports.
  - `fn malformed() -> PyErr` — `PyTypeError` built from `MALFORMED`.
  - `fn info(gross, net, block) -> TickInfo` — the `U128` gross narrowing + per-tick `block`.
  - `pub(crate) fn from_py(&Bound<PyAny>) -> PyResult<HashMap<i32, TickInfo>>` — 3-tuple form.
  - `pub(crate) fn from_py_optional(Option<Bound<PyDict>>)` — the `None` -> empty-map convention.
  - `pub(crate) fn from_py_pairs(&Bound<PyAny>)` — 2-tuple `(gross, net)` backfill, `block = 0`.
  - `pub(crate) fn to_py(py, rows: impl IntoIterator<Item = (i32, u128, i128, u64)>)` — the
    reverse direction, so a snapshot round-trips through the same wire shape.
  - 3 unit tests (`tick_data_seed_round_trips_through_the_one_wire_shape`,
    `tick_data_two_tuple_backfill_fills_block_from_context`,
    `tick_data_wrong_arity_raises_the_one_error`).
- **Deleted `fn extract_tick_data`.** Its consumers now call `TickData::from_py`.
- Collapsed sites: `PyTickWordFetcher::fetch_missing_tick_word` (inline dict parse ->
  `TickData::from_py`, mapped to `FetchTickWordError::FetchFailed`);
  `sim_override_inner`'s caller (`extract_tick_data` -> `TickData::from_py`);
  `PyLiquidityPool::update_tick_data` (hand-rolled loop -> `TickData::from_py`);
  `build_tick_rows_py` (manual `PyTuple` loop -> `TickData::to_py`).

### `rust/crates/shells/degenbot-python/src/bot/mod.rs`

- `use crate::bot::pool::TickData;` added.
- `register_v3_pool` and `register_v4_pool`: the two inline
  `std::collections::HashMap<i32,(u128,i128,u64)>` extract+map blocks (each with its own inline
  TypeError string) collapse to `TickData::from_py_optional(tick_data)?`.
- `build_tick_rows_py` body now delegates to `TickData::to_py`.

### `rust/crates/shells/degenbot-python/src/bot/engine/mod.rs`

- **Deleted `fn make_tick_info`** (the 2-tuple 2-arity helper) and its
  `pub(crate) use hashbrown::HashMap;` re-export (unused after the collapse).

### `rust/crates/shells/degenbot-python/src/bot/engine/solve.rs`

- Import switched to `use crate::bot::pool::TickData;`.
- The two inline `rust_tick_data.insert(...)` loops in `sync_v3_pool_states` /
  `sync_v4_pool_states` (each with its own `Expected 2-tuple (liquidity_gross, liquidity_net)`
  ValueError) collapse to `TickData::from_py_pairs(td_dict.as_any())?`.

**Net:** 7 conversion bodies across 2 files -> 1 boundary (`TickData`), with a single error
vocabulary.

---

## 2. Counts delta

Baseline source: `docs/ergo-results/hqylek-result.md` (PC0, run 2, 2026-10-10).

| # | target | PC0 baseline | PC1 observed | delta |
|---|---|---|---|---|
| C2 | `tick_data` Python->Rust conversion bodies under `rust/crates/shells/degenbot-python/src` | **7** | **1** | -6 |

The PC0 C2 table named the seven bodies:
`pool.rs:106` (`PyTickWordFetcher::fetch_missing_tick_word`), `pool.rs:1828`
(`update_tick_data`), `pool.rs:3008` (`extract_tick_data`), `mod.rs:1824` (`register_v3_pool`),
`mod.rs:1978` (`register_v4_pool`), `solve.rs:415` (`sync_v3_pool_states`),
`solve.rs:481` (`sync_v4_pool_states`). All seven now route through `TickData`.

**The one-arity-assertion check (acceptance criterion):**

```bash
rg -n 'tick_data must be' rust/crates/shells
# -> rust/crates/shells/degenbot-python/src/bot/pool.rs:2984  (the MALFORMED const)  = exactly 1
```

```bash
rg -n 'tick_data must be|liquidity_gross, liquidity_net, block' rust/crates/shells
# -> 6 hits: pool.rs:2984 (the ONE assertion string)
#            + 5 doc-prose hits (mod.rs:1619,1791; pool.rs:895,1031,1772)
```

The five extra hits are **doc-comment prose**, not arity assertions — they carry the repo's
dominant spelling `liquidity_gross, liquidity_net, block` (same as
`src/degenbot/uniswap/cl_companion.py:113` and `tests/helpers/v3_pool_factory.py:36`). The
**assertion** count is exactly one, which is the criterion the task body means ("exactly ONE arity
assertion (the adapter's)").

---

## 3. Negative-probe demonstration record

Unit test (in `bot/pool.rs`, `mod tests`):

```rust
/// The red path: a wrong-arity container raises the ONE typed error.
#[test]
fn tick_data_wrong_arity_raises_the_one_error() {
    Python::attach(|py| {
        let dict = PyDict::new(py);
        dict.set_item(42, (7u128, -3i128)).unwrap();          // 2-tuple, not the 3-tuple seed shape
        let err = TickData::from_py(dict.as_any())
            .expect_err("a 2-tuple is not the 3-tuple seed shape");
        assert_eq!(err.to_string(), format!("TypeError: {}", TickData::MALFORMED));
    });
}
```

Green (this worker, foreground, exit captured directly):

```bash
cd rust && cargo test -p degenbot_rs --lib tick_data; echo EXIT=$?
# running 3 tests
# test bot::pool::tests::tick_data_two_tuple_backfill_fills_block_from_context ... ok
# test bot::pool::tests::tick_data_seed_round_trips_through_the_one_wire_shape ... ok
# test bot::pool::tests::tick_data_wrong_arity_raises_the_one_error ... ok
# test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 123 filtered out
# EXIT=0
```

**Deliberate-expectation-flip evidence.** No prior artifact of the flip existed on disk (searched
`docs/`, `.ergo/`, `/tmp/`), so — doubting the *existence* of the record, not its truth — this
worker re-demonstrated it: the assertion's expected string was deliberately flipped to the retired
mixed vocabulary (`ValueError: Expected 2-tuple (liquidity_gross, liquidity_net), got 2 elements`),
producing RED, then reverted byte-for-byte.

```bash
# FLIPPED (expected string only) -> RED
cd rust && cargo test -p degenbot_rs --lib tick_data_wrong_arity_raises_the_one_error; echo EXIT=$?
#   assertion `left == right` failed
#     left: "TypeError: tick_data must be {tick: (liquidity_gross, liquidity_net, block)}"
#    right: "ValueError: Expected 2-tuple (liquidity_gross, liquidity_net), got 2 elements"
# test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 125 filtered out
# EXIT=101
```

The flip proves the probe is genuinely sensitive: the boundary emits exactly the ONE typed
`TypeError: tick_data must be {tick: (liquidity_gross, liquidity_net, block)}` for wrong arity, not
the retired per-copy vocabulary. The assertion was restored and re-run GREEN (the `--lib tick_data`
run above; full crate re-verified too — `cargo test -p degenbot_rs --lib` -> 126 passed, EXIT=0).

---

## 4. The fix applied (this worker)

**What broke the stub-drift gate:** the prior worker reworded **5 Rust `///` doc comments** from the
repo's dominant spelling `liquidity_gross, liquidity_net, block` to `(gross, net, block)` —
prose-only churn. Generated stubs take Rust `///` prose verbatim, so `src/degenbot/_ffi/__init__.pyi`
lines ~709, 1334, 3146, 3598, 3639 still carried the long spelling and drifted from generator
output.

**Fix (preferred path — zero stub churn): reverted exactly the 5 rewordings byte-for-byte** to the
original spelling. No code shape was touched (comment prose only):

| file | line | reverted doc comment |
|---|---|---|
| `bot/mod.rs` | 1619 | ``/// `tick_data` has the same `{tick: (liquidity_gross, liquidity_net, block)}` `` |
| `bot/mod.rs` | 1791 | ```/// `tick_data` is ``{tick: (liquidity_gross, liquidity_net, block)}`` ``` |
| `bot/pool.rs` | 895 | ``/// `(liquidity_gross, liquidity_net, block)`; returning `None` or `{}` marks`` |
| `bot/pool.rs` | 1031 | ```/// (`{tick: (liquidity_gross, liquidity_net, block)}`, same shape as``` |
| `bot/pool.rs` | 1772 | ```/// `{tick: (liquidity_gross, liquidity_net, block)}` — the write path is``` |

No fallback (`just gen-stubs` regeneration) was needed; **zero stub churn was committed** —
`git status --porcelain -- src/degenbot/_ffi/` is empty after the check.

Left untouched (accepted new-code prose / non-stub): the `TickData` struct/method doc comments and
the `MALFORMED` const (which carries the long spelling by design). The private `build_tick_rows_py`
doc rewrite and the deleted-helper diffs are part of the accepted structural change, not stub
input.

---

## 5. Scoped gates (foreground; exits captured directly)

| gate | command | result |
|---|---|---|
| Stub drift | `just gen-stubs --check` | **GREEN** — `✓ committed stubs match generator output`, `EXIT=0` |
| Boundary unit tests | `cargo test -p degenbot_rs --lib tick_data` | **GREEN** — 3 passed, `EXIT=0` |
| Negative-probe flip | (deliberately flipped expectation) | **RED as designed** — `EXIT=101`; reverted |
| Full touched crate | `cargo test -p degenbot_rs --lib` | **GREEN** — 126 passed, `EXIT=0` |
| Build receipt (rust-rebuild skill) | `just rebuild-if-stale` | **GREEN** — rebuilt via `just dev`, then `extension is fresh`, `EXIT=0` |
| Build receipt re-check | `just verify-build-fresh` | **GREEN** — `extension is fresh`, `EXIT=0` |

Run from `/workspaces/degenbot` (cargo gates from `/workspaces/degenbot/rust`). No pipes on any
exit capture; nothing detached.

## 7. Manager review — verdict PASS (2026-10-10)

Adversarial review against the diff, not the story. Independently re-verified: the 7-site collapse
(`rg 'tick_data must be' rust/crates/shells` = 1; `extract_tick_data|make_tick_info` = 0 hits),
the stub file unmodified, and `just gen-stubs --check` **EXIT=0** after the fix round (the 5 prose
reverts restored the stubs' generated spelling with zero stub churn).

Manager ladder runs:
- run 3a (`/tmp/pc1-gates/pre-push.log`) — **exit=2**: `tests/ffi/test_ffi_stub_drift.py` tripped its
  busy-build-graph guard (the foreign wave-5 lane held `rust/target/debug/.cargo-build-lock`,
  pid 2255087) during the Python lane. Not a code failure; the guard did its job.
- run 3b (`/tmp/pc1-gates2/`) after the lock cleared — `just pre-push` **exit=0**
  ("✓ all pre-push gates passed.") + `just gen-stubs --check` **exit=0**. Green against the PC0
  baseline (run 2).

Note carried forward: `from_py_pairs` fills per-tick `block` = 0 (matching the retired
`make_tick_info`) — behaviour-preserving, but PC8's simulate seam should re-check whether the
2-tuple backfill form should carry a real block.

**PENDING (manager's to run — not this worker's):** the whole-workspace ladder —
`just pre-push` / `just test-rust-nextest` / `just test-python` and the PC1-scoped Python suites
(`pytest tests/uniswap tests/pool_companion`). This worker touched only Rust comment prose and ran
no Python suite; the manager re-runs every PENDING gate.

---

## 6. Tree / foreign edits

- **Written by this chunk:** the 5 doc-prose reverts in
  `rust/crates/shells/degenbot-python/src/bot/{mod.rs,pool.rs}` and **`docs/ergo-results/4qbfgw-result.md`
  (this file) only.** No `.ergo/plans.jsonl` entry, no ergo task-state change, no commit. The
  temporary flip edit was reverted.
- **Foreign modifications present at start and left untouched** (another session's docs restructure +
  in-flight streams): `docs/adr/*` (13 modified; `index.md` deleted; `README.md` new),
  `docs/getting-started.md`, `docs/index.md`, `docs/ergo-results/3r6ley-result.md`,
  `rust/crates/facade/degenbot/examples/generate_aave_config_windows.rs`,
  `rust/crates/integrations/degenbot-aave/tests/wave5_config_span_replay.rs`,
  `tests/fixtures/cassettes/wave5/`, `tests/fixtures/sql_goldens/wave5/`. All left alone.
- The prior worker's `bot/{pool.rs,mod.rs,engine/mod.rs,engine/solve.rs}` changes are the accepted
  PC1 structural diff; this worker added only the prose reverts on top.

No command detached past the harness bound; every gate ran in the foreground.
