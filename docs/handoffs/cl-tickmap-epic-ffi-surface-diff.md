# CL tick-map epic — net FFI surface diff (T4 review, epic OU4SYZ)

Base: `1491947fe` (pre-epic). HEAD: after T1 (`3WTDFK`), T2 (`FBJTUM`), T3 (`OMDCIY`).
Scope: `LiquidityPool` (degenbot-python/src/bot/pool.rs) + registration FFI
(degenbot-python/src/bot/{mod.rs, engine/register.rs}) + the `.pyi` stub
(enforced by `tests/rust/test_ffi_stub_drift.py` — 78 passed, 1 skipped).

## Added

| Entry | Kind | Task | Behavior |
|---|---|---|---|
| `LiquidityPool.ensure_word_known(word: int, block: int) -> bool` | method | T2 | Write-path sparse backfill: calls the state's RUST-stored tick-word fetcher for `word` at `block`, merges via the same core `merge_tick_word` routine the sim loop uses (ticks overlay, word marked known, cache invalidated; checked-empty included). `False` when no fetcher is stored or the fetch failed — the companion gate then RAISES. |
| `LiquidityPool.coverage -> str \| None` | getter/property | T2 | `"sparse"` / `"tracked"` for a registered V3/V4 pool, `None` otherwise. Rust coverage is the fact the companion reads (the double-tracked Python sparseness flag is retired). |

No `Bot`-level and no registration SIGNATURE additions (the `tick_data_fetcher`
registration parameter predates the epic, task MLJT4V).

## Removed

Nothing at the FFI level. Python-companion removals (not FFI, listed for the
consumer-affecting net): the `_bitmap_override` shadow (T1), the
`_sparse_liquidity_map` / `_tick_data_fetcher` flags + `_apply_fetched_tick_word`
(T2). `sparse_liquidity_map` the PROPERTY remains on both families — now
computed live from `coverage`.

## Changed (identical signatures, new contract)

- `LiquidityPool.update_tick_data(tick_bitmap, tick_data, block)` (T1): the
  `tick_bitmap` KEYS are the checked words — recorded Rust-side in
  `known_bitmap_words` for Sparse pools; VALUES not stored (bitmap derives from
  rows); Tracked pools record nothing. Wheel consumers: no call-site change
  (signature unchanged); semantics = checked words now survive as
  present-but-zero in `tick_bitmap_snapshot()`.
- `LiquidityPool.tick_bitmap_snapshot()` (T1): Sparse pools surface
  checked-but-empty words as `(0, tick_data_block)` (the fetch loop breaks);
  Tracked pools return the pure derivation.
- V3/V4 registration intake (T3): an internally inconsistent Tracked DB
  snapshot (bitmap bit vs gross>0 row disagreement) is rejected with a typed
  `PoolBuilderError::TickAssembly(InconsistentTickMap {word, bit, tick,
  bitmap_bit, row_gross_positive})` → `ValueError` naming the conflict. Async
  builder (`build_v3`/`build_v4`) and sync assembly (`assemble_v3/v4_tick_map`)
  share the gate. Consumers with healthy snapshots: no behavior change.
- `BotState::ensure_word_known_by_pool_id` (core, T2): the BotState method
  backing the FFI method — same contract.

## Rust-core consumer net (no-Python path)

- `degenbot-bot`: `BotState::ensure_word_known_by_pool_id` (4 tests);
  `TickMapAssemblyError::InconsistentTickMap` + `verify_tracked_tick_map` in
  `bot_core::tick_assembly` (spacing threaded through V3/V4 Db arms, sync +
  async builder).
- `degenbot-pools`: `ConcentratedLiquidityPoolMut::merge_tick_word` /
  `mark_bitmap_words_known` (T1) unchanged; flip-equivalence pin test added to
  `tick_bitmap::tests` (T2 RED test 3).
- no-pyo3-in-cores gate: OK (FFI additions live in degenbot-python only).

## Doc/.pyi alignment

- `.pyi` stubs updated for both additions; pyi drift gate green (78 passed).
- FFI doc comments on `ensure_word_known`/`coverage` state the contract
  (checked-empty semantics, False-on-failure → caller RAISES, Rust coverage is
  the fact).
- CONTEXT.md glossary: `Known bitmap word` and `Checked-empty word` entries
  present.
- Stale-reference sweep: no live references to `_bitmap_override` /
  `_tick_data_fetcher`; companion docstrings are forward-looking (T1-era
  "cache is retired (T1 3WTDFK)" phrasing removed in T4).

## Gate summary (`just test` / `just lint` / `just dead-code` /
`just check-no-pyo3-in-cores`)

All four green from the committed tree (see T4 task result).
