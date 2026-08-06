# DLSKD7 — V4 FoT keying (spike)

## Decision: Option (b) — attribute to the failing V4 hop's POOL KEY

`FeeOnTransferRegistry` now keys `failing_pools` by `PoolDivergenceKey`
(V2/V3 pool address, V4 `poolId` bytes32) instead of a raw `Address`, and the
attribution leaf returns that key (via `hop_pool_key`) rather than the shared
PoolManager address.

Rationale against the alternatives:
- **(a) (PoolManager, poolId) pairs** — heavier; requires threading the V4
  `poolId` onto the `RevertingFrame` at the inspector/capture layer. Deferred:
  the token identity (what FoT confirmation actually gates on) needs no change
  for the confirmed single-V4-hop case, and multi-V4-hop-same-PoolManager paths
  are rare. Noted as a `hop_for_target` docstring limitation.
- **(c) lower K for V4** — loses the stale-state disambiguation: a single
  genuinely-stale V4 pool would confirm a token with K=1. Rejected.
- **(b) mirrors `diverging_pool_keys`** — the existing, tested PoolDivergence
  pattern for exactly this V4 identity problem; reuses `PoolDivergenceKey` +
  `hop_pool_key` (no synonym introduced, per AGENTS.md).

## Implementation (in `rust/crates/degenbot-backrun-strategy`)

- `fot_registry.rs`
  - `fot_suspected_token_from_reverting_frame` → `Option<(Address, PoolDivergenceKey)>`,
    deriving the key from the matched hop via `hop_pool_key` (not `frame.target`).
  - `fot_suspected_token_from_swap_mismatch` → same key (not `swap.emitter`,
    which is the shared PoolManager for V4).
  - `fot_suspected_token` wrapper updated to the new tuple.
  - `hop_input_token_for_target` → `hop_for_target` (returns the hop, so its
    input token AND pool key both come from one match).
  - `FotTokenRecord::failing_pools` → `HashSet<PoolDivergenceKey>`.
  - `record_suspicion(token, pool_key, block)` signature.
- `dispatch.rs` — step 7.5 call site + comment updated (`pool` → `pool_key`).

The `[fot]`/`is_fot`/skip/fot_tokens surfaces stay token-keyed — unchanged.

## Acceptance

1. **Design decision (a/b/c)** — made: **(b)** (above).
2. **Synthetic V4 FoT token reaches confirmation** — GREEN via new tests:
   - `registry_v4_distinct_pool_ids_flag_token`
   - `v4_currency_not_settled_two_distinct_pool_ids_confirm` (end-to-end
     attribution → registry; this is the meaningful pre-fix RED case — both
     failures used to record under one PoolManager `Address` → 1 entry → no
     confirm).
   - Stale-state protection preserved: `registry_v4_single_pool_id_does_not_flag`,
     `v4_currency_not_settled_single_pool_id_stays_unconfirmed`.
   - Cross-family: `registry_v4_mixed_with_v2_reaches_confirmation`.
   - Ambiguity documented: `v4_two_hops_same_poolmanager_attributes_to_first`.
3. **5MP3HQ calibration (RFI, K=2) still holds for V2** — the V2 registry tests
   (`registry_k_distinct_pools_flag_token`, `registry_same_pool_twice_does_not_flag`,
   `registry_single_failing_pool_does_not_flag_token`, success-clear, decay)
   are unchanged and pass, so V2 (and V3) key identically to before (address).

Full crate suite: 97 passed. Clippy + rustfmt clean. `degenbot_rs` (FFI) builds.
