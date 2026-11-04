//! The hooked-leg black-box re-quote seam (ADR-090 H3).
//!
//! Hop projection admits amount-modifying hooked V4 pools (ADR-090 §1): the
//! search tier ranks them by the standard concentrated-liquidity walk, and
//! [`resolve_hops`](degenbot_substrate::resolve::resolve_hops) unions
//! `Caveats::HOOKED_POOL` onto every path through one. That caveat is the
//! signal this seam consumes: before a finalist is admitted, each hooked leg
//! is priced again — black-box — by executing the chain's own V4 `Quoter` at
//! the solve's anchor (ADR-090 §2, the H1 primitive
//! `degenbot_simulation::V4BlackBoxQuoter`).
//!
//! # Why a seam instead of a call
//!
//! `degenbot-bot` owns the solve pipeline but must not depend on
//! `degenbot-simulation` (the dependency runs the other way: the simulation
//! crate reads `BotState`). So the tier arrives as an installed capability,
//! exactly like [`InlineSimulator`](super::inline_sim::InlineSimulator): the
//! composition root (the `PyO3` shell today) builds the quoter over an
//! anchor-pinned scratch EVM and installs it; a composition root without one
//! simply never re-quotes, and every hooked leg stays approximate.
//!
//! # Fail-soft (ADR-089's pilot philosophy, ADR-090 §4)
//!
//! Nothing here can fail a candidate or a cycle. No installed tier, or a
//! quoter refusal (no scratch EVM, a foreign `PoolManager`, a hook the frame
//! cannot execute) leaves the leg
//! [`Approximate`](HookedLegStage::Approximate): the clamp keeps the
//! white-box figure and the path keeps `Caveats::HOOKED_POOL`, byte-identical
//! to the pre-ADR-090 behavior.
//!
//! # The quote carries the leg's GAS too
//!
//! A re-quote answers with both halves of what a black-box frame measured: the
//! leg's exact output ([`HookedLegQuote::amount_out`]) and the gas that frame
//! burned ([`HookedLegQuote::gas`]) — the `PoolManager` swap AND the hook. The
//! clamp commits the first onto the result's amounts; the admission floor
//! charges the second, because the floor's V4 row is hookless and cannot see
//! the hook at all (`HopGasProfile::Measured`). A refused re-quote carries no
//! gas, so the floor keeps the hookless row and the honest under-prediction.
//!
//! # F2 — the leg's re-quote also MEASURES its charge
//!
//! When the leg's pool carries its charge in its HOOK — keyed at the
//! `DYNAMIC_FEE_FLAG` sentinel, or keyed at zero with an amount-modifying hook
//! ([`degenbot_pools::charge_is_hook_carried`]) — the tier's answer carries the
//! charge it measured beside the exact output: the quote's output against a
//! twin frame of the SAME walk with the hook neutered
//! ([`HookedLegQuote::hook_fee`]). The clamp commits that reading under a fresh
//! guard ([`commit_hook_fees`]) into the search tier's per-direction record, so
//! the NEXT cycle's white-box walk ranks the pool at the charge it actually
//! takes instead of at fee 0 — or, when the chain refuses the direction, does
//! not rank it at all. The record is for RANKING; THIS re-quote stays the
//! exactness backstop (ADR-090 §2 unchanged).
//!
//! # The one hard rule for implementations
//!
//! [`HookedLegReQuote::requote_leg`] is called from the clamp, which holds the
//! `Solver` read guard over [`BotState`] on both of its call sites. An
//! implementation MUST NOT touch `BotState` (no re-entrant state read, no
//! anchor snapshot off the live lock): it prices against its own
//! anchor-pinned scratch EVM and the node. Taking the lock again there is a
//! self-deadlock, not a slow path.

use alloy::primitives::{Address, U256};
use degenbot_core::diag;
use degenbot_pools::solve::MixedPoolRef;
use degenbot_pools::{has_amount_modifying_hook, V4PoolKey};
use degenbot_substrate::BotState;

/// The block a re-quote prices at — the solve's anchor.
///
/// The scratch EVM an implementation builds from this anchor IS the anchor:
/// its layered DB pins the block, and the frame's `number`/`timestamp`/
/// `basefee` come from here. Values are never cached across anchors (ADR-090
/// §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookedLegAnchor {
    /// The solve block every pool state in the candidate reflects.
    pub block: u64,
    /// That block's timestamp.
    pub timestamp: u64,
    /// That block's base fee (wei). The quote frame is a call-shaped replay
    /// whose fee cap only has to clear this.
    pub base_fee: u128,
}

impl HookedLegAnchor {
    /// The anchor a cycle's finalist re-quote prices at: the solve block plus
    /// the block env the cycle already carries.
    #[must_use]
    pub fn at(block: u64, metadata: &degenbot_substrate::BlockMetadata) -> Self {
        Self {
            block,
            timestamp: metadata.timestamp,
            base_fee: u128::from(metadata.base_fee_per_gas.unwrap_or(0)),
        }
    }
}

/// One amount-modifying hooked V4 leg to price black-box.
///
/// Everything the Quoter needs: the `PoolManager` singleton holding the pool,
/// the pool's immutable key (its id is `keccak256(abi.encode(key))`), the
/// direction, and the exact input the clamp committed to this hop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookedLeg {
    /// The `PoolManager` singleton holding this pool.
    pub pool_manager: Address,
    /// The pool's immutable key (currencies, fee, tick spacing, hook address).
    pub pool_key: V4PoolKey,
    /// `true` when currency0 is the input.
    pub zero_for_one: bool,
    /// This hop's committed input (wei), in the input currency's units.
    pub amount_in: U256,
    /// The engine's registered pool id — the key the measured charge is
    /// recorded under (the record IS the search tier's fee input for this
    /// pool's directions).
    pub pool_id: u64,
}

/// One black-box re-quote's answer: the leg's exact output plus the gas its
/// quote frame burned.
///
/// Both halves come off the same frame (the deployed V4 `Quoter` over the
/// anchor), so they describe the same execution — the `PoolManager` swap, the
/// pool's tick walk, and the hook's own `beforeSwap`/`afterSwap` work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookedLegQuote {
    /// The leg's output amount in the output currency's base units — exact by
    /// construction (the chain's own answer).
    pub amount_out: U256,
    /// F2: the charge this leg takes, when the leg's pool carries its charge in
    /// its hook and the tier could measure it (the quote's output against the
    /// same walk with the hook neutered). `None` — a static-fee or hookless
    /// pool, a twin the frame could not run, or a walk with nothing to take a
    /// ratio against — claims no pool fact.
    pub hook_fee: Option<degenbot_pools::HookFeeReading>,
    /// The gas the quote frame burned (`V4BlackBoxQuote::gas_used`) — the same
    /// quantity `docs/gas-estimation.md` fits the floor's V4 row on, so the
    /// admission floor can charge it IN PLACE OF that row.
    ///
    /// The quoter's `gasEstimate` word is deliberately not this: it scales
    /// with the frame's gas limit and is diagnostic only.
    pub gas: u64,
}

/// The installed black-box leg quoter (ADR-090 H1's primitive behind the
/// bot's dependency inversion).
///
/// [`HookedLegAnswer::Unavailable`] is the fail-soft answer: no exact figure is
/// available at this anchor, so the leg stays approximate and nothing is
/// recorded.
pub trait HookedLegReQuote: Send + Sync {
    /// The answer for `leg` at `anchor` — the exact output and the quote frame's
    /// gas when the chain priced the leg, or the direction's refusal when the
    /// chain refused it.
    ///
    /// [`HookedLegAnswer::Unavailable`] is the "no fact at all" arm: no tier at
    /// this point, a manager the quoter does not hold, a frame that never ran.
    /// It must NOT be reported as [`HookedLegAnswer::Refused`] — a refusal is a
    /// FACT about the pool (recorded, and excluded from search in that
    /// direction), while an unavailable tier says nothing about the pool.
    ///
    /// MUST NOT read [`BotState`] — see the module docs.
    fn requote_leg(&self, anchor: HookedLegAnchor, leg: &HookedLeg) -> HookedLegAnswer;
}

/// What one re-quote answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookedLegAnswer {
    /// The chain priced the leg.
    Exact(HookedLegQuote),
    /// The chain REFUSES this leg in this direction: the quote frame reverted
    /// in the hook or the `PoolManager`. No output exists, so the leg can never
    /// be a hop — the strongest admission fact the tier can learn (44% of the
    /// census's pool-direction walks).
    Refused,
    /// The tier has no answer. No fact about the pool: the leg stays
    /// approximate and nothing is recorded.
    Unavailable,
}

/// How one hooked leg staged at the finalist re-quote.
///
/// The ADR-037 vocabulary in `StagedSwapOutcome` terms: `Exact` is the
/// "computed" arm (a real number), `Approximate` is the "hooked pool" arm
/// (the standard-math figure the caveat already flags).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookedLegStage {
    /// The chain's own answer at the anchor — exact by construction, with the
    /// quote frame's measured gas riding along.
    Exact(HookedLegQuote),
    /// The chain refused the leg. The white-box figure stands for this pass (no
    /// output exists to commit) and the path keeps its caveat, but the refusal
    /// is recorded and the direction leaves the search tier.
    Refused,
    /// The white-box approximation stands; the path keeps its caveat.
    Approximate,
}

/// The re-quote verdict for one candidate's hooked legs.
///
/// Empty when the path has no amount-modifying hooked V4 leg — the common
/// case, and the reason a non-hooked candidate pays nothing for this tier.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookedLegPlan {
    /// `(hop index, stage)` for every hooked leg, in path order.
    stages: Vec<(usize, HookedLegStage)>,
    /// The legs themselves, in the same order — the commit's identity source
    /// (the pool id a measured charge is recorded under, and the pool key whose
    /// charge decides whether the fact applies at all).
    legs: Vec<(usize, HookedLeg)>,
}

impl HookedLegPlan {
    /// The empty plan — no hooked leg to re-quote.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            stages: Vec::new(),
            legs: Vec::new(),
        }
    }

    /// Record every leg in `facts` as [`HookedLegStage::Approximate`] — the
    /// fail-soft plan for a call site with no anchor env to quote at.
    #[must_use]
    pub fn approximate(facts: Vec<(usize, HookedLeg)>) -> Self {
        let stages = facts
            .iter()
            .map(|(hop, _leg)| (*hop, HookedLegStage::Approximate))
            .collect();
        Self {
            stages,
            legs: facts,
        }
    }

    /// Price every leg in `facts` with `tier` at `anchor`.
    ///
    /// `tier: None` (not installed) or an
    /// [`Unavailable`](HookedLegAnswer::Unavailable) answer leaves that leg
    /// [`HookedLegStage::Approximate`]; a chain refusal stages
    /// [`HookedLegStage::Refused`] (no output, but a recordable pool fact).
    #[must_use]
    pub fn requote(
        facts: Vec<(usize, HookedLeg)>,
        anchor: HookedLegAnchor,
        tier: Option<&dyn HookedLegReQuote>,
    ) -> Self {
        let mut stages = Vec::with_capacity(facts.len());
        for (hop, leg) in &facts {
            let stage = match tier.map(|tier| tier.requote_leg(anchor, leg)) {
                Some(HookedLegAnswer::Exact(exact)) => HookedLegStage::Exact(exact),
                Some(HookedLegAnswer::Refused) => HookedLegStage::Refused,
                Some(HookedLegAnswer::Unavailable) | None => HookedLegStage::Approximate,
            };
            stages.push((*hop, stage));
        }
        Self {
            stages,
            legs: facts,
        }
    }

    /// The legs the plan re-quoted, in path order.
    #[must_use]
    pub fn legs(&self) -> &[(usize, HookedLeg)] {
        &self.legs
    }

    /// F2: the charge the re-quote measured for `hop` — `None` when the leg was
    /// not re-quoted, the tier answered
    /// [`Unavailable`](HookedLegAnswer::Unavailable), or the pool's charge is
    /// its key's fee (a static-fee or hookless pool has no hook-carried charge
    /// to record).
    #[must_use]
    pub fn fee_reading(&self, hop: usize) -> Option<degenbot_pools::HookFeeReading> {
        self.stages.iter().find_map(|&(i, stage)| match stage {
            HookedLegStage::Exact(quote) if i == hop => quote.hook_fee,
            HookedLegStage::Refused if i == hop => Some(degenbot_pools::HookFeeReading::Refused),
            _ => None,
        })
    }

    /// F2: every `(hop, leg, reading)` the plan measured — the commit's facts.
    #[must_use]
    pub fn fee_readings(&self) -> Vec<(usize, &HookedLeg, degenbot_pools::HookFeeReading)> {
        self.legs
            .iter()
            .filter_map(|(hop, leg)| self.fee_reading(*hop).map(|reading| (*hop, leg, reading)))
            .collect()
    }

    /// The exact output staged for `hop`, if that leg was re-quoted.
    #[must_use]
    pub fn exact_out(&self, hop: usize) -> Option<U256> {
        self.stages.iter().find_map(|&(i, stage)| match stage {
            HookedLegStage::Exact(quote) if i == hop => Some(quote.amount_out),
            _ => None,
        })
    }

    /// The measured quote-frame gas staged for `hop` — `None` for a leg that
    /// was not re-quoted (or is not a hooked leg), which is the clamp's and the
    /// floor's "the approximation stands" signal.
    #[must_use]
    pub fn measured_gas(&self, hop: usize) -> Option<u64> {
        self.stages.iter().find_map(|&(i, stage)| match stage {
            HookedLegStage::Exact(quote) if i == hop => Some(quote.gas),
            _ => None,
        })
    }

    /// The stage recorded for `hop` (`None` = not a hooked leg).
    #[must_use]
    pub fn stage(&self, hop: usize) -> Option<HookedLegStage> {
        self.stages
            .iter()
            .find_map(|&(i, stage)| (i == hop).then_some(stage))
    }

    /// Whether the path carries any hooked leg at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// How many hooked legs the path has.
    #[must_use]
    pub fn hooked_legs(&self) -> usize {
        self.stages.len()
    }

    /// How many of them were re-quoted exact at the anchor.
    #[must_use]
    pub fn exact_legs(&self) -> usize {
        self.stages
            .iter()
            .filter(|(_, stage)| matches!(stage, HookedLegStage::Exact(_)))
            .count()
    }

    /// ADR-090 §4: the caveat lifts off the quote only when EVERY hooked leg
    /// was re-quoted exact at the anchor. One approximate leg (tier
    /// unavailable, a refusal, a hook the frame cannot execute) keeps the
    /// whole path flagged — the settle risk of a mixed path is the sum of its
    /// legs' trust.
    #[must_use]
    pub fn lifts_caveat(&self) -> bool {
        !self.stages.is_empty() && self.exact_legs() == self.stages.len()
    }
}

/// The amount-modifying hooked V4 legs of one candidate, with the input each
/// hop was committed to ("the facts", read under the caller's short core
/// guard).
///
/// Split from [`HookedLegPlan::requote`] because the re-quote itself makes
/// provider calls: the caller drops the state guard between the two so no
/// RPC ever rides a `BotState` read lock.
#[must_use]
pub fn hooked_leg_facts(
    core: &BotState,
    pools: &[MixedPoolRef],
    consumed_inputs: &[U256],
) -> Vec<(usize, HookedLeg)> {
    let mut facts = Vec::new();
    for (hop, pool_ref) in pools.iter().enumerate() {
        if pool_ref.hop_type != degenbot_pools::solve::HopType::V4 {
            continue;
        }
        let Some(identity) = core.pool_slice::<degenbot_pools::V4PoolIdentity>(pool_ref.pool_key)
        else {
            continue;
        };
        if !has_amount_modifying_hook(identity.pool_key.hooks) {
            continue;
        }
        let Some(&amount_in) = consumed_inputs.get(hop) else {
            continue;
        };
        facts.push((
            hop,
            HookedLeg {
                pool_manager: identity.pool_manager,
                pool_key: identity.pool_key.clone(),
                zero_for_one: pool_ref.zero_for_one,
                amount_in,
                pool_id: pool_ref.pool_key,
            },
        ));
    }
    facts
}

/// F2: commit every charge the plan measured into the search tier's
/// per-direction record, stamped with the anchor block it was measured at.
///
/// Returns how many pools' records the write changed. A leg whose pool carries
/// its charge in its KEY (a static fee, hookless or not) records nothing: the
/// record governs only [`degenbot_pools::charge_is_hook_carried`] pools, so a
/// re-quote of every other leg touches no state and takes no write guard.
pub fn commit_hook_fees(core: &mut BotState, plan: &HookedLegPlan, at_block: u64) -> usize {
    let mut written = 0;
    for (_hop, leg, reading) in plan.fee_readings() {
        if !degenbot_pools::charge_is_hook_carried(&leg.pool_key) {
            continue;
        }
        if core.record_hook_fee(leg.pool_id, leg.zero_for_one, reading, at_block) {
            written += 1;
            diag!(
                domain = solver,
                pool_id = leg.pool_id,
                zero_for_one = leg.zero_for_one,
                ?reading,
                block_number = at_block,
                "hook-carried charge recorded — the search tier ranks this direction at it"
            );
        }
    }
    written
}

/// The clamp's commit door: take the state WRITE guard only when there is a
/// fact to commit, so a path with no hook-carried leg (the common case) pays no
/// write at all.
///
/// The clamp's re-quote runs with the read guard DROPPED (the tier prices
/// against its own scratch EVM), so this is a fresh acquisition after it — never
/// a re-entrant one.
pub fn commit_plan_hook_fees(
    core: &std::sync::Arc<degenbot_substrate::state_lock::StateLock<BotState>>,
    plan: &HookedLegPlan,
    at_block: u64,
) -> usize {
    if plan.fee_readings().is_empty() {
        return 0;
    }
    let mut core = core.write_at(degenbot_substrate::state_lock::LockSite::Solver);
    commit_hook_fees(&mut core, plan, at_block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    fn anchor() -> HookedLegAnchor {
        HookedLegAnchor {
            block: 26_320_000,
            timestamp: 1_700_000_000,
            base_fee: 7,
        }
    }

    fn leg(amount_in: u64) -> HookedLeg {
        HookedLeg {
            pool_manager: address!("000000000004444c5dc75cB358380D2e3dE08A90"),
            pool_key: V4PoolKey {
                currency0: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
                currency1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
                fee: 3_000,
                tick_spacing: 60,
                hooks: address!("322dcEc4958C14e021A9F1cD49DF11b9457968cC"),
            },
            zero_for_one: true,
            amount_in: U256::from(amount_in),
            pool_id: 7,
        }
    }

    /// A tier that answers `amount_in + 7` — a figure the white-box walk
    /// cannot produce, so the arm is visible.
    struct StubTier {
        answer: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    const STUB_GAS: u64 = 184_500;

    impl HookedLegReQuote for StubTier {
        fn requote_leg(&self, _anchor: HookedLegAnchor, leg: &HookedLeg) -> HookedLegAnswer {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.answer {
                HookedLegAnswer::Exact(HookedLegQuote {
                    amount_out: leg.amount_in + U256::from(7u64),
                    gas: STUB_GAS,
                    // The leg's pool carries its charge in its KEY (fee 3,000),
                    // so the tier measures no hook-carried charge.
                    hook_fee: None,
                })
            } else {
                HookedLegAnswer::Unavailable
            }
        }
    }

    /// TIER PRESENT: the leg stages Exact with the black-box figure.
    #[test]
    fn a_present_tier_stages_the_exact_figure() {
        let tier = StubTier {
            answer: true,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let plan = HookedLegPlan::requote(vec![(1, leg(1_000))], anchor(), Some(&tier));
        assert_eq!(
            plan.stage(1),
            Some(HookedLegStage::Exact(HookedLegQuote {
                amount_out: U256::from(1_007u64),
                gas: STUB_GAS,
                hook_fee: None,
            }))
        );
        assert_eq!(plan.exact_out(1), Some(U256::from(1_007u64)));
        assert_eq!(
            plan.measured_gas(1),
            Some(STUB_GAS),
            "the quote frame's gas rides the staged figure"
        );
        assert_eq!(
            plan.measured_gas(0),
            None,
            "a hop that is not a hooked leg carries no measured gas"
        );
        assert_eq!(plan.hooked_legs(), 1);
        assert_eq!(plan.exact_legs(), 1);
        assert!(plan.lifts_caveat(), "every hooked leg was priced exact");
        assert_eq!(tier.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// TIER ABSENT: the leg stages Approximate and the caveat stays. No gas is
    /// claimed either — the floor has nothing measured to charge.
    #[test]
    fn an_absent_tier_leaves_the_leg_approximate() {
        let plan = HookedLegPlan::requote(vec![(0, leg(1_000))], anchor(), None);
        assert_eq!(plan.stage(0), Some(HookedLegStage::Approximate));
        assert_eq!(plan.exact_out(0), None);
        assert_eq!(plan.measured_gas(0), None);
        assert!(!plan.lifts_caveat(), "an approximate leg keeps the caveat");
    }

    /// A REFUSING tier (no scratch / foreign manager / halted frame) is
    /// fail-soft: Approximate, never an error.
    #[test]
    fn a_refusing_tier_leaves_the_leg_approximate() {
        let tier = StubTier {
            answer: false,
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let plan = HookedLegPlan::requote(vec![(0, leg(9)), (2, leg(11))], anchor(), Some(&tier));
        assert_eq!(plan.stage(0), Some(HookedLegStage::Approximate));
        assert_eq!(plan.stage(2), Some(HookedLegStage::Approximate));
        assert_eq!(plan.measured_gas(0), None, "a refusal claims no gas");
        assert_eq!(plan.measured_gas(2), None);
        assert_eq!(plan.hooked_legs(), 2);
        assert_eq!(plan.exact_legs(), 0);
        assert!(!plan.lifts_caveat());
        assert_eq!(tier.calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    /// A mixed plan (one exact, one approximate) keeps the caveat: the quote
    /// is only exact when every hooked leg was priced at the anchor.
    #[test]
    fn a_mixed_plan_keeps_the_caveat() {
        struct OneShot {
            seen: std::sync::atomic::AtomicUsize,
        }
        impl HookedLegReQuote for OneShot {
            fn requote_leg(&self, _anchor: HookedLegAnchor, leg: &HookedLeg) -> HookedLegAnswer {
                let n = self.seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n == 0 {
                    HookedLegAnswer::Exact(HookedLegQuote {
                        amount_out: leg.amount_in,
                        gas: STUB_GAS,
                        hook_fee: None,
                    })
                } else {
                    HookedLegAnswer::Unavailable
                }
            }
        }
        let tier = OneShot {
            seen: std::sync::atomic::AtomicUsize::new(0),
        };
        let plan = HookedLegPlan::requote(vec![(0, leg(5)), (1, leg(6))], anchor(), Some(&tier));
        assert_eq!(plan.exact_legs(), 1);
        assert!(!plan.lifts_caveat());
        assert_eq!(plan.measured_gas(0), Some(STUB_GAS), "the exact leg");
        assert_eq!(plan.measured_gas(1), None, "the refused leg");
    }

    /// An empty plan never lifts the caveat and answers nothing.
    #[test]
    fn the_empty_plan_is_inert() {
        let plan = HookedLegPlan::empty();
        assert!(plan.is_empty());
        assert_eq!(plan.hooked_legs(), 0);
        assert!(!plan.lifts_caveat());
        assert_eq!(plan.exact_out(0), None);
        assert_eq!(plan.measured_gas(0), None);
    }
}
