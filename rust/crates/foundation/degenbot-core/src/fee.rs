//! The V2 fee vocabulary: one validated type per fee convention.
//!
//! Two conventions live beside each other in this engine and must never share
//! a bare `(u64, u64)`:
//!
//! - [`V2Fee`] is a **fee fraction** — the share of the input the swap CHARGES
//!   (`amount_in_after_fee = amount_in - amount_in * fee_numer / denominator`).
//!   This is the executor's and the Solidly/Aerodrome math's convention.
//! - [`FeeRatio`] is a **retained gamma** — the share of the input the swapper
//!   KEEPS (`gamma_numer / denominator`). This is the Uniswap-V2 solver
//!   convention the presets and pool identities carry.
//!
//! - [`FeeSpec`] is a **preset declaration**: either a declared [`FeeRatio`] or
//!   the per-pool MARKER ("this deployment reads its fee live"), which carries
//!   only the on-chain read's denominator and is never a ratio.
//!
//! The two number conventions are complements: a `997/1000` gamma is a
//! `3/1000` fee. Passing one numerator where the other belongs type-checks in a
//! bare tuple, which is why both directions are distinct types with `const fn`
//! constructors and typed refusals, and why [`V2Fee::from_retained`] /
//! [`V2Fee::retained_fraction`] are the only sanctioned conversions.

/// Why a discovered V2 fee cannot enter the solver or executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum V2FeeRefusal {
    /// The row carried no fee for this direction (or no denominator).
    #[error("V2 fee is missing")]
    Missing,
    /// The row's signed column was negative.
    #[error("V2 fee is negative")]
    Negative,
    /// The denominator was zero: a fraction with no scale.
    #[error("V2 fee denominator is zero")]
    ZeroDenominator,
    /// The charged share exceeded its denominator, which no fee can.
    #[error("V2 fee exceeds its denominator")]
    FeeExceedsDenominator,
    /// The charged share IS the denominator: a 100% fee, which prices every
    /// swap at zero output. Equality is a live defect class, not a valid fee.
    #[error("V2 fee charges the whole input (fee numerator equals its denominator)")]
    TotalFee,
}

/// Why a retained-gamma fraction was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FeeRatioRefusal {
    /// The denominator was zero: a fraction with no scale.
    #[error("retained fraction denominator is zero")]
    ZeroDenominator,
    /// The kept share was zero — the "read the pool live" marker, which is
    /// never a quotable ratio.
    #[error("retained gamma is zero (a 100% fee)")]
    ZeroRetained,
    /// The kept share exceeded the whole input.
    #[error("retained gamma exceeds its denominator")]
    RetainedExceedsDenominator,
}

/// A validated V2 **retained fraction** (the solver's gamma).
///
/// `gamma_numer / denominator` is the share of the input the swapper KEEPS
/// after the fee — `997/1000` is a 0.3% fee. This is the COMPLEMENT of
/// [`V2Fee`]'s charged fraction; the direction is named on both types because
/// a bare `(u64, u64)` cannot tell them apart, and a numerator passed in the
/// wrong direction type-checks.
///
/// Bounds: `denominator > 0`, `gamma_numer > 0`, `gamma_numer <= denominator`.
/// Equality is a zero fee (legal); zero gamma is a 100% fee (refused — it
/// prices every swap at zero output).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeRatio {
    gamma_numer: u64,
    denominator: u64,
}

impl FeeRatio {
    /// Validate one retained-gamma fraction.
    ///
    /// # Errors
    ///
    /// See [`FeeRatioRefusal`].
    pub const fn new(gamma_numer: u64, denominator: u64) -> Result<Self, FeeRatioRefusal> {
        if denominator == 0 {
            return Err(FeeRatioRefusal::ZeroDenominator);
        }
        if gamma_numer == 0 {
            return Err(FeeRatioRefusal::ZeroRetained);
        }
        if gamma_numer > denominator {
            return Err(FeeRatioRefusal::RetainedExceedsDenominator);
        }
        Ok(Self {
            gamma_numer,
            denominator,
        })
    }

    /// The KEPT share's numerator.
    #[must_use]
    pub const fn gamma_numer(self) -> u64 {
        self.gamma_numer
    }

    /// The fraction's denominator.
    #[must_use]
    pub const fn denominator(self) -> u64 {
        self.denominator
    }

    /// The CHARGED share's numerator (`denominator - gamma_numer`) — the
    /// [`V2Fee`] direction. Total under the construction bound.
    #[must_use]
    pub const fn fee_numer(self) -> u64 {
        self.denominator - self.gamma_numer
    }

    /// The legacy `(gamma_numer, denominator)` projection the Python seam and
    /// the pool-math kernels consume.
    #[must_use]
    pub const fn as_pair(self) -> (u64, u64) {
        (self.gamma_numer, self.denominator)
    }
}

/// A `DexIdentity` preset's declared fee.
///
/// Presets are `const` items, so the payload is const-constructible. The two
/// arms are the two truths a preset can carry:
///
/// - [`Retained`](Self::Retained) — a declared retained ratio, the value a
///   constant-product V2 deployment registers with.
/// - [`Marker`](Self::Marker) — "this deployment declares no fee; read it from
///   the pool". Only the DENOMINATOR is known (the on-chain read's scale); the
///   marker is never a [`FeeRatio`] and every construction door converts or
///   refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeSpec {
    /// A declared retained ratio.
    Retained(FeeRatio),
    /// The per-pool marker: no declared ratio, only the read's denominator.
    Marker {
        /// The on-chain read's denominator (e.g. `10_000`).
        denominator: u64,
    },
}

impl FeeSpec {
    /// A declared retained ratio.
    ///
    /// # Errors
    ///
    /// See [`FeeRatioRefusal`].
    pub const fn retained(gamma_numer: u64, denominator: u64) -> Result<Self, FeeRatioRefusal> {
        match FeeRatio::new(gamma_numer, denominator) {
            Ok(ratio) => Ok(Self::Retained(ratio)),
            Err(refusal) => Err(refusal),
        }
    }

    /// The per-pool marker carrying only the read's denominator.
    ///
    /// # Errors
    ///
    /// [`FeeRatioRefusal::ZeroDenominator`] when `denominator == 0` — a marker
    /// with no scale names nothing.
    pub const fn marker(denominator: u64) -> Result<Self, FeeRatioRefusal> {
        if denominator == 0 {
            return Err(FeeRatioRefusal::ZeroDenominator);
        }
        Ok(Self::Marker { denominator })
    }

    /// The declared ratio, or `None` for the marker. A door that must register
    /// a fee converts here and names its own typed refusal.
    #[must_use]
    pub const fn ratio(self) -> Option<FeeRatio> {
        match self {
            Self::Retained(ratio) => Some(ratio),
            Self::Marker { .. } => None,
        }
    }

    /// The fraction's denominator (both arms carry one).
    #[must_use]
    pub const fn denominator(self) -> u64 {
        match self {
            Self::Retained(ratio) => ratio.denominator(),
            Self::Marker { denominator } => denominator,
        }
    }

    /// The legacy `(gamma_numer, denominator)` projection at the Python seam:
    /// a marker projects as `(0, denominator)`, its historical shape.
    #[must_use]
    pub const fn as_pair(self) -> (u64, u64) {
        match self {
            Self::Retained(ratio) => ratio.as_pair(),
            Self::Marker { denominator } => (0, denominator),
        }
    }
}

/// A retained ratio as a `const` literal — the preset/test constructor.
///
/// Bounds are checked at compile time: an out-of-bound literal is a compile
/// error, so a preset cannot declare a fee a pool could not register.
///
/// # Panics
///
/// Panics when called outside a `const` context with an out-of-bound literal.
#[must_use]
#[expect(
    clippy::panic,
    reason = "const-asserted literal: the bound check runs at compile time at every call site"
)]
pub const fn fee_ratio(gamma_numer: u64, denominator: u64) -> FeeRatio {
    match FeeRatio::new(gamma_numer, denominator) {
        Ok(ratio) => ratio,
        Err(_) => panic!("retained-gamma literal violates the FeeRatio bounds"),
    }
}

/// A fee fraction as a `const` literal — the preset/test constructor.
///
/// Bounds are checked at compile time in a `const` context.
///
/// # Panics
///
/// Panics when called outside a `const` context with an out-of-bound literal.
#[must_use]
#[expect(
    clippy::panic,
    reason = "const-asserted literal: the bound check runs at compile time at every call site"
)]
pub const fn fee_fraction(fee_numer: u64, denominator: u64) -> V2Fee {
    match V2Fee::new(fee_numer, denominator) {
        Ok(fee) => fee,
        Err(_) => panic!("fee-fraction literal violates the V2Fee bounds"),
    }
}

/// A preset's declared retained ratio as a `const` literal.
///
/// # Panics
///
/// Panics when called outside a `const` context with an out-of-bound literal.
#[must_use]
#[expect(
    clippy::panic,
    reason = "const-asserted literal: the bound check runs at compile time at every call site"
)]
pub const fn fee_spec_retained(gamma_numer: u64, denominator: u64) -> FeeSpec {
    match FeeSpec::retained(gamma_numer, denominator) {
        Ok(spec) => spec,
        Err(_) => panic!("preset fee literal violates the FeeRatio bounds"),
    }
}

/// A preset's per-pool marker as a `const` literal — the deployment declares no
/// fee, only the scale of its on-chain read.
///
/// # Panics
///
/// Panics when called outside a `const` context with a zero denominator.
#[must_use]
#[expect(
    clippy::panic,
    reason = "const-asserted literal: the bound check runs at compile time at every call site"
)]
pub const fn fee_spec_marker(denominator: u64) -> FeeSpec {
    match FeeSpec::marker(denominator) {
        Ok(spec) => spec,
        Err(_) => panic!("preset fee marker must carry a non-zero denominator"),
    }
}

/// A validated V2 **fee fraction** charge.
///
/// `fee_numer / denominator` is the share of the input that the swap TAKES as
/// the fee (the complement of the solver's retained gamma — see
/// [`Self::from_retained`]). Construction is the only boundary that admits a
/// discovered fee; downstream values cannot invent or default one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Fee {
    fee_numer: u64,
    denominator: u64,
}

impl V2Fee {
    /// Validate one executor-facing V2 fee charge.
    ///
    /// # Errors
    ///
    /// Refuses a zero denominator ([`V2FeeRefusal::ZeroDenominator`]), a
    /// charge above one ([`V2FeeRefusal::FeeExceedsDenominator`]), and a charge
    /// of exactly one ([`V2FeeRefusal::TotalFee`]) — a 100% fee prices every
    /// swap at zero output, so no pool may register one.
    pub const fn new(fee_numer: u64, denominator: u64) -> Result<Self, V2FeeRefusal> {
        if denominator == 0 {
            return Err(V2FeeRefusal::ZeroDenominator);
        }
        if fee_numer > denominator {
            return Err(V2FeeRefusal::FeeExceedsDenominator);
        }
        if fee_numer == denominator {
            return Err(V2FeeRefusal::TotalFee);
        }
        Ok(Self {
            fee_numer,
            denominator,
        })
    }

    /// Convert a solver identity's RETAINED fraction into this fee direction.
    ///
    /// Total: [`FeeRatio`]'s bounds (`0 < gamma <= denominator`) make the
    /// complement a legal charge (`0 <= fee < denominator`).
    #[must_use]
    pub const fn from_retained(ratio: FeeRatio) -> Self {
        Self {
            fee_numer: ratio.fee_numer(),
            denominator: ratio.denominator(),
        }
    }

    /// The retained fraction consumed by V2 pool-state registration.
    ///
    /// Total: the construction bound (`fee_numer < denominator`) makes the
    /// complement a legal [`FeeRatio`] (`0 < gamma <= denominator`).
    #[must_use]
    pub const fn retained_fraction(self) -> FeeRatio {
        FeeRatio {
            gamma_numer: self.denominator - self.fee_numer,
            denominator: self.denominator,
        }
    }

    /// The charged share's numerator.
    #[must_use]
    pub const fn fee_numer(self) -> u64 {
        self.fee_numer
    }

    /// The fraction's denominator.
    #[must_use]
    pub const fn denominator(self) -> u64 {
        self.denominator
    }

    /// This direction's fee in the executor's bips-of-10,000 convention.
    ///
    /// Derived on demand, never stored: the constructor's `fee_numer <
    /// denominator` bound makes the quotient strictly below 10,000, so the
    /// narrowing to `u16` cannot truncate and no saturation arm exists.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "const fn cannot call `TryFrom`; the constructor bound proves the quotient < 10_000 at every construction site"
    )]
    pub const fn executor_bips(&self) -> u16 {
        ((self.fee_numer as u128 * 10_000) / self.denominator as u128) as u16
    }
}

/// A discovered V2 fee, or the typed reason it cannot be used.
pub type V2FeeProjection = Result<V2Fee, V2FeeRefusal>;

/// The two directional solver fees carried by a V2 pool identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2Fees {
    /// `token0->token1` fee fraction.
    pub token0: V2Fee,
    /// `token1->token0` fee fraction.
    pub token1: V2Fee,
}

impl V2Fees {
    /// The executor fee for one canonical token direction.
    #[must_use]
    pub const fn direction(self, zfo: bool) -> V2Fee {
        if zfo {
            self.token0
        } else {
            self.token1
        }
    }
}

/// Direction-specific fee projections loaded with a V2 connector edge.
/// Refusals remain typed on the edge so admission can reject the pool before
/// workspace registration instead of discovering bad fee data during solve or
/// composition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V2FeePair {
    /// `token0->token1` projection.
    pub token0: V2FeeProjection,
    /// `token1->token0` projection.
    pub token1: V2FeeProjection,
}

impl V2FeePair {
    /// Construct a pair from already-validated directional fees.
    #[must_use]
    pub const fn new(token0: V2Fee, token1: V2Fee) -> Self {
        Self {
            token0: Ok(token0),
            token1: Ok(token1),
        }
    }

    /// Project the signed DB columns from one discovered V2 pool row.
    ///
    /// This is THE projection over the raw `fee_token0` / `fee_token1` /
    /// `fee_denominator` columns: the columns are fee NUMERATORS, and
    /// `degenbot-db` keeps them raw (no fee rule of its own), so every
    /// consumer routes through here and one rule decides what a row means.
    #[must_use]
    pub fn from_discovered(
        fee_token0: Option<i64>,
        fee_token1: Option<i64>,
        denominator: Option<i64>,
    ) -> Self {
        Self {
            token0: Self::project(fee_token0, denominator),
            token1: Self::project(fee_token1, denominator),
        }
    }

    /// The explicit refusal used when a base row has no discovered subclass
    /// identity. It carries no substitute fee.
    #[must_use]
    pub const fn missing() -> Self {
        Self {
            token0: Err(V2FeeRefusal::Missing),
            token1: Err(V2FeeRefusal::Missing),
        }
    }

    fn project(fee: Option<i64>, denominator: Option<i64>) -> V2FeeProjection {
        let (Some(fee), Some(denominator)) = (fee, denominator) else {
            return Err(V2FeeRefusal::Missing);
        };
        if fee < 0 || denominator < 0 {
            return Err(V2FeeRefusal::Negative);
        }
        let (Ok(fee), Ok(denominator)) = (u64::try_from(fee), u64::try_from(denominator)) else {
            return Err(V2FeeRefusal::Negative);
        };
        V2Fee::new(fee, denominator)
    }

    /// Resolve both directions for solver admission.
    ///
    /// # Errors
    ///
    /// Returns the first typed directional refusal.
    pub fn resolve(self) -> Result<V2Fees, V2FeeRefusal> {
        Ok(V2Fees {
            token0: self.token0?,
            token1: self.token1?,
        })
    }

    /// Resolve the fee used by one executable direction.
    ///
    /// # Errors
    ///
    /// Returns the typed directional refusal.
    pub fn resolve_direction(self, zfo: bool) -> Result<V2Fee, V2FeeRefusal> {
        if zfo {
            self.token0
        } else {
            self.token1
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::{fee_ratio, FeeRatio, FeeRatioRefusal, FeeSpec, V2Fee, V2FeePair, V2FeeRefusal};

    /// The 100%-fee hole: a charge that takes the whole input used to be a
    /// valid `V2Fee` and reached registration as a zero-gamma retained pair.
    /// Construction is where it now dies.
    #[test]
    fn v2_fee_refuses_a_total_fee() {
        assert_eq!(V2Fee::new(1_000, 1_000), Err(V2FeeRefusal::TotalFee));
        assert_eq!(
            V2Fee::new(1_001, 1_000),
            Err(V2FeeRefusal::FeeExceedsDenominator)
        );
        assert_eq!(V2Fee::new(1, 0), Err(V2FeeRefusal::ZeroDenominator));
        assert!(V2Fee::new(999, 1_000).is_ok());
    }

    /// The executor bips are DERIVED from the stored fraction: the value the
    /// old stored field carried, computed at the boundary instead.
    #[test]
    fn executor_bips_are_derived_from_the_fraction() {
        let fee = V2Fee::new(3, 1_000).expect("0.3% fee is valid");
        assert_eq!(fee.executor_bips(), 30);
        assert_eq!(fee.fee_numer(), 3);
        assert_eq!(fee.denominator(), 1_000);
        // Just below the whole input: 9_999 bips, still inside the executor's
        // `u16` without a saturation arm.
        let almost_all = V2Fee::new(9_999, 10_000).expect("99.99% fee is a fee");
        assert_eq!(almost_all.executor_bips(), 9_999);
    }

    /// The discovered-column projection is ONE rule over the raw columns, and
    /// the tightened bound refuses at this door too.
    #[test]
    fn from_discovered_projects_raw_numerators_and_refuses_a_total_fee() {
        let fees = V2FeePair::from_discovered(Some(3), Some(25), Some(10_000));
        let resolved = fees.resolve().expect("both directions are fees");
        assert_eq!(resolved.token0.executor_bips(), 3);
        assert_eq!(resolved.token1.executor_bips(), 25);
        assert_eq!(
            V2FeePair::from_discovered(Some(10_000), Some(3), Some(10_000)).resolve_direction(true),
            Err(V2FeeRefusal::TotalFee)
        );
        assert_eq!(
            V2FeePair::from_discovered(None, Some(3), Some(10_000)).resolve_direction(true),
            Err(V2FeeRefusal::Missing)
        );
    }

    /// The retired `spec_bounds::validate_stable_swap_fee` bound, re-homed on
    /// the type: a retained gamma is scaled and positive.
    #[test]
    fn fee_ratio_bounds_replace_the_retired_stable_swap_fee_validator() {
        assert_eq!(fee_ratio(99_960, 100_000).gamma_numer(), 99_960);
        assert_eq!(fee_ratio(1, 100_000).fee_numer(), 99_999);
        assert_eq!(
            FeeRatio::new(0, 100_000),
            Err(FeeRatioRefusal::ZeroRetained)
        );
        assert_eq!(FeeRatio::new(997, 0), Err(FeeRatioRefusal::ZeroDenominator));
        assert_eq!(
            FeeRatio::new(1_001, 1_000),
            Err(FeeRatioRefusal::RetainedExceedsDenominator)
        );
        // Equality is a zero fee, which is legal.
        let zero_fee = fee_ratio(1_000, 1_000);
        assert_eq!(zero_fee.fee_numer(), 0);
        assert_eq!(V2Fee::from_retained(zero_fee).fee_numer(), 0);
    }

    /// The DIRECTION pin: gamma in -> charged fraction out -> gamma back. The
    /// two typed names exist because a bare numerator pair cannot say which
    /// direction it is, and the opposite reading is a different fee.
    #[test]
    fn retained_and_charged_directions_round_trip() {
        let retained = fee_ratio(997, 1_000);
        let charged = V2Fee::from_retained(retained);
        assert_eq!(charged.fee_numer(), 3);
        assert_eq!(charged.retained_fraction(), retained);

        // The mistaken direction is a *different* charge, never the same value.
        let inverted = fee_ratio(3, 1_000);
        assert_ne!(
            V2Fee::from_retained(inverted).fee_numer(),
            charged.fee_numer()
        );

        // A zero-gamma marker is not a ratio at all, and its legacy projection
        // is the historical `(0, denominator)` shape.
        let marker = FeeSpec::marker(10_000).expect("a scaled marker");
        assert_eq!(marker.ratio(), None);
        assert_eq!(marker.as_pair(), (0, 10_000));
        assert_eq!(marker.denominator(), 10_000);
        assert_eq!(FeeSpec::marker(0), Err(FeeRatioRefusal::ZeroDenominator));
        assert_eq!(
            FeeSpec::retained(997, 1_000)
                .expect("a declared ratio")
                .as_pair(),
            (997, 1_000)
        );
    }
}
