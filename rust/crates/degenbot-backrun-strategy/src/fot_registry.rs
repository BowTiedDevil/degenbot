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

use crate::pool_divergence::{captured_swap_output, is_solver_calc_failure};
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
/// reverting pool's address, if the failure's `reverting_frame.label` is a
/// FoT signature (`IIA` for V3, `CurrencyNotSettled` for V4,
/// `UniswapV2: K` for V2 — confirmed by spike `5MP3HQ`'s mainnet experiment
/// with RFI). Returns `None` for non-FoT-classifiable failures, missing
/// `reverting_frame`, or when the reverting pool's address cannot be matched
/// to a hop in `hops`.
///
/// Returns `(token, reverting_pool)` so the [`FeeOnTransferRegistry`] can
/// track the DISTINCT failing pool addresses per token — the disambiguation
/// between FoT (fails across ≥ K distinct pools, 0 successes) and stale-state
/// (fails at 1 pool only, token succeeds elsewhere).
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
/// multiple V4 hops through the same PoolManager. The spike surfaces whether
/// this is a real ambiguity on mainnet (most paths have at most one V4 hop);
/// the production version may need the V4 `poolId` carried on the reverting
/// frame (currently only the target address is).
#[must_use]
pub fn fot_suspected_token_from_reverting_frame(
    failure: &SimFailure,
    hops: &[HopInfo],
) -> Option<(Address, Address)> {
    let frame = failure.reverting_frame.as_ref()?;
    if !FOT_REVERT_LABELS.contains(&frame.label.as_str()) {
        return None;
    }
    hop_input_token_for_target(hops, frame.target).map(|token| (token, frame.target))
}

/// The V2 non-reverting FoT case — the swap committed, K-invariant held (no
/// revert), but the captured swap output is SHORTER than the solver's
/// `hop_outputs[i]` because the fee ate some of the input. Reuses the existing
/// `is_solver_calc_failure` mismatch path + attributes via the mismatching
/// hop's input token. Returns `None` when the failure is not
/// `SolverCalc`-class or the mismatching hop isn't found.
///
/// **DEAD CODE for the FoT case** (spike `5MP3HQ` finding F4): V2 FoT
/// tokens revert at the root frame (the pool's own `UniswapV2: K` revert)
/// BEFORE any `Swap` event fires, so `captured_swaps` is always empty for
/// V2 FoT failures. This arm is kept for a potential non-reverting
/// forced-mismatch scenario but is structurally unreachable for the FoT
/// case; the `PoolDivergence` feature owns the captured-swap-mismatch path.
/// Mirrors `diverging_pool_keys` — zips captured_swaps ↔ hop_outputs ↔ hops
/// (the triple-length guard), returns the input token of the first mismatch
/// + the captured swap's emitter as the reverting pool.
#[must_use]
pub fn fot_suspected_token_from_swap_mismatch(
    failure: &SimFailure,
    hops: &[HopInfo],
) -> Option<(Address, Address)> {
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
                Some((hop_input_token(hop), swap.emitter))
            }
        })
}

/// Convenience wrapper — the V3/V4 reverting case OR the V2 swap-mismatch
/// case, whichever fires first (the V2 case requires `captured_swaps`
/// populated, which the V3/V4 root-frame revert empty-captures, so the two
/// are mutually exclusive in practice). Returns `(token, reverting_pool)`
/// so the [`FeeOnTransferRegistry`] can track distinct failing pools.
#[must_use]
pub fn fot_suspected_token(failure: &SimFailure, hops: &[HopInfo]) -> Option<(Address, Address)> {
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

/// The input token of the hop whose pool address matches `target`. V2/V3
/// pools match on `pool_address`; V4 matches on `pool_manager_address` (the
/// PoolManager; the spike notes the multi-V4-hop ambiguity). Returns `None`
/// when no hop matches — the reverting pool isn't on this path (a dispatch
/// bookkeeping bug, or the frame's target was an inner callback contract).
fn hop_input_token_for_target(hops: &[HopInfo], target: Address) -> Option<Address> {
    hops.iter().find_map(|hop| match hop {
        HopInfo::V2(v2) if v2.pool_address == target => Some(hop_input_token(hop)),
        HopInfo::V3(v3) if v3.pool_address == target => Some(hop_input_token(hop)),
        HopInfo::V4(v4) if v4.pool_manager_address == target => Some(hop_input_token(hop)),
        _ => None,
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
/// distinct reverting pool addresses + whether any path involving the token
/// has ever succeeded (within the decay window) + the last-flagged block.
///
/// The disambiguation between FoT and stale-state:
/// - FoT token: `failing_pools.len() >= K` AND `has_any_success == false`
///   (a permanent token property — fails regardless of which pool).
/// - Stale-state pool: `failing_pools.len() < K` (fails at 1 pool only; the
///   token succeeds through other pools).
#[derive(Debug, Clone, Default)]
pub struct FotTokenRecord {
    /// The distinct V2/V3/V4 pool addresses that reverted involving this
    /// token as the input.
    pub failing_pools: HashSet<Address>,
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

    /// Record that `token` flagged a FoT suspicion at `pool_address` at
    /// `current_block`. Adds the pool to the token's failing-pool set +
    /// updates `last_flagged_block`.
    pub fn record_suspicion(&mut self, token: Address, pool_address: Address, current_block: u64) {
        let record = self.records.entry(token).or_default();
        record.failing_pools.insert(pool_address);
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
        self.assert_not_verified_non_fot(token, confirmed)
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
        let confirmed: Vec<(Address, &FotTokenRecord)> = self
            .records
            .iter()
            .filter(|(_, record)| Self::confirmed_within_window(record, current_block))
            .map(|(token, record)| (*token, record))
            .collect();
        for (token, _) in &confirmed {
            self.assert_not_verified_non_fot(*token, true);
        }
        confirmed
    }

    /// The confirmation predicate shared by `is_fot` + `fot_tokens`.
    fn confirmed_within_window(record: &FotTokenRecord, current_block: u64) -> bool {
        !record.has_any_success
            && record.failing_pools.len() >= FOT_SUSPICION_THRESHOLD_POOLS
            && current_block.saturating_sub(record.last_flagged_block) < FOT_DECAY_BLOCKS
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulator::RevertingFrame;
    use crate::CapturedSwap;
    use alloy::primitives::{address, Bytes};

    const V2_POOL: Address = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    const V3_POOL: Address = address!("cccccccccccccccccccccccccccccccccccccccc");
    const V4_PM: Address = address!("dddddddddddddddddddddddddddddddddddddddd");
    const TOKEN_IN: Address = address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
    const TOKEN_OUT: Address = address!("ffffffffffffffffffffffffffffffffffffffff");

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
    fn v4_hop(pm: Address) -> HopInfo {
        HopInfo::V4(degenbot_executor::composers::V4HopInfo {
            pool_manager_address: pm,
            pool_id_hex: "0xabcd000000000000000000000000000000000000000000000000000000000001"
                .to_string(),
            currency0_address: TOKEN_IN,
            currency1_address: TOKEN_OUT,
            fee: 3000,
            tick_spacing: 60,
            hook_address: Address::ZERO,
            zfo: true,
        })
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
        }
    }

    // =====================================================================
    // reverting-frame attribution (V3 IIA / V4 CurrencyNotSettled)
    // =====================================================================

    #[test]
    fn v3_iia_revert_attributes_to_input_token_zfo_true() {
        let hops = vec![v3_hop(V3_POOL)];
        let f = failure_no_captures("IIA", V3_POOL);
        assert_eq!(fot_suspected_token(&f, &hops), Some((TOKEN_IN, V3_POOL)));
    }

    #[test]
    fn v4_currency_not_settled_attributes_to_input_token() {
        let hops = vec![v4_hop(V4_PM)];
        let f = failure_no_captures("CurrencyNotSettled", V4_PM);
        assert_eq!(fot_suspected_token(&f, &hops), Some((TOKEN_IN, V4_PM)));
    }

    #[test]
    fn v2_hop_zfo_false_attributes_to_token1() {
        // zfo = false → input token is token1 (= TOKEN_IN, the FoT token here).
        let hops = vec![v2_hop_one_for_zero(V2_POOL)];
        let f = failure_no_captures("IIA", V2_POOL);
        assert_eq!(fot_suspected_token(&f, &hops), Some((TOKEN_IN, V2_POOL)));
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
        assert_eq!(fot_suspected_token(&f, &hops), Some((TOKEN_IN, V2_POOL)));
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
            Some((TOKEN_IN, second_pool))
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
        assert_eq!(fot_suspected_token(&f, &hops), Some((TOKEN_IN, V2_POOL)));
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
        assert_eq!(fot_suspected_token(&f, &hops), Some((TOKEN_IN, V3_POOL)));
    }

    #[test]
    fn combined_wrapper_picks_swap_mismatch_when_no_reverting_frame() {
        let hops = vec![v2_hop_zfo(V2_POOL)];
        let f = failure_with_swap_mismatch(
            vec![swap_output_short(degenbot_simulation::SwapFamily::V2, 2950)],
            vec![3000],
        );
        assert_eq!(fot_suspected_token(&f, &hops), Some((TOKEN_IN, V2_POOL)));
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
        reg.record_suspicion(TOKEN_IN, V2_POOL, 100);
        assert!(!reg.is_fot(TOKEN_IN, 100));
    }

    #[test]
    fn registry_k_distinct_pools_flag_token() {
        // 2 distinct failing pools + 0 successes → flagged.
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, V2_POOL, 100);
        reg.record_suspicion(TOKEN_IN, SECOND_POOL, 100);
        assert!(reg.is_fot(TOKEN_IN, 100));
    }

    #[test]
    fn registry_same_pool_twice_does_not_flag() {
        // 2 suspicions at the SAME pool → failing_pools.len() == 1 → not
        // flagged (the disambiguation: stale state fails at 1 pool).
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, V2_POOL, 100);
        reg.record_suspicion(TOKEN_IN, V2_POOL, 101);
        assert!(!reg.is_fot(TOKEN_IN, 101));
    }

    #[test]
    fn registry_success_clears_flag_within_decay_window() {
        // 2 distinct failing pools, but a success was recorded → the
        // 0-success disambiguator keeps it unflagged (a token that ever
        // succeeds is not FoT).
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, V2_POOL, 100);
        reg.record_suspicion(TOKEN_IN, SECOND_POOL, 100);
        assert!(reg.is_fot(TOKEN_IN, 100));
        reg.record_success(TOKEN_IN, 101);
        assert!(!reg.is_fot(TOKEN_IN, 101));
    }

    #[test]
    fn registry_decays_after_clean_window() {
        // A flagged token clears after FOT_DECAY_BLOCKS of clean history.
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, V2_POOL, 100);
        reg.record_suspicion(TOKEN_IN, SECOND_POOL, 100);
        assert!(reg.is_fot(TOKEN_IN, 100));
        assert!(!reg.is_fot(TOKEN_IN, 100 + FOT_DECAY_BLOCKS));
    }

    #[test]
    fn registry_refresh_suspicion_extends_window() {
        // A fresh suspicion pushes the decay window forward.
        let mut reg = FeeOnTransferRegistry::new();
        reg.record_suspicion(TOKEN_IN, V2_POOL, 100);
        reg.record_suspicion(TOKEN_IN, SECOND_POOL, 100);
        // A second suspicion at block 150 → the decay window starts at 150.
        reg.record_suspicion(TOKEN_IN, THIRD_POOL, 150);
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
        reg.record_suspicion(TOKEN_IN, V2_POOL, 100);
        reg.record_suspicion(TOKEN_IN, SECOND_POOL, 100);
        // TOKEN_OUT: 1 pool → not confirmed.
        reg.record_suspicion(TOKEN_OUT, V2_POOL, 100);
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
        reg.record_suspicion(token, V2_POOL, 100);
        reg.record_suspicion(token, SECOND_POOL, 100);
    }

    #[test]
    #[should_panic(expected = "verified non-FoT token confirmed")]
    fn verified_token_confirmed_via_is_fot_panics() {
        // The operator verified TOKEN_IN as standard ERC-20; the classifier
        // confirms it (2 distinct failing pools, 0 successes). Hard invariant:
        // panic, do NOT silently exempt.
        let mut reg = FeeOnTransferRegistry::new();
        reg.set_verified_non_fot([TOKEN_IN].into_iter().collect());
        reg_flagged_at_two_pools(&mut reg, TOKEN_IN);
        let _ = reg.is_fot(TOKEN_IN, 100);
    }

    #[test]
    #[should_panic(expected = "verified non-FoT token confirmed")]
    fn verified_token_confirmed_via_fot_tokens_panics() {
        let mut reg = FeeOnTransferRegistry::new();
        reg.set_verified_non_fot([TOKEN_IN].into_iter().collect());
        reg_flagged_at_two_pools(&mut reg, TOKEN_IN);
        let _ = reg.fot_tokens(100);
    }

    #[test]
    fn verified_token_not_confirmed_does_not_panic() {
        // Verified token with only 1 distinct failing pool (< K) is NOT
        // confirmed, so the guard passes and no panic fires.
        let mut reg = FeeOnTransferRegistry::new();
        reg.set_verified_non_fot([TOKEN_IN].into_iter().collect());
        reg.record_suspicion(TOKEN_IN, V2_POOL, 100);
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
        assert_eq!(hop_output_token(&v4_hop(V4_PM)), TOKEN_OUT);
    }
}
