# T26KFM (PC8, epic GZPPS3): swap simulation gets one seam — signed deltas, thresholds core-side

STATUS: **LANDED (implementation + green scoped gates).** The four CL swap-sim
`Pool` pymethods collapse to ONE `simulate` verb; the unsigned->signed mapping
lives once, seam-side, and both red paths are demonstrated (recorded verbatim
below). BASELINE CITED: `docs/ergo-results/hqylek-result.md` run 2; landed
`937153a29, 4a03f5e04, 231334a8f, ad035c48a, 345a8c118, 5de576784, d18c49b8d,
c2f35dafd, e33b2c6c8`.

## What changed

- **`rust/crates/shells/degenbot-python/src/bot/pool.rs`** — the four pymethods
  `simulate_swap_with_fetch`, `simulate_exact_output_swap_with_fetch`,
  `simulate_swap_with_override`, `simulate_exact_output_swap_with_override`
  are replaced by ONE `Pool.simulate(zero_for_one, amount_specified, block,
  sqrt_price_limit_x96=None, override_state=None)`. `amount_specified` is signed
  user-perspective (negative = exact input, positive = exact output). The seam
  returns a typed read dict: `status` in `computed` / `not_computable` /
  `fetch_failed` / `fetch_exhausted`, with signed per-token deltas
  (deposited `+`, sent `-`) and the end state on `computed`.
  `build_simulate_read` is THE sign-mapping site; `extract_signed_amount` reads
  the full `uint256`-range sign; `TickData::from_py_liquidity_rows` reads the
  `{tick: LiquidityAtTick}` mapping off the override state object (the
  `LiquidityAtTick`->row translation now lives once, Rust-side, where the row
  shape is defined).
- **`rust/crates/engine/degenbot-bot/src/bot_core/pool_ops.rs`** — new
  `simulate_mainline` (staged pre-pass + disarmed sim) returning the full typed
  `SwapRead`, so the shell can tell a compute miss from an exhausted fetch.
- **`src/degenbot/uniswap/v3_liquidity_pool.py`** — `simulate_exact_input_swap`
  / `simulate_exact_output_swap` are thin wrappers over one `_simulate_swap`
  that calls `self._py_pool.simulate`; the four hand-written sign-mapping
  branches and the `override_tick_data` flattening are deleted.
- **`src/degenbot/uniswap/v4_liquidity_pool.py`** — the four branches in the two
  `calculate_tokens_*` methods collapse to one `self._py_pool.simulate` call
  each; `_signed_amounts` splits the signed deltas into `(amount_in,
  amount_out)`.
- **Tests** — `test_v3/v4_calc_delegation` spies now record the single
  `simulate` verb; a new `test_rust_seam_sign_mapping_dense` is the ONE
  seam-side sign pin (all four call shapes); the parity suites route through
  the seam.
- **`src/degenbot/_ffi/__init__.pyi`** — regenerated; surface moved.

## Stub-diff defs (surface moved)

Removed: `simulate_exact_output_swap_with_fetch`,
`simulate_exact_output_swap_with_override`, `simulate_swap_with_fetch`,
`simulate_swap_with_override`. Added: `simulate`. Four verbs -> one.

## Counts delta (baseline hqylek run 2)

| # | target | before | after |
|---|---|---|---|
| C6 | `amount0_delta = rust_amount0` copies in `src/degenbot` | 4 | **0** |
| C6' | sign-mapping pin sites (seam-side) | (per-copy) | **1** (`test_rust_seam_sign_mapping_dense`) |
| — | CL swap-sim `Pool` pymethods | 4 | **1** (`simulate`) |
| — | `calculate_tokens_*` method definitions (unchanged verbs) | 16 | 16 |

Acceptance one-liner: `rg "amount0_delta = rust_amount0" src/degenbot` -> empty
(0 lines).

## Red paths (flip once, recorded verbatim, reverted byte-for-byte)

Sign mapping (flipped `input_delta`/`output_delta` signs in `build_simulate_read`,
rebuilt, `test_rust_seam_sign_mapping_dense`):

```
>       assert exact_in_zfo["amount0_delta"] == 1_000
E       assert -1000 == 1000
tests/uniswap/v3/test_v3_calc_delegation.py:185: AssertionError
1 failed
```

Threshold (flipped `<` to `<=` in `v3_pool_calc.py`,
`test_v3_offline.py::TestV3PoolCalculations::test_calculate_tokens_out_from_tokens_in`):

```
>           raise IncompleteSwap(amount_in=amount0_delta, amount_out=-amount1_delta)
E           degenbot.exceptions.pool.IncompleteSwap: Insufficient liquidity to swap for the requested amount.
src/degenbot/uniswap/v3_pool_calc.py:126: IncompleteSwap
1 failed
```

Both flips reverted byte-for-byte; the installed extension rebuilt to a
fingerprint matching the pre-flip source, and the pin re-greened.

## 2-tuple per-tick block decision (PC1 carried note)

Through THIS seam the override hypothetical now carries the **real per-tick
`block`**: `TickData::from_py_liquidity_rows` reads `LiquidityAtTick.block` by
attribute, so the `block` the Python `state` snapshot carries rides in. The
2-tuple `TickData::from_py_pairs` backfill form (engine state-sync seam) is
untouched and still fills `block = 0` from context — it is not on this path.

## Gate exits (captured directly)

- `just rebuild-if-stale` -> EXIT 0 (fresh; receipt 2223 matches sources)
- `just test-rust-nextest` -> EXIT 0 (4726 passed, 29 skipped in 10.5s)
- `just lint-rust-check` -> EXIT 0 (clippy green)
- `just lint-python-check` -> EXIT 0 (ruff + ty green)
- `just gen-stubs --check` -> EXIT 0
- `just lint-comment-hygiene` -> EXIT 0
- `just fmt-check` (rustfmt) -> EXIT 0
- `just fmt-check-python` (ruff format) -> EXIT 0
- `uv run --no-sync pytest tests/uniswap tests/pool_companion tests/test_ffi_boundary.py tests/test_v3_rust_swap_parity.py tests/test_v4_rust_swap_parity.py` -> EXIT 0 (212 passed, 1 skipped)
- `pytest tests/test_ffi_boundary.py` -> EXIT 0 (8 passed)

## Remaining / not done

- The `IncompleteSwap` threshold still lives Python-side (v3_pool_calc, and
  inline in V4). It was NOT folded core-side; the seam already returns
  `specified_met`, so a follow-up can lift the comparison. Flagged for the
  epic's next slice.
- V3/V4 still expose `simulate_exact_input_swap` / `simulate_exact_output_swap`
  as thin public wrappers over the one private verb (public API preserved for
  the calc mixins and existing callers).
- `cl_companion` scalar reads unchanged: the `state` property already takes a
  single `concentrated_liquidity()` crossing.

## PENDING

None.

## Manager review — verdict PASS (2026-10-10)

Verified against the tree: `amount0_delta = rust_amount0` sign-mapping copies in `src/degenbot`
= ZERO; the four simulation verbs are gone (`Pool.simulate` is the one verb); exactly one
sign-mapping pin seam-side (`tests/uniswap/v3/test_v3_calc_delegation.py`); PC1's carried note is
resolved honestly — the override hypothetical now carries the REAL per-tick `block` (read off
`LiquidityAtTick.block`) while the engine state-sync 2-tuple backfill keeps `block = 0` (the retired
`make_tick_info` behaviour), documented as out of this seam's path. Both red paths recorded with
flips + exact exits and reverted byte-for-byte. Manager battery `/tmp/pc8-gates/`: pre-push 0,
gen-stubs 0, comment-hygiene 0.
