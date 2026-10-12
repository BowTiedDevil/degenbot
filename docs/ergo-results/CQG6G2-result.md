# CQG6G2 — P1: tracker family removal

Agent: `p1-tracker-removal` (dispatched worker). Tree left dirty; nothing committed.

## What changed (files + intent)

### Deleted whole files (6)
- `src/degenbot/uniswap/trackers.py`
- `src/degenbot/aerodrome/trackers.py`
- `src/degenbot/sushiswap/trackers.py`
- `src/degenbot/pancakeswap/trackers.py`
- `src/degenbot/curve/trackers.py`
- `src/degenbot/types/abstract/pool_tracker.py`

`DeploymentOverrides` lived only in `uniswap/trackers.py`; grep found no importer
outside the dying family + the by-tokens test, so it was deleted with the file
(no survivor).

### Sibling exports
- uniswap/aerodrome/curve/sushiswap/pancakeswap `__init__.py`: tracker imports and
  `__all__` entries removed (sushiswap `__all__` is now empty; docstrings de-trackered).
- `types/abstract/__init__.py`: `AbstractPoolTracker` import + `__all__` entry removed.
- `src/degenbot/__init__.py`: the 6 tracker names removed from imports and `__all__`.

### Bot integration
- `bot/_bot.py`: `add_tracker` method, the `self._trackers` dict, the
  `TrackerAlreadyInitialized` import, the `AbstractPoolTracker` TYPE_CHECKING import,
  and the tracker-cycle import-order comment removed; `release_python_state` docstring
  updated.
- `bot_lifecycle.py`: `_trackers` protocol member, the tracker-cache loop in
  `release_python_state`, `bot._trackers = None` in `close`, and tracker mentions in
  docstrings/comments removed.

### Exceptions
- Removed `TrackerAlreadyInitialized`. Deleting the trackers orphaned the whole
  family, so `TrackerError` and its other subclasses `PoolNotAssociated` /
  `PoolCreationFailed` were removed too (all four had zero non-tracker users —
  grep-verified). Exports in `exceptions/__init__.py` and the "Pool trackers"
  docstring bullet removed.
- Also fixed two PRE-EXISTING ruff violations in `exceptions/pool.py`
  (`MissingLiquidityData` docstring summary/blank-line + a 101-char line) so the
  touched file passes `ruff check`.

### Docs
- `docs/adr/ADR-007-pool-unregister-seam.md`: the `get_pool_from_tokens` snippet
  replaced with the `generate_v2_pool_address` + `Bot.build_pool` composition; the
  tracker-file references in the Context paragraph dropped.
- Stale comment cleanups: `uniswap/deployments.py` lazy-loader comment,
  `camelot/__init__.py` docstring.

## Test migration
- `tests/uniswap/test_uniswap_trackers.py`: rewritten offline. Preserves the
  derive→build→register flow (`generate_v2_pool_address` / `generate_v3_pool_address`
  → `Bot.build_pool` → `Bot.pools`). Includes `test_pool_remove_and_recreate` (the
  ADR-007 reference test name kept). No fork/RPC; runs in the default selection.
- `tests/pool_companion/test_io_free_behavior.py`: deleted `TestV2PoolTrackerWithBot` /
  `TestV3PoolTrackerWithBot` / `TestAerodromeV2PoolTrackerWithBot`; imports pruned.
- `tests/bot/test_close.py`: rewritten off trackers (registry-release postconditions
  via `FakeSessionPool`).
- `tests/bot/test_release_python_state.py`: tracker-specific cases deleted; add_tracker
  usages removed.
- `tests/test_bot.py`: `TestBotAddTracker` + tracker init/independence cases deleted.
- `tests/curve/test_curve_pool_tracker.py`: deleted (100% tracker-specific).
- `tests/registry/test_pool_subclass_selection.py`: reduced to a module-level skip (its
  premise — `SushiswapV2Pool` + tracker subclasses — is gone); kept as a collectible
  skipped placeholder because the scoped gate names this path.
- `tests/uniswap/v3/test_v3_tracker_snapshot_events_golden.py`: tracker replaced with the
  inline snapshot-apply step (`snapshot.pending_updates(...)` →
  `pool.update_liquidity_map`); filename/test-name kept to preserve the golden oracle key.
- `tests/arbitrage/test_pool_construction_route.py`: vestigial `add_tracker` guard removed.

## Gates run
- `rg -n 'PoolTracker|add_tracker|TrackerAlreadyInitialized' src/` → no matches. (AC1)
- `uv run --no-sync python -c "import degenbot; assert not [n for n in degenbot.__all__ if 'Tracker' in n]"` → passes. (AC2)
- `uv run --no-sync ruff check <touched files>` → All checks passed. (AC3)
- `uv run --no-sync pytest tests/pool_companion/ tests/registry/test_pool_subclass_selection.py tests/bot/ tests/test_bot.py tests/curve/ tests/uniswap/test_uniswap_trackers.py tests/arbitrage/test_pool_construction_route.py tests/uniswap/v3/test_v3_tracker_snapshot_events_golden.py -q` → **145 passed, 1 skipped, 6 deselected**. (AC3)
- `uv run --no-sync pytest tests/ --collect-only -q` → **3131/3451 collected, 0 errors** (no import breakage anywhere).

## ExternalUpdate audit (AUDIT ONLY — nothing deleted)
`*PoolExternalUpdate` dataclass CONSTRUCTIONS surviving the change, all in tests/README
(no src constructions exist — src only DEFINES them):
- `tests/pool_companion/test_io_free_construction.py`:341,429,535 (V2/V3/V4)
- `tests/aerodrome/test_aerodrome_pools.py`:107,129,140,149 (AerodromeV2)
- `tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py`:304,364,743,792,804,828,837,929,1003 (V3)
- `tests/uniswap/v2/test_uniswap_v2_liquidity_pool.py`:433,502,756,768,780 (V2)
- `README.md`:277,436 (V2)
None are tracker-owned; all remain valid. Left untouched.

## PENDING
None. All gates ran foreground to completion.

## Foreign tree activity (named, not touched)
Another session is actively editing the Rust tree. Observed while I worked (left alone):
- Modified: `rust/crates/engine/degenbot-solvers/src/{mixed/mod.rs,mixed/solve.rs,profit_envelope/derive.rs,profit_envelope/mod.rs}`, `rust/crates/integrations/degenbot-aave/**` (src/updater/run.rs + tests), `rust/crates/shells/degenbot-cli*/**`, `rust/crates/facade/degenbot/{benches,examples}/**`, `rust/crates/foundation/degenbot-rpc/**`, `rust/crates/shells/degenbot-python/src/aave_updater.rs`.
- Untracked: `rust/crates/engine/degenbot-solvers/src/mixed/solidly_t.rs`.
- `src/degenbot/_ffi/aave.pyi`: regenerated by my `maturin develop` rebuild (reflects the foreign Rust aave edits), not hand-edited.
- The Rust extension was STALE at session start (installed fingerprint `9e4e8dbf` vs repo receipt `2b9c10d0`), which blocks pytest collection (conftest's `verify_build_fresh`). I ran `just rebuild-if-stale`; it compiled the foreign Rust edits and installed a fresh extension (receipt `2325/1e62c624821c29fe`). If the foreign session advances `.build-number` again, re-run the rebuild before the pytest gates.

## Not in scope (left alone, now stale references)
`docs/architecture/rust-settlement-bot-parity.md` (S24 row names `UniswapV3PoolTracker`),
`docs/adr/ADR-064` ("trackers KEPT, deliberately"), ADR-055/ADR-059/ADR-023 tracker
mentions, `docs/ergo-results/*` — all outside the ADR-007-only docs scope.
