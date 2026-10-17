//! The concentrated-liquidity family read module (ADR-073 D1/D4) — the
//! foundation-layer home of everything the CL family (V3 + V4) contributes to
//! being read and resolved.
//!
//! GLOSSARY.md "Family read module": this tree owns the state slices, the
//! swap-read arm (every variant, miss-recovery and disarm-fetch included),
//! the resolve projection as a pure function of the family state slice, and
//! the family-local apply/get/calc behavior over that slice. It reads NO
//! state owner and consumes NO solver kernel — the hop is solver vocabulary,
//! so the wrapper stays at the dispatch row (the substrate resolve row does
//! the registry lookup and wraps the solver's `ResolvedHop`).
//!
//! # The four intent doors (D4)
//!
//! The tree's export surface is intent-keyed. Design selection IS the state
//! slice type ([`ConcentratedLiquidityState`] /
//! [`ConcentratedLiquidityStateMut`]); the V3/V4 match happens inside the
//! tree and is compiler-exhaustive over the two slices:
//!
//! - [`capture_hop`] — the resolve projection: one fresh observation of one
//!   hop, refused rather than defaulted;
//! - [`read_swap`] — the swap-read arm, ALL variants (registered stateless /
//!   armed miss recovery / disarm-fetch / transient override / missing-words
//!   discovery) behind ONE entry;
//! - `apply_event` — one buffered pool event onto one slice (the
//!   [`super::door::FamilyApply`] contract);
//! - [`observe`] — family-local GET behavior over one slice.
//!
//! # The projection guardrail (structural, load-bearing)
//!
//! [`v3`] and [`v4`] hold the design-private projections, deliberately
//! SELF-CONTAINED: no shared V3/V4 constructor and no public projection
//! helper crossing the two files — fee convention, current-tick drain
//! framing, per-range net-sign direction, and the engine sign mapping differ
//! (see each file's guardrail header). Sharing no code between the two files
//! IS the enforcement, not a header comment.
//!
//! What the two designs DO share inside this tree is only the door plumbing:
//! the family-blind request/outcome/probe vocabulary in [`super::door`] and
//! the ONE fetch→merge→retry policy in [`drive`] (ADR-037's drive loop —
//! formerly copy-pasted per call shape, collapsed once; re-duplicating it per
//! design would resurrect the seam ADR-037 closed). The policy encodes none
//! of the design differences.

/// The family's solve entry (ADR-073 endgame): the solve half re-homed from
/// the dissolved solver crate — the family story is ONE tree again.
pub(crate) mod solve;

/// The state module: the family's shared state verbs, written once
/// data-generically over the two designs' state structs.
pub(crate) mod state_journal;

/// Bitmap-word seeding and registration-coverage bookkeeping: the family's
/// shared seeding verbs, written once data-generically over the two designs'
/// state structs.
pub(crate) mod state_seeding;

/// Swap-walk state verbs — the directional viability gate, the two-stamp
/// clock advances, the lazy tick-range cache, and the integer tick-range
/// sequence walks — written once data-generically over the two designs'
/// state structs.
pub(crate) mod state_walk;

pub mod v3;
pub mod v4;

use std::sync::Arc;

use alloy::primitives::{Address, U256};
use hashbrown::{HashMap, HashSet};

use super::door::{Observe, Observed, ReadOutcome, SwapReadRequest};

use crate::int_v3_hop::IntV3TickRangeSequence;
use crate::registry::ConcentratedLiquidityPoolMut;
use crate::tick_fetch::{FetchedTickWord, TickWordFetcher};
use crate::v3_state::{SimulateSwapError, V3SwapOutcome};
pub use crate::TickInfo;

/// The state slices and event/registration DTOs, presented at the tree root
/// through the design files that own them.
pub use self::v3::{
    BufferedV3LiquidityUpdate, BufferedV3PoolEvent, BufferedV3SwapEvent, RegisterV3PoolError,
    RegisterV3PoolParams, V3PoolIdentity, V3PoolState, V3SwapUpdate,
};
pub use self::v4::{
    BufferedV4LiquidityUpdate, BufferedV4PoolEvent, BufferedV4SwapEvent, RegisterV4PoolError,
    RegisterV4PoolParams, V4PoolIdentity, V4PoolKey, V4PoolState, V4StateSync, V4SwapUpdate,
};

/// One design's registration scalars the slice does not carry (immutable
/// identity fields the read needs: the fee-convention input and the tick
/// spacing).
#[derive(Clone, Copy, Debug)]
pub struct DesignScalars {
    /// The pool's fee — V3's `lp_fee` pips, V4's `pool_key.fee` pips.
    pub fee: u32,
    /// The pool's tick spacing.
    pub tick_spacing: i32,
}

impl DesignScalars {
    #[must_use]
    pub const fn new(fee: u32, tick_spacing: i32) -> Self {
        Self { fee, tick_spacing }
    }
}

/// Design selection by state slice (read-only): the variant IS the design —
/// the V3/V4 match happens inside the tree, never at the callers.
#[derive(Clone, Copy)]
pub enum ConcentratedLiquidityState<'a> {
    /// The Uniswap V3 design slice (factory-addressed pool).
    V3 {
        state: &'a V3PoolState,
        scalars: DesignScalars,
    },
    /// The Uniswap V4 design slice (singleton + `PoolId`); the design's hook
    /// permission address rides the variant — V3 has no hook concept.
    V4 {
        state: &'a V4PoolState,
        scalars: DesignScalars,
        hooks: Address,
    },
}

/// Design selection by state slice (mutable) — the write-side twin of
/// [`ConcentratedLiquidityState`].
pub enum ConcentratedLiquidityStateMut<'a> {
    /// The Uniswap V3 design slice, mutable.
    V3 {
        state: &'a mut V3PoolState,
        scalars: DesignScalars,
    },
    /// The Uniswap V4 design slice, mutable (hooks ride the variant).
    V4 {
        state: &'a mut V4PoolState,
        scalars: DesignScalars,
        hooks: Address,
    },
}

/// The registered identity + state pair a transient source stages from —
/// the identity carries the immutable registration fields the transient's
/// `from_params` rebuild needs (the state slice does not carry them).
#[derive(Clone, Copy)]
pub enum ConcentratedLiquidityTransientSource<'a> {
    /// A registered V3 pool (identity + state).
    V3 {
        identity: &'a V3PoolIdentity,
        state: &'a V3PoolState,
    },
    /// A registered V4 pool (identity + state).
    V4 {
        identity: &'a V4PoolIdentity,
        state: &'a V4PoolState,
    },
}

/// Why [`capture_hop`] refused to observe a hop — the projection is refused,
/// never defaulted. The dispatch row maps these onto its own invalidation
/// vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureRefusal {
    /// The pool cannot host a swap in this direction at its current state
    /// (directional viability gate — the archived-Python `swap_is_viable`
    /// port; O(1), checked BEFORE any tick-range walk).
    NotViable,
    /// The integer tick-range sequence could not be built for the direction.
    SequenceUnavailable,
    /// The pool carries an amount-modifying V4 hook (ADR-037: admitted at
    /// registration, never solvable). V4-only.
    HookedPool,
}

/// Which miss-recovery posture [`read_swap`] runs — "all variants and both
/// fetch modes behind one entry" (ADR-073 D4).
pub enum FetchMode<'a> {
    /// Pattern B, stateless: no recovery at all — a sparse-map miss surfaces
    /// as DATA ([`ReadOutcome::Stateless`] carrying
    /// [`SimulateSwapError::MissingTickWord`]); the caller owns fetch + retry.
    Stateless,
    /// Miss recovery ARMED: a miss fetches `word` through the stored fetcher
    /// at `block`, merges, and retries (single-strike per word).
    Armed {
        /// The pool's stored fetcher, cloned off the registered state before
        /// the read began.
        fetcher: &'a Arc<dyn TickWordFetcher>,
        /// The pool id the fetcher is bound to.
        pool_id: u64,
        /// The fetch context block.
        block: u64,
    },
    /// Miss recovery DISARMED: a residual missing word surfaces as
    /// [`ReadOutcome::FetchExhausted`] (additive, in-contract) instead of
    /// fetching under the caller's write guard.
    Disarmed,
    /// Missing-words DISCOVERY: never fetches — a forged-empty fill lands on
    /// the (transient) target and the walk steps past each discovered word,
    /// so one pass lists EVERY missing word on the crossing. Only meaningful
    /// with a transient source; a [`SwapReadSource::Slice`] read downgrades
    /// this to [`FetchMode::Stateless`] (a registered slice must never take a
    /// forged fill).
    Discovery,
}

/// The read target: a caller-held mutable slice (registered state, merged in
/// place), or a transient the family builds from registered identity — the
/// override (hypothetical) and discovery sources.
pub enum SwapReadSource<'a> {
    /// Read-only against a caller-held shared slice — the stateless Pattern-B
    /// read (no merge target exists; any recovery mode downgrades to
    /// [`FetchMode::Stateless`], which never merges).
    Shared(ConcentratedLiquidityState<'a>),
    /// Read + merge against a caller-held mutable slice.
    Slice(ConcentratedLiquidityStateMut<'a>),
    /// A fresh transient mirroring a registered pool's scalars, coverage, AND
    /// checked-word set (the missing-words discovery staging walk). The
    /// transient is owned by the door and discarded with the read.
    Discovery(ConcentratedLiquidityTransientSource<'a>, usize),
    /// A fresh transient built from registered identity + override scalars at
    /// `Sparse` coverage — the hypothetical (override) sim. Carries the
    /// registered state's `update_block` for the two-stamp clocks and the
    /// registry's journal depth for the transient's own reorg journal.
    Override(
        ConcentratedLiquidityTransientSource<'a>,
        OverrideScalars<'a>,
        u64,
        usize,
    ),
}

/// Hypothetical (override) pool scalars: "what if the pool were at state X?"
/// The transient borrows the registered pool's immutable identity — only
/// these four scalars differ.
pub struct OverrideScalars<'a> {
    /// Override `sqrtPriceX96`.
    pub sqrt_price_x96: U256,
    /// Override active liquidity.
    pub liquidity: u128,
    /// Override current tick.
    pub tick: i32,
    /// Replacement tick data for the hypothetical walk.
    pub tick_data: &'a HashMap<i32, TickInfo>,
}

/// Why a missing-word recovery could not run — the drive policy's fetch
/// verdict.
enum FetchVerdict {
    /// No fetcher is registered (⇒ [`ReadOutcome::FetchExhausted`]).
    NoFetcher,
    /// The fetcher ran and failed (⇒ [`ReadOutcome::FetchFailed`]).
    Errored,
}

/// The capture door — the resolve projection: one fresh observation of one
/// hop, refused rather than defaulted. Pure over the family state slice; the
/// dispatch row does the registry lookup and the solver-vocabulary wrap.
///
/// # Errors
///
/// [`CaptureRefusal`] — see the type's docs for the per-design reasons.
pub fn capture_hop(
    design: ConcentratedLiquidityState<'_>,
    zero_for_one: bool,
) -> Result<Arc<IntV3TickRangeSequence>, CaptureRefusal> {
    match design {
        ConcentratedLiquidityState::V3 { state, scalars } => {
            v3::capture_hop(state, scalars, zero_for_one)
        }
        ConcentratedLiquidityState::V4 {
            state,
            scalars,
            hooks,
        } => v4::capture_hop(state, scalars, hooks, zero_for_one),
    }
}

/// The swap-read door — ALL variants behind ONE entry. The drive policy is
/// the family's ONE fetch→merge→retry loop; the design-private compute (fee
/// convention + engine sign mapping) lives in [`v3`] / [`v4`].
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "the length is the door's dispatch shape (four sources × two \
     designs, each arm a single drive call); per-arm leaf extraction would \
     re-duplicate the ONE drive policy per design — the seam ADR-037 closed"
)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "the door contract is settled: callers move their FetchMode in; \
     the door itself only reads it now that the drive policy borrows the mode"
)]
pub fn read_swap(
    source: SwapReadSource<'_>,
    request: SwapReadRequest<'_>,
    mode: FetchMode<'_>,
) -> ReadOutcome {
    // The CL family answers only its canonical variant; a request another
    // family carries is a typed refusal (never a guessed mapping).
    let SwapReadRequest::ConcentratedLiquidity {
        zero_for_one,
        amount_specified,
        sqrt_price_limit,
    } = request
    else {
        return ReadOutcome::NotComputable;
    };
    // The effective walk limit: the caller's bound or the shared CL default.
    let limit =
        sqrt_price_limit.unwrap_or_else(|| V3PoolState::default_sqrt_price_limit(zero_for_one));
    match source {
        SwapReadSource::Shared(design) => match design {
            ConcentratedLiquidityState::V3 { state, scalars } => drive_shared(state, |st| {
                v3::compute(st, scalars, zero_for_one, amount_specified, limit)
            }),
            ConcentratedLiquidityState::V4 {
                state,
                scalars,
                hooks: _,
            } => drive_shared(state, |st| {
                v4::compute(st, scalars, zero_for_one, amount_specified, limit)
            }),
        },
        SwapReadSource::Slice(design) => match design {
            ConcentratedLiquidityStateMut::V3 { state, scalars } => drive(
                state,
                |st| v3::compute(st, scalars, zero_for_one, amount_specified, limit),
                &downgrade_discovery(&mode),
            ),
            ConcentratedLiquidityStateMut::V4 {
                state,
                scalars,
                hooks: _,
            } => drive(
                state,
                |st| v4::compute(st, scalars, zero_for_one, amount_specified, limit),
                &downgrade_discovery(&mode),
            ),
        },
        SwapReadSource::Discovery(source, journal_depth) => {
            let mode = FetchMode::Discovery;
            match source {
                ConcentratedLiquidityTransientSource::V3 { identity, state } => {
                    let mut transient = v3::discovery_state(identity, state, journal_depth);
                    drive(
                        &mut *transient,
                        |st| {
                            v3::compute(
                                st,
                                DesignScalars::new(identity.fee, identity.tick_spacing),
                                zero_for_one,
                                amount_specified,
                                limit,
                            )
                        },
                        &mode,
                    )
                }
                ConcentratedLiquidityTransientSource::V4 { identity, state } => {
                    let mut transient = v4::discovery_state(identity, state, journal_depth);
                    drive(
                        &mut *transient,
                        |st| {
                            v4::compute(
                                st,
                                DesignScalars::new(
                                    identity.pool_key.fee,
                                    identity.pool_key.tick_spacing,
                                ),
                                zero_for_one,
                                amount_specified,
                                limit,
                            )
                        },
                        &mode,
                    )
                }
            }
        }
        SwapReadSource::Override(source, over, update_block, journal_depth) => match source {
            ConcentratedLiquidityTransientSource::V3 { identity, state: _ } => {
                let mut transient =
                    v3::override_state(identity, &over, update_block, journal_depth);
                drive(
                    &mut *transient,
                    |st| {
                        v3::compute(
                            st,
                            DesignScalars::new(identity.fee, identity.tick_spacing),
                            zero_for_one,
                            amount_specified,
                            limit,
                        )
                    },
                    &mode,
                )
            }
            ConcentratedLiquidityTransientSource::V4 { identity, state: _ } => {
                let mut transient =
                    v4::override_state(identity, &over, update_block, journal_depth);
                drive(
                    &mut *transient,
                    |st| {
                        v4::compute(
                            st,
                            DesignScalars::new(
                                identity.pool_key.fee,
                                identity.pool_key.tick_spacing,
                            ),
                            zero_for_one,
                            amount_specified,
                            limit,
                        )
                    },
                    &mode,
                )
            }
        },
    }
}

/// A registered slice never takes a forged discovery fill — a discovery
/// request against [`SwapReadSource::Slice`] downgrades to [`FetchMode::Stateless`]
/// (the miss surfaces as data; the slice is never mutated by a staging walk).
fn downgrade_discovery<'a>(mode: &'a FetchMode<'a>) -> FetchMode<'a> {
    match mode {
        FetchMode::Discovery | FetchMode::Stateless => FetchMode::Stateless,
        FetchMode::Disarmed => FetchMode::Disarmed,
        FetchMode::Armed {
            fetcher,
            pool_id,
            block,
        } => FetchMode::Armed {
            fetcher,
            pool_id: *pool_id,
            block: *block,
        },
    }
}

/// The observe door — family-local GET behavior over one slice.
#[must_use]
pub fn observe(design: ConcentratedLiquidityState<'_>, probe: Observe) -> Observed {
    match design {
        ConcentratedLiquidityState::V3 { state, .. } => v3::observe(state, probe),
        ConcentratedLiquidityState::V4 { state, hooks, .. } => v4::observe(state, probe, hooks),
    }
}

// ---------------------------------------------------------------------------
// The ONE drive policy (ADR-037 fetch → merge → retry)
// ---------------------------------------------------------------------------

/// The stateless-only read against a SHARED slice: one compute attempt, no
/// merge target exists, so the verdict is always the raw Pattern-B shape
/// ([`ReadOutcome::Stateless`]).
fn drive_shared<S: ConcentratedLiquidityPoolMut>(
    state: &S,
    compute: impl Fn(&S) -> Result<V3SwapOutcome, SimulateSwapError>,
) -> ReadOutcome {
    ReadOutcome::Stateless(compute(state))
}

/// THE fetch → merge → retry loop — formerly copy-pasted per call shape in
/// the swap-simulation gate, collapsed once at the family door. Dedup rule:
/// a second miss on the SAME word means the previous merge did not satisfy
/// the walker — give up rather than loop forever. No-fetcher and
/// exhausted-miss collapse to [`ReadOutcome::FetchExhausted`]; a fetcher
/// error is [`ReadOutcome::FetchFailed`].
///
/// Fetch errors are deliberately single-strike: the abort surfaces honestly
/// and the CALLER owns its retry budget. Discovery mode never fetches: a
/// forged-empty fill lands on the (transient) target and the walk steps past
/// the word, surfacing EVERY missing word in one pass.
fn drive<S: ConcentratedLiquidityPoolMut>(
    state: &mut S,
    mut compute: impl FnMut(&S) -> Result<V3SwapOutcome, SimulateSwapError>,
    mode: &FetchMode<'_>,
) -> ReadOutcome {
    let mut attempted: HashSet<i32> = HashSet::new();
    let mut fetched_words: Vec<i32> = Vec::new();
    loop {
        match compute(state) {
            Ok(outcome) => {
                return match mode {
                    FetchMode::Stateless => ReadOutcome::Stateless(Ok(outcome)),
                    // Discovery: the walk established every word — the pass
                    // verdict IS the collected list (the transient is
                    // discarded with the read).
                    FetchMode::Discovery => ReadOutcome::MissingWords(fetched_words),
                    _ => ReadOutcome::Computed {
                        outcome,
                        fetched_words,
                    },
                };
            }
            Err(SimulateSwapError::NotComputable) => {
                return match mode {
                    FetchMode::Stateless => {
                        ReadOutcome::Stateless(Err(SimulateSwapError::NotComputable))
                    }
                    FetchMode::Discovery => ReadOutcome::MissingWords(fetched_words),
                    _ => ReadOutcome::NotComputable,
                }
            }
            Err(SimulateSwapError::MissingTickWord(word)) => match mode {
                // Pattern B: the miss IS the answer (data, for the caller's
                // fetch+retry shell one layer up).
                FetchMode::Stateless => {
                    return ReadOutcome::Stateless(Err(SimulateSwapError::MissingTickWord(word)))
                }
                // Discovery: list every missing word in one pass, stepping the
                // walker past each with a forged-empty fill on the (transient)
                // target. Never fetches.
                FetchMode::Discovery => {
                    if !attempted.insert(word) {
                        return ReadOutcome::MissingWords(fetched_words);
                    }
                    fetched_words.push(word);
                    let _ = state.merge_tick_word(&FetchedTickWord {
                        word,
                        ticks: HashMap::new(),
                    });
                }
                FetchMode::Armed { .. } | FetchMode::Disarmed => {
                    if !attempted.insert(word) {
                        return ReadOutcome::FetchExhausted { word };
                    }
                    match fetch_word(mode, word) {
                        Ok(data) => {
                            let _ = state.merge_tick_word(&data);
                            fetched_words.push(word);
                        }
                        Err(FetchVerdict::NoFetcher) => {
                            return ReadOutcome::FetchExhausted { word };
                        }
                        Err(FetchVerdict::Errored) => {
                            return ReadOutcome::FetchFailed { word };
                        }
                    }
                }
            },
        }
    }
}

/// One recovery attempt for `word`. `NoFetcher` means no fetcher is
/// registered (⇒ `FetchExhausted`); `Errored` means the recovery itself
/// failed (⇒ `FetchFailed`). A disarmed read never fetches — its caller
/// staged the missing words beforehand and the typed contract answers
/// `FetchExhausted` for any residual miss.
fn fetch_word(mode: &FetchMode<'_>, word: i32) -> Result<FetchedTickWord, FetchVerdict> {
    let FetchMode::Armed {
        fetcher,
        pool_id,
        block,
    } = mode
    else {
        return Err(FetchVerdict::NoFetcher);
    };
    fetcher
        .fetch_missing_tick_word(*pool_id, word, *block)
        .map_err(|_| FetchVerdict::Errored)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic)]

    use super::*;
    use crate::tick_fetch::FetchTickWordError;
    use crate::v3_state::PoolTickCoverage;

    // -------------------------------------------------------------------
    // THE drive-policy pin (moved with the family from the substrate
    // swap-simulation gate): a scripted merge target exercises every branch
    // of the ONE fetch→merge→retry loop without constructing real pools.
    // Outcomes are byte-identical to the policy this loop replaced.
    struct ScriptedMergeTarget {
        fetched: std::cell::RefCell<Vec<i32>>,
    }

    impl crate::registry::ConcentratedLiquidityPool for ScriptedMergeTarget {
        fn sqrt_price_x96(&self) -> U256 {
            U256::ONE
        }
        fn liquidity(&self) -> u128 {
            1
        }
        fn tick(&self) -> i32 {
            0
        }
        fn update_block(&self) -> u64 {
            0
        }
        fn tick_data_block(&self) -> u64 {
            0
        }
        fn tick_data(&self) -> &HashMap<i32, TickInfo> {
            unreachable!("the drive policy never reads tick data")
        }
        fn coverage(&self) -> PoolTickCoverage {
            PoolTickCoverage::Sparse
        }
        fn known_bitmap_words(&self) -> &HashSet<i32> {
            unreachable!("the drive policy never reads the checked-word set")
        }
    }

    impl ConcentratedLiquidityPoolMut for ScriptedMergeTarget {
        fn replace_tick_data(
            &mut self,
            _tick_data: HashMap<i32, TickInfo>,
            _tick_data_block: u64,
            _tick_spacing: i32,
        ) -> bool {
            false
        }
        fn merge_tick_word(&mut self, fetched: &FetchedTickWord) -> bool {
            self.fetched.borrow_mut().push(fetched.word);
            true
        }
        fn mark_bitmap_words_known(&mut self, _words: &[i32]) {}
        fn apply_swap(
            &mut self,
            _sqrt_price_x96: U256,
            _liquidity: u128,
            _tick: i32,
            _block_number: u64,
            _tick_priors: &[(i32, TickInfo)],
        ) {
        }
        fn apply_liquidity_update(
            &mut self,
            _tick_lower: i32,
            _tick_upper: i32,
            _liquidity_delta: i128,
            _block_number: u64,
        ) {
        }
    }

    /// A scripted fetcher: ok/err per flag, counting every call.
    #[derive(Debug)]
    struct ScriptedFetcher {
        ok: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl TickWordFetcher for ScriptedFetcher {
        fn fetch_missing_tick_word(
            &self,
            _pool_id: u64,
            word: i32,
            _block: u64,
        ) -> Result<FetchedTickWord, FetchTickWordError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.ok {
                Ok(FetchedTickWord {
                    word,
                    ticks: HashMap::new(),
                })
            } else {
                Err(FetchTickWordError::FetchFailed)
            }
        }
    }

    fn outcome_stub() -> V3SwapOutcome {
        V3SwapOutcome::default()
    }

    /// Queue-driven compute: replays `script` in order, then `NotComputable`.
    fn scripted_compute(
        script: Vec<Result<V3SwapOutcome, SimulateSwapError>>,
    ) -> impl FnMut(&ScriptedMergeTarget) -> Result<V3SwapOutcome, SimulateSwapError> {
        let mut script = script.into_iter();
        move |_| {
            script
                .next()
                .unwrap_or(Err(SimulateSwapError::NotComputable))
        }
    }

    #[test]
    fn drive_immediate_success() {
        let mut target = ScriptedMergeTarget {
            fetched: std::cell::RefCell::new(Vec::new()),
        };
        let outcome = drive(
            &mut target,
            scripted_compute(vec![Ok(outcome_stub())]),
            &FetchMode::Disarmed,
        );
        match outcome {
            ReadOutcome::Computed { fetched_words, .. } => assert!(fetched_words.is_empty()),
            other => panic!("expected Computed, got {other:?}"),
        }
        assert!(target.fetched.borrow().is_empty());
    }

    #[test]
    fn drive_miss_then_fetch_then_success_records_words() {
        let mut target = ScriptedMergeTarget {
            fetched: std::cell::RefCell::new(Vec::new()),
        };
        let fetcher: Arc<dyn TickWordFetcher> = Arc::new(ScriptedFetcher {
            ok: true,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let outcome = drive(
            &mut target,
            scripted_compute(vec![
                Err(SimulateSwapError::MissingTickWord(-4)),
                Ok(outcome_stub()),
            ]),
            &FetchMode::Armed {
                fetcher: &fetcher,
                pool_id: 17,
                block: 17,
            },
        );
        match outcome {
            ReadOutcome::Computed { fetched_words, .. } => assert_eq!(fetched_words, vec![-4]),
            other => panic!("expected Computed, got {other:?}"),
        }
        assert_eq!(*target.fetched.borrow(), vec![-4]);
    }

    #[test]
    fn drive_repeated_miss_is_exhausted() {
        let mut target = ScriptedMergeTarget {
            fetched: std::cell::RefCell::new(Vec::new()),
        };
        let fetcher: Arc<dyn TickWordFetcher> = Arc::new(ScriptedFetcher {
            ok: true,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let miss = Err(SimulateSwapError::MissingTickWord(9));
        let outcome = drive(
            &mut target,
            scripted_compute(vec![miss.clone(), miss, Ok(outcome_stub())]),
            &FetchMode::Armed {
                fetcher: &fetcher,
                pool_id: 17,
                block: 17,
            },
        );
        assert_eq!(outcome, ReadOutcome::FetchExhausted { word: 9 });
    }

    #[test]
    fn drive_no_fetcher_is_exhausted() {
        // The legacy path collapsed "no fetcher registered" into the same
        // silent give-up as a failed fetch; the door keeps them distinct:
        // Disarmed/NoFetcher => FetchExhausted (nothing was attempted),
        // while a real fetcher error => FetchFailed.
        let mut target = ScriptedMergeTarget {
            fetched: std::cell::RefCell::new(Vec::new()),
        };
        let outcome = drive(
            &mut target,
            scripted_compute(vec![Err(SimulateSwapError::MissingTickWord(2))]),
            &FetchMode::Disarmed,
        );
        assert_eq!(outcome, ReadOutcome::FetchExhausted { word: 2 });

        let mut errored_target = ScriptedMergeTarget {
            fetched: std::cell::RefCell::new(Vec::new()),
        };
        let fetcher: Arc<dyn TickWordFetcher> = Arc::new(ScriptedFetcher {
            ok: false,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let outcome = drive(
            &mut errored_target,
            scripted_compute(vec![Err(SimulateSwapError::MissingTickWord(2))]),
            &FetchMode::Armed {
                fetcher: &fetcher,
                pool_id: 17,
                block: 17,
            },
        );
        assert_eq!(outcome, ReadOutcome::FetchFailed { word: 2 });
    }

    #[test]
    fn drive_not_computable_short_circuits() {
        let mut target = ScriptedMergeTarget {
            fetched: std::cell::RefCell::new(Vec::new()),
        };
        let outcome = drive(
            &mut target,
            scripted_compute(vec![Err(SimulateSwapError::NotComputable)]),
            &FetchMode::Disarmed,
        );
        assert_eq!(outcome, ReadOutcome::NotComputable);
    }

    #[test]
    fn drive_discovery_lists_every_missing_word_and_never_fetches() {
        let mut target = ScriptedMergeTarget {
            fetched: std::cell::RefCell::new(Vec::new()),
        };
        // Two distinct words crossed in one pass: the forged-empty fills step
        // the walker past each, and the pass surfaces BOTH (pair-review
        // condition 5), in first-miss order.
        let outcome = drive(
            &mut target,
            scripted_compute(vec![
                Err(SimulateSwapError::MissingTickWord(8)),
                Err(SimulateSwapError::MissingTickWord(12)),
                Ok(outcome_stub()),
            ]),
            &FetchMode::Discovery,
        );
        assert_eq!(outcome, ReadOutcome::MissingWords(vec![8, 12]));
        // The forged-empty fills DID land (the walk stepped past each word).
        assert_eq!(*target.fetched.borrow(), vec![8, 12]);
    }

    #[test]
    fn drive_stateless_surfaces_the_miss_as_data() {
        // Pattern B: the stateless read against a shared slice never merges
        // and never fetches — the missing word IS the answer (data).
        let mut target = ScriptedMergeTarget {
            fetched: std::cell::RefCell::new(Vec::new()),
        };
        let outcome = drive(
            &mut target,
            scripted_compute(vec![Err(SimulateSwapError::MissingTickWord(5))]),
            &FetchMode::Stateless,
        );
        assert_eq!(
            outcome,
            ReadOutcome::Stateless(Err(SimulateSwapError::MissingTickWord(5)))
        );
        assert!(target.fetched.borrow().is_empty());
    }
}
