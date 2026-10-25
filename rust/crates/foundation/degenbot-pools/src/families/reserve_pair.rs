//! The reserve-pair family read module (ADR-073 D1) — the foundation-layer
//! home of everything the reserve-pair family (Uniswap V2 + Aerodrome V2
//! volatile) contributes to being read and resolved.
//!
//! GLOSSARY.md "Family read module": this module owns the state slices (the
//! I/O-free `crate::v2_state` / `crate::aerodrome_v2_state` types, ADR-001),
//! the swap-read arm (every variant — the V2 constant-product exact-input
//! read, the V2 exact-output closed form, and the Aerodrome volatile
//! fee-fraction read — behind one entry), the resolve projection as a pure
//! function of the family state slice, and the family-local apply/observe
//! behavior over those slices. It reads NO state owner and consumes NO
//! solver kernel — the hop payload is `degenbot_math::v2::IntHopState` (the
//! math leaf the substrate resolve row wraps into the solver's
//! `ResolvedHop`).
//!
//! # The family map
//!
//! Reserve-pair = two `U112` reserves + `update_block`, full-state block
//! deltas ([`crate::state::state_history::V2BlockDelta`]). The two designs are
//! SPECIES of one structure (ADR-059 D3 — manifest entries, never new
//! types): V2 carries the per-direction RETAINED fraction
//! `(gamma_numer, fee_denom)`; Aerodrome V2 volatile carries the
//! unidirectional FEE fraction. Aerodrome V2 **stable** is the Solidly
//! family's design (its own read arm, per the ADR-073 D6 landing order) —
//! the central rows route a `stable` identity to that arm; this module's
//! Aerodrome door refuses it defensively rather than mis-price it with the
//! volatile constant-product math.
//!
//! # The four intent doors (ADR-073 D4)
//!
//! - [`capture_hop`] — the resolve projection (the V2 design): one fresh
//!   projection of one hop as the math leaf's hop payload;
//! - [`read_swap`] — the swap-read arm, ALL variants behind one entry;
//! - [`read_swap_override`] — the override (hypothetical) read arm: the same
//!   read code over a transient carrying the caller's reserves, answering the
//!   POST-swap reserves alongside the amounts;
//! - [`apply_event`] — one buffered `Sync` event onto one slice;
//! - [`observe`] — family-local GET behavior over one slice.
//!
//! The reserve-pair family has no separate calc doors: its contract-level
//! view surface (`getAmountOut` and the `getAmountIn` closed form) IS the
//! read door.
//!
//! # The sentinel contract is NOT re-litigated here
//!
//! The legacy `Ok(U256::ZERO)` sentinel semantics stay AS-IS: a zero
//! exact-input read answers `Ok(ZERO)` (on-chain `getAmountOut(0) = 0`), and
//! the exact-output refusals (zero request, overdraw, vanishing denominator)
//! answer [`SimulateSwapError::NotComputable`] — the mapping the substrate
//! row applied to the retired helper's `None`. The sentinel's retirement is
//! C6's.
//!
//! # Design-private math (structural, load-bearing)
//!
//! The V2 arm and the Aerodrome volatile arm share NO math: the V2
//! retained-fraction constant-product read goes through
//! `degenbot_math::v2::IntHopState`, the Aerodrome fee-fraction read through
//! `degenbot_math::solidly::calc_exact_in_volatile` — different leaves with
//! different fee conventions. Sharing no code between the two arms IS the
//! enforcement, not a header comment. What the two designs DO share is only
//! the door plumbing and the event/probe vocabulary below (the ADR-017 D3
//! collapse: both slices' `apply_sync` bodies are byte-identical full-state
//! [`crate::state::state_history::V2BlockDelta`] pushes).

use alloy::primitives::aliases::U112;
use alloy::primitives::U256;

use degenbot_math::solidly;
use degenbot_math::v2::IntHopState;

use crate::families::solidly as solidly_read;

use crate::state::state_history::{ReorgJournal, ReservePairPoolState, V2BlockDelta};
use crate::state::v3_state::SimulateSwapError;

use super::door::{FamilyApply, Observe, Observed, ReadOutcome, SwapReadRequest, SyncEvent};

/// The state slices + their registration DTOs — the family's read surface
/// lives in `crate::v2_state` / `crate::aerodrome_v2_state` (I/O-free per
/// ADR-001) and is re-exported here so the family module presents the whole
/// read story under one path.
pub use crate::state::aerodrome_v2_state::{
    AerodromeV2PoolIdentity, AerodromeV2PoolState, RegisterAerodromePoolError,
    RegisterAerodromeV2PoolParams,
};
pub use crate::state::v2_state::{
    RegisterV2PoolError, RegisterV2PoolParams, V2PoolIdentity, V2PoolState,
};

/// Design selection by state slice (read-only): the variant IS the design —
/// the V2/Aerodrome match happens inside the module, never at the callers.
#[derive(Clone, Copy)]
pub enum ReservePairState<'a> {
    /// The Uniswap V2 design slice (per-direction retained fee fraction).
    V2 {
        /// The pool's immutable registration identity (the fees ride here).
        identity: &'a V2PoolIdentity,
        /// The pool's mutable state (reserves + journal + price clock).
        state: &'a V2PoolState,
    },
    /// The Aerodrome V2 design slice (unidirectional fee fraction). The
    /// volatile design — a `stable` identity is refused (see [`read_swap`]).
    Aerodrome {
        /// The pool's immutable registration identity (fee + `stable` flag).
        identity: &'a AerodromeV2PoolIdentity,
        /// The pool's mutable state (reserves + journal + price clock).
        state: &'a AerodromeV2PoolState,
    },
}

/// The capture door — the resolve projection: one fresh projection of one V2
/// hop, refused rather than defaulted. Pure over the family state slice; the
/// substrate resolve row does the registry lookup and the solver-vocabulary
/// wrap ([`degenbot_math::v2::IntHopState`] → `ResolvedHop::V2`).
///
/// The family's resolve projection is the V2 hop: the family's `HopType::V2`
/// row projects V2 entries (an Aerodrome pool routed to the V2 lane refuses
/// at the registry seam — neither identity present — and that behavior is
/// unchanged here). The projection is total: the constant-product payload is
/// derivable from every well-formed slice, and directional viability is the
/// solve branch's discovery (the math surfaces the revert classes).
///
/// Orientation selection is design-private to the V2 arm and shared with
/// [`read_swap`]'s V2 read ([`v2_orientation`]) — one design, one convention,
/// stated once.
#[must_use]
pub fn capture_hop(
    state: &V2PoolState,
    identity: &V2PoolIdentity,
    zero_for_one: bool,
) -> IntHopState {
    // Orientation selection: the per-direction RETAINED fraction
    // `(gamma_numer, fee_denom)` rides the identity (`fee_token0` for
    // token0→token1, `fee_token1` for token1→token0); the reserves widen
    // `uint112` → `uint256` at the same stroke (the widening Solidity
    // performs before `getAmountOut`'s math).
    let (reserve_in, reserve_out, gamma_numer, fee_denom) = if zero_for_one {
        (
            state.reserve0.to::<U256>(),
            state.reserve1.to::<U256>(),
            identity.fee_token0.0,
            identity.fee_token0.1,
        )
    } else {
        (
            state.reserve1.to::<U256>(),
            state.reserve0.to::<U256>(),
            identity.fee_token1.0,
            identity.fee_token1.1,
        )
    };
    IntHopState::new(reserve_in, reserve_out, gamma_numer, fee_denom)
}

/// The Aerodrome volatile design's resolve projection: the orientation-
/// selected reserves plus the unidirectional FEE fraction converted to the
/// retained fraction the V2 hop payload carries (`gamma = denom − fee`;
/// algebraically the same constant-product read — the V2 form folds the
/// retained fraction into the input-side term, the fee-fraction form into
/// the amount, and the quotients coincide). The V2 solve lane's one wrap
/// for the lane's second entry arm (ADR-059 D6). A `stable` identity never
/// reaches this projection — the Solidly family's [`solidly_read`]
/// `capture_hop` owns it and the central rows route it there.
#[must_use]
pub fn capture_aerodrome_hop(
    state: &AerodromeV2PoolState,
    identity: &AerodromeV2PoolIdentity,
    zero_for_one: bool,
) -> IntHopState {
    let (reserve_in, reserve_out) = if zero_for_one {
        (state.reserve0.to::<U256>(), state.reserve1.to::<U256>())
    } else {
        (state.reserve1.to::<U256>(), state.reserve0.to::<U256>())
    };
    let (fee_numer, fee_denom) = identity.fee;
    IntHopState::new(
        reserve_in,
        reserve_out,
        fee_denom.saturating_sub(fee_numer),
        fee_denom,
    )
}

/// The swap-read door — ALL variants behind one entry. The V2 retained-
/// fraction convention and the Aerodrome fee-fraction convention are the
/// design-private arms' own; callers never map fees.
///
/// # Errors
///
/// [`SimulateSwapError::NotComputable`] on arithmetic overflow / invariant
/// violation (mirroring the on-chain `SafeMath` revert contract), on an
/// exact-output refusal (see [`SwapReadRequest::ExactOut`]), when an
/// Aerodrome design carries a `stable` identity (the Solidly family's read
/// arm owns that invariant — refusing beats mis-pricing with volatile math),
/// and when a V2 design carries a `stable_swap` identity WITHOUT its
/// construction-time decimal scales (the behavioral axis routes the Camelot
/// arm at this door, and a scale-less stable identity refuses rather than
/// guess). (The family carries no sparse-map miss — a reserve-pair slice has
/// no tick words to miss.)
#[must_use]
pub fn read_swap(design: ReservePairState<'_>, request: SwapReadRequest) -> ReadOutcome {
    match design {
        ReservePairState::V2 { identity, state } => {
            // Behavioral-axis routing (ADR-073): a `stable_swap = true`
            // identity's math family is the Solidly door's Camelot arm — the
            // constant product would mis-price it. The shared request passes
            // through UNCHANGED (one door vocabulary, no per-family adapter);
            // the `10**decimals` scales ride the identity (captured at
            // construction from the token registry, ADR-003 — the C5
            // variant-carried-scales precedent), and a stable identity
            // WITHOUT them answers the typed refusal here — never a guessed
            // scale, never CP math.
            if identity.stable_swap {
                let Some((d0, d1)) = identity.token0_decimals.zip(identity.token1_decimals) else {
                    return ReadOutcome::NotComputable;
                };
                let decimals = (
                    U256::from(10u64).pow(U256::from(d0)),
                    U256::from(10u64).pow(U256::from(d1)),
                );
                return solidly_read::read_swap(
                    solidly_read::SolidlyState::Camelot {
                        identity,
                        state,
                        decimals,
                    },
                    request,
                );
            }
            ReadOutcome::simple(read_v2_swap(state, identity, request))
        }
        ReservePairState::Aerodrome { identity, state } => {
            if identity.stable {
                // The stable invariant is the Solidly family's read arm (the
                // ADR-073 D6 landing order): the central rows route a stable
                // identity there. Refuse rather than mis-price with the
                // volatile constant-product math.
                return ReadOutcome::NotComputable;
            }
            ReadOutcome::simple(read_aerodrome_volatile_swap(state, identity, request))
        }
    }
}

// ---------------------------------------------------------------------------
// The override (hypothetical) read arm
// ---------------------------------------------------------------------------

/// The transient reserves a hypothetical (override) read runs against — the
/// caller's view of the pair, narrowed to the on-chain `uint112` width at the
/// FFI edge. The family's IMMUTABLE params (the per-direction fee fractions,
/// the Camelot `10**decimals` scales) are not carried here: they are borrowed
/// from the registered identity, exactly as the registered read borrows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverrideReserves {
    /// Hypothetical token0 reserve.
    pub reserve0: U112,
    /// Hypothetical token1 reserve.
    pub reserve1: U112,
}

/// The override read's computed payload: the swap's per-side magnitudes AND
/// the POST-swap reserves the transient implies.
///
/// A hypothetical read leaves no registered state to re-read, so the end state
/// is part of the read's own answer — the shell and the Python companion
/// consume it as data and never re-derive it from the deltas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverrideAmounts {
    /// Taken from the user (unsigned, input-token units).
    pub amount_in: U256,
    /// Given to the user (unsigned, output-token units).
    pub amount_out: U256,
    /// Post-swap reserves.
    pub end_reserves: OverrideReserves,
}

/// The override read's verdict — the registered arm's refusal set, plus the
/// end-state payload a transient read owes its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverrideReadOutcome {
    /// The transient read computed.
    Computed(OverrideAmounts),
    /// The registered arm's typed refusal: a zero exact-output request,
    /// overdraw against the transient reserves, a `stable` Aerodrome identity
    /// at the volatile door, a scale-less Camelot identity, an
    /// arithmetic/invariant overflow, an input past the `uint112` reserve
    /// width, or a request variant this family does not answer.
    NotComputable,
}

/// The override (hypothetical) read arm: the caller's reserves + the
/// registered pool's immutable params through the SAME read code the
/// registered slice takes ([`read_swap`] over a transient mirror — the
/// concentrated-liquidity override precedent, `v3::override_state`).
///
/// I/O-free by construction: a reserve-pair slice has no tick words, no
/// fetcher, and this arm reads no state owner — the transient is the whole
/// input besides the identity.
#[must_use]
pub fn read_swap_override(
    design: ReservePairState<'_>,
    reserves: OverrideReserves,
    journal_depth: usize,
    request: SwapReadRequest,
) -> OverrideReadOutcome {
    // The specified side is the caller's, the computed side the math's; the
    // direction selects the end-state orientation below.
    let (zero_for_one, specified_is_output, specified_amount) = match request {
        SwapReadRequest::ExactIn {
            zero_for_one,
            amount_in,
        } => (zero_for_one, false, amount_in),
        SwapReadRequest::ExactOut {
            zero_for_one,
            amount_out,
        } => (zero_for_one, true, amount_out),
        // A variant this family doesn't answer is the typed refusal (the
        // registered arm's own rule).
        _ => return OverrideReadOutcome::NotComputable,
    };
    let read = match design {
        ReservePairState::V2 { identity, state } => {
            let transient = transient_v2(state, reserves, journal_depth);
            read_swap(
                ReservePairState::V2 {
                    identity,
                    state: &transient,
                },
                request,
            )
        }
        ReservePairState::Aerodrome { identity, state } => {
            let transient = transient_aerodrome(state, reserves, journal_depth);
            read_swap(
                ReservePairState::Aerodrome {
                    identity,
                    state: &transient,
                },
                request,
            )
        }
    };
    let Ok(computed) = read.amount() else {
        return OverrideReadOutcome::NotComputable;
    };
    let (amount_in, amount_out) = if specified_is_output {
        (computed, specified_amount)
    } else {
        (specified_amount, computed)
    };
    let Some(end_reserves) = end_reserves(reserves, zero_for_one, amount_in, amount_out) else {
        return OverrideReadOutcome::NotComputable;
    };
    OverrideReadOutcome::Computed(OverrideAmounts {
        amount_in,
        amount_out,
        end_reserves,
    })
}

/// The post-swap reserves a computed read implies: the input side grows by the
/// amount that entered, the output side shrinks by the amount that left. Both
/// ends are CHECKED against the on-chain `uint112` width — an input too large
/// for a reserve refuses rather than wrapping.
fn end_reserves(
    reserves: OverrideReserves,
    zero_for_one: bool,
    amount_in: U256,
    amount_out: U256,
) -> Option<OverrideReserves> {
    let (input_side, output_side) = if zero_for_one {
        (reserves.reserve0, reserves.reserve1)
    } else {
        (reserves.reserve1, reserves.reserve0)
    };
    let grown = narrow_u112(amount_in).and_then(|delta| input_side.checked_add(delta))?;
    let shrunk = narrow_u112(amount_out).and_then(|delta| output_side.checked_sub(delta))?;
    Some(if zero_for_one {
        OverrideReserves {
            reserve0: grown,
            reserve1: shrunk,
        }
    } else {
        OverrideReserves {
            reserve0: shrunk,
            reserve1: grown,
        }
    })
}

/// Narrow an amount to the on-chain `uint112` reserve width (`None` = it does
/// not fit, so no such end reserve exists).
fn narrow_u112(amount: U256) -> Option<U112> {
    if amount > U256::from(U112::MAX) {
        None
    } else {
        Some(amount.to::<U112>())
    }
}

/// A transient mirror of the V2 slice carrying the caller's reserves — the
/// reserve-pair analogue of the concentrated-liquidity override transient: the
/// same slice type the design arms read, the hypothetical reserves, the
/// registered price clock and nonce, and a fresh journal (a read never pushes
/// a delta, so the journal stays inert).
fn transient_v2(
    state: &V2PoolState,
    reserves: OverrideReserves,
    journal_depth: usize,
) -> V2PoolState {
    V2PoolState {
        reserve0: reserves.reserve0,
        reserve1: reserves.reserve1,
        update_block: state.update_block,
        state_nonce: state.state_nonce,
        journal: ReorgJournal::<V2BlockDelta>::new(journal_depth),
    }
}

/// The Aerodrome V2 design's transient mirror (see [`transient_v2`]).
fn transient_aerodrome(
    state: &AerodromeV2PoolState,
    reserves: OverrideReserves,
    journal_depth: usize,
) -> AerodromeV2PoolState {
    AerodromeV2PoolState {
        reserve0: reserves.reserve0,
        reserve1: reserves.reserve1,
        update_block: state.update_block,
        state_nonce: state.state_nonce,
        journal: ReorgJournal::<V2BlockDelta>::new(journal_depth),
    }
}

/// The apply door — one buffered Sync event onto a reserve-pair slice
/// ([`door::FamilyApply`]): both designs' apply bodies are byte-identical
/// (the ADR-017 D3 collapse — full-state
/// [`crate::state::state_history::V2BlockDelta`] push + reserve overwrite + clock
/// advance), so one shared event ([`door::SyncEvent`]) and one impl per
/// slice serve BOTH structural families — the Solidly family's designs
/// (Aerodrome V2 stable, Camelot `stable_swap`) land on these same slices.
/// The neutral `apply_sync_by_pool_id` orchestration row stays on the
/// family-blind trait projection (`PoolEntry::as_reserve_pair_mut`) — a
/// per-family match there would reintroduce the arms ADR-017 D3 collapsed.
impl FamilyApply for V2PoolState {
    type Event = SyncEvent;

    fn apply_event(&mut self, event: Self::Event) {
        self.apply_sync(event.reserve0, event.reserve1, event.block_number);
    }
}

impl FamilyApply for AerodromeV2PoolState {
    type Event = SyncEvent;

    fn apply_event(&mut self, event: Self::Event) {
        self.apply_sync(event.reserve0, event.reserve1, event.block_number);
    }
}

/// The observe door — family-local GET behavior over one slice. Both probes
/// are answerable over both designs; a probe another family carries is a
/// caller bug (the door refuses to answer it rather than defaulting).
///
/// # Panics
///
/// Only on a foreign probe (never constructible by a correct caller — the
/// same backstop posture as the compiler-enforced event/slice pairing).
#[must_use]
pub fn observe(design: ReservePairState<'_>, probe: Observe) -> Observed {
    match probe {
        Observe::UpdateBlock => Observed::UpdateBlock(match design {
            ReservePairState::V2 { state, .. } => state.update_block,
            ReservePairState::Aerodrome { state, .. } => state.update_block,
        }),
        Observe::StateNonce => Observed::StateNonce(match design {
            ReservePairState::V2 { state, .. } => state.state_nonce,
            ReservePairState::Aerodrome { state, .. } => state.state_nonce,
        }),
        _ => unreachable!("a reserve-pair slice answers only the sync probes"),
    }
}

// ---------------------------------------------------------------------------
// V2 design-private read arm (retained-fraction constant product)
// ---------------------------------------------------------------------------

/// The V2 design's swap-read arm: the constant-product exact-input read
/// (`IntHopState::swap`, EVM-exact `getAmountOut`) and the exact-output
/// closed form (`IntHopState::swap_exact_out`, the router-side `getAmountIn`
/// with the `+1` floor-division compensation). Both arms compose the
/// capture door's projection — project, then swap or invert. The
/// zero-request exact-input sentinel is the legacy `Ok(ZERO)` (see the
/// module docs).
fn read_v2_swap(
    state: &V2PoolState,
    identity: &V2PoolIdentity,
    request: SwapReadRequest,
) -> Result<U256, SimulateSwapError> {
    // (The `stable_swap` behavioral-axis routing lives at the door: a stable
    // identity is delegated to the Solidly door's Camelot arm there, the
    // shared request passing through unchanged.)
    match request {
        SwapReadRequest::ExactIn {
            zero_for_one,
            amount_in,
        } => {
            // Legacy sentinel semantics, AS-IS: `getAmountOut(0) = 0`.
            if amount_in.is_zero() {
                return Ok(U256::ZERO);
            }
            capture_hop(state, identity, zero_for_one)
                .swap(amount_in)
                .map_err(|_| SimulateSwapError::NotComputable)
        }
        SwapReadRequest::ExactOut {
            zero_for_one,
            amount_out,
        } => {
            // Legacy `None` verdicts (the substrate row mapped them to
            // `NotComputable`): a zero request, overdraw, and the vanishing
            // denominator all refuse — `swap_exact_out` carries the same
            // refusal set, mapped onto the one error variant.
            if amount_out.is_zero() {
                return Err(SimulateSwapError::NotComputable);
            }
            capture_hop(state, identity, zero_for_one)
                .swap_exact_out(amount_out)
                .map_err(|_| SimulateSwapError::NotComputable)
        }
        // A variant this family doesn't answer (the N-token pair reads, the
        // concentrated liquidity canonical request) is a typed refusal.
        _ => Err(SimulateSwapError::NotComputable),
    }
}

// ---------------------------------------------------------------------------
// Aerodrome V2 volatile design-private read arm (fee-fraction constant product)
// ---------------------------------------------------------------------------

/// The Aerodrome V2 volatile design's swap-read arm: the Solidly volatile
/// (constant-product) read via `calc_exact_in_volatile`. The Aerodrome fee is
/// a unidirectional `(fee_numer, fee_denom)` FEE fraction (taken from the
/// input), matching `calc_exact_in_volatile`'s convention exactly. A stable
/// identity never reaches this arm (refused at the door); an exact-output
/// request is refused defensively — the central row answers it with its
/// typed family gap before the door is consulted.
fn read_aerodrome_volatile_swap(
    state: &AerodromeV2PoolState,
    identity: &AerodromeV2PoolIdentity,
    request: SwapReadRequest,
) -> Result<U256, SimulateSwapError> {
    let SwapReadRequest::ExactIn {
        zero_for_one,
        amount_in,
    } = request
    else {
        return Err(SimulateSwapError::NotComputable);
    };
    // Legacy sentinel semantics, AS-IS: `getAmountOut(0) = 0`.
    if amount_in.is_zero() {
        return Ok(U256::ZERO);
    }
    let token_in: u8 = u8::from(!zero_for_one);
    let (fee_numer, fee_denom) = identity.fee;
    solidly::calc_exact_in_volatile(
        amount_in,
        token_in,
        state.reserve0.to::<U256>(),
        state.reserve1.to::<U256>(),
        U256::from(fee_numer),
        U256::from(fee_denom),
    )
    .map_err(|_| SimulateSwapError::NotComputable)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic)]

    use super::*;

    use alloy::primitives::aliases::U112;
    use alloy::primitives::U512;

    use crate::state::state_history::{ReorgJournal, V2BlockDelta};

    // -------------------------------------------------------------------
    // Family pins (the sentinel + closed-form constants are the canonical
    // facade fixture's — `parity_v2_swap.rs` derives them from the closed
    // form; mirrored here so the door's arithmetic is pinned at the same
    // constants without crossing the facade seam).
    const RESERVE0: u64 = 1_000_000_000_000; // 1e6 USDC (6dp)
    const RESERVE1: u64 = 500_000_000_000_000_000; // 0.5 WETH (18dp)
    const AMOUNT_IN: u64 = 1_000_000_000;
    const EXPECTED_AMOUNT_OUT: u64 = 498_003_490_519_951;
    const EXPECTED_AMOUNT_IN: u64 = 1_000_000_000; // round-trips AMOUNT_IN

    fn v2_fixture() -> (V2PoolIdentity, V2PoolState) {
        let identity = V2PoolIdentity {
            address: alloy::primitives::Address::repeat_byte(0x11),
            token0: alloy::primitives::Address::repeat_byte(0x01),
            token1: alloy::primitives::Address::repeat_byte(0x02),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: alloy::primitives::Address::repeat_byte(0xfa),
            deployer: alloy::primitives::Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            variant: degenbot_uniswap::dex_identity::DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            token0_decimals: None,
            token1_decimals: None,
        };
        let state = V2PoolState {
            reserve0: U112::from(RESERVE0),
            reserve1: U112::from(RESERVE1),
            update_block: 19_000_000,
            state_nonce: 0,
            journal: ReorgJournal::<V2BlockDelta>::new(8),
        };
        (identity, state)
    }

    fn aerodrome_fixture(stable: bool) -> (AerodromeV2PoolIdentity, AerodromeV2PoolState) {
        let identity = AerodromeV2PoolIdentity {
            address: alloy::primitives::Address::repeat_byte(0xae),
            token0: alloy::primitives::Address::repeat_byte(0x01),
            token1: alloy::primitives::Address::repeat_byte(0x02),
            factory: alloy::primitives::Address::repeat_byte(0xaf),
            variant: degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile,
            stable,
            fee: (3, 1000),
            token0_decimals: 18,
            token1_decimals: 18,
        };
        let state = AerodromeV2PoolState {
            reserve0: U112::from(1_000_000u64),
            reserve1: U112::from(2_000_000u64),
            update_block: 7,
            state_nonce: 0,
            journal: ReorgJournal::<V2BlockDelta>::new(8),
        };
        (identity, state)
    }

    #[test]
    fn capture_hop_selects_orientation_specific_reserves_and_fees() {
        let (identity, state) = v2_fixture();
        // token0 in: input reserve = reserve0, retained fraction fee_token0.
        let hop = capture_hop(&state, &identity, true);
        assert_eq!(hop.reserve_in, U256::from(RESERVE0));
        assert_eq!(hop.reserve_out, U256::from(RESERVE1));
        assert_eq!(hop.gamma_numer, U256::from(997u64));
        assert_eq!(hop.fee_denom, U256::from(1000u64));

        // token1 in: orientation flips (reserves AND fee selection).
        let mut flipped = identity;
        flipped.fee_token1 = (9995, 10_000);
        let hop = capture_hop(&state, &flipped, false);
        assert_eq!(hop.reserve_in, U256::from(RESERVE1));
        assert_eq!(hop.reserve_out, U256::from(RESERVE0));
        assert_eq!(hop.gamma_numer, U256::from(9995u64));
        assert_eq!(hop.fee_denom, U256::from(10_000u64));
    }

    #[test]
    fn v2_exact_in_matches_the_closed_form_get_amount_out() {
        let (identity, state) = v2_fixture();
        let out = match read_swap(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(AMOUNT_IN),
            },
        ) {
            ReadOutcome::Amount(out) => out,
            other => panic!("canonical fixture computes, got {other:?}"),
        };
        assert_eq!(out, U256::from(EXPECTED_AMOUNT_OUT));
    }

    #[test]
    fn v2_exact_in_zero_request_keeps_the_legacy_sentinel() {
        let (identity, state) = v2_fixture();
        let out = match read_swap(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::ZERO,
            },
        ) {
            ReadOutcome::Amount(out) => out,
            other => panic!("zero input is the sentinel, not an error: {other:?}"),
        };
        assert_eq!(out, U256::ZERO);
    }

    #[test]
    fn v2_exact_out_closed_form_round_trips_the_fixture() {
        let (identity, state) = v2_fixture();
        let required = match read_swap(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            SwapReadRequest::ExactOut {
                zero_for_one: true,
                amount_out: U256::from(EXPECTED_AMOUNT_OUT),
            },
        ) {
            ReadOutcome::Amount(required) => required,
            other => panic!("canonical fixture computes, got {other:?}"),
        };
        assert_eq!(required, U256::from(EXPECTED_AMOUNT_IN));
    }

    #[test]
    fn v2_exact_out_refusals_are_the_legacy_none_verdicts() {
        let (identity, state) = v2_fixture();
        let design = ReservePairState::V2 {
            identity: &identity,
            state: &state,
        };
        // Zero request: the retired helper's `None` → `NotComputable`.
        assert_eq!(
            read_swap(
                design,
                SwapReadRequest::ExactOut {
                    zero_for_one: true,
                    amount_out: U256::ZERO,
                }
            ),
            ReadOutcome::NotComputable
        );
        // Overdraw: `amount_out >= reserve_out` is undefined.
        assert_eq!(
            read_swap(
                design,
                SwapReadRequest::ExactOut {
                    zero_for_one: true,
                    amount_out: U256::from(RESERVE1),
                }
            ),
            ReadOutcome::NotComputable
        );
    }

    // The exact-out saturation regime: the retired helper computed the
    // numerator `reserve_in * amount_out * fee_denom` in U256 and saturated
    // there (wrapping on the final `+1`), so its verdicts in this regime were
    // garbage. The door computes in U512 intermediates: while the exact input
    // fits U256 it must answer that exact value, and once it does not it must
    // refuse (`Overflow` mapped onto `NotComputable`) — never a saturated
    // value. The regime is reachable only at extreme fee parameters (with
    // realistic V2 fees and U112 reserves the U256 numerator cannot
    // overflow), which is why the legacy saturation stayed dormant.

    // The test-local independent oracle: the exact-out closed form recomputed
    // from raw parameters in U512 (the door's own intermediate width),
    // sharing no production code path. The U256 numerator rides along so each
    // case can pin where it sits relative to the disclosed
    // `reserve_in * amount_out * fee_denom > U256::MAX` edge.
    fn exact_out_oracle_u512(
        reserve_in: U256,
        reserve_out: U256,
        gamma_numer: u64,
        fee_denom: u64,
        amount_out: U256,
    ) -> (U512, U512) {
        let numerator = U512::from(reserve_in) * U512::from(amount_out) * U512::from(fee_denom);
        let denominator = U512::from(reserve_out - amount_out) * U512::from(gamma_numer);
        (numerator, numerator / denominator + U512::ONE)
    }

    #[test]
    fn v2_exact_out_saturation_is_exact_or_refused_not_legacy_garbage() {
        let (mut identity, mut state) = v2_fixture();
        // Reserves at the uint112 ceiling with a maximal fee denominator: the
        // only shape that reaches the saturation regime.
        state.reserve0 = U112::MAX;
        state.reserve1 = U112::MAX;
        let reserve_in = state.reserve0.to::<U256>();
        let reserve_out = state.reserve1.to::<U256>();
        let fee_denom = u64::MAX;

        // Deep saturation, exact input still fits: gamma one below the fee
        // denominator, half the output reserve requested. The legacy U256
        // numerator saturates here; the door must answer the exact value.
        let gamma_numer = fee_denom - 1;
        let amount_out = reserve_out / U256::from(2u64);
        identity.fee_token0 = (gamma_numer, fee_denom);
        identity.fee_token1 = (gamma_numer, fee_denom);
        let (numerator, oracle) =
            exact_out_oracle_u512(reserve_in, reserve_out, gamma_numer, fee_denom, amount_out);
        assert!(numerator > U512::from(U256::MAX));
        assert!(oracle <= U512::from(U256::MAX));
        let out = match read_swap(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            SwapReadRequest::ExactOut {
                zero_for_one: true,
                amount_out,
            },
        ) {
            ReadOutcome::Amount(out) => out,
            other => panic!("the exact input fits U256, got {other:?}"),
        };
        assert_eq!(U512::from(out), oracle);

        // The `+1`-wrap corner: one wei below the output reserve with gamma
        // one — the exact input itself exceeds U256::MAX (where the legacy
        // U256 `+1` wrapped). The door refuses; it never answers a wrapped
        // value.
        let gamma_numer = 1;
        let amount_out = reserve_out - U256::from(1u64);
        identity.fee_token0 = (gamma_numer, fee_denom);
        identity.fee_token1 = (gamma_numer, fee_denom);
        let (numerator, oracle) =
            exact_out_oracle_u512(reserve_in, reserve_out, gamma_numer, fee_denom, amount_out);
        assert!(numerator > U512::from(U256::MAX));
        assert!(oracle > U512::from(U256::MAX));
        assert_eq!(
            read_swap(
                ReservePairState::V2 {
                    identity: &identity,
                    state: &state,
                },
                SwapReadRequest::ExactOut {
                    zero_for_one: true,
                    amount_out,
                },
            ),
            ReadOutcome::NotComputable
        );
    }

    #[test]
    fn v2_exact_out_just_below_the_saturation_edge_still_computes() {
        let (mut identity, mut state) = v2_fixture();
        state.reserve0 = U112::MAX;
        state.reserve1 = U112::MAX;
        let reserve_in = state.reserve0.to::<U256>();
        let reserve_out = state.reserve1.to::<U256>();
        let amount_out = reserve_out - U256::from(1u64);
        // The largest fee denominator that keeps the legacy U256 numerator
        // `reserve_in * amount_out * fee_denom` below the U256::MAX edge
        // (asserted below: one more unit crosses), with a four-wei effective
        // denominator so the exact input lands just under the same edge.
        // Guards the boundary against a lazy blanket `NotComputable`.
        let fee_denom = 4_294_967_296u64;
        let gamma_numer = 4u64;
        identity.fee_token0 = (gamma_numer, fee_denom);
        identity.fee_token1 = (gamma_numer, fee_denom);

        let (numerator, oracle) =
            exact_out_oracle_u512(reserve_in, reserve_out, gamma_numer, fee_denom, amount_out);
        let one_more_fee_unit = U512::from(reserve_in) * U512::from(amount_out);
        assert!(numerator <= U512::from(U256::MAX));
        assert!(numerator + one_more_fee_unit > U512::from(U256::MAX));
        assert!(oracle <= U512::from(U256::MAX));
        let out = match read_swap(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            SwapReadRequest::ExactOut {
                zero_for_one: true,
                amount_out,
            },
        ) {
            ReadOutcome::Amount(out) => out,
            other => panic!("just below the edge the exact value computes, got {other:?}"),
        };
        assert_eq!(U512::from(out), oracle);
    }

    #[test]
    fn aerodrome_volatile_exact_in_matches_the_fee_fraction_convention() {
        let (identity, state) = aerodrome_fixture(false);
        let design = ReservePairState::Aerodrome {
            identity: &identity,
            state: &state,
        };
        // token0 in, fee (3, 1000): after-fee 997 of 1000;
        // out = 997 * 2_000_000 // (1_000_000 + 997) = 1992.
        let out = match read_swap(
            design,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(1000u64),
            },
        ) {
            ReadOutcome::Amount(out) => out,
            other => panic!("volatile fixture computes, got {other:?}"),
        };
        assert_eq!(out, U256::from(1992u64));

        // token1 in: the same unidirectional fee fraction, reserves flipped.
        let out = match read_swap(
            design,
            SwapReadRequest::ExactIn {
                zero_for_one: false,
                amount_in: U256::from(1000u64),
            },
        ) {
            ReadOutcome::Amount(out) => out,
            other => panic!("volatile fixture computes, got {other:?}"),
        };
        assert_eq!(out, U256::from(498u64));

        // The zero-request sentinel, AS-IS.
        assert_eq!(
            read_swap(
                design,
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::ZERO,
                }
            ),
            ReadOutcome::Amount(U256::ZERO)
        );
    }

    #[test]
    fn aerodrome_stable_identity_is_refused_not_mis_priced() {
        let (identity, state) = aerodrome_fixture(true);
        assert_eq!(
            read_swap(
                ReservePairState::Aerodrome {
                    identity: &identity,
                    state: &state,
                },
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::from(1000u64),
                }
            ),
            ReadOutcome::NotComputable
        );
    }

    #[test]
    fn aerodrome_exact_out_is_refused_defensively() {
        let (identity, state) = aerodrome_fixture(false);
        assert_eq!(
            read_swap(
                ReservePairState::Aerodrome {
                    identity: &identity,
                    state: &state,
                },
                SwapReadRequest::ExactOut {
                    zero_for_one: true,
                    amount_out: U256::from(1u64),
                }
            ),
            ReadOutcome::NotComputable
        );
    }

    // The Camelot-stable routing pin: the frozen Python-oracle snapshot's
    // SECOND `calc_exact_in_stable_camelot` case (independent oracle — the
    // constant comes from the Python companion, not from the leaf under
    // call). The fee fraction the oracle case records is (999, 1000), so the
    // identity stores the RETAINED gamma (1, 1000); the scales ride the
    // identity's construction-time decimals (6, 6).
    const CAM2: [u64; 8] = [
        29_963,  // amount_in
        115_634, // reserve0
        17_204,  // reserve1
        6,       // token0 decimals
        6,       // token1 decimals
        1,       // token0 retained gamma — (denom - fee) of the case's (999, 1000)
        1_000,   // fee denominator
        12,      // the recorded Python-oracle output
    ];

    fn camelot_stable_fixture() -> (V2PoolIdentity, V2PoolState) {
        let identity = V2PoolIdentity {
            address: alloy::primitives::Address::repeat_byte(0xc1),
            token0: alloy::primitives::Address::repeat_byte(0x01),
            token1: alloy::primitives::Address::repeat_byte(0x02),
            fee_token0: (CAM2[5], CAM2[6]),
            fee_token1: (CAM2[5], CAM2[6]),
            factory: alloy::primitives::Address::repeat_byte(0xfa),
            deployer: alloy::primitives::Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            variant: degenbot_uniswap::dex_identity::DexVariant::CamelotV2Stable,
            stable_swap: true,
            fee_denominator: Some(10_000),
            token0_decimals: Some(6),
            token1_decimals: Some(6),
        };
        let state = V2PoolState {
            reserve0: U112::from(CAM2[1]),
            reserve1: U112::from(CAM2[2]),
            update_block: 19_000_000,
            state_nonce: 0,
            journal: ReorgJournal::<V2BlockDelta>::new(8),
        };
        (identity, state)
    }

    #[test]
    fn camelot_stable_read_routes_to_the_solidly_door_and_matches_the_python_oracle() {
        // What: a `stable_swap = true` identity read through the reserve-pair
        // door must answer the Solidly door's Camelot invariant — the Python
        // companion's output — NOT the constant product (which answers a
        // different number on this fixture; asserted below). Where CP and the
        // Camelot invariant differ, the read follows the behavioral axis.
        let (identity, state) = camelot_stable_fixture();
        let out = match read_swap(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(CAM2[0]),
            },
        ) {
            ReadOutcome::Amount(out) => out,
            other => panic!("oracle fixture computes, got {other:?}"),
        };
        assert_eq!(out, U256::from(CAM2[7]));

        // The same request through the constant product answers a DIFFERENT
        // number on this fixture — the routing is load-bearing, not cosmetic.
        let cp_out = match capture_hop(&state, &identity, true).swap(U256::from(CAM2[0])) {
            Ok(out) => out,
            other => panic!("CP arithmetic on the fixture fits, got {other:?}"),
        };
        assert_ne!(
            cp_out,
            U256::from(CAM2[7]),
            "fixture must sit where CP and the Camelot invariant diverge"
        );
    }

    #[test]
    fn camelot_stable_identity_without_scales_refuses_at_the_door() {
        // What: a stable identity whose construction-time scales are absent
        // (token unregistered at registration) answers the typed refusal —
        // never a guessed scale, never CP math.
        let (mut identity, state) = camelot_stable_fixture();
        identity.token0_decimals = None;
        assert_eq!(
            read_swap(
                ReservePairState::V2 {
                    identity: &identity,
                    state: &state,
                },
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::from(CAM2[0]),
                },
            ),
            ReadOutcome::NotComputable
        );
        // One side missing is equally refused.
        identity.token0_decimals = Some(6);
        identity.token1_decimals = None;
        assert_eq!(
            read_swap(
                ReservePairState::V2 {
                    identity: &identity,
                    state: &state,
                },
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::from(CAM2[0]),
                },
            ),
            ReadOutcome::NotComputable
        );
    }

    #[test]
    fn capture_aerodrome_hop_converts_the_fee_fraction_to_the_retained_gamma() {
        // What: the V2 lane's Aerodrome volatile projection converts the
        // unidirectional FEE fraction (3, 1000) to the retained fraction
        // (997, 1000) the IntHopState payload carries, and the projected hop
        // answers the SAME getAmountOut the fee-fraction leaf does — the two
        // forms are algebraically one constant-product read.
        let (identity, state) = aerodrome_fixture(false);
        let hop = capture_aerodrome_hop(&state, &identity, true);
        assert_eq!(hop.reserve_in, U256::from(1_000_000u64));
        assert_eq!(hop.reserve_out, U256::from(2_000_000u64));
        assert_eq!(hop.gamma_numer, U256::from(997u64));
        assert_eq!(hop.fee_denom, U256::from(1000u64));

        // The projected hop's swap == the fee-fraction read's verdict.
        let via_hop = match hop.swap(U256::from(1000u64)) {
            Ok(out) => out,
            other => panic!("hop arithmetic on the fixture fits, got {other:?}"),
        };
        let via_read = match read_aerodrome_volatile_swap(
            &state,
            &identity,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(1000u64),
            },
        ) {
            Ok(out) => out,
            other => panic!("volatile fixture computes, got {other:?}"),
        };
        assert_eq!(via_hop, via_read);

        // Orientation flips the reserves; the unidirectional fee is shared.
        let hop = capture_aerodrome_hop(&state, &identity, false);
        assert_eq!(hop.reserve_in, U256::from(2_000_000u64));
        assert_eq!(hop.reserve_out, U256::from(1_000_000u64));
        assert_eq!(hop.gamma_numer, U256::from(997u64));
    }

    // -------------------------------------------------------------------
    // The override (hypothetical) read arm: caller-supplied reserves + the
    // registered identity's immutable params, through the registered read
    // code. I/O-free (a reserve-pair slice has no tick words and no fetcher).
    // -------------------------------------------------------------------
    const JOURNAL_DEPTH: usize = 8;

    /// The independent closed-form oracle (on-chain `getAmountOut`), recomputed
    /// from the transient reserves in `U256` — shares no production code path.
    fn exact_in_oracle(reserve_in: u64, reserve_out: u64, amount_in: u64) -> U256 {
        let amount_in_with_fee = U256::from(amount_in) * U256::from(997u64);
        amount_in_with_fee * U256::from(reserve_out)
            / (U256::from(reserve_in) * U256::from(1000u64) + amount_in_with_fee)
    }

    fn override_reserves(reserve0: u64, reserve1: u64) -> OverrideReserves {
        OverrideReserves {
            reserve0: U112::from(reserve0),
            reserve1: U112::from(reserve1),
        }
    }

    #[test]
    fn reserve_pair_override_exact_in_reads_the_transient_reserves() {
        // What: an override exact-in read follows the CALLER's reserves (not
        // the registered slice), keeps the registered identity's fee, and
        // reports the post-swap reserves the transient implies.
        let (identity, state) = v2_fixture();
        let reserves = override_reserves(2_000_000_000_000, 1_000_000_000_000_000_000);
        let read = read_swap_override(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            reserves,
            JOURNAL_DEPTH,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(AMOUNT_IN),
            },
        );
        let OverrideReadOutcome::Computed(amounts) = read else {
            panic!("the transient read computes, got {read:?}")
        };
        let expected_out = exact_in_oracle(2_000_000_000_000, 1_000_000_000_000_000_000, AMOUNT_IN);
        assert_eq!(amounts.amount_in, U256::from(AMOUNT_IN));
        assert_eq!(amounts.amount_out, expected_out);
        assert_ne!(
            expected_out,
            U256::from(EXPECTED_AMOUNT_OUT),
            "the transient reserves must answer a different number than the slice"
        );
        // Post-swap: the input side grew by the amount that entered, the
        // output side shrank by the amount that left.
        assert_eq!(
            amounts.end_reserves.reserve0,
            U112::from(2_000_000_000_000u64 + AMOUNT_IN)
        );
        assert_eq!(
            amounts.end_reserves.reserve1,
            U112::from(1_000_000_000_000_000_000u64 - expected_out.to::<u64>())
        );
        // The registered slice is untouched — the override is hypothetical.
        assert_eq!(state.reserve0, U112::from(RESERVE0));
        assert_eq!(state.reserve1, U112::from(RESERVE1));
    }

    #[test]
    fn reserve_pair_override_exact_out_closes_the_form_and_carries_the_end_reserves() {
        let (identity, state) = v2_fixture();
        let reserves = override_reserves(1_000_000_000_000, 500_000_000_000_000_000);
        let amount_out = U256::from(400_000_000_000_000_000u64);
        let read = read_swap_override(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            reserves,
            JOURNAL_DEPTH,
            SwapReadRequest::ExactOut {
                zero_for_one: true,
                amount_out,
            },
        );
        let OverrideReadOutcome::Computed(amounts) = read else {
            panic!("the exact-output transient read computes, got {read:?}")
        };
        // The closed form over the transient reserves:
        // ceil(reserve_in * amount_out * fee_denom / ((reserve_out - amount_out) * gamma)) + 1.
        let expected_in = U256::from(1_000_000_000_000u64) * amount_out * U256::from(1000u64)
            / ((U256::from(500_000_000_000_000_000u64) - amount_out) * U256::from(997u64))
            + U256::ONE;
        assert_eq!(amounts.amount_in, expected_in);
        assert_eq!(amounts.amount_out, amount_out);
        assert_eq!(
            amounts.end_reserves.reserve0,
            U112::from(1_000_000_000_000u64 + expected_in.to::<u64>())
        );
        assert_eq!(
            amounts.end_reserves.reserve1,
            U112::from(100_000_000_000_000_000u64)
        );
    }

    #[test]
    fn reserve_pair_override_overdraw_is_the_registered_arms_typed_refusal() {
        // What: an exact-output request that could not be paid out of the
        // TRANSIENT reserves is the same typed refusal the registered arm
        // answers — never a wrapped `uint112` end reserve.
        let (identity, state) = v2_fixture();
        let reserves = override_reserves(1_000_000_000_000, 500_000_000_000_000_000);
        let design = ReservePairState::V2 {
            identity: &identity,
            state: &state,
        };
        // The registered slice holds enough; the transient does not — the
        // refusal follows the override.
        assert_eq!(
            read_swap_override(
                design,
                override_reserves(1_000_000_000_000, 1_000u64),
                JOURNAL_DEPTH,
                SwapReadRequest::ExactOut {
                    zero_for_one: true,
                    amount_out: U256::from(1_001u64),
                },
            ),
            OverrideReadOutcome::NotComputable
        );
        // The zero request refuses exactly as it does at the registered arm.
        assert_eq!(
            read_swap_override(
                design,
                reserves,
                JOURNAL_DEPTH,
                SwapReadRequest::ExactOut {
                    zero_for_one: true,
                    amount_out: U256::ZERO,
                },
            ),
            OverrideReadOutcome::NotComputable
        );
    }

    #[test]
    fn reserve_pair_override_end_reserves_refuse_rather_than_wrap_uint112() {
        // What: an input that would push the transient reserve past the
        // on-chain `uint112` width refuses — the end state is never a wrapped
        // reserve.
        let (identity, state) = v2_fixture();
        let read = read_swap_override(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            override_reserves(RESERVE0, RESERVE1),
            JOURNAL_DEPTH,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(U112::MAX),
            },
        );
        assert_eq!(read, OverrideReadOutcome::NotComputable);
    }

    #[test]
    fn reserve_pair_override_fee_is_the_registered_identitys_retained_fraction() {
        // What: the transient carries reserves ONLY — the directional fee
        // fraction rides the registered identity, so a fee change with
        // identical reserves changes the read.
        let (mut identity, state) = v2_fixture();
        let read = |identity: &V2PoolIdentity| match read_swap_override(
            ReservePairState::V2 {
                identity,
                state: &state,
            },
            override_reserves(RESERVE0, RESERVE1),
            JOURNAL_DEPTH,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(AMOUNT_IN),
            },
        ) {
            OverrideReadOutcome::Computed(amounts) => amounts.amount_out,
            OverrideReadOutcome::NotComputable => panic!("the override read computes"),
        };
        // The transient reserves equal the registered ones, so the override
        // read answers the registered constant exactly.
        assert_eq!(read(&identity), U256::from(EXPECTED_AMOUNT_OUT));
        identity.fee_token0 = (99, 100);
        assert!(read(&identity) < U256::from(EXPECTED_AMOUNT_OUT));
    }

    #[test]
    fn reserve_pair_override_aerodrome_volatile_reads_the_fee_fraction() {
        let (identity, state) = aerodrome_fixture(false);
        let read = read_swap_override(
            ReservePairState::Aerodrome {
                identity: &identity,
                state: &state,
            },
            override_reserves(1_000_000, 2_000_000),
            JOURNAL_DEPTH,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(1000u64),
            },
        );
        let OverrideReadOutcome::Computed(amounts) = read else {
            panic!("the volatile override read computes, got {read:?}")
        };
        assert_eq!(amounts.amount_in, U256::from(1000u64));
        assert_eq!(amounts.amount_out, U256::from(1992u64));
        assert_eq!(
            amounts.end_reserves,
            override_reserves(1_001_000, 1_998_008)
        );

        // A stable identity stays the Solidly family's: the volatile door
        // refuses it exactly as the registered arm does.
        let (stable_identity, stable_state) = aerodrome_fixture(true);
        assert_eq!(
            read_swap_override(
                ReservePairState::Aerodrome {
                    identity: &stable_identity,
                    state: &stable_state,
                },
                override_reserves(1_000_000, 2_000_000),
                JOURNAL_DEPTH,
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::from(1000u64),
                },
            ),
            OverrideReadOutcome::NotComputable
        );
    }

    #[test]
    fn reserve_pair_override_camelot_stable_routes_to_the_solidly_door() {
        // What: a `stable_swap` identity read through the OVERRIDE arm keeps
        // the behavioral routing (ADR-073) — the transient reserves go through
        // the Solidly door's Camelot arm, not the constant product.
        let (identity, state) = camelot_stable_fixture();
        let reserves = override_reserves(CAM2[1], CAM2[2]);
        let read = read_swap_override(
            ReservePairState::V2 {
                identity: &identity,
                state: &state,
            },
            reserves,
            JOURNAL_DEPTH,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(CAM2[0]),
            },
        );
        let OverrideReadOutcome::Computed(amounts) = read else {
            panic!("the Camelot override read computes, got {read:?}")
        };
        assert_eq!(amounts.amount_out, U256::from(CAM2[7]));
        assert_eq!(
            amounts.end_reserves,
            override_reserves(CAM2[1] + CAM2[0], CAM2[2] - CAM2[7])
        );
        // The constant product on the same reserves answers a different
        // number — the routing is load-bearing on the override arm too.
        assert_ne!(
            exact_in_oracle(CAM2[1], CAM2[2], CAM2[0]),
            U256::from(CAM2[7])
        );
    }

    #[test]
    fn reserve_pair_override_zero_input_keeps_the_sentinel_and_the_reserves() {
        let (identity, state) = v2_fixture();
        let reserves = override_reserves(RESERVE0, RESERVE1);
        assert_eq!(
            read_swap_override(
                ReservePairState::V2 {
                    identity: &identity,
                    state: &state,
                },
                reserves,
                JOURNAL_DEPTH,
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::ZERO,
                },
            ),
            OverrideReadOutcome::Computed(OverrideAmounts {
                amount_in: U256::ZERO,
                amount_out: U256::ZERO,
                end_reserves: reserves,
            })
        );
    }

    #[test]
    fn apply_event_lands_the_sync_on_both_designs() {
        // The door is the family's named apply arm: both slices route the
        // same event vocabulary through their (byte-identical) ADR-017
        // `apply_sync` bodies — reserves, price clock, nonce, and journal.
        let event = SyncEvent {
            reserve0: U112::from(150u64),
            reserve1: U112::from(250u64),
            block_number: 7,
        };

        let (v2_identity, mut v2_state) = v2_fixture();
        v2_state.apply_event(event);
        assert_eq!(v2_state.reserve0, U112::from(150u64));
        assert_eq!(v2_state.reserve1, U112::from(250u64));
        assert_eq!(v2_state.update_block, 7);
        assert_eq!(v2_state.state_nonce, 1);
        assert_eq!(v2_state.journal.newest_block(), Some(7));
        let _ = v2_identity;

        let (aero_identity, mut aero_state) = aerodrome_fixture(false);
        aero_state.apply_event(event);
        assert_eq!(aero_state.reserve0, U112::from(150u64));
        assert_eq!(aero_state.reserve1, U112::from(250u64));
        assert_eq!(aero_state.update_block, 7);
        assert_eq!(aero_state.state_nonce, 1);
        assert_eq!(aero_state.journal.newest_block(), Some(7));
        let _ = aero_identity;
    }

    #[test]
    fn observe_answers_both_probes_over_both_designs() {
        let (v2_identity, v2_state) = v2_fixture();
        let v2 = ReservePairState::V2 {
            identity: &v2_identity,
            state: &v2_state,
        };
        assert_eq!(
            observe(v2, Observe::UpdateBlock),
            Observed::UpdateBlock(19_000_000)
        );
        assert_eq!(observe(v2, Observe::StateNonce), Observed::StateNonce(0));

        let (aero_identity, aero_state) = aerodrome_fixture(false);
        let aero = ReservePairState::Aerodrome {
            identity: &aero_identity,
            state: &aero_state,
        };
        assert_eq!(
            observe(aero, Observe::UpdateBlock),
            Observed::UpdateBlock(7)
        );
        assert_eq!(observe(aero, Observe::StateNonce), Observed::StateNonce(0));
    }
}
