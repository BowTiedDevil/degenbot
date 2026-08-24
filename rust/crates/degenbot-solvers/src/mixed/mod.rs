//! Mixed-path solver intake contract — hop-state value types + boundary
//! types + consts (ADR-015).
//!
//! These are the solver's **intake protocol**, not pool vocabulary and not
//! math-leaf vocabulary. `degenbot-bot`'s `resolve_path` projects `BotState`
//! into these types under `core.read()`, then the guard drops, then the
//! pure solve family (`solve_path` + the `solve_*_path_int` arms) runs
//! lock-free over a `ResolvedMixedPath`. See `CONTEXT.md` → "Resolve→solve
//! boundary" and ADR-015 for the seam rationale and the deferred hop-shape
//! review.
//!
//! Relocated from `degenbot-bot/src/solvers/arb_engine/mod.rs`; re-exported
//! at the old path so callers compile unchanged during the staged relocation.

use std::collections::HashMap;
use std::sync::Arc;

use alloy::primitives::{Address, U256};

use crate::mobius_v3_int::{IntV3TickRangeSequence, V3WordProfile};
use degenbot_math::balancer::PowVersion;
use degenbot_math::curve::stableswap::{DVariant, YVariant};
use degenbot_math::v2::IntHopState;
use degenbot_uniswap::dex_identity::DexVariant;

// ---------------------------------------------------------------------------
// Snapshot data (V3/V4 registration intake)
// ---------------------------------------------------------------------------

/// V3 snapshot data: pool address → tick data (consumed at registration).
pub type V3SnapshotData = HashMap<Address, HashMap<i32, degenbot_pools::TickInfo>>;

/// V4 snapshot data: (`pool_manager`, `pool_id`) → tick data (consumed at registration).
pub type V4SnapshotData = HashMap<(Address, [u8; 32]), HashMap<i32, degenbot_pools::TickInfo>>;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum value that fits in a signed 128-bit integer.
///
/// V4's `BalanceDelta` packs two `int128` values. The `toInt128()` cast in
/// V4's `toBalanceDelta()` reverts with `SafeCastOverflow` if either
/// component exceeds this value. The solver must reject paths where any
/// V4 hop would produce amounts exceeding this limit.
pub const INT128_MAX: U256 = match U256::from_str_radix("ffffffffffffffffffffffffffffffff", 16) {
    Ok(v) => v,
    Err(_) => panic!("INT128_MAX hex is a valid U256"),
};

// ---------------------------------------------------------------------------
// Path types
// ---------------------------------------------------------------------------

/// Which engine owns a given hop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HopType {
    /// V2 constant-product hop
    V2,
    /// V3 concentrated-liquidity hop
    V3,
    /// V4 concentrated-liquidity hop (same math as V3, different settlement)
    V4,
    /// Solidly/Aerodrome/Camelot stable or volatile hop. Owns its own solve
    /// branch (Möbius precheck + golden-section over the
    /// `degenbot-solidly-math` leaf) — not concentrated-liquidity.
    SolidlyStable,
    /// Balancer V2 weighted pool hop (∏xᵂⁱ ≥ k). Owns its own solve branch
    /// (Möbius precheck + golden-section over the
    /// `degenbot-balancer-math` weighted leaf) — not concentrated-liquidity.
    BalancerWeighted,
    /// Balancer V2 stable pool hop (`StableSwap` invariant). Owns its own
    /// solve branch — not concentrated-liquidity.
    BalancerStable,
    /// Curve stableswap pool hop (`StableSwap` y-iteration). Owns its own
    /// solve branch — not concentrated-liquidity.
    CurveStableswap,
}

impl HopType {
    /// Whether this hop type uses concentrated-liquidity math.
    ///
    /// V3 and V4 hops are both CL — they share the same solver dispatch.
    #[must_use]
    pub const fn is_concentrated_liquidity(&self) -> bool {
        matches!(self, Self::V3 | Self::V4)
    }
}

/// A pool reference in a mixed path.
///
/// Internal representation: `hop_type` is derived from the associated
/// `BotState`'s `PoolEntry` variant at `register_path` time (ADR-006 D3 — the
/// engine never constructs pools, so it learns each hop's family from the
/// `BotState` that owns it). The public intake is [`PoolHop`].
#[derive(Clone, Debug)]
pub struct MixedPoolRef {
    /// Which engine owns this hop
    pub hop_type: HopType,
    /// For V2: `pool_id` in V2 engine. For V3: `pool_idx` in V3 engine.
    pub pool_key: u64,
    /// Direction (V2: implied by `pool_id` orientation; V3: explicit)
    pub zero_for_one: bool,
}

/// A single hop in a path submitted to [`ArbitrageEngine::register_path`].
///
/// The caller supplies the `BotState`-owned `pool_id` (obtained from
/// `PyBot::register_v*_pool` / `BotState::register_v*_pool`) and the swap
/// direction; the engine derives the pool family (`hop_type`) from the
/// `BotState`'s `PoolEntry` and rejects any `pool_id` not registered there
/// (ADR-006 D3). One `pool_id` per pool; orientation is selected via
/// `zero_for_one` (no forward/reverse id duplication).
#[derive(Clone, Copy, Debug)]
pub struct PoolHop {
    /// The `BotState`-owned pool id this hop references.
    pub pool_id: u64,
    /// Swap direction: token0→token1 when `true`.
    pub zero_for_one: bool,
}

/// A registered mixed arbitrage path.
#[derive(Clone, Debug)]
pub struct MixedPath {
    pub pools: Vec<MixedPoolRef>,
}

/// Resolved state for a Solidly/Aerodrome/Camelot hop, ready for the
/// Solidly solve branch. Carries everything the `degenbot-solidly-math`
/// leaf needs in the leaf's own contract shape: **un-oriented**
/// `reserves_0`/`reserves_1` + a `token_in` direction flag (matching
/// [`degenbot_math::solidly::calc_exact_in_stable_solidly`] etc. 1:1).
///
/// `variant` + `stable` together dispatch the math:
/// - `(AerodromeV2Stable, true)` → `calc_exact_in_stable_solidly`
/// - `(CamelotV2Stable, true)` → `calc_exact_in_stable_camelot`
/// - `(_, false)` → `calc_exact_in_volatile`
///
/// Decimals are carried as the **scale** (`10^decimals`, e.g. `1_000_000`
/// for a 6-decimal token) — what `calc_k` divides by. The `fee` pair is the
/// **fee fraction** (`amount_in_after_fee = amount_in - amount_in *
/// fee_numer / fee_denom`); the resolve arm converts each family's stored
/// fee shape into this convention (Aerodrome stores it directly as the fee
/// fraction; Camelot stores the retained-fraction gamma and the arm inverts
/// it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolidlyHopState {
    /// Reserve of token0 (un-oriented).
    pub reserves_0: U256,
    /// Reserve of token1 (un-oriented).
    pub reserves_1: U256,
    /// Decimals scale of token0 (`10^decimals`, e.g. `1_000_000`).
    pub decimals_0: U256,
    /// Decimals scale of token1 (`10^decimals`, e.g. `1_000_000`).
    pub decimals_1: U256,
    /// Direction flag: `0` swaps token0→token1, `1` swaps token1→token0.
    /// Mirrors `token_in` in [`degenbot_math::solidly::calc_exact_in_stable_solidly`].
    pub token_in: u8,
    /// Fee numerator — the fee fraction is `fee_numer / fee_denom`.
    pub fee_numer: U256,
    /// Fee denominator.
    pub fee_denom: U256,
    /// `true` for the stable (x³y + xy³) invariant, `false` for volatile
    /// (constant-product).
    pub stable: bool,
    /// DEX+variant discriminator — selects the solidly vs camelot math leaf.
    pub variant: DexVariant,
}

/// Resolved state for a Balancer V2 weighted pool hop, ready for the
/// weighted solve branch. Carries everything the
/// `degenbot-balancer-math::weighted_math::calc_out_given_in` leaf needs.
///
/// The fee is stored as a fraction of `ONE = 1e18` (the Balancer fixed-point
/// convention): `amount_in_after_fee = amount_in - amount_in * swap_fee / ONE`.
/// The `PowVersion` selects `FixedPoint::pow` fast paths (V1 general / V2
/// fast paths for y ∈ {1, 2, 4}).
///
/// For the V2-equivalent Möbius precheck, `gamma_numer` / `fee_denom` give
/// the retained fraction `gamma_numer / fee_denom` = `(ONE - swap_fee) / ONE`
/// scaled to a `u64` pair (the `IntHopState` fee convention).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancerWeightedHopState {
    /// Balance of the input token (upscaled to 18-decimal fixed-point).
    pub balance_in: U256,
    /// Balance of the output token (upscaled to 18-decimal fixed-point).
    pub balance_out: U256,
    /// Normalized weight of the input token (18-decimal, e.g. 5e17 for 50%).
    pub weight_in: U256,
    /// Normalized weight of the output token (18-decimal).
    pub weight_out: U256,
    /// Swap fee as a fraction of `ONE = 1e18` (e.g. 3e15 for 0.3%).
    pub swap_fee: U256,
    /// `PowVersion` discriminator — selects `FixedPoint::pow` fast paths.
    pub pow_version: PowVersion,
    /// Scaling factor for the input token (`10^(18 - decimals_in)`).
    /// The input amount (in native decimals) is multiplied by this before
    /// passing to the math leaf, and the math result is divided by the
    /// output's scaling factor to downscale back to native decimals.
    pub scaling_factor_in: U256,
    /// Scaling factor for the output token (`10^(18 - decimals_out)`).
    pub scaling_factor_out: U256,
}

/// Resolved state for a Balancer V2 stable pool hop (`StableSwap` invariant),
/// ready for the stable solve branch.
///
/// Balances are upscaled to 18-decimal fixed-point and BPT-skipped (for
/// `ComposableStablePools`). The invariant `D` is pre-computed at resolve
/// time (one-shot under the core lock) — never recomputed per search
/// iteration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalancerStableHopState {
    /// Amplification coefficient `amp` (deployed convention, includes
    /// `AMP_PRECISION=1000`).
    pub amp: U256,
    /// BPT-skipped upscaled balances (all non-BPT tokens).
    pub balances: Vec<U256>,
    /// Token index of the input (in the BPT-skipped list).
    pub token_index_in: usize,
    /// Token index of the output (in the BPT-skipped list).
    pub token_index_out: usize,
    /// Pre-computed invariant `D` (via `calculate_invariant` or
    /// `calculate_invariant_deployed`).
    pub invariant: U256,
    /// Swap fee as a fraction of `ONE = 1e18`.
    pub swap_fee: U256,
    /// Scaling factor for the input token (`10^(18 - decimals_in)`).
    pub scaling_factor_in: U256,
    /// Scaling factor for the output token.
    pub scaling_factor_out: U256,
}

/// Resolved state for a Curve stableswap pool hop, ready for the curve
/// solve branch.
///
/// Carries everything needed to simulate a swap: the rate-adjusted XP
/// array, the input/output token indices, amp (with `A_PRECISION` baked in),
/// fee, and the y/d variant discriminators.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurveStableswapHopState {
    /// `amp = a_coefficient * A_PRECISION` (Curve convention).
    pub amp: U256,
    /// `A_PRECISION` (100 for standard pools).
    pub a_precision: U256,
    /// Rate-adjusted balances `xp[i] = balances[i] * rate_multipliers[i] / PRECISION`.
    /// Pre-computed at resolve time.
    pub xp: Vec<U256>,
    /// Token index of the input.
    pub token_index_in: usize,
    /// Token index of the output.
    pub token_index_out: usize,
    /// Number of coins (== `xp.len()`).
    pub n_coins: U256,
    /// Swap fee in `FEE_DENOMINATOR = 1e10` units.
    pub fee: U256,
    /// `FEE_DENOMINATOR = 10^10`.
    pub fee_denom: U256,
    /// `PRECISION = 10^18`.
    pub precision: U256,
    /// Rate multiplier for the input token.
    pub rate_multiplier_in: U256,
    /// Rate multiplier for the output token.
    pub rate_multiplier_out: U256,
    /// y-iteration variant.
    pub y_variant: YVariant,
    /// d-iteration variant.
    pub d_variant: DVariant,
}

/// Resolved state for a single hop in a mixed path.
///
/// Each variant bundles only the data its hop type needs — no parallel
/// `Vec<Option<T>>` with `None` placeholders. The hop type is the enum
/// variant itself, not a separate index.
///
/// Variant sizes differ because V2 carries full reserve/fee state while
/// CL hops only carry a pre-built integer sequence pointer. This is
/// intentional and not a hot allocation path.
#[derive(Clone, Debug)]
pub enum ResolvedHop {
    /// V2 constant-product hop
    V2 { state: IntHopState },
    /// V3 concentrated-liquidity hop. `word_profiles` is the Stage-1 precomputed
    /// dense-range word-boundary profile table (parallel to `int_seq.ranges`), built
    /// once per `(pool, direction)` projection and shared via `Arc` so N paths
    /// reusing the pool re-walk a dense range once, not once per solve.
    V3 {
        int_seq: IntV3TickRangeSequence,
        word_profiles: Arc<Vec<Option<Arc<V3WordProfile>>>>,
    },
    /// V4 concentrated-liquidity hop (same CL math as V3, different settlement).
    /// `word_profiles`: as `Self::V3`.
    V4 {
        int_seq: IntV3TickRangeSequence,
        word_profiles: Arc<Vec<Option<Arc<V3WordProfile>>>>,
    },
    /// Solidly/Aerodrome/Camelot stable or volatile hop. Owns its own solve
    /// branch — NOT concentrated-liquidity, so `as_int_sequence()` returns
    /// `None`. See [`SolidlyHopState`].
    SolidlyStable { state: SolidlyHopState },
    /// Balancer V2 weighted hop. Owns its own solve branch — NOT
    /// concentrated-liquidity, so `as_int_sequence()` returns `None`.
    /// See [`BalancerWeightedHopState`].
    BalancerWeighted { state: BalancerWeightedHopState },
    /// Balancer V2 stable hop (`StableSwap`). Owns its own solve branch —
    /// NOT concentrated-liquidity, so `as_int_sequence()` returns `None`.
    /// See [`BalancerStableHopState`].
    BalancerStable { state: BalancerStableHopState },
    /// Curve stableswap hop. See [`CurveStableswapHopState`].
    CurveStableswap { state: CurveStableswapHopState },
}

impl ResolvedHop {
    /// The hop type for this resolved hop.
    #[must_use]
    pub const fn hop_type(&self) -> HopType {
        match self {
            Self::V2 { .. } => HopType::V2,
            Self::V3 { .. } => HopType::V3,
            Self::V4 { .. } => HopType::V4,
            Self::SolidlyStable { .. } => HopType::SolidlyStable,
            Self::BalancerWeighted { .. } => HopType::BalancerWeighted,
            Self::BalancerStable { .. } => HopType::BalancerStable,
            Self::CurveStableswap { .. } => HopType::CurveStableswap,
        }
    }

    /// The V2 `IntHopState`, if this is a V2 hop.
    #[must_use]
    pub const fn as_v2_state(&self) -> Option<&IntHopState> {
        match self {
            Self::V2 { state, .. } => Some(state),
            _ => None,
        }
    }

    /// The integer tick-range sequence, if this is a CL hop (V3 or V4).
    #[must_use]
    pub const fn as_int_sequence(&self) -> Option<&IntV3TickRangeSequence> {
        match self {
            Self::V3 { int_seq, .. } | Self::V4 { int_seq, .. } => Some(int_seq),
            Self::V2 { .. }
            | Self::SolidlyStable { .. }
            | Self::BalancerWeighted { .. }
            | Self::BalancerStable { .. }
            | Self::CurveStableswap { .. } => None,
        }
    }

    /// The precomputed dense-range word-boundary profile table (Stage-1 cache),
    /// if this is a CL hop (V3 or V4). `Arc`-shared across paths reusing the hop.
    #[must_use]
    pub fn as_word_profiles(&self) -> Option<&Arc<Vec<Option<Arc<V3WordProfile>>>>> {
        match self {
            Self::V3 { word_profiles, .. } | Self::V4 { word_profiles, .. } => Some(word_profiles),
            Self::V2 { .. }
            | Self::SolidlyStable { .. }
            | Self::BalancerWeighted { .. }
            | Self::BalancerStable { .. }
            | Self::CurveStableswap { .. } => None,
        }
    }

    /// The [`SolidlyHopState`], if this is a Solidly/Aerodrome/Camelot hop.
    #[must_use]
    pub const fn as_solidly_state(&self) -> Option<&SolidlyHopState> {
        match self {
            Self::SolidlyStable { state, .. } => Some(state),
            _ => None,
        }
    }
}

/// Resolved state for a mixed path, ready for solving.
///
/// V3 and V4 hops both use the same `IntV3TickRangeSequence` type (CL math
/// is identical). The [`ResolvedHop`] enum variant distinguishes which engine
/// owns each hop at the path level.
#[derive(Clone, Debug, Default)]
pub struct ResolvedMixedPath {
    /// Hops in path order.
    pub hops: Vec<ResolvedHop>,
    /// Whether this path is valid for solving
    pub valid: bool,
    /// Per-hop state nonces captured at resolve time (AV42C7 staleness gate).
    /// `state_nonces[i]` is `pool_state_nonce(pool_refs[i].pool_key)` at the
    /// resolve-time `core.read()` snapshot. The dispatch seam re-reads each
    /// hop's current nonce and skips candidates whose nonce has advanced —
    /// the solver computed against state the pump has since superseded.
    pub state_nonces: Vec<u64>,
    /// The max price-clock `update_block` across the path's CL (and V2) hops,
    /// captured at resolve time (two-stamp). The dispatch seam rejects a path
    /// whose max `update_block` is AHEAD of the solve `block_number` (the
    /// backfill/dispatch race, live path 10956) — solving with a future price
    /// is never legitimate and is caught BEFORE the publish-point verifier.
    pub max_update_block: u64,
}

/// Result from solving a single arbitrage path.
///
/// Includes optimality data, per-hop output amounts for the encoder, and
/// per-hop consumed input amounts for correct profit calculation and V4
/// int128 overflow detection.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SolvePathResult {
    /// Optimal input amount (uint256).
    pub optimal_input: U256,
    /// Profit = `final_output` - `consumed_inputs[0]` (uint256).
    /// Uses consumed input (not full specified input) for correct profit
    /// when the first hop partial-fills at a range boundary.
    pub profit: U256,
    /// Per-hop output amounts. `hop_outputs[i]` = output after hop `i`.
    /// For a 2-hop path: `[forward_out, final_output]`.
    pub hop_outputs: Vec<U256>,
    /// Per-hop consumed input amounts. `consumed_inputs[i]` = gross input
    /// actually consumed by hop `i` (including fees). For V2 hops, this
    /// equals the input to that hop. For V3/V4 hops, if the range boundary
    /// is hit, this may be less than the input — the unused remainder is
    /// retained by the caller (matching on-chain partial-fill behavior).
    pub consumed_inputs: Vec<U256>,
    /// Per-hop state nonces captured at resolve time (AV42C7 staleness gate).
    /// The dispatch seam re-reads each hop's current nonce and skips
    /// candidates whose nonce has advanced — the solver computed against
    /// state the pump has since superseded (the block-N solve used pool@N-1
    /// while on-chain@N has pool@N after a user swap).
    pub state_nonces: Vec<u64>,
    /// Per-hop solver pool state at solve time — for diagnostic cross-
    /// referencing against the sim's captured swaps. Each element is a
    /// hop-family-specific string, e.g.
    /// `"V3:sq=0x...,liq=...,fee=...,zfo=..."` or
    /// `"V2:rin=...,rout=..."`. Empty when the hop type has no pool-state
    /// diagnostic. Populated by `solve_path`; threaded into the
    /// `[sim-diag]` JSON so solver-state vs sim-state can be compared
    /// directly on `no-profit` / revert failures.
    pub solver_pool_states: Vec<String>,
}

// ---------------------------------------------------------------------------
// Pure solve + simulate family (ADR-015)
// ---------------------------------------------------------------------------

pub mod solve;

// Re-export the solve dispatcher + the simulate entry points reached by the
// orchestrator/tests so callers can use `degenbot_solvers::mixed::solve_path`
// without descending into `solve`.
pub use solve::{simulate_solidly_path, solve_path};
