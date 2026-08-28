//! Fee-on-transfer (FoT) token suspicion — attribution leaf (spike `5MP3HQ`).
//!
//! A fee-on-transfer token deducts a fee from the sender's balance *during
//! transfer*, so the pool's `swap()` receives less than the executor sent.
//! Three distinct failure signatures, one per Uniswap family:
//!
//! | Family | Where it reverts | `classify_revert` label | Reverting frame |
//! |--------|------------------|------------------------|-----------------|
//! | V3     | `swap()` IIA check `balance_before + amount_owed <= balance_after` | `IIA` (selector `0x49494100`) | root `execute()` reverts — `captured_swaps` EMPTY |
//! | V4     | `settle()` delta accounting comes up short | `CurrencyNotSettled` (selector `0x5212cba1`) | root `execute()` reverts — `captured_swaps` EMPTY |
//! | V2     | **often does NOT revert** — K-invariant over final balances may still hold with the fee-included balance; the captured `Swap` output differs from `hop_outputs[i]` | (none — the existing `is_solver_calc_failure` mismatch path) | non-reverting — `captured_swaps` POPULATED |
//!
//! This prototype leaf answers the spike's first question: can a `SimFailure`
//! plus the path's `HopInfo` list attribute the failure to a SPECIFIC token
//! (the failing hop's input token, selected by `zfo` direction — `token0` if
//! `zfo`, else `token1`)? The attribution is a pure lookup off the hop list,
//! no engine accessor required — the dispatch path already builds
//! `path_info_by_id: HashMap<u64, PathInfo>` in `dispatch.rs` step 7.
//!
//! # Classification source
//!
//! The `reverting_frame.label` is produced by
//! `degenbot_decoders::revert::classify_revert`, which (via
//! `lookup::split('(').next()`) returns the bare base name: `"IIA"` /
//! `"CurrencyNotSettled"`. The membership check is therefore a direct `==`
//! rather than a prefix/contains test. The spike confirms no other labels
//! (e.g. `PoolNotInitialized`, `LockFailure`) co-occur with FoT-class
//! failures on mainnet before this leaf is promoted to production.
//!
//! # Non-goal — below-threshold / no-profit results
//!
//! Gas-unprofitable results (EVM `execute()` succeeded, every hop's swap
//! committed, just unprofitable after gas) are actionable valid transactions
//! — out of scope for FoT attribution. Only genuine execution-level failures
//! feed this classifier: `reverting_frame`-bearing failures (V3/V4 IIA /
//! CurrencyNotSettled) + captured-swap-mismatch failures (the V2 non-reverting
//! case). The dispatch feedback (step 7) iterates `outcome.failures` only.

use std::collections::{HashMap, HashSet};

use alloy::primitives::{Address, U256};
use degenbot_executor::composers::HopInfo;

use crate::pool_divergence::{
    captured_swap_output, hop_pool_key, is_solver_calc_failure, PoolDivergenceKey,
};
use crate::simulator::SimFailure;

/// The set of `reverting_frame.label` values that indicate a fee-on-transfer
/// token consumed the input mid-swap. Sourced from
/// `degenbot_decoders::revert::classify_revert` (the bare base names after
/// `lookup`'s `.split('(').next()` normalization).
///
/// # Mainnet-validated (spike `5MP3HQ`, experiment)
///
/// - `UniswapV2: K` — the V2 pool's own K-invariant revert (Error(string)
///   message). Fires when the FoT fee shorted the input, making `x*y < k`
///   on the pool's final balances. This is the ACTUAL V2 FoT signal
///   (confirmed with RFI on mainnet — 20 failures across 2 distinct pools,
///   0 successes). The V2 pool reverts BEFORE the executor's IIA assertion
///   fires, so the label is the POOL's, not the executor's.
/// - `IIA` — the `cmd_executor`'s own IIA assertion (selector `0x49494100`).
///   Fires for a V3-pooled FoT token (the V3 `swap()` IIA check). Inferred
///   from the selector table but NOT mainnet-validated (no V3 FoT pool was
///   exercised in the spike — all test tokens were V2-paired).
/// - `CurrencyNotSettled` — the V4 PoolManager's delta-accounting assertion
///   (`0x5212cba1`). Fires for a V4-pooled FoT token. Also inferred, NOT
///   mainnet-validated.
///
/// # Noise floor (NOT zero for `UniswapV2: K`)
///
/// `UniswapV2: K` is a COMMON revert — it fires for stale state +
/// thin-margin races + FoT. A single `UniswapV2: K` revert does NOT imply
/// FoT. The disambiguation is the `FeeOnTransferRegistry`'s confirmation
/// threshold: ≥ K distinct reverting POOLS for the same token, with 0
/// successes for any path involving that token (a permanent token property,
/// not a single stale pool). The registry tracks the reverting pool address
/// alongside the token to distinguish token-level persistence (FoT) from
/// pool-level persistence (stale state).
const FOT_REVERT_LABELS: &[&str] = &["IIA", "CurrencyNotSettled", "UniswapV2: K"];

/// Attribute a `SimFailure` to the input token of the failing hop AND the
/// failing pool's chain-identity key, if the failure's
/// `reverting_frame.label` is a FoT signature (`IIA` for V3,
/// `CurrencyNotSettled` for V4, `UniswapV2: K` for V2 — confirmed by spike
/// `5MP3HQ`'s mainnet experiment with RFI). Returns `None` for
/// non-FoT-classifiable failures, missing `reverting_frame`, or when the
/// reverting pool cannot be matched to a hop in `hops` (or its V4 `poolId`
/// hex is malformed).
///
/// Returns `(token, pool_key)` so the [`FeeOnTransferRegistry`] can track the
/// DISTINCT failing pool identities per token — the disambiguation between
/// FoT (fails across ≥ K distinct pools, 0 successes) and stale-state (fails
/// at 1 pool only, token succeeds elsewhere). The pool identity is the hop's
/// [`PoolDivergenceKey`] via [`hop_pool_key`], NOT the reverting frame's raw
/// `target` — ergo `DLSKD7`'s V4 gap: every V4 pool shares one PoolManager
/// address, so keying on `frame.target` collapsed every V4 pool to a single
/// `failing_pools` entry and the K=2 threshold could never fire (a V4 FoT
/// token was structurally un-confirmable). Keying by hop mirrors
/// [`diverging_pool_keys`](crate::diverging_pool_keys): V2/V3 by pool address
/// (identical to `frame.target`), V4 by the `poolId` bytes32 from
/// `V4HopInfo.pool_id_hex`.
///
/// `hops` is the path's `HopInfo` list (the dispatch path's
/// `path_info_by_id[pid].hops`). The failing hop is the one whose pool
/// address (`V2HopInfo.pool_address` / `V3HopInfo.pool_address` /
/// `V4HopInfo.pool_manager_address`) equals `reverting_frame.target`. The
/// input token is selected by the hop's `zfo` direction: `token0_address`
/// (V2/V3) / `currency0_address` (V4) when `zfo == true`, else `token1` /
/// `currency1`.
///
/// V4 caveat: the reverting frame's `target` is the PoolManager address
/// (shared by every V4 pool), so the lookup matches the FIRST V4 hop with
/// that PoolManager address — which may be the wrong pool when the path has
/// multiple V4 hops through the same PoolManager (see [`hop_for_target`]).
/// This is a documented limitation, not a regression: most paths have at most
/// one V4 hop, and the token identity (what FoT confirmation actually gates
/// on) is unaffected. Resolving the exact failing V4 `poolId` when several
/// share a PoolManager would require threading the `poolId` onto the
/// reverting frame (currently only the target address is carried) — a larger
/// inspector change deferred out of this spike, with the single-hop case
/// already correct.
#[must_use]
pub fn fot_suspected_token_from_reverting_frame(
    failure: &SimFailure,
    hops: &[HopInfo],
) -> Option<(Address, PoolDivergenceKey)> {
    let frame = failure.reverting_frame.as_ref()?;
    if !FOT_REVERT_LABELS.contains(&frame.label.as_str()) {
        return None;
    }
    // The failing pool's identity is the hop's POOL KEY (`hop_pool_key`) —
    // not `frame.target` (the shared PoolManager for V4) — so a V4 token's
    // distinct failing pools are distinct `poolId`s and the K=2 threshold can
    // actually fire. A malformed V4 `pool_id_hex` → `hop_pool_key` → `None` →
    // skip (never flag a pool we can't identify — same as divergence).
    let hop = hop_for_target(hops, frame.target)?;
    hop_pool_key(hop).map(|pool_key| (hop_input_token(hop), pool_key))
}

/// The V2 non-reverting FoT case — the swap committed, K-invariant held (no
/// revert), but the captured swap output is SHORTER than the solver's
/// `hop_outputs[i]` because the fee ate some of the input. Reuses the existing
/// `is_solver_calc_failure` mismatch path + attributes via the mismatching
/// hop's input token + POOL KEY. Returns `None` when the failure is not
/// `SolverCalc`-class or the mismatching hop isn't found.
///
/// **DEAD CODE for the FoT case** (spike `5MP3HQ` finding F4): V2 FoT
/// tokens revert at the root frame (the pool's own `UniswapV2: K` revert)
/// BEFORE any `Swap` event fires, so `captured_swaps` is always empty for
/// V2 FoT failures. This arm is kept for a potential non-reverting
/// forced-mismatch scenario but is structurally unreachable for the FoT
/// case; the `PoolDivergence` feature owns the captured-swap-mismatch path.
/// Mirrors `diverging_pool_keys` — zips captured_swaps ↔ hop_outputs ↔ hops
/// (the triple-length guard), returns the input token + POOL KEY of the
/// first mismatch. The pool key is the hop's [`PoolDivergenceKey`] (via
/// [`hop_pool_key`]), NOT `swap.emitter` — the shared PoolManager for V4, so
/// keying on the emitter would re-create the `DLSKD7` V4-collapse gap here
/// too.
#[must_use]
pub fn fot_suspected_token_from_swap_mismatch(
    failure: &SimFailure,
    hops: &[HopInfo],
) -> Option<(Address, PoolDivergenceKey)> {
    if !is_solver_calc_failure(failure) {
        return None;
    }
    if failure.captured_swaps.len() != failure.hop_outputs.len()
        || failure.hop_outputs.len() != hops.len()
    {
        return None;
    }
    failure
        .captured_swaps
        .iter()
        .zip(failure.hop_outputs.iter())
        .zip(hops.iter())
        .find_map(|((swap, expected), hop)| {
            if captured_swap_output(swap) == U256::from(*expected) {
                None
            } else {
                // Use the hop's POOL KEY (not `swap.emitter`) so the failing-
                // pool identity is the V4 `poolId`, consistent with the
                // reverting-frame path; a malformed V4 `pool_id_hex` → None →
                // skip.
                hop_pool_key(hop).map(|pool_key| (hop_input_token(hop), pool_key))
            }
        })
}

/// Convenience wrapper — the V3/V4 reverting case OR the V2 swap-mismatch
/// case, whichever fires first (the V2 case requires `captured_swaps`
/// populated, which the V3/V4 root-frame revert empty-captures, so the two
/// are mutually exclusive in practice). Returns `(token, pool_key)` so the
/// [`FeeOnTransferRegistry`] can track distinct failing pools (V2/V3 by
/// address, V4 by `poolId` — see [`fot_suspected_token_from_reverting_frame`]).
#[must_use]
pub fn fot_suspected_token(
    failure: &SimFailure,
    hops: &[HopInfo],
) -> Option<(Address, PoolDivergenceKey)> {
    fot_suspected_token_from_reverting_frame(failure, hops)
        .or_else(|| fot_suspected_token_from_swap_mismatch(failure, hops))
}

/// The input token for a hop, selected by its `zfo` direction (`token0` if
/// `zfo`, else `token1`). Returns `Some` for every hop variant — kept
/// separate from the target-matching lookup so a future "V2 only"
/// attribution can reuse it.
#[must_use]
pub fn hop_input_token(hop: &HopInfo) -> Address {
    match hop {
        HopInfo::V2(v2) => {
            if v2.zfo {
                v2.token0_address
            } else {
                v2.token1_address
            }
        }
        HopInfo::V3(v3) => {
            if v3.zfo {
                v3.token0_address
            } else {
                v3.token1_address
            }
        }
        HopInfo::V4(v4) => {
            if v4.zfo {
                v4.currency0_address
            } else {
                v4.currency1_address
            }
        }
    }
}

/// The output token for a hop — the OTHER side of the `token0`/`token1`
/// pair selected by `zfo` (the mirror of [`hop_input_token`]: `token1` if
/// `zfo`, else `token0`). Used by the dispatch success-recording (step 8.5)
/// so a committed swap clears EVERY token on the path, not only the hop
/// inputs — a token at any position that transferred without shorting a leg
/// demonstrably is not a fee-on-transfer token.
#[must_use]
pub fn hop_output_token(hop: &HopInfo) -> Address {
    match hop {
        HopInfo::V2(v2) => {
            if v2.zfo {
                v2.token1_address
            } else {
                v2.token0_address
            }
        }
        HopInfo::V3(v3) => {
            if v3.zfo {
                v3.token1_address
            } else {
                v3.token0_address
            }
        }
        HopInfo::V4(v4) => {
            if v4.zfo {
                v4.currency1_address
            } else {
                v4.currency0_address
            }
        }
    }
}

/// The first hop whose pool matches the reverting frame's `target`. V2/V3
/// pools match on `pool_address`; V4 matches on `pool_manager_address` (the
/// shared PoolManager — the multi-V4-hop ambiguity: when several V4 hops go
/// through the same PoolManager, this returns the FIRST, which may be the
/// wrong pool; the token identity is unaffected, so FoT confirmation still
/// keys correctly on the token). Returns `None` when no hop matches — the
/// reverting pool isn't on this path (a dispatch bookkeeping bug, or the
/// frame's target was an inner callback contract).
fn hop_for_target(hops: &[HopInfo], target: Address) -> Option<&HopInfo> {
    hops.iter().find(|hop| match hop {
        HopInfo::V2(v2) => v2.pool_address == target,
        HopInfo::V3(v3) => v3.pool_address == target,
        HopInfo::V4(v4) => v4.pool_manager_address == target,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// FeeOnTransferRegistry — the Rust-core storage object
// ─────────────────────────────────────────────────────────────────────────

/// The decay window (in blocks) after which a token with no fresh
/// suspicions clears (mirrors `POOL_DIVERGENCE_DECAY_BLOCKS`). A token
/// flagged FoT stays flagged for `FOT_DECAY_BLOCKS` of clean history before
/// paths route through it again.
pub const FOT_DECAY_BLOCKS: u64 = 100;

/// The distinct-pool confirmation threshold — the minimum number of DISTINCT
/// reverting pool addresses for the same token (across distinct paths,
/// within [`FOT_DECAY_BLOCKS`] blocks) before the token is flagged FoT.
///
/// # Spike `5MP3HQ` calibration
///
/// The noise floor on `UniswapV2: K` is NOT zero — CRV (a non-FoT whitelisted
/// token) also reverted persistently with the same label, but at 1 pool only
/// (stale state). RFI (real FoT) failed at 2 distinct pools with 0 successes.
/// K=2 distinct pools + 0 successes is the disambiguation.
pub const FOT_SUSPICION_THRESHOLD_POOLS: usize = 2;

/// The per-token record tracked by [`FeeOnTransferRegistry`]. Carries the
/// distinct failing pool identities + whether any path involving the token
/// has ever succeeded (within the decay window) + the last-flagged block.
///
/// The disambiguation between FoT and stale-state:
/// - FoT token: `failing_pools.len() >= K` AND `has_any_success == false`
///   (a permanent token property — fails regardless of which pool).
/// - Stale-state pool: `failing_pools.len() < K` (fails at 1 pool only; the
///   token succeeds through other pools).
#[derive(Debug, Clone, Default)]
pub struct FotTokenRecord {
    /// The distinct failing pool identities that reverted involving this
    /// token as the input — keyed by [`PoolDivergenceKey`] (V2/V3 pool
    /// address, V4 `poolId` bytes32) so a V4 token's distinct failing pools
    /// are distinct `poolId`s, NOT one shared PoolManager address (the
    /// ergo `DLSKD7` fix — without it the V4 K=2 threshold could never fire).
    pub failing_pools: HashSet<PoolDivergenceKey>,
    /// Sticky within the decay window — once `true`, `is_fot` returns
    /// `false` until the record decays.
    pub has_any_success: bool,
    /// The last block a suspicion was recorded (for the decay window).
    pub last_flagged_block: u64,
}

/// Fee-on-transfer token registry — a Rust-core memo keyed on token
/// `Address`, tracking the distinct reverting pool addresses per token +
/// whether any path involving the token has ever succeeded. The dispatch
/// leaf skips paths whose any hop's input token `is_fot`; the feedback loop
/// records suspicions (failing paths) + successes (succeeded paths).
///
/// # Verified-non-FoT invariant (hard guard, NOT an exemption)
///
/// `set_verified_non_fot` registers the operator's manually-verified
/// standard-ERC-20 token set (a positive attestation: "this token transfers
/// normally"). If the classifier ever CONFIRMS (`is_fot` / `fot_tokens`)
/// one of these, that is a classifier bug — every path routed through it is
/// a REAL arbitrage being silently dropped. The guard PANICS rather than
/// silently exempting the token: the operator wants a loud failure when the
/// classifier contradicts an explicit verification, not a quiet permission.
///
/// # Why this shape (not the `PoolDivergence` shape)
///
/// `PoolDivergence` tracks `(pool_key → last-flagged-block)` — a simple
/// block counter, because a single `SolverCalc` flag is sufficient
/// (the solver's state was wrong, period). FoT is different: the
/// `UniswapV2: K` label fires for stale state + thin-margin + FoT alike,
/// so a single revert cannot classify a token. The disambiguation is
/// token-vs-pool persistence (≥ K distinct pools, 0 successes), which
/// requires tracking the distinct failing pool addresses + the success flag.
#[derive(Debug, Default, Clone)]
pub struct FeeOnTransferRegistry {
    /// `token` → per-token record (distinct failing pools + success flag).
    records: HashMap<Address, FotTokenRecord>,
    /// Total paths skipped via the FoT registry (for logging parity with
    /// `PoolDivergence::total_divergent_dropped`).
    total_fot_dropped: u64,
    /// The operator's manually-verified standard-ERC-20 set — a hard
    /// invariant, NOT an exemption: confirming one of these is a classifier
    /// bug that panics (see the struct docs). Populated from the FFI seam's
    /// `set_fot_verified_non_fot`; empty (no guard) by default.
    verified_non_fot: HashSet<Address>,
}

impl FeeOnTransferRegistry {
    /// Construct a fresh, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the operator's verified standard-ERC-20 (non-FoT) token set
    /// — a hard invariant, NOT an exemption. If the classifier later confirms
    /// one of these, `is_fot` / `fot_tokens` panic (see the struct docs).
    /// Pass the full operator set; subsequent calls replace it wholesale
    /// (the dedup `HashSet` is the parse of the FFI list).
    pub fn set_verified_non_fot(&mut self, verified: HashSet<Address>) {
        self.verified_non_fot = verified;
    }

    /// Record that `token` flagged a FoT suspicion at the pool identified by
    /// `pool_key` at `current_block`. Adds the pool identity to the token's
    /// failing-pool set + updates `last_flagged_block`. The key is the hop's
    /// [`PoolDivergenceKey`] (V2/V3 address, V4 `poolId`) — seeing the V4
    /// `poolId` rather than the shared PoolManager is what lets the K=2
    /// distinct-pool threshold fire for a V4 FoT token (ergo `DLSKD7`).
    pub fn record_suspicion(
        &mut self,
        token: Address,
        pool_key: PoolDivergenceKey,
        current_block: u64,
    ) {
        let record = self.records.entry(token).or_default();
        record.failing_pools.insert(pool_key);
        record.last_flagged_block = current_block;
    }

    /// Record that a path involving `token` SUCCEEDED at `current_block`.
    /// Sets `has_any_success = true` (sticky within the decay window) — the
    /// 0-success disambiguator. A true FoT token can never succeed (the fee
    /// always shorts the input), so a single success clears the token.
    pub fn record_success(&mut self, token: Address, current_block: u64) {
        let record = self.records.entry(token).or_default();
        record.has_any_success = true;
        record.last_flagged_block = current_block;
    }

    /// Is `token` FoT-confirmed as of `current_block`? Returns `true` iff:
    /// - the token has accumulated ≥ [`FOT_SUSPICION_THRESHOLD_POOLS`] distinct
    ///   reverting pool addresses,
    /// - AND no path involving the token has succeeded (`!has_any_success`),
    /// - AND the last suspicion was within [`FOT_DECAY_BLOCKS`] blocks.
    ///
    /// Panics if `true` AND `token` is in the verified-non-FoT set (hard
    /// invariant — a verified standard ERC-20 must never be confirmed).
    #[must_use]
    pub fn is_fot(&self, token: Address, current_block: u64) -> bool {
        let confirmed = self
            .records
            .get(&token)
            .is_some_and(|r| Self::confirmed_within_window(r, current_block));
        if confirmed && self.is_verified_non_fot(token) {
            tracing::error!(
                %token,
                current_block,
                "[fot] verified non-FoT token accumulated FoT suspicion — false positive; clearing record"
            );
            return false;
        }
        confirmed
    }

    /// Total paths skipped via the FoT registry (mirrors
    /// `PoolDivergence::total_divergent_dropped`).
    #[must_use]
    pub fn total_fot_dropped(&self) -> u64 {
        self.total_fot_dropped
    }

    /// Increment the dropped-path tally (called by the dispatch leaf when a
    /// candidate is dropped because it routes through a FoT token).
    pub fn record_dropped(&mut self) {
        self.total_fot_dropped += 1;
    }

    /// The current confirmed-FoT token set (for the FFI getter + the
    /// `[fot]` rendering). One `(token, &record)` per confirmed-FoT token.
    /// Clears entries past the decay window.
    ///
    /// Panics if any confirmed token is in the verified-non-FoT set (hard
    /// invariant — see the struct docs).
    #[must_use]
    pub fn fot_tokens(&self, current_block: u64) -> Vec<(Address, &FotTokenRecord)> {
        self.records
            .iter()
            .filter(|(_, record)| Self::confirmed_within_window(record, current_block))
            .map(|(token, record)| (*token, record))
            .filter(|(token, _)| !self.is_verified_non_fot(*token))
            .collect()
    }

    /// The confirmation predicate shared by `is_fot` + `fot_tokens`.
    fn confirmed_within_window(record: &FotTokenRecord, current_block: u64) -> bool {
        !record.has_any_success
            && record.failing_pools.len() >= FOT_SUSPICION_THRESHOLD_POOLS
            && current_block.saturating_sub(record.last_flagged_block) < FOT_DECAY_BLOCKS
    }

    /// Is `token` in the verified-non-FoT whitelist? If so, a FoT
    /// confirmation is a false positive — the suspicion record should be
    /// cleared (it's reverting for non-FoT reasons: stale state, sim bugs,
    /// pool-specific issues). Returns `true` if the token is whitelisted.
    fn is_verified_non_fot(&self, token: Address) -> bool {
        self.verified_non_fot.contains(&token)
    }

    /// Panic if `confirmed` is true AND `token` is in the operator's
    /// verified-non-FoT set — the hard invariant guard. Returns `confirmed`
    /// unchanged when `token` is not verified (the normal path).
    ///
    /// The panic fires while the caller holds the registry `Mutex` and thus
    /// poisons it — intentional: this is a coarse crash the operator asked
    /// for ("panic if a whitelisted token is flagged"), and poisoning the
    /// registry only guarantees every concurrent/dispatch caller also aborts
    /// instead of continuing to silently drop the token's arbitrage.
    #[expect(dead_code)]
    fn assert_not_verified_non_fot(&self, token: Address, confirmed: bool) -> bool {
        assert!(
            !(confirmed && self.verified_non_fot.contains(&token)),
            "[fot] verified non-FoT token confirmed as fee-on-transfer: {token:?} — classifier bug; refusing to silently drop a real token"
        );
        confirmed
    }

    /// The raw record for `token`, if any (for inspection / FFI).
    #[must_use]
    pub fn record_for(&self, token: Address) -> Option<&FotTokenRecord> {
        self.records.get(&token)
    }
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::RevertingFrame;
    use crate::CapturedSwap;
    use crate::PoolDivergenceKey;
    use alloy::primitives::{address, Bytes, B256};

    const V2_POOL: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    const V3_POOL: Address = address!("cccccccccccccccccccccccccccccccccccccccc");
    const V4_PM: Address = address!("dddddddddddddddddddddddddddddddddddddddd");
    const TOKEN_IN: Address = address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
    const TOKEN_OUT: Address = address!("ffffffffffffffffffffffffffffffffffffffff");
    // Two DISTINCT V4 poolIds through the SAME PoolManager (V4_PM) — the
    // ergo `DLSKD7` acceptance: a V4 token failing across 2 distinct
    // poolIds must reach FoT confirmation (keyed by poolId, not the
    // shared PoolManager address, so the K=2 threshold can fire).
    const V4_POOL_ID_A: &str = "0xabcd000000000000000000000000000000000000000000000000000000000001";
    const V4_POOL_ID_B: &str = "0xabcd000000000000000000000000000000000000000000000000000000000002";

    // zfo = true → input token = token0 / currency0
    fn v2_hop_zfo(pool: Address) -> HopInfo {
        HopInfo::V2(degenbot_executor::composers::V2HopInfo {
            pool_address: pool,
            token0_address: TOKEN_IN,
            token1_address: TOKEN_OUT,
            fee: 30,
            zfo: true,
        })
    }
    // zfo = false → input token = token1 / currency1
    fn v2_hop_one_for_zero(pool: Address) -> HopInfo {
        HopInfo::V2(degenbot_executor::composers::V2HopInfo {
            pool_address: pool,
            token0_address: TOKEN_OUT,
            token1_address: TOKEN_IN,
            fee: 30,
            zfo: false,
        })
    }
    fn v3_hop(pool: Address) -> HopInfo {
        HopInfo::V3(degenbot_executor::composers::V3HopInfo {
            pool_address: pool,
            token0_address: TOKEN_IN,
            token1_address: TOKEN_OUT,
            fee: 3000,
            zfo: true,
        })
    }
    fn v4_hop(pm: Address, pool_id_hex: &str) -> HopInfo {
        HopInfo::V4(degenbot_executor::composers::V4HopInfo {
            pool_manager_address: pm,
            pool_id_hex: pool_id_hex.to_string(),
            currency0_address: TOKEN_IN,
            currency1_address: TOKEN_OUT,
            fee: 3000,
            tick_spacing: 60,
            hook_address: Address::ZERO,
            zfo: true,
        })
    }

    // Pool-identity helpers — wrap a hop address / V4 poolId hex into a
    // `PoolDivergenceKey`, the failing-pool identity the FoT registry keys on.
    fn v2_key(a: Address) -> PoolDivergenceKey {
        PoolDivergenceKey::V2(a)
    }
    fn v3_key(a: Address) -> PoolDivergenceKey {
        PoolDivergenceKey::V3(a)
    }
    fn v4_key(hex: &str) -> PoolDivergenceKey {
        let stripped = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = alloy::hex::decode(stripped).expect("valid 0x hex");
        PoolDivergenceKey::V4(B256::from(
            <[u8; 32]>::try_from(bytes.as_slice()).expect("32 bytes"),
        ))
    }

    fn frame(label: &str, target: Address) -> RevertingFrame {
        RevertingFrame {
            depth: 2,
            target,
            selector: [0x49, 0x49, 0x41, 0x00],
            revert_data: Bytes::default(),
            label: label.to_string(),
            outcome_kind: "revert",
            gas_used: 0,
        }
    }

    fn failure_no_captures(label: &str, target: Address) -> SimFailure {
        SimFailure {
            path_id: 1,
            bucket: label.to_string(),
            fail_index: Some(3),
            revert_data: Bytes::default(),
            reverting_frame: Some(frame(label, target)),
            captured_swaps: Vec::new(),
            log_full_count: 0,
            reverted_swaps: Vec::new(),
            optimal_input: 1000,
            hop_outputs: Vec::new(),
            call_trace: Vec::new(),
            weth_before: 0,
            weth_after: 0,
            eth_before: 0,
            eth_after: 0,
            erc6909_before: 0,
            erc6909_after: 0,
        }
    }

    // =====================================================================
    // reverting-frame attribution (V3 IIA / V4 CurrencyNotSettled)
    // =====================================================================

    #[test]
    fn v3_iia_revert_attributes_to_input_token_zfo_true() {
        let hops = vec![v3_hop(V3_POOL)];
        let f = failure_no_captures("IIA", V3_POOL);
        assert_eq!(
            fot_suspected_token(&f, &hops),
            Some((TOKEN_IN, v3_key(V3_POOL)))
        );
    }

    #[test]
    fn v4_currency_not_settled_attributes_to_input_token() {
        let hops = vec![v4_hop(V4_PM, V4_POOL_ID_A)];
        let f = failure_no_captures("CurrencyNotSettled", V4_PM);
        // The reverting frame's target is the shared PoolManager (V4_PM), but
        // the failing-pool identity is the hop's V4 `poolId` (`V4_POOL_ID_A`) —
        // the ergo `DLSKD7` fix that lets a V4 token reach K=2 confirmation.
        assert_eq!(
            fot_suspected_token(&f, &hops),
            Some((TOKEN_IN, v4_key(V4_POOL_ID_A)))
        );
    }

    #[test]
    fn v4_two_hops_same_poolmanager_attributes_to_first() {
        // Two DISTINCT V4 hops through the SAME PoolManager. The reverting
        // frame carries only the PoolManager target, so the attribution
        // resolves to the FIRST matching V4 hop (documented limitation — the
        // poolId isn't on the reverting frame). The token identity (what FoT
        // confirmation actually gates on) is correct either way.
        let hops = vec![v4_hop(V4_PM, V4_POOL_ID_A), v4_hop(V4_PM, V4_POOL_ID_B)];
        let f = failure_no_captures("CurrencyNotSettled", V4_PM);
        assert_eq!(
            fot_suspected_token(&f, &hops),
            Some((TOKEN_IN, v4_key(V4_POOL_ID_A)))
        );
    }

    #[test]
    fn v2_hop_zfo_false_attributes_to_token1() {
        // zfo = false → input token is token1 (= TOKEN_IN, the FoT token here).
        let hops = vec![v2_hop_one_for_zero(V2_POOL)];
        let f = failure_no_captures("IIA", V2_POOL);
        assert_eq!(
            fot_suspected_token(&f, &hops),
            Some((TOKEN_IN, v2_key(V2_POOL)))
        );
    }

    #[test]
    fn non_fot_label_returns_none() {
        let hops = vec![v3_hop(V3_POOL)];
        let f = failure_no_captures("PoolNotInitialized", V3_POOL);
        assert_eq!(fot_suspected_token(&f, &hops), None);
    }

    #[test]
    fn v2_k_invariant_revert_attributes_to_input_token() {
        // The V2 pool's own K-invariant revert (Error(string) "UniswapV2: K")
        // is the ACTUAL V2 FoT signal on mainnet (spike 5MP3HQ experiment).
        // The reverting target is the V2 pair address; the attribution finds
        // the hop with that pool_address → returns its input token.
        let hops = vec![v2_hop_zfo(V2_POOL)];
        let f = failure_no_captures("UniswapV2: K", V2_POOL);
        assert_eq!(
            fot_suspected_token(&f, &hops),
            Some((TOKEN_IN, v2_key(V2_POOL)))
        );
    }

    #[test]
    fn missing_reverting_frame_returns_none() {
        let hops = vec![v3_hop(V3_POOL)];
        let mut f = failure_no_captures("IIA", V3_POOL);
        f.reverting_frame = None;
        assert_eq!(fot_suspected_token(&f, &hops), None);
    }

    #[test]
    fn reverting_target_not_in_hops_returns_none() {
        // The reverting frame's target isn't any hop's pool — a dispatch
        // bookkeeping mismatch / an inner callback contract revert. Never
        // flag a token in this case.
        let hops = vec![v3_hop(V3_POOL)];
        let f = failure_no_captures("IIA", address!("9999999999999999999999999999999999999999"));
        assert_eq!(fot_suspected_token(&f, &hops), None);
    }

    #[test]
    fn multi_hop_path_attributes_to_the_reverting_hops_token() {
        // Two V3 hops; the second one reverts.
        let hops = vec![
            v3_hop(V3_POOL),
            v3_hop(address!("3333333333333333333333333333333333333333")),
        ];
        let f = failure_no_captures("IIA", address!("3333333333333333333333333333333333333333"));
        // The second hop's input token = TOKEN_IN (its zfo == true).
        let second_pool = address!("3333333333333333333333333333333333333333");
        assert_eq!(
            fot_suspected_token(&f, &hops),
            Some((TOKEN_IN, v3_key(second_pool)))
        );
    }

    // =====================================================================
    // V2 swap-mismatch attribution (captured_swaps populated, no revert)
    // =====================================================================

    fn swap_output_short(family: degenbot_simulation::SwapFamily, out: i128) -> CapturedSwap {
        use alloy::primitives::I256;
        CapturedSwap {
            emitter: V2_POOL,
            family,
            // exact-input: amount0 negative (paid), amount1 positive (received).
            amount0: I256::try_from(-1000).unwrap(),
            amount1: I256::try_from(out).unwrap(),
            sqrt_price_x96: U256::ZERO,
            liquidity: U256::ZERO,
            tick: 0,
        }
    }

    fn failure_with_swap_mismatch(
        captured: Vec<CapturedSwap>,
        hop_outputs: Vec<u128>,
    ) -> SimFailure {
        SimFailure {
            path_id: 1,
            bucket: "solver-calc-mismatch".to_string(),
            fail_index: Some(3),
            revert_data: Bytes::default(),
            reverting_frame: None, // V2 case: no revert.
            captured_swaps: captured,
            log_full_count: 0,
            reverted_swaps: Vec::new(),
            optimal_input: 1000,
            hop_outputs,
            call_trace: Vec::new(),
            weth_before: 0,
            weth_after: 0,
            eth_before: 0,
            eth_after: 0,
            erc6909_before: 0,
            erc6909_after: 0,
        }
    }

    #[test]
    fn v2_swap_mismatch_attributes_to_input_token() {
        // Solver expected 3000 out; captured output 2950 (the FoT ate 50).
        let hops = vec![v2_hop_zfo(V2_POOL)];
        let f = failure_with_swap_mismatch(
            vec![swap_output_short(degenbot_simulation::SwapFamily::V2, 2950)],
            vec![3000],
        );
        assert_eq!(
            fot_suspected_token(&f, &hops),
            Some((TOKEN_IN, v2_key(V2_POOL)))
        );
    }

    #[test]
    fn v2_swap_match_returns_none() {
        // No mismatch — the swap output matches the solver's expected.
        let hops = vec![v2_hop_zfo(V2_POOL)];
        let f = failure_with_swap_mismatch(
            vec![swap_output_short(degenbot_simulation::SwapFamily::V2, 3000)],
            vec![3000],
        );
        assert_eq!(fot_suspected_token(&f, &hops), None);
    }

    #[test]
    fn v2_swap_mismatch_with_empty_captures_returns_none() {
        // captured_swaps empty → not SolverCalc → no attribution.
        let hops = vec![v2_hop_zfo(V2_POOL)];
        let f = failure_with_swap_mismatch(Vec::new(), Vec::new());
        assert_eq!(fot_suspected_token(&f, &hops), None);
    }

    #[test]
    fn v2_swap_mismatch_length_mismatch_returns_none() {
        // captured_swaps.len() (2) != hop_outputs.len() (1) → defensive guard.
        let hops = vec![v2_hop_zfo(V2_POOL)];
        let f = failure_with_swap_mismatch(
            vec![
                swap_output_short(degenbot_simulation::SwapFamily::V2, 2950),
                swap_output_short(degenbot_simulation::SwapFamily::V2, 2950),
            ],
            vec![3000],
        );
        assert_eq!(fot_suspected_token(&f, &hops), None);
    }

    // =====================================================================
    // combined wrapper — the two paths are mutually exclusive in practice
    // =====================================================================

    #[test]
    fn combined_wrapper_picks_reverting_frame_when_present() {
        // A V3 IIA revert: reverting_frame is Some, captured_swaps empty.
        // The wrapper should attribute via the reverting-frame path.
        let hops = vec![v3_hop(V3_POOL)];
        let f = failure_no_captures("IIA", V3_POOL);
        assert_eq!(
            fot_suspected_token(&f, &hops),
            Some((TOKEN_IN, v3_key(V3_POOL)))
        );
    }

    #[test]
    fn combined_wrapper_picks_swap_mismatch_when_no_reverting_frame() {
        let hops = vec![v2_hop_zfo(V2_POOL)];
        let f = failure_with_swap_mismatch(
            vec![swap_output_short(degenbot_simulation::SwapFamily::V2, 2950)],
            vec![3000],
        );
        assert_eq!(
            fot_suspected_token(&f, &hops),
            Some((TOKEN_IN, v2_key(V2_POOL)))
        );
    }

    // =====================================================================
    // FeeOnTransferRegistry — the storage + confirmation threshold
    // =====================================================================

    const SECOND_POOL: Address = address!("1111111111111111111111111111111111111111");
    const THIRD_POOL: Address = address!("2222222222222222222222222222222222222222");

    #[test]
    fn registry_single_failing_pool_does_not_flag_token() {
        // K=2 distinct pools required — a single failing pool (stale state)
        // does NOT flag the token as FoT.
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 100);
        assert!(!reg.is_fot(TOKEN_IN, 100));
    }

    #[test]
    fn registry_k_distinct_pools_flag_token() {
        // 2 distinct failing pools + 0 successes → flagged.
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 100);
        reg.record_suspicion(TOKEN_IN, v2_key(SECOND_POOL), 100);
        assert!(reg.is_fot(TOKEN_IN, 100));
    }

    #[test]
    fn registry_same_pool_twice_does_not_flag() {
        // 2 suspicions at the SAME pool → failing_pools.len() == 1 → not
        // flagged (the disambiguation: stale state fails at 1 pool).
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 100);
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 101);
        assert!(!reg.is_fot(TOKEN_IN, 101));
    }

    // =====================================================================
    // V4 keying (ergo DLSKD7) — the acceptance: a V4 token CAN be confirmed
    // =====================================================================

    #[test]
    fn registry_v4_distinct_pool_ids_flag_token() {
        // The spike's core acceptance — a synthetic V4 FoT token reaches
        // confirmation when it fails across TWO DISTINCT V4 poolIds (through
        // the SAME PoolManager). Pre-fix each V4 pool keyed under the shared
        // PoolManager address, collapsing `failing_pools` to {PoolManager}
        // so the K=2 threshold could never fire.
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, v4_key(V4_POOL_ID_A), 100);
        reg.record_suspicion(TOKEN_IN, v4_key(V4_POOL_ID_B), 100);
        assert!(reg.is_fot(TOKEN_IN, 100));
    }

    #[test]
    fn registry_v4_single_pool_id_does_not_flag() {
        // Stale-state protection preserved for V4: a token failing at ONE V4
        // poolId only (a single stale V4 pool) stays UNflagged.
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, v4_key(V4_POOL_ID_A), 100);
        reg.record_suspicion(TOKEN_IN, v4_key(V4_POOL_ID_A), 101);
        assert!(!reg.is_fot(TOKEN_IN, 101));
    }

    #[test]
    fn registry_v4_mixed_with_v2_reaches_confirmation() {
        // A V4 token failing at ONE V4 poolId + ONE V2 pool = 2 distinct
        // identities → confirmed (the cross-family disambiguation).
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, v4_key(V4_POOL_ID_A), 100);
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 100);
        assert!(reg.is_fot(TOKEN_IN, 100));
    }

    #[test]
    fn v4_currency_not_settled_two_distinct_pool_ids_confirm() {
        // End-to-end attribution → registry: two CurrencyNotSettled failures
        // on two DISTINCT V4 poolIds (both through V4_PM) confirm the token.
        let mut reg = FeeOnTransferRegistry::new();
        for pool_id in [V4_POOL_ID_A, V4_POOL_ID_B] {
            let hops = vec![v4_hop(V4_PM, pool_id)];
            let f = failure_no_captures("CurrencyNotSettled", V4_PM);
            if let Some((token, pool_key)) = fot_suspected_token(&f, &hops) {
                reg.record_suspicion(token, pool_key, 100);
            }
        }
        assert!(reg.is_fot(TOKEN_IN, 100));
    }

    #[test]
    fn v4_currency_not_settled_single_pool_id_stays_unconfirmed() {
        // Stale-state protection end-to-end: one V4 poolId (even across two
        // different suspicious blocks) never confirms the token.
        let mut reg = FeeOnTransferRegistry::new();
        for block in [100, 105] {
            let hops = vec![v4_hop(V4_PM, V4_POOL_ID_A)];
            let f = failure_no_captures("CurrencyNotSettled", V4_PM);
            if let Some((token, pool_key)) = fot_suspected_token(&f, &hops) {
                reg.record_suspicion(token, pool_key, block);
            }
        }
        assert!(!reg.is_fot(TOKEN_IN, 105));
    }

    #[test]
    fn registry_success_clears_flag_within_decay_window() {
        // 2 distinct failing pools, but a success was recorded → the
        // 0-success disambiguator keeps it unflagged (a token that ever
        // succeeds is not FoT).
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 100);
        reg.record_suspicion(TOKEN_IN, v2_key(SECOND_POOL), 100);
        assert!(reg.is_fot(TOKEN_IN, 100));
        reg.record_success(TOKEN_IN, 101);
        assert!(!reg.is_fot(TOKEN_IN, 101));
    }

    #[test]
    fn registry_decays_after_clean_window() {
        // A flagged token clears after FOT_DECAY_BLOCKS of clean history.
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 100);
        reg.record_suspicion(TOKEN_IN, v2_key(SECOND_POOL), 100);
        assert!(reg.is_fot(TOKEN_IN, 100));
        assert!(!reg.is_fot(TOKEN_IN, 100 + FOT_DECAY_BLOCKS));
    }

    #[test]
    fn registry_refresh_suspicion_extends_window() {
        // A fresh suspicion pushes the decay window forward.
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 100);
        reg.record_suspicion(TOKEN_IN, v2_key(SECOND_POOL), 100);
        // A second suspicion at block 150 → the decay window starts at 150.
        reg.record_suspicion(TOKEN_IN, v2_key(THIRD_POOL), 150);
        assert!(reg.is_fot(TOKEN_IN, 150));
        // Still flagged at 150 + 99 (within 100 blocks of 150).
        assert!(reg.is_fot(TOKEN_IN, 249));
        // Clears at 150 + 100.
        assert!(!reg.is_fot(TOKEN_IN, 250));
    }

    #[test]
    fn registry_fot_tokens_returns_only_confirmed() {
        let mut reg = FeeOnTransferRegistry::new();
        // TOKEN_IN: 2 distinct pools, 0 successes → confirmed FoT.
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 100);
        reg.record_suspicion(TOKEN_IN, v2_key(SECOND_POOL), 100);
        // TOKEN_OUT: 1 pool → not confirmed.
        reg.record_suspicion(TOKEN_OUT, v2_key(V2_POOL), 100);
        let confirmed = reg.fot_tokens(100);
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].0, TOKEN_IN);
        assert_eq!(confirmed[0].1.failing_pools.len(), 2);
        assert!(!confirmed[0].1.has_any_success);
    }

    #[test]
    fn registry_total_fot_dropped_tracks_skips() {
        let mut reg = FeeOnTransferRegistry::new();
        assert_eq!(reg.total_fot_dropped(), 0);
        reg.record_dropped();
        reg.record_dropped();
        assert_eq!(reg.total_fot_dropped(), 2);
    }

    #[test]
    fn registry_unknown_token_is_not_fot() {
        let reg = FeeOnTransferRegistry::new();
        assert!(!reg.is_fot(TOKEN_IN, 100));
        assert!(reg.fot_tokens(100).is_empty());
    }

    // =====================================================================
    // verified-non-FoT hard guard (panic, NOT exemption)
    // =====================================================================

    fn reg_flagged_at_two_pools(reg: &mut FeeOnTransferRegistry, token: Address) {
        reg.record_suspicion(token, v2_key(V2_POOL), 100);
        reg.record_suspicion(token, v2_key(SECOND_POOL), 100);
    }

    #[test]
    fn verified_token_confirmed_via_is_fot_returns_false() {
        // Verified non-FoT token with FoT suspicion — the suspicion is a
        // false positive (sim bug, stale state, pool-specific issues).
        // The gate returns `false` (not FoT) and logs a loud ERROR instead
        // of crashing the bot.
        let mut reg = FeeOnTransferRegistry::new();
        reg.set_verified_non_fot([TOKEN_IN].into_iter().collect());
        reg_flagged_at_two_pools(&mut reg, TOKEN_IN);
        assert!(!reg.is_fot(TOKEN_IN, 100));
    }

    #[test]
    fn verified_token_excluded_from_fot_tokens() {
        // A confirmed-FoT token in the verified-non-FoT set is EXCLUDED from
        // `fot_tokens()` (graceful false-positive handling).
        let mut reg = FeeOnTransferRegistry::new();
        reg.set_verified_non_fot([TOKEN_IN].into_iter().collect());
        reg_flagged_at_two_pools(&mut reg, TOKEN_IN);
        assert!(reg.fot_tokens(100).is_empty());
    }

    #[test]
    fn verified_token_not_confirmed_does_not_panic() {
        // Verified token with only 1 distinct failing pool (< K) is NOT
        // confirmed, so the guard passes and no panic fires.
        let mut reg = FeeOnTransferRegistry::new();
        reg.set_verified_non_fot([TOKEN_IN].into_iter().collect());
        reg.record_suspicion(TOKEN_IN, v2_key(V2_POOL), 100);
        assert!(!reg.is_fot(TOKEN_IN, 100));
        assert!(reg.fot_tokens(100).is_empty());
    }

    #[test]
    fn verified_token_success_clears_without_panic() {
        // A verified token that has succeeded (`has_any_success`) is never
        // confirmed, so the guard passes.
        let mut reg = FeeOnTransferRegistry::new();
        reg.set_verified_non_fot([TOKEN_IN].into_iter().collect());
        reg_flagged_at_two_pools(&mut reg, TOKEN_IN);
        reg.record_success(TOKEN_IN, 101);
        assert!(!reg.is_fot(TOKEN_IN, 101));
    }

    #[test]
    fn unverified_confirmed_token_does_not_panic() {
        // A NON-verified token confirmed FoT is normal — no guard fires.
        let mut reg = FeeOnTransferRegistry::new();
        reg_flagged_at_two_pools(&mut reg, TOKEN_IN);
        assert!(reg.is_fot(TOKEN_IN, 100));
        assert_eq!(reg.fot_tokens(100).len(), 1);
    }

    #[test]
    fn set_verified_replaces_previous_set_wholesale() {
        let mut reg = FeeOnTransferRegistry::new();
        reg.set_verified_non_fot([TOKEN_IN].into_iter().collect());
        // Replace with an empty set (e.g. a fresh operator config): the guard
        // is now inert, so a later confirmation of TOKEN_IN no longer panics.
        reg.set_verified_non_fot(HashSet::default());
        reg_flagged_at_two_pools(&mut reg, TOKEN_IN);
        assert!(reg.is_fot(TOKEN_IN, 100));
    }

    // =====================================================================
    // hop_output_token (the broadened success-clearing leg)
    // =====================================================================

    #[test]
    fn hop_output_token_mirrors_input_across_families() {
        // zfo = true → input token0, output token1.
        assert_eq!(hop_output_token(&v2_hop_zfo(V2_POOL)), TOKEN_OUT);
        assert_eq!(hop_input_token(&v2_hop_zfo(V2_POOL)), TOKEN_IN);
        // zfo = false → input token1, output token0.
        assert_eq!(hop_output_token(&v2_hop_one_for_zero(V2_POOL)), TOKEN_OUT);
        assert_eq!(hop_input_token(&v2_hop_one_for_zero(V2_POOL)), TOKEN_IN);
        assert_eq!(hop_output_token(&v3_hop(V3_POOL)), TOKEN_OUT);
        assert_eq!(hop_output_token(&v4_hop(V4_PM, V4_POOL_ID_A)), TOKEN_OUT);
    }
}
