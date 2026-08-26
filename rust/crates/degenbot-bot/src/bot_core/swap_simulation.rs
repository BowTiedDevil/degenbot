//! The swap-simulation gate (ADR-037): ONE owner of "simulate a swap
//! against current pool state".
//!
//! Every simulation read goes through [`BotState::swap_simulation`] —
//! replacing the former `*_miss_aware` / `*_with_fetch` method twins,
//! `calculate_tokens_in`, and the override path's ad-hoc retry shell. This
//! module owns three things:
//!
//! 1. the **request** shape ([`SwapRequest`], signed user-perspective),
//! 2. the **outcome** shapes ([`SwapRead`] / [`SwapOutcome`], typed failure
//!    modes instead of silent `U256::ZERO`),
//! 3. the single **fetch → merge → retry** policy for sparse tick-map misses
//!    ([`ComputeMerge`] / [`drive`]), generic over the merge target so the
//!    override path can reuse it without touching registered state.
//!
//! The pure family math stays in `degenbot-pools` (`simulate_swap`,
//! `v3_simulate_swap`, `v4_simulate_swap`).
//!
//! ## Sign convention (ADR-037)
//!
//! `SwapRequest::amount_specified` follows the **user perspective**: positive
//! = exact-output (the pool delivers that magnitude to the user), negative =
//! exact-input (the user sends that magnitude). Neither engine's internal
//! convention is canonical at this seam; [`engine_amount_specified`] maps to
//! each family in exactly one place (V3 negates both directions vs canonical;
//! V4 is identity).

use std::collections::HashSet;
use std::sync::Arc;

use alloy::primitives::{I256, U256};

use ::degenbot_pools::registry::PoolEntry;
use ::degenbot_pools::simulate_swap::simulate_swap;
use ::degenbot_pools::tick_fetch::{FetchedTickWord, TickWordFetcher};
use ::degenbot_pools::v3_state::{
    v3_simulate_swap, PoolTickCoverage, SimulateSwapError, V3PoolState, V3SwapOutcome,
};
use ::degenbot_pools::v4_state::v4_simulate_swap;

use super::BotState;

/// A swap-simulation request over registered pool state.
///
/// `amount_specified` is **signed, user perspective** (ADR-037):
/// - **positive** → exact-OUTPUT swap of that magnitude (the pool delivers);
/// - **negative** → exact-INPUT swap of that magnitude (the user sends).
///
/// Outcome deltas (`ClSwapOutcome::consumed` / `delivered`) use the same
/// perspective: negative = taken from the user, positive = given to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapRequest {
    /// Direction of the swap (token0 → token1 when `true`).
    pub zero_for_one: bool,
    /// Signed requested amount — see the type-level sign convention.
    pub amount_specified: I256,
    /// Price-limit bound for the walk; `None` selects the family default
    /// ([`V3PoolState::default_sqrt_price_limit`]).
    pub sqrt_price_limit: Option<U256>,
}

/// Additive trust flags carried on every [`SwapOutcome`] variant.
///
/// An EMPTY set means "this number is exact". The set is deliberately
/// `#[non_exhaustive]`: new inaccuracy sources become new flags, never a
/// signature change. (A `HOOKED_POOL` flag is reserved by ADR-037 and wired
/// up by ergo task X4EU3J; admission currently rejects hooked pools.)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Caveats(u8);

impl Caveats {
    /// Sparse tick coverage: the walk may have traversed unfetched regions.
    pub const SPARSE_COVERAGE: Self = Self(1 << 0);

    /// No caveats — the outcome is trustworthy.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether `self` carries all flags in `other`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Thin payload for constant-product families (V2, Curve, Balancer,
/// Aerodrome). Amounts are signed user-perspective deltas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct V2SwapOutcome {
    /// Taken FROM the user (negative magnitude of the input sent).
    pub consumed: I256,
    /// Given TO the user (positive magnitude of the output delivered).
    pub delivered: I256,
    /// Constant-product families carry no sparse-tick risk today.
    pub caveats: Caveats,
}

/// Rich payload for concentrated-liquidity swaps (V3/V4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClSwapOutcome {
    /// Taken FROM the user (negative; `-input_consumed`).
    pub consumed: I256,
    /// Given TO the user (positive; token1 for zfo swaps, token0 otherwise).
    pub delivered: I256,
    /// Final `sqrtPriceX96` after the walk (end-state reconstruction).
    pub end_sqrt_price_x96: U256,
    /// Final active liquidity after the walk.
    pub end_liquidity: u128,
    /// Final tick after the walk.
    pub end_tick: i32,
    /// Gross input actually converted by the pool (solver clamp bound — see
    /// `V3SwapOutcome::input_consumed`). Unsigned input-token units.
    pub input_consumed: U256,
    /// Tick-bitmap words fetched during miss recovery, in fetch order.
    pub fetched_words: Vec<i32>,
    /// Trust flags (see [`Caveats`]).
    pub caveats: Caveats,
}

/// Per-family simulation payloads (invalid states unrepresentable: a V2 hop
/// has no end-state detail to fake; CL outcomes always carry full state).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwapOutcome {
    /// Constant-product family result.
    V2(V2SwapOutcome),
    /// V3 concentrated-liquidity result.
    V3(ClSwapOutcome),
    /// V4 concentrated-liquidity result.
    V4(ClSwapOutcome),
}

/// The typed result of [`BotState::swap_simulation`].
///
/// Formerly-silent failures are observable variants (hard cutover per
/// ADR-037 — no bare `U256::ZERO` / `Option::None` collapse):
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwapRead {
    /// The swap computed; payload per family.
    Computed(SwapOutcome),
    /// Zero amount, unregistered pool, non-computable arithmetic/invariant,
    /// or an exact-output request against a constant-product family.
    NotComputable,
    /// Miss recovery ran but the fetch itself failed for `word`.
    FetchFailed { word: i32 },
    /// Miss recovery was impossible or exhausted for `word` (no fetcher
    /// registered, or the same word missed twice after a merge).
    FetchExhausted { word: i32 },
}

/// Which engine a mapped `amountSpecified` feeds (sign-convention table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineFamily {
    V3Engine,
    V4Engine,
}

/// Map a canonical user-perspective request amount onto an engine's
/// internal `amountSpecified` convention — THE single mapping site
/// (ADR-037): V3 negates both directions vs canonical; V4 is identity.
#[must_use]
pub(crate) fn engine_amount_specified(request_amount: I256, family: EngineFamily) -> I256 {
    match family {
        EngineFamily::V3Engine => -request_amount,
        EngineFamily::V4Engine => request_amount,
    }
}

/// Trust flags implied by a pool's registration-time tick coverage.
#[must_use]
pub(crate) fn caveats_for_coverage(coverage: PoolTickCoverage) -> Caveats {
    match coverage {
        // Tracked snapshots provided complete tick data — solver results are
        // trustworthy (glossary). Sparse results may be inaccurate.
        PoolTickCoverage::Tracked => Caveats::default(),
        PoolTickCoverage::Sparse => Caveats::SPARSE_COVERAGE,
    }
}

/// Convert an unsigned engine amount into the user-perspective delta.
/// Amounts are physically bounded far below `I256::MAX`; saturation there is
/// a defensive clamp, not a reachable state.
fn unsigned_to_user_delta(x: U256) -> I256 {
    I256::try_from(x).unwrap_or(I256::MAX)
}

/// Build the rich CL payload from a raw engine outcome.
#[must_use]
pub(crate) fn cl_payload(
    outcome: &V3SwapOutcome,
    zero_for_one: bool,
    caveats: Caveats,
    fetched_words: Vec<i32>,
) -> ClSwapOutcome {
    let delivered_raw = if zero_for_one {
        outcome.amount1
    } else {
        outcome.amount0
    };
    ClSwapOutcome {
        consumed: -unsigned_to_user_delta(outcome.input_consumed),
        delivered: unsigned_to_user_delta(delivered_raw),
        end_sqrt_price_x96: outcome.sqrt_price_x96,
        end_liquidity: outcome.liquidity,
        end_tick: outcome.tick,
        input_consumed: outcome.input_consumed,
        fetched_words,
        caveats,
    }
}

/// The policy driver's contract: recompute against a (possibly mutated)
/// target, merge fetched words into it, and surface its stored fetcher.
/// Splitting compute/merge/fetch across `&self` / `&mut self` methods keeps
/// borrows sequenced per call — the reentrancy discipline the old tripled
/// loops encoded by hand (fetcher cloned off the entry before looping).
pub(crate) trait ComputeMerge {
    /// One simulation attempt against the current target state.
    fn compute(&self) -> Result<V3SwapOutcome, SimulateSwapError>;
    /// Merge a fetched word into the target state (registered OR transient —
    /// the override path never merges into registered `BotState`).
    fn merge_word(&mut self, fetched: &FetchedTickWord);
    /// Attempt the RPC-side recovery for `word`. `Err(NoFetcher)` means no
    /// fetcher is registered (⇒ [`SwapRead::FetchExhausted`]); `Err(Errored)`
    /// means the recovery itself failed (⇒ [`SwapRead::FetchFailed`]).
    fn fetch_word(&self, word: i32, block: u64) -> Result<FetchedTickWord, FetchFailure>;
}

/// Why a missing-word recovery could not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FetchFailure {
    /// No fetcher registered for this pool.
    NoFetcher,
    /// The fetcher ran and failed.
    Errored,
}

/// Internal drive result before payload assembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PolicyAttempt {
    Computed(V3SwapOutcome, Vec<i32>),
    NotComputable,
    FetchFailed(i32),
    FetchExhausted(i32),
}

/// THE fetch → merge → retry loop (formerly copy-pasted three times).
///
/// Dedup rule: a second miss on the SAME word means the previous merge did
/// not satisfy the walker — give up rather than loop forever. No-fetcher
/// and exhausted-miss collapse to `FetchExhausted`; a fetcher error is
/// `FetchFailed`. All four legacy failure collapses are observable here.
pub(crate) fn drive(sim: &mut impl ComputeMerge, block: u64) -> PolicyAttempt {
    let mut attempted: HashSet<i32> = HashSet::new();
    let mut fetched_words: Vec<i32> = Vec::new();
    loop {
        match sim.compute() {
            Ok(outcome) => return PolicyAttempt::Computed(outcome, fetched_words),
            Err(SimulateSwapError::NotComputable) => return PolicyAttempt::NotComputable,
            Err(SimulateSwapError::MissingTickWord(word)) => {
                if !attempted.insert(word) {
                    return PolicyAttempt::FetchExhausted(word);
                }
                match sim.fetch_word(word, block) {
                    Ok(data) => {
                        sim.merge_word(&data);
                        fetched_words.push(word);
                    }
                    Err(FetchFailure::NoFetcher) => return PolicyAttempt::FetchExhausted(word),
                    Err(FetchFailure::Errored) => return PolicyAttempt::FetchFailed(word),
                }
            }
        }
    }
}

/// One simulation attempt against REGISTERED pool state (`pool_id`). The
/// compute re-resolves the entry each call so a mid-loop fetch-merge is
/// visible on the next attempt; borrows are sequenced through `&self` /
/// `&mut self` rather than competing closures.
pub(crate) struct RegisteredClSim<'a> {
    pub state: &'a mut BotState,
    pub pool_id: u64,
    pub zero_for_one: bool,
    /// Engine-mapped `amountSpecified` (see [`engine_amount_specified`]).
    pub spec: I256,
    pub limit: U256,
}

impl ComputeMerge for RegisteredClSim<'_> {
    fn compute(&self) -> Result<V3SwapOutcome, SimulateSwapError> {
        let Some(entry) = self.state.pools.get(&self.pool_id) else {
            return Err(SimulateSwapError::NotComputable);
        };
        match entry {
            PoolEntry::V3(identity, st) => v3_simulate_swap(
                st,
                identity.fee,
                identity.tick_spacing,
                self.zero_for_one,
                self.spec,
                self.limit,
            ),
            PoolEntry::V4(identity, st) => v4_simulate_swap(
                st,
                identity.pool_key.fee,
                identity.pool_key.tick_spacing,
                self.zero_for_one,
                self.spec,
                self.limit,
            ),
            _ => Err(SimulateSwapError::NotComputable),
        }
    }

    fn merge_word(&mut self, fetched: &FetchedTickWord) {
        self.state.merge_tick_word(self.pool_id, fetched);
    }

    fn fetch_word(&self, word: i32, block: u64) -> Result<FetchedTickWord, FetchFailure> {
        // Clone the stored fetcher off the registered V3/V4 state BEFORE any
        // mutation (avoids the self-referential borrow hazard the legacy
        // loops documented at bot_core/mod.rs:811).
        let fetcher: Option<Arc<dyn TickWordFetcher>> = match self.state.pools.get(&self.pool_id) {
            Some(PoolEntry::V3(_, st)) => st.fetcher.clone(),
            Some(PoolEntry::V4(_, st)) => st.fetcher.clone(),
            _ => None,
        };
        let Some(fetcher) = fetcher else {
            return Err(FetchFailure::NoFetcher);
        };
        fetcher
            .fetch_missing_tick_word(self.pool_id, word, block)
            .map_err(|_| FetchFailure::Errored)
    }
}

impl BotState {
    /// Simulate a swap against current registered pool state — THE swap-read
    /// interface (ADR-037). See module docs for the sign convention and the
    /// typed failure modes.
    ///
    /// `block` is the fetch context threaded into the tick-word fetcher on
    /// sparse-miss recovery; it does not affect pure computation.
    pub fn swap_simulation(&mut self, block: u64, pool_id: u64, request: SwapRequest) -> SwapRead {
        if request.amount_specified.is_zero() || !self.pools.contains_key(&pool_id) {
            return SwapRead::NotComputable;
        }
        let magnitude = request.amount_specified.into_sign_and_abs().1;
        let exact_output = request.amount_specified.is_positive();

        let Some(entry) = self.pools.get(&pool_id) else {
            return SwapRead::NotComputable;
        };
        match entry {
            PoolEntry::V2(..)
            | PoolEntry::Curve(..)
            | PoolEntry::BalancerWeighted(..)
            | PoolEntry::BalancerStable(..)
            | PoolEntry::AerodromeV2(..) => {
                // Constant-product families: exact-input only (an exact-output
                // request was NotComputable on every legacy path too).
                if exact_output {
                    return SwapRead::NotComputable;
                }
                match simulate_swap(entry, request.zero_for_one, magnitude) {
                    Ok(out) => SwapRead::Computed(SwapOutcome::V2(V2SwapOutcome {
                        consumed: -unsigned_to_user_delta(magnitude),
                        delivered: unsigned_to_user_delta(out),
                        caveats: Caveats::default(),
                    })),
                    Err(_) => SwapRead::NotComputable,
                }
            }
            PoolEntry::V3(_, v3_state) => {
                let coverage = v3_state.coverage;
                let family = EngineFamily::V3Engine;
                let limit = request
                    .sqrt_price_limit
                    .unwrap_or_else(|| V3PoolState::default_sqrt_price_limit(request.zero_for_one));
                let mut sim = RegisteredClSim {
                    state: self,
                    pool_id,
                    zero_for_one: request.zero_for_one,
                    spec: engine_amount_specified(request.amount_specified, family),
                    limit,
                };
                finish_cl(&mut sim, block, SwapOutcomeFamily::V3, coverage, request)
            }
            PoolEntry::V4(_, v4_state) => {
                let coverage = v4_state.coverage;
                let family = EngineFamily::V4Engine;
                let limit = request
                    .sqrt_price_limit
                    .unwrap_or_else(|| V3PoolState::default_sqrt_price_limit(request.zero_for_one));
                let mut sim = RegisteredClSim {
                    state: self,
                    pool_id,
                    zero_for_one: request.zero_for_one,
                    spec: engine_amount_specified(request.amount_specified, family),
                    limit,
                };
                finish_cl(&mut sim, block, SwapOutcomeFamily::V4, coverage, request)
            }
        }
    }
}

/// Which outcome variant a CL drive should assemble.
#[derive(Clone, Copy)]
enum SwapOutcomeFamily {
    V3,
    V4,
}

fn finish_cl(
    sim: &mut impl ComputeMerge,
    block: u64,
    family: SwapOutcomeFamily,
    coverage: PoolTickCoverage,
    request: SwapRequest,
) -> SwapRead {
    match drive(sim, block) {
        PolicyAttempt::Computed(outcome, fetched_words) => {
            let caveats = caveats_for_coverage(coverage);
            let payload = cl_payload(&outcome, request.zero_for_one, caveats, fetched_words);
            SwapRead::Computed(match family {
                SwapOutcomeFamily::V3 => SwapOutcome::V3(payload),
                SwapOutcomeFamily::V4 => SwapOutcome::V4(payload),
            })
        }
        PolicyAttempt::NotComputable => SwapRead::NotComputable,
        PolicyAttempt::FetchFailed(word) => SwapRead::FetchFailed { word },
        PolicyAttempt::FetchExhausted(word) => SwapRead::FetchExhausted { word },
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    #![allow(clippy::used_underscore_binding)]

    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---------------------------------------------------------------------
    // Sign-convention table (ADR-037): THE pinning test. Canonical request
    // sign -> engine amountSpecified, for all four shapes x both engines.
    #[test]
    fn sign_mapping_table() {
        let in_pos = I256::try_from(1_000_u64).unwrap();
        let out_neg = -I256::try_from(2_000_u64).unwrap();
        // exact-input request (negative, user sends 2000):
        assert_eq!(
            engine_amount_specified(out_neg, EngineFamily::V3Engine),
            I256::try_from(2_000_u64).unwrap()
        );
        assert_eq!(
            engine_amount_specified(out_neg, EngineFamily::V4Engine),
            out_neg
        );
        // exact-output request (positive, pool delivers 1000):
        assert_eq!(
            engine_amount_specified(in_pos, EngineFamily::V3Engine),
            -in_pos
        );
        assert_eq!(
            engine_amount_specified(in_pos, EngineFamily::V4Engine),
            in_pos
        );
    }

    // ---------------------------------------------------------------------
    // Caveats: empty-means-exact, additive union.
    #[test]
    fn caveats_bit_semantics() {
        assert!(Caveats::default().is_empty());
        let sparse = Caveats::SPARSE_COVERAGE;
        assert!(!sparse.is_empty());
        assert!(sparse.contains(Caveats::SPARSE_COVERAGE));
        assert_eq!(sparse.union(sparse), sparse);
    }

    #[test]
    fn coverage_to_caveats() {
        assert!(caveats_for_coverage(PoolTickCoverage::Tracked).is_empty());
        assert!(caveats_for_coverage(PoolTickCoverage::Sparse).contains(Caveats::SPARSE_COVERAGE));
    }

    // ---------------------------------------------------------------------
    // CL payload: user-perspective signs from unsigned engine amounts.
    #[test]
    fn cl_payload_signs_follow_user_perspective() {
        let outcome = V3SwapOutcome {
            amount0: U256::from(500_u64),
            amount1: U256::from(700_u64),
            sqrt_price_x96: U256::from(42_u64),
            liquidity: 9,
            tick: -3,
            input_consumed: U256::from(1_234_u64),
        };
        let payload = cl_payload(&outcome, true, Caveats::SPARSE_COVERAGE, vec![7]);
        assert_eq!(payload.consumed, -I256::try_from(1_234_u64).unwrap());
        assert_eq!(payload.delivered, I256::try_from(700_u64).unwrap());
        assert_eq!(payload.end_tick, -3);
        assert_eq!(payload.fetched_words, vec![7]);
        assert!(payload.caveats.contains(Caveats::SPARSE_COVERAGE));

        let ofz = cl_payload(&outcome, false, Caveats::default(), Vec::new());
        assert_eq!(ofz.delivered, I256::try_from(500_u64).unwrap());
    }

    // ---------------------------------------------------------------------
    // Policy drive: a scripted ComputeMerge exercises every branch without
    // constructing real pools.
    struct ScriptedSim {
        script: Vec<Result<V3SwapOutcome, SimulateSwapError>>,
        call: AtomicUsize,
        fetcher_ok: bool,
        has_fetcher: bool,
        fetched: Vec<FetchedTickWord>,
    }

    impl ScriptedSim {
        fn word(word: i32) -> FetchedTickWord {
            FetchedTickWord {
                word,
                ticks: HashMap::new(),
            }
        }
    }

    impl ComputeMerge for ScriptedSim {
        fn compute(&self) -> Result<V3SwapOutcome, SimulateSwapError> {
            let idx = self.call.load(Ordering::SeqCst);
            self.call.store(idx + 1, Ordering::SeqCst);
            self.script
                .get(idx)
                .cloned()
                .unwrap_or(Err(SimulateSwapError::NotComputable))
        }
        fn merge_word(&mut self, fetched: &FetchedTickWord) {
            self.fetched.push(fetched.clone());
        }
        fn fetch_word(&self, _word: i32, _block: u64) -> Result<FetchedTickWord, FetchFailure> {
            if !self.has_fetcher {
                Err(FetchFailure::NoFetcher)
            } else if self.fetcher_ok {
                Ok(Self::word(_word))
            } else {
                Err(FetchFailure::Errored)
            }
        }
    }

    fn outcome_stub() -> V3SwapOutcome {
        V3SwapOutcome::default()
    }

    #[test]
    fn drive_immediate_success() {
        let mut sim = ScriptedSim {
            script: vec![Ok(outcome_stub())],
            call: AtomicUsize::new(0),
            fetcher_ok: true,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        match drive(&mut sim, 17) {
            PolicyAttempt::Computed(_, words) => assert!(words.is_empty()),
            other => panic!("expected Computed, got {other:?}"),
        }
        assert!(sim.fetched.is_empty());
    }

    #[test]
    fn drive_miss_then_fetch_then_success_records_words() {
        let mut sim = ScriptedSim {
            script: vec![
                Err(SimulateSwapError::MissingTickWord(-4)),
                Ok(outcome_stub()),
            ],
            call: AtomicUsize::new(0),
            fetcher_ok: true,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        match drive(&mut sim, 17) {
            PolicyAttempt::Computed(_, words) => assert_eq!(words, vec![-4]),
            other => panic!("expected Computed, got {other:?}"),
        }
        assert_eq!(sim.fetched.len(), 1);
    }

    #[test]
    fn drive_repeated_miss_is_exhausted() {
        let miss = Err(SimulateSwapError::MissingTickWord(9));
        let mut sim = ScriptedSim {
            script: vec![miss.clone(), miss, Ok(outcome_stub())],
            call: AtomicUsize::new(0),
            fetcher_ok: true,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        assert_eq!(drive(&mut sim, 17), PolicyAttempt::FetchExhausted(9));
    }

    #[test]
    fn drive_no_fetcher_is_exhausted() {
        // The legacy path collapsed "no fetcher registered" into the same
        // silent give-up as a failed fetch; the gate keeps them distinct:
        // NoFetcher => FetchExhausted (nothing was attempted), while a real
        // fetcher error => FetchFailed.
        let mut sim = ScriptedSim {
            script: vec![Err(SimulateSwapError::MissingTickWord(2))],
            call: AtomicUsize::new(0),
            fetcher_ok: false,
            has_fetcher: false,
            fetched: Vec::new(),
        };
        assert_eq!(drive(&mut sim, 17), PolicyAttempt::FetchExhausted(2));

        let mut errored = ScriptedSim {
            script: vec![Err(SimulateSwapError::MissingTickWord(2))],
            call: AtomicUsize::new(0),
            fetcher_ok: false,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        assert_eq!(drive(&mut errored, 17), PolicyAttempt::FetchFailed(2));
    }

    #[test]
    fn drive_not_computable_short_circuits() {
        let mut sim = ScriptedSim {
            script: vec![Err(SimulateSwapError::NotComputable)],
            call: AtomicUsize::new(0),
            fetcher_ok: true,
            has_fetcher: true,
            fetched: Vec::new(),
        };
        assert_eq!(drive(&mut sim, 17), PolicyAttempt::NotComputable);
    }

    // ---------------------------------------------------------------------
    // Integration: registered canonical V3 fixture shape through the gate.
    const FIXTURE_LIQUIDITY: u128 = 1_000_000_000_000; // 1e12
    const FIXTURE_AMOUNT_IN: u128 = 1_000_000_000; // 1e9
    /// Recorded constant of the shared V3 fixture (`tests/standalone_parity/fixtures/v3_swap.json`)
    /// — mirrored here deliberately; the parity suite remains the single
    /// source of truth for drift detection.
    const FIXTURE_EXPECTED_OUT: u128 = 996_006_981;

    fn register_canonical_v3(
        bot: &mut BotState,
        coverage: PoolTickCoverage,
        with_ticks: bool,
    ) -> u64 {
        let mut tick_data = HashMap::new();
        if with_ticks {
            tick_data.insert(
                0_i32,
                degenbot_pools::TickInfo {
                    liquidity_gross: alloy::primitives::U128::from(FIXTURE_LIQUIDITY),
                    liquidity_net: I256::ZERO,
                    block: 0,
                },
            );
        }
        bot.register_v3_pool(&degenbot_pools::v3_state::RegisterV3PoolParams {
            address: "0x00000000000000000000000000000000000d0001"
                .parse()
                .unwrap(),
            token0: "0x00000000000000000000000000000000000a0001"
                .parse()
                .unwrap(),
            token1: "0x00000000000000000000000000000000000a0002"
                .parse()
                .unwrap(),
            fee: 3000,
            tick_spacing: 60,
            factory: "0x00000000000000000000000000000000000f0001"
                .parse()
                .unwrap(),
            sqrt_price_x96: U256::from(2_u8).pow(U256::from(96_u8)),
            liquidity: FIXTURE_LIQUIDITY,
            tick: 0,
            tick_data,
            update_block: 0,
            coverage,
            fetcher: None,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("register canonical v3 pool: {e:?}"))
    }

    #[test]
    fn tracked_v3_pool_computes_fixture_constant_through_gate() {
        let mut bot = BotState::new();
        let pid = register_canonical_v3(&mut bot, PoolTickCoverage::Tracked, true);
        let read = bot.swap_simulation(
            0,
            pid,
            SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(FIXTURE_AMOUNT_IN).unwrap(),
                sqrt_price_limit: None,
            },
        );
        match read {
            SwapRead::Computed(SwapOutcome::V3(payload)) => {
                assert_eq!(
                    payload.delivered,
                    I256::try_from(FIXTURE_EXPECTED_OUT).unwrap()
                );
                assert_eq!(
                    payload.consumed,
                    -I256::try_from(FIXTURE_AMOUNT_IN).unwrap()
                );
                assert!(payload.caveats.is_empty());
                assert!(payload.fetched_words.is_empty());
            }
            other => panic!("expected Computed(V3), got {other:?}"),
        }
    }

    #[test]
    fn unregistered_pool_and_zero_amount_are_not_computable() {
        let mut bot = BotState::new();
        assert_eq!(
            bot.swap_simulation(
                0,
                999,
                SwapRequest {
                    zero_for_one: true,
                    amount_specified: -I256::ONE,
                    sqrt_price_limit: None
                }
            ),
            SwapRead::NotComputable
        );
        let pid = register_canonical_v3(&mut bot, PoolTickCoverage::Tracked, true);
        assert_eq!(
            bot.swap_simulation(
                0,
                pid,
                SwapRequest {
                    zero_for_one: true,
                    amount_specified: I256::ZERO,
                    sqrt_price_limit: None
                }
            ),
            SwapRead::NotComputable
        );
    }

    #[test]
    fn sparse_v3_pool_missing_word_surfaces_exhaustion_with_caveat_free_error() {
        // Sparse + empty ticks + no fetcher: the walk cannot establish its
        // first bitmap word → FetchExhausted (the legacy silent-ZERO path).
        let mut bot = BotState::new();
        let pid = register_canonical_v3(&mut bot, PoolTickCoverage::Sparse, false);
        let read = bot.swap_simulation(
            0,
            pid,
            SwapRequest {
                zero_for_one: true,
                amount_specified: -I256::try_from(FIXTURE_AMOUNT_IN * 10).unwrap(),
                sqrt_price_limit: None,
            },
        );
        assert!(matches!(read, SwapRead::FetchExhausted { word } if word != i32::MIN));
    }
}
