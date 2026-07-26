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

use alloy::primitives::{Address, U256};
use degenbot_executor::composers::HopInfo;

use crate::pool_divergence::{captured_swap_output, is_solver_calc_failure};
use crate::simulator::SimFailure;

/// The set of `reverting_frame.label` values that indicate a fee-on-transfer
/// token consumed the input mid-swap. Sourced from
/// `degenbot_decoders::revert::classify_revert` (the bare base names after
/// `lookup`'s `.split('(').next()` normalization).
const FOT_REVERT_LABELS: &[&str] = &["IIA", "CurrencyNotSettled"];

/// Attribute a `SimFailure` to the input token of the failing hop, if the
/// failure's `reverting_frame.label` is a FoT signature (`IIA` for V3,
/// `CurrencyNotSettled` for V4) — returns `None` for non-FoT-classifiable
/// failures, missing `reverting_frame`, or when the reverting pool's address
/// cannot be matched to a hop in `hops`.
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
) -> Option<Address> {
    let frame = failure.reverting_frame.as_ref()?;
    if !FOT_REVERT_LABELS.contains(&frame.label.as_str()) {
        return None;
    }
    hop_input_token_for_target(hops, frame.target)
}

/// The V2 non-reverting FoT case — the swap committed, K-invariant held (no
/// revert), but the captured swap output is SHORTER than the solver's
/// `hop_outputs[i]` because the fee ate some of the input. Reuses the existing
/// `is_solver_calc_failure` mismatch path + attributes via the mismatching
/// hop's input token. Returns `None` when the failure is not
/// `SolverCalc`-class or the mismatching hop isn't found.
///
/// Mirrors `diverging_pool_keys` — zips captured_swaps ↔ hop_outputs ↔ hops
/// (the triple-length guard), returns the input token of the first mismatch
/// (the typical single-FoT-hop case; multiple would be unusual + the caller
/// can extend to `Vec` if needed).
#[must_use]
pub fn fot_suspected_token_from_swap_mismatch(
    failure: &SimFailure,
    hops: &[HopInfo],
) -> Option<Address> {
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
                Some(hop_input_token(hop))
            }
        })
}

/// Convenience wrapper — the V3/V4 reverting case OR the V2 swap-mismatch
/// case, whichever fires first (the V2 case requires `captured_swaps`
/// populated, which the V3/V4 root-frame revert empty-captures, so the two
/// are mutually exclusive in practice).
#[must_use]
pub fn fot_suspected_token(failure: &SimFailure, hops: &[HopInfo]) -> Option<Address> {
    fot_suspected_token_from_reverting_frame(failure, hops)
        .or_else(|| fot_suspected_token_from_swap_mismatch(failure, hops))
}

/// The input token for a hop, selected by its `zfo` direction (`token0` if
/// `zfo`, else `token1`). Returns `Some` for every hop variant — kept
/// separate from the target-matching lookup so a future "V2 only"
/// attribution can reuse it.
fn hop_input_token(hop: &HopInfo) -> Address {
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
            optimal_input: 1000,
            hop_outputs: Vec::new(),
        }
    }

    // =====================================================================
    // reverting-frame attribution (V3 IIA / V4 CurrencyNotSettled)
    // =====================================================================

    #[test]
    fn v3_iia_revert_attributes_to_input_token_zfo_true() {
        let hops = vec![v3_hop(V3_POOL)];
        let f = failure_no_captures("IIA", V3_POOL);
        assert_eq!(fot_suspected_token(&f, &hops), Some(TOKEN_IN));
    }

    #[test]
    fn v4_currency_not_settled_attributes_to_input_token() {
        let hops = vec![v4_hop(V4_PM)];
        let f = failure_no_captures("CurrencyNotSettled", V4_PM);
        assert_eq!(fot_suspected_token(&f, &hops), Some(TOKEN_IN));
    }

    #[test]
    fn v2_hop_zfo_false_attributes_to_token1() {
        // zfo = false → input token is token1 (= TOKEN_IN, the FoT token here).
        let hops = vec![v2_hop_one_for_zero(V2_POOL)];
        let f = failure_no_captures("IIA", V2_POOL);
        assert_eq!(fot_suspected_token(&f, &hops), Some(TOKEN_IN));
    }

    #[test]
    fn non_fot_label_returns_none() {
        let hops = vec![v3_hop(V3_POOL)];
        let f = failure_no_captures("PoolNotInitialized", V3_POOL);
        assert_eq!(fot_suspected_token(&f, &hops), None);
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
        assert_eq!(fot_suspected_token(&f, &hops), Some(TOKEN_IN));
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
            optimal_input: 1000,
            hop_outputs,
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
        assert_eq!(fot_suspected_token(&f, &hops), Some(TOKEN_IN));
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
        assert_eq!(fot_suspected_token(&f, &hops), Some(TOKEN_IN));
    }

    #[test]
    fn combined_wrapper_picks_swap_mismatch_when_no_reverting_frame() {
        let hops = vec![v2_hop_zfo(V2_POOL)];
        let f = failure_with_swap_mismatch(
            vec![swap_output_short(degenbot_simulation::SwapFamily::V2, 2950)],
            vec![3000],
        );
        assert_eq!(fot_suspected_token(&f, &hops), Some(TOKEN_IN));
    }
}
