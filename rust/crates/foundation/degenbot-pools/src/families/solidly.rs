//! The Solidly family read module (ADR-073 D1) — the foundation-layer home
//! of everything the Solidly stable family (Aerodrome V2 stable + Camelot
//! `stable_swap`) contributes to being read and resolved.
//!
//! GLOSSARY.md "Family read module": this module owns the state slices (the
//! I/O-free `crate::aerodrome_v2_state` / `crate::v2_state` types, ADR-001),
//! the swap-read arm (every variant behind one entry), the resolve projection
//! as a pure function of the family state slice, and the family-local
//! apply/observe behavior over those slices. It reads NO state owner and
//! consumes NO solver kernel — the hop payload it projects is wrapped into
//! the solvers crate's `SolidlyHopState` by the substrate resolve row (the
//! wrap is the row's named seam, not a dependency).
//!
//! # The family map
//!
//! Solidly = the STABLE swap style of the Solidly-derived DEXes: Aerodrome
//! V2 **stable** (the unidirectional-fee identity with `stable = true`) and
//! Camelot V2 **stable** (the `V2PoolIdentity` species with
//! `stable_swap = true`). The two designs are SPECIES of one structure
//! (ADR-059 D3 — manifest entries, never new types) sharing the stable
//! invariant shape (`x^3*y + y^3*x >= k`); they differ in the `k`/`get_y` pair
//! the invariant evaluation uses (the math leaf pre-bakes the two
//! orchestration flavors as `calc_exact_in_stable_solidly` /
//! `calc_exact_in_stable_camelot`) and in the fee convention (Aerodrome
//! stores the fee fraction directly; Camelot stores the per-direction
//! RETAINED gamma and the family inverts it).
//!
//! The VOLATILE designs of the same DEXes (Aerodrome V2 volatile, Camelot V2
//! volatile) are the reserve-pair family's constant-product designs —
//! `super::reserve_pair` owns them, and the central rows route a volatile
//! identity there. Each family's door refuses the other family's design
//! defensively rather than mis-price it (see [`read_swap`]).
//!
//! # The four intent doors (ADR-073 D4)
//!
//! - [`capture_hop`] — the resolve projection: one fresh projection of one
//!   hop from the family state slice + the caller's registry scales;
//! - [`read_swap`] — the swap-read arm, ALL variants behind one entry;
//! - [`apply_event`] — one buffered `Sync` event onto one slice;
//! - [`observe`] — family-local GET behavior over one slice.
//!
//! No separate calc doors: the family's contract-level view surface (the
//! on-chain `getAmountOut` over the stable invariant) IS the read door; the
//! solve-side envelope lines stay in the solvers crate's family module
//! (GLOSSARY "Family module").
//!
//! # The sentinel contract is NOT re-litigated here
//!
//! The legacy `Ok(U256::ZERO)` sentinel semantics stay AS-IS: a zero
//! exact-input read answers `Ok(ZERO)` (on-chain `getAmountOut(0) = 0`).
//! (The not-yet-Rust-side sentinel on non-STANDARD Curve styles is C6's
//! retirement, untouched here.)
//!
//! # Design-private math (structural, load-bearing)
//!
//! The Aerodrome arm and the Camelot arm share NO math: the Aerodrome read
//! goes through `degenbot_math::solidly::calc_exact_in_stable_solidly`, the
//! Camelot read through `degenbot_math::solidly::calc_exact_in_stable_camelot`
//! — different `k`/`get_y` pairs with different fee conventions. Sharing no code
//! between the two arms IS the enforcement, not a header comment. What the
//! two designs DO share is only the door plumbing and the event/probe
//! vocabulary below (the ADR-017 D3 collapse: both slices' `apply_sync`
//! bodies are byte-identical full-state
//! [`crate::state_history::V2BlockDelta`] pushes — Solidly mirrors v2-core's
//! `Sync(uint112, uint112)`).
//!
//! # Decimal scales (the one caller-supplied input)
//!
//! The stable invariant evaluates in the `10**decimals`-scaled domain. The
//! Aerodrome identity carries the decimal counts as registration facts and
//! its read arm scales from them; the Camelot species' `V2PoolIdentity`
//! carries no decimals (ADR-003: the token registry is the single source),
//! so the scales ride the [`SolidlyState::Camelot`] variant — a registry
//! fact beside the slice (the V4-hooks precedent: a fact the slice lacks
//! rides the variant). The RESOLVE projection takes its scales from the
//! caller in BOTH designs: the substrate resolve row owns the token-registry
//! lookup (the dedup'd token-decimals fetch, ADR-003) and passes the scales
//! into [`capture_hop`].

use alloy::primitives::{aliases::U112, U256};

use degenbot_math::solidly;
use degenbot_uniswap::dex_identity::DexVariant;

use crate::state_history::ReservePairPoolState;
use crate::v3_state::SimulateSwapError;

/// The state slices — the family's read surface lives in
/// `crate::aerodrome_v2_state` / `crate::v2_state` (I/O-free per ADR-001)
/// and is re-exported here so the family module presents the whole read
/// story under one path. The registration entries stay with the state
/// owner's named seams (`reserve_pair_orchestration` rows: Aerodrome pools
/// register through `register_aerodrome_pool`, Camelot stable pools through
/// `register_v2_pool`) — the Solidly family adds no registration surface.
pub use crate::aerodrome_v2_state::{AerodromeV2PoolIdentity, AerodromeV2PoolState};
pub use crate::v2_state::{V2PoolIdentity, V2PoolState};

/// One buffered Solidly-family event (a `Sync(uint112, uint112)`): the new
/// reserves and the block they landed at. The event type pairs with both
/// family slices at the apply door; the slices' own
/// [`ReservePairPoolState::apply_sync`] owns the journal + clock discipline
/// (ADR-017 D3 — Solidly mirrors v2-core's `Sync(uint112, uint112)` shape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SolidlyEvent {
    /// The new token0 reserve (on-chain `uint112`).
    pub reserve0: U112,
    /// The new token1 reserve (on-chain `uint112`).
    pub reserve1: U112,
    /// The block the update landed at (advances the price clock).
    pub block_number: u64,
}

/// Design selection by state slice (read-only): the variant IS the design —
/// the Aerodrome/Camelot match happens inside the module, never at the
/// callers.
#[derive(Clone, Copy)]
pub enum SolidlyState<'a> {
    /// The Aerodrome V2 design slice. The unidirectional fee fraction AND
    /// the decimal counts the stable read scales from ride the identity
    /// (registration facts).
    Aerodrome {
        /// The pool's immutable registration identity (fee + `stable` flag).
        identity: &'a AerodromeV2PoolIdentity,
        /// The pool's mutable state (reserves + journal + price clock).
        state: &'a AerodromeV2PoolState,
    },
    /// The Camelot stable design slice. The per-direction RETAINED fee
    /// fraction rides the identity; the identity carries no decimals
    /// (ADR-003: the token registry is the single source), so the
    /// `10**decimals` scales the stable invariant evaluates at ride the
    /// variant — a registry fact beside the slice (the V4-hooks precedent).
    Camelot {
        /// The pool's immutable registration identity (retained-fee gamma +
        /// `stable_swap` flag).
        identity: &'a V2PoolIdentity,
        /// The pool's mutable state (reserves + journal + price clock).
        state: &'a V2PoolState,
        /// The token decimal scales (`10**decimals`) the stable invariant
        /// evaluates at, `(token0, token1)`.
        decimals: (U256, U256),
    },
}

/// The family-local GET probes the observe door answers over one slice.
/// Every probe is answerable over both designs — the door never defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observe {
    /// The block of the last `Sync` (the family's price clock).
    UpdateBlock,
    /// The per-mutation state nonce (the staleness gate).
    StateNonce,
}

/// One observed fact, keyed by the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    /// The slice's price clock.
    UpdateBlock(u64),
    /// The slice's per-mutation nonce.
    StateNonce(u64),
}

/// The capture door's hop payload — the family's fresh projection of one
/// hop in the `degenbot-solidly-math` leaf's calling shape (un-oriented
/// reserves + `token_in` direction flag + the FEE fraction). The substrate
/// resolve row wraps it into the solvers crate's `SolidlyHopState` field by
/// field; this module owns no solver vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SolidlyHopProjection {
    /// Reserve of token0 (un-oriented).
    pub reserves_0: U256,
    /// Reserve of token1 (un-oriented).
    pub reserves_1: U256,
    /// Decimal scale of token0 (`10**decimals`).
    pub decimals_0: U256,
    /// Decimal scale of token1 (`10**decimals`).
    pub decimals_1: U256,
    /// Direction flag: `0` swaps token0→token1, `1` swaps token1→token0
    /// (the leaf's `token_in` convention).
    pub token_in: u8,
    /// Fee numerator — the fee fraction is `fee_numer / fee_denom`.
    pub fee_numer: U256,
    /// Fee denominator.
    pub fee_denom: U256,
    /// `true` for the stable (x³y + xy³) invariant, `false` for volatile
    /// (constant-product) — rides the identity; the solve branch owns the
    /// volatile/stable dispatch.
    pub stable: bool,
    /// DEX+variant discriminator — selects the solidly vs camelot math leaf.
    pub variant: DexVariant,
}

/// The capture door — the resolve projection: one fresh projection of one
/// Solidly hop. Pure over the family state slice + the caller's registry
/// scales; the substrate resolve row does the registry lookup and the
/// solver-vocabulary wrap.
///
/// The projection is total: the payload is derivable from every well-formed
/// slice, the stable flag rides the identity, and directional viability is
/// the solve branch's discovery (the math surfaces the revert classes).
///
/// Fee conventions are design-private here: the Aerodrome arm reads the fee
/// fraction off the identity directly; the Camelot arm inverts the stored
/// retained gamma (`fee = denom - gamma` over `denom`), selecting the
/// per-direction gamma by `zero_for_one` — the split the retired
/// `project_solidly` body owned.
#[must_use]
pub fn capture_hop(
    design: SolidlyState<'_>,
    decimals: (U256, U256),
    zero_for_one: bool,
) -> SolidlyHopProjection {
    match design {
        SolidlyState::Aerodrome { identity, state } => SolidlyHopProjection {
            reserves_0: state.reserve0.to::<U256>(),
            reserves_1: state.reserve1.to::<U256>(),
            decimals_0: decimals.0,
            decimals_1: decimals.1,
            token_in: u8::from(!zero_for_one),
            // The Aerodrome fee is stored as the fee fraction directly
            // (cf. the Camelot arm below).
            fee_numer: U256::from(identity.fee.0),
            fee_denom: U256::from(identity.fee.1),
            stable: identity.stable,
            variant: identity.variant,
        },
        SolidlyState::Camelot {
            identity, state, ..
        } => {
            // Camelot stores the per-direction RETAINED fraction
            // `(gamma_numer, fee_denom)`; the solidly math takes the FEE
            // fraction, so invert: `fee_numer = denom - gamma`,
            // `fee_denom = denom`. Selected by `zero_for_one` (token0 in →
            // fee_token0; token1 in → fee_token1).
            let (gamma, denom) = if zero_for_one {
                identity.fee_token0
            } else {
                identity.fee_token1
            };
            SolidlyHopProjection {
                reserves_0: state.reserve0.to::<U256>(),
                reserves_1: state.reserve1.to::<U256>(),
                decimals_0: decimals.0,
                decimals_1: decimals.1,
                token_in: u8::from(!zero_for_one),
                fee_numer: U256::from(denom.saturating_sub(gamma)),
                fee_denom: U256::from(denom),
                stable: identity.stable_swap,
                variant: identity.variant,
            }
        }
    }
}

/// The swap-read request: EVERY Solidly read variant, keyed by direction.
/// The family's contract-level view surface is the exact-input read (the
/// on-chain `getAmountOut` over the stable invariant); an exact-output
/// request is refused defensively — the central rows answer it with their
/// typed family gap before the door is consulted (the exact-out math the
/// Python companion uses is not a Rust read-path contract).
#[derive(Clone, Copy, Debug)]
pub enum SwapReadRequest {
    /// Exact-input swap (on-chain `getAmountOut`); a zero request answers the
    /// legacy `Ok(U256::ZERO)` sentinel (retired by C6, not here).
    ExactIn {
        /// Direction of the swap (token0 → token1 when `true`).
        zero_for_one: bool,
        /// The input amount (native decimals).
        amount_in: U256,
    },
    /// Exact-output — refused (see the enum docs). No central row routes an
    /// exact-output read to this family.
    ExactOut {
        /// Direction of the swap (token0 → token1 when `true`).
        zero_for_one: bool,
        /// The requested output amount (native decimals).
        amount_out: U256,
    },
}

/// The swap-read door — ALL variants behind one entry. The Aerodrome
/// fee-fraction convention and the Camelot retained-gamma convention are the
/// design-private arms' own; callers never map fees or `k`/`get_y` pairs.
///
/// # Errors
///
/// [`SimulateSwapError::NotComputable`] on arithmetic overflow /
/// non-convergence / invariant violation (mirroring the on-chain revert
/// contract the math leaf ports), on an exact-output request (see
/// [`SwapReadRequest::ExactOut`]), and when a design carries the OTHER
/// family's identity (a volatile Aerodrome identity, or a `stable_swap =
/// false` V2 identity — the reserve-pair family's read arm owns those
/// invariants; refusing beats mis-pricing with the wrong math). (The family
/// carries no sparse-map miss — a Solidly slice has no tick words to miss.)
pub fn read_swap(
    design: SolidlyState<'_>,
    request: SwapReadRequest,
) -> Result<U256, SimulateSwapError> {
    match design {
        SolidlyState::Aerodrome { identity, state } => {
            read_aerodrome_stable_swap(state, identity, request)
        }
        SolidlyState::Camelot {
            identity,
            state,
            decimals,
        } => read_camelot_stable_swap(state, identity, decimals, request),
    }
}

/// The apply door — one buffered Solidly-family event (a `Sync`) onto one
/// slice, generic over the family trait: both designs' apply bodies are
/// byte-identical (the ADR-017 D3 collapse — full-state
/// [`crate::state_history::V2BlockDelta`] push + reserve overwrite + clock
/// advance), so one door serves both. The neutral `apply_sync_by_pool_id`
/// orchestration row stays on the family-blind trait projection
/// (`PoolEntry::as_reserve_pair_mut`) — a per-family match there would
/// reintroduce the arms ADR-017 D3 collapsed.
pub fn apply_event<S: ReservePairPoolState>(state: &mut S, event: SolidlyEvent) {
    state.apply_sync(event.reserve0, event.reserve1, event.block_number);
}

/// The observe door — family-local GET behavior over one slice.
#[must_use]
pub fn observe(design: SolidlyState<'_>, probe: Observe) -> Observed {
    match probe {
        Observe::UpdateBlock => Observed::UpdateBlock(match design {
            SolidlyState::Aerodrome { state, .. } => state.update_block,
            SolidlyState::Camelot { state, .. } => state.update_block,
        }),
        Observe::StateNonce => Observed::StateNonce(match design {
            SolidlyState::Aerodrome { state, .. } => state.state_nonce,
            SolidlyState::Camelot { state, .. } => state.state_nonce,
        }),
    }
}

// ---------------------------------------------------------------------------
// Aerodrome stable design-private read arm (fee-fraction stable invariant)
// ---------------------------------------------------------------------------

/// The Aerodrome stable design's swap-read arm: the Solidly stable invariant
/// via `calc_exact_in_stable_solidly`. The identity carries the
/// unidirectional `(fee_numer, fee_denom)` FEE fraction and the token decimal
/// counts; the scales are derived at the leaf's calling convention
/// (`10**decimals`, mirroring the Python `AerodromeV2Pool` companion). A
/// volatile identity never reaches this arm (the central rows route it to
/// the reserve-pair family's Aerodrome door); refused defensively here.
fn read_aerodrome_stable_swap(
    state: &AerodromeV2PoolState,
    identity: &AerodromeV2PoolIdentity,
    request: SwapReadRequest,
) -> Result<U256, SimulateSwapError> {
    if !identity.stable {
        // The constant-product invariant is the reserve-pair family's
        // Aerodrome door (ADR-073 D1): the central rows route a volatile
        // identity there. Refuse rather than mis-price with stable math.
        return Err(SimulateSwapError::NotComputable);
    }
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
    solidly::calc_exact_in_stable_solidly(
        amount_in,
        token_in,
        state.reserve0.to::<U256>(),
        state.reserve1.to::<U256>(),
        U256::from(10u64).pow(U256::from(identity.token0_decimals)),
        U256::from(10u64).pow(U256::from(identity.token1_decimals)),
        U256::from(fee_numer),
        U256::from(fee_denom),
    )
    .map_err(|_| SimulateSwapError::NotComputable)
}

// ---------------------------------------------------------------------------
// Camelot stable design-private read arm (retained-gamma stable invariant)
// ---------------------------------------------------------------------------

/// The Camelot stable design's swap-read arm: the Camelot-variant stable
/// invariant via `calc_exact_in_stable_camelot` (Camelot's `k_camelot` /
/// `get_y_camelot` pair). The identity stores the per-direction RETAINED
/// fraction; the arm inverts it to the fee fraction the leaf takes. The
/// decimal scales ride the [`SolidlyState::Camelot`] variant (the V2
/// identity carries none — ADR-003). A `stable_swap = false` identity never
/// reaches this arm (the central rows route it to the reserve-pair family's
/// V2 door); refused defensively here.
///
/// No central row routes the Camelot stable READ through this door yet (the
/// resolve row routes the family's PROJECTION through [`capture_hop`]); the
/// read arm completes the family's contract so a routed row has one door to
/// call — the family's read story is whole even where the wiring lags.
fn read_camelot_stable_swap(
    state: &V2PoolState,
    identity: &V2PoolIdentity,
    decimals: (U256, U256),
    request: SwapReadRequest,
) -> Result<U256, SimulateSwapError> {
    if !identity.stable_swap {
        // The constant-product invariant is the reserve-pair family's V2
        // door (ADR-073 D1): the central rows route a volatile identity
        // there. Refuse rather than mis-price with stable math.
        return Err(SimulateSwapError::NotComputable);
    }
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
    let (gamma, denom) = if zero_for_one {
        identity.fee_token0
    } else {
        identity.fee_token1
    };
    solidly::calc_exact_in_stable_camelot(
        amount_in,
        token_in,
        state.reserve0.to::<U256>(),
        state.reserve1.to::<U256>(),
        decimals.0,
        decimals.1,
        U256::from(denom.saturating_sub(gamma)),
        U256::from(denom),
    )
    .map_err(|_| SimulateSwapError::NotComputable)
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic)]

    use super::*;

    use alloy::primitives::Address;

    use crate::state_history::{ReorgJournal, V2BlockDelta};

    // -------------------------------------------------------------------
    // Family pins. The Aerodrome stable constant is the canonical
    // `pool_handle_aerodrome` recorded value (the independent Python
    // `AerodromeV2Pool` companion's cross-check, ADR-005 Tier-2) — mirrored
    // here so the door's arithmetic is pinned at the same constant without
    // crossing the test-target seam. The Camelot constant is lifted from the
    // frozen Python-oracle snapshot (`degenbot-math`'s
    // `oracle_solidly_snapshot.txt`, first `calc_exact_in_stable_camelot`
    // case): the door's retained-gamma inversion marshals the identity's
    // `(3, 1000)` gamma into the leaf call that case records.
    const AERO_RESERVES: u128 = 1_000_000_000_000_000_000; // 1e18 (18dp)
    const AERO_AMOUNT_IN: u64 = 1_000_000_000_000_000_000; // 1e18
    const AERO_EXPECTED_OUT: u64 = 753_627_265_063_405_946;

    const CAM_SNAPSHOT: [u64; 8] = [
        243_373, // amount_in
        267_440, // reserve0
        495_969, // reserve1
        6,       // token0 decimals (the snapshot's scale argument, as recorded)
        6,       // token1 decimals (as recorded)
        3,       // token0 fee gamma — the identity stores (3, 1000)
        1_000,   // fee denominator
        770,     // the recorded oracle output
    ];

    fn aerodrome_fixture(stable: bool) -> (AerodromeV2PoolIdentity, AerodromeV2PoolState) {
        let identity = AerodromeV2PoolIdentity {
            address: Address::repeat_byte(0xae),
            token0: Address::repeat_byte(0x01),
            token1: Address::repeat_byte(0x02),
            factory: Address::repeat_byte(0xaf),
            variant: if stable {
                degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Stable
            } else {
                degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Volatile
            },
            stable,
            fee: (3, 10_000),
            token0_decimals: 18,
            token1_decimals: 18,
        };
        let state = AerodromeV2PoolState {
            reserve0: U112::from(AERO_RESERVES),
            reserve1: U112::from(AERO_RESERVES),
            update_block: 7,
            state_nonce: 0,
            journal: ReorgJournal::<V2BlockDelta>::new(8),
        };
        (identity, state)
    }

    fn camelot_fixture() -> (V2PoolIdentity, V2PoolState) {
        let identity = V2PoolIdentity {
            address: Address::repeat_byte(0xc1),
            token0: Address::repeat_byte(0x01),
            token1: Address::repeat_byte(0x02),
            fee_token0: (CAM_SNAPSHOT[5], CAM_SNAPSHOT[6]),
            fee_token1: (CAM_SNAPSHOT[5], CAM_SNAPSHOT[6]),
            factory: Address::repeat_byte(0xfa),
            deployer: Address::ZERO,
            init_hash: alloy::primitives::B256::ZERO,
            variant: degenbot_uniswap::dex_identity::DexVariant::CamelotV2Stable,
            stable_swap: true,
            fee_denominator: Some(10_000),
            token0_decimals: Some(6),
            token1_decimals: Some(6),
        };
        let state = V2PoolState {
            reserve0: U112::from(CAM_SNAPSHOT[1]),
            reserve1: U112::from(CAM_SNAPSHOT[2]),
            update_block: 19_000_000,
            state_nonce: 0,
            journal: ReorgJournal::<V2BlockDelta>::new(8),
        };
        (identity, state)
    }

    fn camelot_scales() -> (U256, U256) {
        (U256::from(CAM_SNAPSHOT[3]), U256::from(CAM_SNAPSHOT[4]))
    }

    #[test]
    fn aerodrome_stable_exact_in_matches_the_recorded_constant() {
        let (identity, state) = aerodrome_fixture(true);
        let design = SolidlyState::Aerodrome {
            identity: &identity,
            state: &state,
        };
        let out = match read_swap(
            design,
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(AERO_AMOUNT_IN),
            },
        ) {
            Ok(out) => out,
            other => panic!("canonical fixture computes, got {other:?}"),
        };
        assert_eq!(out, U256::from(AERO_EXPECTED_OUT));

        // Symmetry: equal balances + equal decimals ⇒ both directions yield
        // the same output.
        let out_rev = match read_swap(
            design,
            SwapReadRequest::ExactIn {
                zero_for_one: false,
                amount_in: U256::from(AERO_AMOUNT_IN),
            },
        ) {
            Ok(out) => out,
            other => panic!("canonical fixture computes, got {other:?}"),
        };
        assert_eq!(out_rev, out, "symmetric direction on equal balances");
    }

    #[test]
    fn aerodrome_stable_zero_request_keeps_the_legacy_sentinel() {
        let (identity, state) = aerodrome_fixture(true);
        assert_eq!(
            read_swap(
                SolidlyState::Aerodrome {
                    identity: &identity,
                    state: &state,
                },
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::ZERO,
                }
            ),
            Ok(U256::ZERO)
        );
    }

    #[test]
    fn aerodrome_volatile_identity_is_refused_not_mis_priced() {
        let (identity, state) = aerodrome_fixture(false);
        assert_eq!(
            read_swap(
                SolidlyState::Aerodrome {
                    identity: &identity,
                    state: &state,
                },
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::from(1000u64),
                }
            ),
            Err(SimulateSwapError::NotComputable)
        );
    }

    #[test]
    fn exact_out_is_refused_defensively() {
        let (identity, state) = aerodrome_fixture(true);
        assert_eq!(
            read_swap(
                SolidlyState::Aerodrome {
                    identity: &identity,
                    state: &state,
                },
                SwapReadRequest::ExactOut {
                    zero_for_one: true,
                    amount_out: U256::from(1u64),
                }
            ),
            Err(SimulateSwapError::NotComputable)
        );
    }

    #[test]
    fn camelot_stable_exact_in_matches_the_frozen_oracle_case() {
        let (identity, state) = camelot_fixture();
        // The door inverts the stored gamma (3, 1000) to the fee fraction
        // (997, 1000) the frozen oracle case records as its fee arguments —
        // the recorded output pins the inversion + the scale/direction
        // marshalling end to end (non-circular: the constant comes from the
        // independent Python oracle, not from the leaf under call).
        let out = match read_swap(
            SolidlyState::Camelot {
                identity: &identity,
                state: &state,
                decimals: camelot_scales(),
            },
            SwapReadRequest::ExactIn {
                zero_for_one: true,
                amount_in: U256::from(CAM_SNAPSHOT[0]),
            },
        ) {
            Ok(out) => out,
            other => panic!("oracle fixture computes, got {other:?}"),
        };
        assert_eq!(out, U256::from(CAM_SNAPSHOT[7]));

        // The zero-request sentinel, AS-IS.
        assert_eq!(
            read_swap(
                SolidlyState::Camelot {
                    identity: &identity,
                    state: &state,
                    decimals: camelot_scales(),
                },
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::ZERO,
                }
            ),
            Ok(U256::ZERO)
        );
    }

    #[test]
    fn camelot_volatile_identity_is_refused_not_mis_priced() {
        let (mut identity, state) = camelot_fixture();
        identity.stable_swap = false;
        assert_eq!(
            read_swap(
                SolidlyState::Camelot {
                    identity: &identity,
                    state: &state,
                    decimals: camelot_scales(),
                },
                SwapReadRequest::ExactIn {
                    zero_for_one: true,
                    amount_in: U256::from(1000u64),
                }
            ),
            Err(SimulateSwapError::NotComputable)
        );
    }

    #[test]
    fn capture_hop_projects_the_aerodrome_design_from_the_registry_scales() {
        let (identity, state) = aerodrome_fixture(true);
        let scales = (
            U256::from(10u64).pow(U256::from(6u64)),
            U256::from(10u64).pow(U256::from(18u64)),
        );
        // token0 in: token_in = 0; the fee fraction rides the identity
        // directly; the scales are the caller's registry lookup.
        let hop = capture_hop(
            SolidlyState::Aerodrome {
                identity: &identity,
                state: &state,
            },
            scales,
            true,
        );
        assert_eq!(hop.reserves_0, U256::from(AERO_RESERVES));
        assert_eq!(hop.reserves_1, U256::from(AERO_RESERVES));
        assert_eq!(hop.decimals_0, scales.0);
        assert_eq!(hop.decimals_1, scales.1);
        assert_eq!(hop.token_in, 0);
        assert_eq!(hop.fee_numer, U256::from(3u64));
        assert_eq!(hop.fee_denom, U256::from(10_000u64));
        assert!(hop.stable);
        assert_eq!(
            hop.variant,
            degenbot_uniswap::dex_identity::DexVariant::AerodromeV2Stable
        );

        // token1 in: the direction flag flips, the fee does not (the
        // unidirectional fee convention).
        let hop = capture_hop(
            SolidlyState::Aerodrome {
                identity: &identity,
                state: &state,
            },
            scales,
            false,
        );
        assert_eq!(hop.token_in, 1);
        assert_eq!(hop.fee_numer, U256::from(3u64));
    }

    #[test]
    fn capture_hop_inverts_the_camelot_retained_gamma_per_direction() {
        let (identity, state) = camelot_fixture();
        // gamma 3/1000 retained → fee = (997, 1000). token0 in reads
        // fee_token0.
        let hop = capture_hop(
            SolidlyState::Camelot {
                identity: &identity,
                state: &state,
                decimals: camelot_scales(),
            },
            camelot_scales(),
            true,
        );
        assert_eq!(hop.fee_numer, U256::from(997u64));
        assert_eq!(hop.fee_denom, U256::from(1000u64));
        assert_eq!(hop.token_in, 0);
        assert!(hop.stable);
        assert_eq!(
            hop.variant,
            degenbot_uniswap::dex_identity::DexVariant::CamelotV2Stable
        );

        // token1 in: the gamma selection flips (fee_token1 here).
        let (mut flipped_identity, flipped_state) = camelot_fixture();
        flipped_identity.fee_token1 = (9_970, 10_000);
        let hop = capture_hop(
            SolidlyState::Camelot {
                identity: &flipped_identity,
                state: &flipped_state,
                decimals: camelot_scales(),
            },
            camelot_scales(),
            false,
        );
        assert_eq!(hop.token_in, 1);
        assert_eq!(hop.fee_numer, U256::from(30u64));
        assert_eq!(hop.fee_denom, U256::from(10_000u64));

        let _ = identity;
        let _ = state;
    }

    #[test]
    fn apply_event_lands_the_sync_on_both_designs() {
        // The door is the family's named apply arm: both slices route the
        // same event vocabulary through their (byte-identical) ADR-017
        // `apply_sync` bodies — reserves, price clock, nonce, and journal.
        let event = SolidlyEvent {
            reserve0: U112::from(150u64),
            reserve1: U112::from(250u64),
            block_number: 7,
        };

        let (aero_identity, mut aero_state) = aerodrome_fixture(true);
        apply_event(&mut aero_state, event);
        assert_eq!(aero_state.reserve0, U112::from(150u64));
        assert_eq!(aero_state.reserve1, U112::from(250u64));
        assert_eq!(aero_state.update_block, 7);
        assert_eq!(aero_state.state_nonce, 1);
        assert_eq!(aero_state.journal.newest_block(), Some(7));
        let _ = aero_identity;

        let (cam_identity, mut cam_state) = camelot_fixture();
        apply_event(&mut cam_state, event);
        assert_eq!(cam_state.reserve0, U112::from(150u64));
        assert_eq!(cam_state.reserve1, U112::from(250u64));
        assert_eq!(cam_state.update_block, 7);
        assert_eq!(cam_state.state_nonce, 1);
        assert_eq!(cam_state.journal.newest_block(), Some(7));
        let _ = cam_identity;
    }

    #[test]
    fn observe_answers_both_probes_over_both_designs() {
        let (aero_identity, aero_state) = aerodrome_fixture(true);
        let aero = SolidlyState::Aerodrome {
            identity: &aero_identity,
            state: &aero_state,
        };
        assert_eq!(
            observe(aero, Observe::UpdateBlock),
            Observed::UpdateBlock(7)
        );
        assert_eq!(observe(aero, Observe::StateNonce), Observed::StateNonce(0));

        let (cam_identity, cam_state) = camelot_fixture();
        let cam = SolidlyState::Camelot {
            identity: &cam_identity,
            state: &cam_state,
            decimals: camelot_scales(),
        };
        assert_eq!(
            observe(cam, Observe::UpdateBlock),
            Observed::UpdateBlock(19_000_000)
        );
        assert_eq!(observe(cam, Observe::StateNonce), Observed::StateNonce(0));
    }
}
