# QNGZVR (PC4, epic GZPPS3): the Curve companion thin modules die; the prefetch becomes a typed outcome

STATUS: **PC4 structural work STANDS.** Three single-consumer modules deleted; the construction-time
prefetch is now a typed, deliberately-ignored fact; the metapool zero-balance refusal moved
core-side. The manager's whole-workspace ladder is PENDING (not mine to run). The required negative
probe is recorded below with its deliberate flip.

Baseline cited: `docs/ergo-results/hqylek-result.md` run 2 (green), plus landed chunks
`937153a29`, `4a03f5e04`, `231334a8f`.

---

## 1. What changed (from the diff, read by this worker)

### Deleted modules (Acceptance: `ls src/degenbot/curve`)

| module | lines | disposition |
|---|---|---|
| `src/degenbot/curve/per_block_cache.py` | **−173** | inlined into `CurveStableswapPool` (see §3) |
| `src/degenbot/curve/strategies.py` | **−43** | `PoolStrategies` folded onto `stableswap_pool_state.py` |
| `src/degenbot/curve/dy/__init__.py` | **−16** | deleted; its only importers were parity tests |

`ls src/degenbot/curve` now shows no `per_block_cache.py`, `strategies.py`, or `dy/`.

### Survivors' new homes

- `get_cached_block_timestamp` → **`CurveStableswapPool._get_cached_block_timestamp`**
  (`src/degenbot/curve/curve_stableswap_liquidity_pool.py`), used by `_a()`.
- `get_cached_virtual_price` → **`CurveStableswapPool._get_cached_virtual_price`**, used by the
  construction prefetch; the metapool `_vp_rate_ro()` base-cache expiry is inlined there. The two
  backing `BoundedCache`s are direct pool fields (`_cache_block_timestamps`, `_cache_virtual_price`).
  `BASE_CACHE_EXPIRES` moved back onto the pool as a class constant.
- `PoolStrategies` (the frozen dataclass) → **`src/degenbot/curve/stableswap_pool_state.py`** (its
  two readers were the state mixin and the pool; the state module is the cycle-free home). Import
  sites updated: `curve_stableswap_liquidity_pool.py`, `tests/helpers/curve_pool_factory.py`,
  `tests/curve/test_curve_onchain_parity.py`.

### `suppress` removed (Acceptance: `rg "suppress" src/degenbot/curve` empty)

- The prefetch's `contextlib.suppress(Exception)` is gone (typed outcome, §4).
- `per_block_cache.py`'s four `contextlib.suppress(KeyError)` cache probes became `.get(...)` /
  the inlined accessors.
- `trackers.py`'s one surviving `contextlib.suppress(KeyError)` (`get_pool`) became a plain
  `try: ... except KeyError: pass` so the whole `src/degenbot/curve` tree is `suppress`-free.
  `rg -n suppress src/degenbot/curve` → no matches.

### Curve `dy` seam

`tests/standalone_parity/test_curve_swap_dual_driver.py` now reaches the PyO3 seam directly:
`from degenbot._ffi.curve_dy import DyCalculationInputs, calculate_dy` (docstring updated). This is
the plan's settled shape ("more honest for a seam with no production consumer"). **FFI-barrier
check (constraint 1):** `tests/test_ffi_boundary.py` scans only `src/degenbot/` and skips every
`__init__.py`, so test-file `_ffi` imports are outside its scope — and the sibling imports in that
same directory already use that form (`from degenbot._ffi.provider import AlloyProvider`,
`from degenbot._ffi.simulation import ...`). The gate was run green anyway (§5). No gate logic was
weakened and no leaf exception was added. The Rust facade doc comment
(`rust/crates/facade/degenbot/tests/parity_curve_swap.rs`) was updated to the same path.

---

## 2. Counts delta

| # | shrink target | before | after | delta |
|---|---|---|---|---|
| 1 | `src/degenbot/curve/per_block_cache.py` | 173 | 0 | **−173** |
| 2 | `src/degenbot/curve/strategies.py` | 43 | 0 | **−43** |
| 3 | `src/degenbot/curve/dy/__init__.py` | 16 | 0 | **−16** |
| 4 | `_get_dy_metapool_with_base` pre-validation (TYPE_CHECKING asserts + 8 `NoLiquidity` raises + the TODO) | 33 lines | 0 | **−33** |
| 5 | `contextlib.suppress` sites under `src/degenbot/curve` | 6 | 0 | **−6** |
| 6 | `get_cached_*` accessors reachable | 4 | 2 | **−2** |

`_get_dy_metapool_with_base` body: **66 → 39 lines** (the docstring grew by the 3-line core-ownership
note). `tests/curve/test_per_block_cache.py` (−129) was replaced by a rewritten
`tests/curve/test_curve_per_block_cache.py` that exercises the inlined pool accessors (including the
metapool expiry cases and the negative probe).

---

## 3. Method: inlining `PerBlockCache` (settled shape)

`_get_cached_block_timestamp` keeps the try-cache → provider → store pattern. `_get_cached_virtual_price`
inlines the metapool expiry: a plain pool fetches `virtual_price` directly; a metapool reads
`base_cache_updated`, the block timestamp, and either the live `virtual_price` (expired) or the
cached base pool's `base_virtual_price` (fresh). The intermediate `base_cache_updated` /
`base_virtual_price` caches were dropped — the final per-block `virtual_price` is cached, so the
branch runs at most once per block (the pre-validation read of the intermediates was not a
reachable-value contract).

---

## 4. The typed prefetch outcome — taxonomy (constraint 2)

Before: `with contextlib.suppress(Exception): self._cache.get_cached_virtual_price(...)` swallowed
**any** construction-time failure. Now:

- **Deliberately-ignored typed fact — `MissingCurveData`.** A provider-less pool is legal (the Rust
  core resolves the virtual price at calc time), so the metapool warm-up catch is scoped to the
  typed `MissingCurveData` and the ignored fact is recorded via `logger.debug`. Note the
  `_HandleCurveDataProviderAdapter` / `fetch_curve_*` seam collapses provider fetch errors to
  `None` → `MissingCurveData`, so both "no provider stored" and "provider has no value" land in the
  ignored class (unchanged reachability vs. the old broad catch).
- **Loud — everything else.** Any non-`MissingCurveData` failure at construction (a plumbing bug,
  a wrong type) now aborts loudly. This is the GLOSSARY loud-abort rule applied to the one case the
  old code could not distinguish: only the typed missing-data fact is silent.

The negative probe is `test_missing_provider_at_construction_is_typed_and_ignored` (§6).

---

## 5. The NoLiquidity contract moves core-side (constraint 3)

- **Rust (`degenbot-pools`):** `check_metapool_liquidity(meta_balances, base_balances) -> Result<(),
  CurveInputsError>` in `curve_dy_io.rs`; new `CurveInputsError::NoLiquidity` variant.
- **Rust (`degenbot-substrate`):** `BotState::curve_get_dy_underlying` calls it right after the base
  pool id resolves — where the balance vectors (`state.balances`, the base `CurvePoolState.balances`)
  are read, before the provider is needed. `curve_error_into_swap` gained the exhaustive arm.
- **PyO3 seam (`degenbot_rs`):** `PyLiquidityPool::curve_get_dy_underlying` maps
  `CurveInputsError::NoLiquidity` to the Python-owned
  `degenbot.exceptions.arbitrage.NoLiquidity(message=...)` (mirrors the existing
  `pathfinding/mod.rs::direction_resolution_error` pattern), so the **Python-visible class is
  preserved** — `NoLiquidity`, not a `ValueError`, and therefore not the `EVMRevertError` the
  companion's `except ValueError` would synthesize.
- **Python:** the 33-line pre-validation in `_get_dy_metapool_with_base` is gone; it now only maps
  the combined underlying indices and delegates to `_get_dy_underlying`. The plain-pool
  zero-balance check in `calculate_tokens_out_from_tokens_in` (`any(balance == 0)`) is untouched —
  it is a different path (both tokens in this pool) and out of this chunk's scope.

Documented shape: the `NoLiquidity` message string collapses to the contract's generic
"One or more of the tokens has a zero balance." The pre-existing per-token messages
(`{token} has a zero balance.`) were unreachable — the `any(balance == 0)` guard above them always
raised first — so no reachable behaviour changed. No test in `tests/curve` asserts a `NoLiquidity`
message or a narrow `NoLiquidity` raise (the suite catches it only inside broad
`except (..., NoLiquidity, ...)` tuples).

---

## 6. Negative-probe record (red path shown once, reverted byte-for-byte)

Probe: `tests/curve/test_curve_per_block_cache.py::TestConstructionPrefetchOutcome::test_missing_provider_at_construction_is_typed_and_ignored`
— **fault-injects a missing provider at construction** (a metapool built with `data_provider=None`
and `state_block=12345`, so the prefetch runs).

Green (exit captured directly):

```
uv run --no-sync pytest "tests/curve/test_curve_per_block_cache.py::TestConstructionPrefetchOutcome::test_missing_provider_at_construction_is_typed_and_ignored" -q -p no:randomly; echo PROBE_GREEN_EXIT=$?
# 1 passed in 3.20s
PROBE_GREEN_EXIT=0
```

The construction succeeded (the pool instance exists) and the captured log shows the deliberately
ignored typed fact was **observed**:

```
DEBUG degenbot.logging:curve_stableswap_liquidity_pool.py:392 Curve metapool 0x...:
  base-cache virtual price not prefetched: virtual_price requires a data_provider. ...
```

Flip — the expected exception is changed to `ValueError` (proving the typed class is load-bearing):

```
with pytest.raises(ValueError):  # FLIPPED PROBE: must fail
    lp._get_cached_virtual_price(12_345)
# E  degenbot.exceptions.pool.MissingCurveData: virtual_price requires a data_provider. ...
# 1 failed in 3.51s
PROBE_FLIP_EXIT=1
```

Reverted byte-for-byte; `rg FLIPPED tests/curve/test_curve_per_block_cache.py` → no matches, and the
probe is green again (`PROBE_REVERTED_EXIT=0`).

Rust twins (in the touched-crate suite, all green): `metapool_liquidity_rejects_a_zero_metapool_balance`,
`metapool_liquidity_rejects_a_zero_base_balance`, and the end-to-end
`curve_get_dy_underlying_refuses_a_zero_balance_metapool`.

---

## 7. Scoped gates (foreground; exits captured directly — no pipes on the captures)

| gate | command | result |
|---|---|---|
| Rust (touched crates) | `just test-rust-nextest -p degenbot-pools -p degenbot-substrate -p degenbot_rs` | **GREEN** — `4721 tests run: 4721 passed, 29 skipped`, `RTEST_EXIT=0` |
| Python (scoped + FFI gate) | `uv run --no-sync pytest tests/curve tests/standalone_parity tests/test_ffi_boundary.py -q -p no:randomly` | **GREEN** — `80 passed in 3.34s`, `PYTEST_EXIT=0` |
| Negative probe green | see §6 | `PROBE_GREEN_EXIT=0` |
| Negative probe flip | see §6 | **RED as designed** — real `MissingCurveData`, `PROBE_FLIP_EXIT=1`; reverted |
| Build receipt | `just rebuild-if-stale` | **GREEN** — `2200 (158882d16a408367)`, `extension is fresh`, `REBUILD_EXIT=0` |
| Editable rebuild | `just dev` | **GREEN** — `DEV_EXIT=0` |
| **rustfmt authority** | `rustfmt --check --edition 2021 --config skip_children=true -- <8 touched .rs>` | **GREEN** — `RUSTFMT_CHECK_EXIT=0` |
| **ruff format authority** | `uv run --no-sync ruff format --check -- <8 touched .py>` | **GREEN** — `8 files already formatted`, `FMT_EXIT=0` |
| ruff check (touched src) | `uv run --no-sync ruff check src/degenbot/curve/{curve_stableswap_liquidity_pool,stableswap_pool_state,trackers}.py` | **GREEN** — `All checks passed!`, `SRC_CHECK_EXIT=0` |

The Rust gate ran the whole workspace because `just test-rust-nextest` retains `--workspace` — every
crate's suite executed, not a slice. My four new Rust tests are present and PASS in the log.

Note (not introduced by this chunk): `ruff check` on the touched *test* file also reports a
pre-existing `import-outside-top-level` at `test_curve_swap_dual_driver.py:83` (a nested
`from degenbot._ffi import Bot`). It fails identically at `HEAD` (verified in a clean worktree), it
is outside the canonical Python lint gate (`ruff check src/ examples/`), and the `tests/*`
per-file-ignore still lists the stale rule name `module-import-not-at-top-of-file`. Left as-is.

---

## 8. Manager review — PENDING

**PENDING (manager's to run — not this worker's):** the whole-workspace ladder — `just test` /
`just test-rust-nextest` / `just test-python` and `just fmt-check`. This worker ran the scoped gates
in §7 only. The manager re-runs every PENDING gate.

---

## 9. Tree / foreign edits

- **Written by this chunk:** `src/degenbot/curve/curve_stableswap_liquidity_pool.py`,
  `src/degenbot/curve/stableswap_pool_state.py`, `src/degenbot/curve/trackers.py` (modified);
  deleted `src/degenbot/curve/per_block_cache.py`, `src/degenbot/curve/strategies.py`,
  `src/degenbot/curve/dy/__init__.py`, `tests/curve/test_per_block_cache.py`; new
  `tests/curve/test_curve_per_block_cache.py`; `tests/curve/test_curve_onchain_parity.py`,
  `tests/curve/test_curve_stableswap_pool.py`, `tests/helpers/curve_pool_factory.py`,
  `tests/standalone_parity/test_curve_swap_dual_driver.py` (modified);
  `rust/crates/foundation/degenbot-pools/src/{curve_dy_io.rs,curve_dy_io/tests.rs,lib.rs}`,
  `rust/crates/foundation/degenbot-substrate/src/{lib.rs,balance_vector_orchestration.rs,tests/registration.rs}`,
  `rust/crates/shells/degenbot-python/src/bot/pool.rs`,
  `rust/crates/facade/degenbot/tests/parity_curve_swap.rs` (modified); and this file. No
  `.ergo/plans.jsonl` entry, no ergo task-state change, no commit. The probe flip was reverted
  byte-for-byte (`rg FLIPPED` → no matches).
- **Foreign activity observed:** none this run — `git status --short` listed only this chunk's
  files (the docs/wave-5 stream named in the brief is not present in the working tree). Nothing
  foreign was touched, staged, or reverted.
- No command detached past the harness bound; every gate ran in the foreground. No
  `.cargo-build-lock` collision occurred.

---

## Manager review — verdict PASS (2026-10-10)

Adversarial review against the diff: deletions verified live (`ls src/degenbot/curve` —
`per_block_cache.py`/`strategies.py`/`dy/` gone; `rg suppress src/degenbot/curve` empty; trackers'
remaining catch is a narrow `KeyError`); the NoLiquidity contract is core-side
(`degenbot-pools::curve_dy_io::check_metapool_liquidity` -> `CurveInputsError::NoLiquidity`, called
from `balance_vector_orchestration.rs` where balances are read) with the Python class preserved
(`degenbot.exceptions.arbitrage.NoLiquidity`); the `dy` importer now routes per the boundary gate's
actual scope (tests are out of it — verified, no gate logic touched).

Manager battery: run 4a — `just pre-push` **exit=101** at `lint-rust-check` (clippy
`similar_names`: `meta2_id` vs `meta_id` in the new substrate test) — the worker's scoped gates ran
tests, not Clippy; the manager ladder caught it. Fix round renamed to `meta_pool_b_id` (no lint
suppression). Run 4b (`/tmp/pc4-gates2/`): `just pre-push` **exit=0** + `just gen-stubs --check`
**exit=0**. Both fmt authorities verified green pre-commit. Green against the PC0 baseline (run 2).

Scope note carried forward: the DIRECT-pool zero-balance `raise NoLiquidity` in
`calculate_tokens_out_from_tokens_in` remains Python-side (outside this task's named scope — the
metapool contract moved as specified). If PC5 collapses the construction residue, consider whether
the direct-path contract belongs core-side too.

## 10. Fix round (manager finding)

The manager's whole-workspace ladder was RED on Clippy — `just lint-rust-check` failed to compile
`degenbot-substrate` (lib test) on `clippy::similar_names` (denied via `-D warnings`):

```
error: binding's name is too similar to existing binding
   --> crates/foundation/degenbot-substrate/src/tests/registration.rs:191:9
191 |     let meta2_id = core.register_curve_pool(&params(...));
    |         ^^^^^^^^
note: existing binding defined here
   --> crates/foundation/degenbot-substrate/src/tests/registration.rs:180:9
180 |     let meta_id = core.register_curve_pool(&meta);
    |         ^^^^^^^
```

Rename (and its one use) in
`rust/crates/foundation/degenbot-substrate/src/tests/registration.rs`, test
`curve_get_dy_underlying_refuses_a_zero_balance_metapool`:

- `let meta2_id = ...` → `let meta_pool_b_id = ...` (line 191)
- `.curve_get_dy_underlying(meta2_id, ...)` → `.curve_get_dy_underlying(meta_pool_b_id, ...)` (line 194)

No `#[allow]`/`#[expect]` added; lint configuration untouched. No other code changes.

Gates (foreground; exits captured directly — no pipes on the captures):

| gate | command | result |
|---|---|---|
| Clippy ladder | `just lint-rust-check; echo $?` | **GREEN** — `0`, finished clean |
| Rust nextest (crate) | `just test-rust-nextest -p degenbot-substrate; echo $?` | **GREEN** — `4721 tests run: 4721 passed, 29 skipped`, `0` |
| rustfmt authority | `rustfmt --check --edition 2021 --config skip_children=true -- rust/crates/foundation/degenbot-substrate/src/tests/registration.rs; echo $?` | **GREEN** — `0` |

Tree hygiene: the fix round touched only `registration.rs` and this file. No PENDING; no command
detached; no `.cargo-build-lock` collision. No foreign edits observed or touched.
