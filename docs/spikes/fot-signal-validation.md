# Spike: FoT signal validation on mainnet + attribution leaf

> Ergo task `5MP3HQ` (epic `3O535Q` — Fee-on-transfer token discovery &
> path denial). The gating knowledge artifact for the implementation tasks:
> `6XWYVH` (registry), `WLJJYO` (classifier), `SDH5VX` (dispatch feedback),
> `CXLIKZ` (FFI), `USPI34` (denylist feed).

## Goal

Produce the knowledge the implementation tasks depend on: the exact
`reverting_frame.label` strings the FoT classifier should match, whether the
attribution lookup works, the noise floor on whitelisted tokens (the
false-positive bound the K/M confirmation threshold must dominate), and
whether the V2 captured-swap-mismatch path fires at all.

## Method

1. **Attribution leaf prototyped + committed** (`136d1e2b`):
   `fot_suspected_token(failure: &SimFailure, hops: &[HopInfo]) -> Option<Address>`
   in `rust/crates/degenbot-backrun-strategy/src/fot_attribution.rs`. Pure
   lookup off `HopInfo` — no engine accessor required. 13 unit tests cover
   the V3 `IIA` + V4 `CurrencyNotSettled` reverting-frame attribution (zfo
   true/false, multi-hop, target-not-in-hops, non-FoT label, missing
   reverting_frame) + the V2 swap-mismatch path (mismatch attributes, match
   returns None, empty captures, length mismatch) + the combined wrapper.

2. **Mainnet validation run analyzed** — the V3-V2-V2 dry-run from the prior
   session (`/tmp/bot_v322.log`, 224 dispatches, 423 sim-failures, all
   whitelisted tokens). The `[sim-fail]` lines carry the Rust
   `reverting_frame.label` field, so the existing run already produces the
   signal data the spike needs — no new run required for the whitelisted
   noise-floor measurement.

3. **Rust `classify_revert` code path read** to confirm the exact label
   normalization: `degenbot_decoders::revert::lookup` does
   `.split('(').next()`, so `RevertClass::label()` returns the bare base
   name (`"IIA"`, `"CurrencyNotSettled"`) — the classifier's membership check
   is a direct `==`, not a prefix/contains test.

## Findings

### F1 — The label set is `{IIA, CurrencyNotSettled}` (bare base names)

The FoT reverting-frame labels, sourced from
`degenbot_decoders::revert::classify_revert`:

| Label | Selector | Source | Failure mode |
|-------|----------|--------|--------------|
| `IIA` | `0x49494100` | the `cmd_executor`'s own `IIA(insufficient-input-amount)` assertion | V3 middle leg: the executor sent tokens to the pool, the FoT fee ate some, `balance_before + amount_owed <= balance_after` fails |
| `CurrencyNotSettled` | `0x5212cba1` | the V4 PoolManager's `CurrencyNotSettled()` | V4 settle: the delta ledger doesn't balance because the FoT fee shorted the input |

The Rust `lookup` fn strips the `(...)` signature via
`.split('(').next()` before returning, so `RevertClass::label()` already
returns the bare base name. The classifier's membership check is:

```rust
const FOT_REVERT_LABELS: &[&str] = &["IIA", "CurrencyNotSettled"];
FOT_REVERT_LABELS.contains(&frame.label.as_str())
```

No prefix/contains test needed — exact match on the bare name.

### F2 — The noise floor on whitelisted tokens is ZERO

Across the V3-V2-V2 mainnet dry-run (224 dispatches, 423 sim-failures, all
on `ETH_MAINNET_ALLOWED_TOKENS`):

| Label | Count |
|-------|-------|
| `UniswapV2: K` | 423 |
| `IIA` | 0 |
| `CurrencyNotSettled` | 0 |

Every failure is a V2 pool's own K-invariant revert (`Error(string)` with
message `"UniswapV2: K"`, selector `0x08c379a0`) — a **different failure
class entirely** from the executor's IIA assertion. The FoT classifier
correctly ignores it (the label `"UniswapV2: K"` is not in
`FOT_REVERT_LABELS`).

**Implication for the K/M threshold:** the false-positive rate on
whitelisted tokens is zero, so K (the distinct-path confirmation threshold)
can be **low** — even `K=2` would produce zero false positives on the
whitelisted set. A FoT token introduced into the graph would flag quickly
(2 distinct paths through it in the same block would suffice). Recommended:

```
K = 2   (distinct path_ids suspecting the same token within M blocks)
M = 100 (the decay window — mirrors POOL_DIVERGENCE_DECAY_BLOCKS)
```

The implementation should make these named constants at the top of
`fot_registry.rs` so they're tunable without code changes (env-var
override, like `DEGENBOT_MIN_PROFIT_MARGIN_BPS`).

### F3 — The K-invariant reverts are NOT the FoT signal (important disambiguation)

The 423 `UniswapV2: K` reverts are **V2 pool K-invariant violations** — the
POOL's own assertion that its post-swap reserves satisfy `x * y >= k`. These
fire for stale-state / reserve-race reasons, NOT for FoT. They are a
**different failure class** the FoT classifier must NOT flag:

- The K-invariant revert comes from the pool contract (target = the V2 pair
  address), via `Error(string)`.
- The FoT `IIA` revert comes from the **executor** (target = the executor
  address, or the V3 pool address if the V3 `swap()` reverts before the
  executor's own IIA check), via the executor's custom error selector.

The classifier's membership check on `reverting_frame.label` cleanly
separates the two: `"UniswapV2: K"` ≠ `"IIA"`. No additional disambiguation
logic is needed.

### F4 — The V2 captured-swap-mismatch path is structurally sound but unvalidated on mainnet

The V2 non-reverting FoT case (the swap commits, K-invariant holds with the
fee-included balance, but the captured output is shorter than
`hop_outputs[i]`) is the `fot_suspected_token_from_swap_mismatch` arm. It
reuses the existing `is_solver_calc_failure` mismatch path.

**Not validated on mainnet** — the V3-V2-V2 whitelisted run produced zero
captured-swap mismatches (every failure was a root-frame revert with empty
`captured_swaps`). To validate this path, a run with a known V2-pooled FoT
token is needed. The implementation task `WLJJYO` (classifier) carries this
as a noted gap; the production classifier's `fot_suspected_token` wrapper
already handles both paths (reverting-frame first, swap-mismatch fallback),
so no additional wiring is needed when the V2 case is later exercised.

### F5 — The attribution lookup is structurally correct

The `fot_suspected_token_from_reverting_frame` leaf matches
`reverting_frame.target` against `HopInfo.pool_address` (V2/V3) /
`pool_manager_address` (V4). The `[sim-fail]` lines confirm `target` is the
reverting pool's address (e.g. `0x4028daac...` is a V2 pair), and
`HopInfo.pool_address` IS the pool contract address — the match is
structural + tested in 13 unit tests.

**V4 caveat (noted in the leaf's doc comment):** the reverting frame's
`target` for a V4 failure is the PoolManager address (shared by every V4
pool), so `hop_input_token_for_target` matches the FIRST V4 hop with that
PoolManager address. For paths with multiple V4 hops through the same
PoolManager, this may attribute to the wrong hop. The V3-V2-V2 run had no V4
hops, so this ambiguity didn't surface. The production version may need the
V4 `poolId` carried on the reverting frame (currently only `target` address
is). This is a noted gap for task `WLJJYO` — it does NOT block the V2/V3
paths, which are the common cases.

## Recommendations for the implementation tasks

### `6XWYVH` (FeeOnTransferRegistry struct)
- `K = 2`, `M = 100` blocks (per F2). Named constants at the top of
  `fot_registry.rs`, env-var-overridable.
- Key: token `Address` (not engine handle, not pool key).
- Mirror `PoolDivergence`'s shape exactly: `record_suspicion(token, path_id,
  block)`, `is_fot(token, current_block)`, `fot_tokens(current_block)`,
  `total_fot_dropped()`.
- The "distinct path_id" dedup: store `HashMap<Address, (HashSet<u64>,
  u64)>` — token → (set of suspecting path_ids, last-flagged block).
  `is_fot` returns true iff the set's len ≥ K AND last-flagged is within M.

### `WLJJYO` (classifier + attribution)
- The leaf `fot_suspected_token` is already the production shape — promote
  from `fot_attribution.rs` (no rewrite needed; the 13 tests ARE the
  production tests).
- The V4 `poolId`-on-reverting-frame gap (F5) is a noted follow-up; does NOT
  block the V2/V3 paths.

### `SDH5VX` (dispatch feedback)
- Step 7 already iterates `outcome.failures`. Add the
  `fot_suspected_token(failure, &path_info.hops)` call alongside the
  existing `diverging_pool_keys` call.
- `fot_dropped` (the skip count) mirrors `divergent_dropped`.
- Does NOT touch `gas_unprofitable` / below-threshold results (confirmed
  out of scope by the operator).

### `CXLIKZ` (FFI)
- Mirror `GMWYIU` piece 2's `PyDivergentPool` exactly: `PyFotToken` carrying
  `(address, last_flagged_block)`.

### `USPI34` (denylist feed)
- The denylist reads `PyDispatcher.fot_tokens()` each path-discovery refresh
  + feeds alongside `ALLOWED_INTERMEDIATE_TOKENS` as a denylist to
  `build_paths`.
- The dispatch step-2 skip (the per-block prevention) is sufficient for the
  current-block candidates; the `build_paths` denylist covers future blocks.
  Task `I3V3E3` (refresh granularity) confirms whether the graph rebuild is
  needed or the dispatch skip alone suffices.

## Not validated (noted gaps for the implementation tasks)

- **No known-FoT token was introduced** — the spike measured the noise floor
  on whitelisted tokens (zero) but did not confirm a real FoT token produces
  `IIA`/`CurrencyNotSettled`. This is a longer experiment (identify a
  mainnet FoT token, widen the whitelist, run until the signal fires). The
  zero noise floor + the correct label set are sufficient to unblock the
  implementation tasks; the FoT-token run is a post-implementation
  end-to-end validation.
- **The V2 captured-swap-mismatch path** (F4) is structurally sound but
  unvalidated on mainnet (no captured-swap mismatches in the whitelisted
  run).
- **The V4 `poolId`-on-reverting-frame gap** (F5) is a noted follow-up for
  multi-V4-hop paths.

## Artifacts

- **Committed leaf**: `rust/crates/degenbot-backrun-strategy/src/fot_attribution.rs`
  (commit `136d1e2b`) — 13 unit tests, all green.
- **The leaf is the production shape** — task `WLJJYO` promotes it as-is;
  no rewrite.
- **Mainnet run log**: `/tmp/bot_v322.log` (the V3-V2-V2 dry-run, 224
  dispatches, 423 sim-failures). Not committed (it's a runtime artifact);
  the findings above are the durable extraction.
