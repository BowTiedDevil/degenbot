# Spike: FoT signal validation on mainnet + attribution leaf

> Ergo task `5MP3HQ` (epic `3O535Q` — Fee-on-transfer token discovery &
> path denial). The gating knowledge artifact for the implementation tasks.

## Goal

Confirm — empirically, on mainnet, with a real fee-on-transfer token — that
the FoT signal fires at the expected `reverting_frame.label`, at the expected
call depth, with the expected reverting target. Without this confirmation
the inferred label set (`{IIA, CurrencyNotSettled}`) is an untested
hypothesis, and the entire implementation chain would be built on a
possibly-wrong classifier.

## Method

1. **Attribution leaf prototyped + committed** (`136d1e2b`):
   `fot_suspected_token(failure: &SimFailure, hops: &[HopInfo]) -> Option<Address>`
   in `rust/crates/degenbot-backrun-strategy/src/fot_attribution.rs`.
2. **Mainnet experiment**: added RFI (`0xA1AFFfE3...`), SFM
   (`0xe574c0c3...`), BabyDoge (`0xAC57De9C...`) to the whitelist + ran
   the V3-V2-V2 dry-run (19 dispatches, ~6 blocks).
3. **Analyzed the prior V3-V2-V2 whitelisted run** (224 dispatches, 423
   sim-failures) for the noise floor (F2 in the preliminary findings).

## Findings — REVISED by the mainnet experiment

The preliminary findings (F1-F5, in the first version of this doc) inferred
the label set from the executor's selector table + the Rust `classify_revert`
normalization. The mainnet experiment with a real FoT token **revised F1-F3
and confirmed F4-F5**. The revised findings below supersede the preliminary
ones.

### F1 (REVISED) — The V2 FoT label is `UniswapV2: K`, NOT `IIA`

The inferred label set (`{IIA, CurrencyNotSettled}`) was WRONG for V2-pooled
FoT tokens. The actual mainnet signal:

| Token | Pooled on | Expected label | ACTUAL label | Selector |
|-------|-----------|---------------|--------------|----------|
| RFI | V2 pairs | `IIA` | `UniswapV2: K` | `0x08c379a0` (Error(string) `"UniswapV2: K"`) |

**Why:** the V2 pool's own `swap()` function checks the K-invariant on FINAL
balances and reverts via `Error(string)` BEFORE the executor's own IIA
assertion fires. The executor sends FoT tokens (with the fee shorted) to the
V2 pair → the pair receives less than expected → the fixed-output withdrawal
makes `balance0_final * balance1_final < k` → the pool reverts with
`UniswapV2: K` — the POOL's revert, not the executor's.

The `IIA` / `CurrencyNotSettled` labels are the EXECUTOR's own assertions
(firing for V3/V4-pooled FoT tokens), and were NOT observed because all
three test tokens (RFI, SFM, BabyDoge) are V2-paired. SFM + BabyDoge found
no paths in the run (their V2/V3 pools aren't in the DB snapshot against
whitelisted tokens — only RFI has USDC/WETH pairs).

**Implication:** the `FOT_REVERT_LABELS` constant must include
`"UniswapV2: K"` (the V2 pool's K-invariant revert message). Whether the V3
`IIA` / V4 `CurrencyNotSettled` labels ever fire for V3/V4-pooled FoT tokens
remains unvalidated — no V3/V4 FoT pool was exercised.

### F2 (REVISED) — The noise floor on `UniswapV2: K` is NOT zero

The preliminary finding (zero noise floor) measured only the `IIA` /
`CurrencyNotSettled` labels — which never fired. But `UniswapV2: K` is a
COMMON revert that fires for stale state + thin-margin races + FoT. Across
the prior V3-V2-V2 whitelisted run (224 dispatches, 423 sim-failures),
**every single failure was `UniswapV2: K`** — both FoT (RFI) and non-FoT
(USDC/stETH/CRV paths).

The CRV path (path_id 1669, a non-FoT whitelisted token) reverted 5-6 times
with `UniswapV2: K` at the **SAME V2 pool** (0x3da1313a...) — a stale-state
issue, NOT FoT.

**Implication:** a single `UniswapV2: K` revert cannot classify a token as
FoT. The disambiguation requires a PATTERN, not a label match (see F3
below). The K/M threshold must be about DISTINCT POOLS, not distinct paths.

### F3 (REVISED) — The disambiguation is token-vs-pool persistence, not the label

The experiment revealed the actual disambiguation signal:

| Signal | FoT token (RFI) | Stale-state (CRV path 1669) |
|--------|----------------|-----------------------------|
| Label | `UniswapV2: K` | `UniswapV2: K` (identical) |
| Distinct reverting V2 pools | **2** (0x4c834137... + 0x6fc4819a...) | **1** (0x3da1313a...) |
| Any path succeeding? | **0 successes** across 10 distinct path_ids, 20 attempts | (stale-state paths eventually succeed once state catches up) |

The key differentiator: **RFI fails across 2 DISTINCT V2 pools with 0
successes** (the token fails regardless of which pool — a permanent token
property). The stale-state CRV path reverts at **1 pool** only (the pool's
state was stale, the token is fine).

**The revised classifier design:**
- A `UniswapV2: K` revert records a FoT SUSPICION for the reverting hop's
  input token + the reverting pool address.
- The `FeeOnTransferRegistry` tracks per-token: the SET of distinct failing
  pool addresses + whether ANY path involving the token has ever succeeded.
- `is_fot(token)` returns true iff: the failing-pool set has ≥ K distinct
  pools AND no path involving the token has ever succeeded.
- K = 2 (distinct pools) is the minimum — 2 distinct pools reverting with
  the same token + zero successes is a permanent token property, not a
  single stale pool.

This is a RICHER classifier than the preliminary spike assumed (a pure
label-match on a single failure). The implementation task `WLJJYO`
(classifier) + `6XWYVH` (registry) must be revised to track the
(token → failing-pool-set, success-flag) shape, not the simple
(token → last-flagged-block) shape of `PoolDivergence`.

### F4 (CONFIRMED) — V2 captured-swap-mismatch path is structurally unreachable

The `fot_suspected_token_from_swap_mismatch` arm (the V2 non-reverting case)
was never exercised: every FoT path reverted at the root frame (V2 pool's
K-invariant), so `captured_swaps` was empty for all 20 RFI failures. The V2
pool reverts in its `swap()` call BEFORE any `Swap` event fires, so the
inspector captures nothing.

**Implication:** the V2 captured-swap-mismatch path is DEAD CODE for the
V2 FoT case. The V2 FoT signal is ALWAYS a root-frame revert with empty
`captured_swaps`, classified via `reverting_frame.label == "UniswapV2: K"`.
The `fot_suspected_token_from_swap_mismatch` arm could fire for stale-state
forced-mismatch scenarios (a non-reverting V2 swap whose output differs from
`hop_outputs[i]`), but that's the `PoolDivergence` path, not the FoT path.

Consider removing the swap-mismatch arm from the FoT classifier (it's the
`PoolDivergence` feature's responsibility, not the FoT feature's). The FoT
classifier should be reverting-frame-label-only.

### F5 (CONFIRMED) — The attribution lookup works, with the revised label set

The `fot_suspected_token_from_reverting_frame` leaf correctly attributed
RFI failures to the FoT token:
- `reverting_frame.target` = the V2 pair address (e.g. 0x6fc4819a...)
- The leaf finds the hop whose `pool_address` matches → returns the hop's
  input token (RFI, selected by `zfo`)
- Tested in 13 unit tests (all green)

The V4 `poolId`-on-reverting-frame gap (the PoolManager address is shared
by every V4 pool) remains a noted follow-up for multi-V4-hop paths.

### Additional finding — SFM + BabyDoge found no paths

Only RFI had V2 pairs against whitelisted tokens (USDC/RFI + RFI/WETH) in
the DB snapshot. SFM + BabyDoge found no paths — their pools either aren't
in the DB or don't connect to the whitelisted set. A V3/V4-heavy run with
these tokens would need a different token-set or pool-loading strategy to
exercise the V3 `IIA` / V4 `CurrencyNotSettled` labels. This remains an
unvalidated gap for the V3/V4 FoT case.

## The go/no-go decision: GO (with revised design)

The experiment confirmed:
1. ✅ The FoT signal IS observable on mainnet (RFI → `UniswapV2: K`, 20
   failures, 0 successes, 2 distinct pools).
2. ✅ The attribution leaf works (reverting target → V2 pair → input token).
3. ✅ The K/M threshold is reachable (2 distinct pools failing + 0 successes
   is the confirmation pattern, observable within ~6 blocks / 19 dispatches).

The revised design (from F3):
- `FOT_REVERT_LABELS = ["IIA", "CurrencyNotSettled", "UniswapV2: K"]`
  (broadened from `{IIA, CurrencyNotSettled}` — the `UniswapV2: K` label is
  the actual V2 FoT signal, and the K/M threshold filters the stale-state
  noise).
- The `FeeOnTransferRegistry` tracks `(token → failing-pool-set, success-flag)`
  — NOT the simple `(token → last-flagged-block)` shape of `PoolDivergence`.
  The confirmation is "≥ K distinct failing pools AND 0 successes", not
  "≥ K suspicions within M blocks".
- The `fot_suspected_token_from_swap_mismatch` arm is dead code for the FoT
  case (F4) — consider removing it; the PoolDivergence feature owns that path.

## Revised recommendations for the implementation tasks

### `6XWYVH` (FeeOnTransferRegistry struct)
- Track `HashMap<Address, FotTokenRecord>` where `FotTokenRecord` carries
  `failing_pools: HashSet<Address>` + `has_any_success: bool` +
  `last_flagged_block: u64`.
- `is_fot(token)` returns true iff `failing_pools.len() >= K` AND
  `!has_any_success` AND `last_flagged_block` within the decay window M.
- `record_suspicion(token, pool_address, block)` adds the pool to the
  failing set.
- `record_success(token)` sets `has_any_success = true` (irreversible
  within the decay window — once a token succeeds, it's not FoT).
- K = 2 distinct pools, M = 100 blocks decay (a token with no suspicions
  for 100 blocks clears; but `has_any_success` is sticky within the
  decay window).

### `WLJJYO` (classifier + attribution)
- Update `FOT_REVERT_LABELS` to `["IIA", "CurrencyNotSettled",
  "UniswapV2: K"]`.
- Remove or deprecate `fot_suspected_token_from_swap_mismatch` (F4 — dead
  code for the FoT case; the V2 non-reverting mismatch is PoolDivergence's
  responsibility).
- The classifier returns `Some((token, reverting_pool))` — the registry
  needs the pool address too (for the distinct-pool-set tracking).

### `SDH5VX` (dispatch feedback)
- Step 7 records suspicions: `fot_suspected_token(failure, &hops)` returns
  `Some((token, pool))` → `registry.record_suspicion(token, pool, block)`.
- Step 2 skip: drop paths whose any hop's input token `is_fot(token)`.
- NEW: whenever a path SUCCEEDS, call `registry.record_success(token)` for
  each hop's input token — this is the 0-success disambiguator (a token that
  ever succeeds is not FoT).

### `CXLIKZ` (FFI) + `USPI34` (denylist feed)
- Unchanged — mirror `GMWYIU` piece 2's `PyDivergentPool` for `PyFotToken`.

## Artifacts

- **Committed leaf**: `rust/crates/degenbot-backrun-strategy/src/fot_attribution.rs`
  (commit `136d1e2b`) — 13 unit tests, all green. The leaf needs the label
  set updated + the swap-mismatch arm reconsidered before promotion.
- **Mainnet run log**: `/tmp/bot_fot_v322.log` (19 dispatches, RFI through
  2 distinct V2 pools, 0 successes). Not committed (runtime artifact).
- **Whitelist changes**: reverted (the FoT tokens + margin-floor disable were
  for the experiment only).
