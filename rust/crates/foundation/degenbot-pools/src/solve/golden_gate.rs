//! The two-sided golden replay gate: the single owner of the
//! concentrated-liquidity golden-gate correctness contract.
//!
//! One implementation of the rules every consumer of a capture replay
//! shares: the owning integration tests
//! (`tests/concentrated_liquidity_golden_replay.rs`) that gate the committed
//! corpora, and the solver-lab replay's measurement/readout pass, whose
//! over-shoot-only golden regeneration classifies through the same verdict.
//! No consumer carries its own copy of the tolerances or the comparison.
//!
//! The contract mirrors the solver's "never under-shoot the fine-grid
//! oracle" guarantee at a diagnostic epsilon:
//!
//! - under-shoot of the exact-wei golden profit is bounded by [`PROFIT_EPS`];
//! - over-shoot beyond [`OVER_SHOOT_TOLERANCE_WEI`] is a hard failure — the
//!   golden is the recorded exact result, so a solver exceeding it is either
//!   phantom profit (claiming profit that does not exist) or a stale golden
//!   needing regeneration. Never silent.
//! - the solve is pure integer math, so repeated runs must agree;
//! - a null golden marks a gate-skipped capture: production answered
//!   "nothing" for this pool state (the envelope bound fell below the
//!   floor), so the row is reconciled out of the gate and reported as
//!   telemetry, never failed against the gateless replay.
//!
//! Feature-gated beside the fixture machinery it evaluates (ADR-084 D3): the
//! C-build dependency stays out of every default dependency tree, and the
//! test lanes that own the corpora enable the feature explicitly.

// Misuse here is fatal by design: a malformed recorded golden is fixture
// corruption, so the parse panics with the field name instead of silently
// dropping a row out of a correctness gate (matches the fixture reader's
// convention).
#![expect(clippy::panic)]

use alloy::primitives::U256;
use serde_json::Value;

/// Max acceptable profit under-shoot (wei) of the exact-wei golden for the
/// coarsened search to count as within tolerance.
pub const PROFIT_EPS: u128 = 100_000;

/// Any over-shoot beyond this few-wei rounding tolerance is a hard failure.
pub const OVER_SHOOT_TOLERANCE_WEI: u128 = 8;

/// A replayed solution in gate terms: the solver's chosen input and the
/// per-hop outputs it reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaySolution {
    pub optimal_input: U256,
    pub hop_outputs: Vec<U256>,
}

impl ReplaySolution {
    /// The replayed net profit: the final hop output minus the input,
    /// saturating at zero.
    #[must_use]
    pub fn profit(&self) -> U256 {
        self.hop_outputs
            .last()
            .copied()
            .unwrap_or(U256::ZERO)
            .saturating_sub(self.optimal_input)
    }
}

/// The recorded golden for one capture row, reduced to what the gate
/// compares: the recorded optimal input and the golden net profit (final hop
/// output minus that input, saturating at zero).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoldenOutcome {
    pub optimal_input: U256,
    pub profit: U256,
}

/// The gate's verdict for one capture row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateVerdict {
    /// Null golden: gate-skipped capture, reconciled out of the gate
    /// (non-failing; callers surface the ungated replay profit as
    /// false-skip telemetry).
    GateSkipped,
    /// Both profit directions within tolerance.
    Within {
        under: U256,
        over: U256,
        input_delta: U256,
    },
    /// Under-shoot past [`PROFIT_EPS`]: the search lost profit vs the
    /// recorded optimum. Hard failure.
    UnderShoot {
        under: U256,
        over: U256,
        input_delta: U256,
    },
    /// Over-shoot past [`OVER_SHOOT_TOLERANCE_WEI`] — under-shoot is zero
    /// whenever this fires, so it is the only regeneration-eligible shape.
    /// Hard failure: phantom profit or a stale golden.
    OverShoot {
        under: U256,
        over: U256,
        input_delta: U256,
    },
    /// The replay produced no solution but a golden is recorded. Hard
    /// failure.
    MissingResult,
}

impl GateVerdict {
    /// Whether this verdict fails the gate.
    #[must_use]
    pub fn is_gate_failure(&self) -> bool {
        matches!(
            self,
            Self::UnderShoot { .. } | Self::OverShoot { .. } | Self::MissingResult
        )
    }

    /// The compared deltas (`under`, `over`, `input_delta`) when a replay
    /// result was compared against a recorded golden; `None` for the verdicts
    /// that never compare (gate-skipped, missing result).
    #[must_use]
    pub fn deltas(&self) -> Option<(&U256, &U256, &U256)> {
        match self {
            Self::Within {
                under,
                over,
                input_delta,
            }
            | Self::UnderShoot {
                under,
                over,
                input_delta,
            }
            | Self::OverShoot {
                under,
                over,
                input_delta,
            } => Some((under, over, input_delta)),
            Self::GateSkipped | Self::MissingResult => None,
        }
    }
}

/// Parse a capture row's `golden` field into the gate's terms.
///
/// `None` when the field is absent or JSON null — a gate-skipped capture.
///
/// # Panics
///
/// Panics when a present golden is malformed (a missing field, a
/// non-decimal string, or a non-array `hop_outputs`): fixture corruption is
/// unrecoverable for a correctness gate.
#[must_use]
pub fn parse_golden(golden: Option<&Value>) -> Option<GoldenOutcome> {
    let v = golden?;
    if v.is_null() {
        return None;
    }
    let optimal_input: U256 = v
        .get("optimal_input")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("golden optimal_input is missing or not a string"))
        .parse()
        .unwrap_or_else(|e| panic!("golden optimal_input does not parse as U256 ({e})"));
    let hop_outputs: Vec<U256> = v
        .get("hop_outputs")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("golden hop_outputs is missing or not an array"))
        .iter()
        .map(|x| {
            let s = x
                .as_str()
                .unwrap_or_else(|| panic!("golden hop_outputs entry is not a string"));
            s.parse().unwrap_or_else(|e| {
                panic!("golden hop_outputs entry {s:?} does not parse as U256 ({e})")
            })
        })
        .collect();
    let profit = hop_outputs
        .last()
        .copied()
        .unwrap_or(U256::ZERO)
        .saturating_sub(optimal_input);
    Some(GoldenOutcome {
        optimal_input,
        profit,
    })
}

/// Whether the ungated replay found profit the gate declined to solve — the
/// null-golden reconciliation volume callers surface as false-skip
/// telemetry.
#[must_use]
pub fn replay_profitable(replay: &ReplaySolution) -> bool {
    !replay.optimal_input.is_zero() && !replay.profit().is_zero()
}

/// Compare the replayed profit against the recorded golden in both
/// directions. The under-shoot and over-shoot deltas are mutually exclusive
/// by construction (one side is always zero), so an
/// [`GateVerdict::OverShoot`] verdict always carries `under == 0` and is the
/// only shape regeneration may refresh.
#[must_use]
pub fn classify(golden: Option<&GoldenOutcome>, replay: Option<&ReplaySolution>) -> GateVerdict {
    let Some(g) = golden else {
        return GateVerdict::GateSkipped;
    };
    let Some(r) = replay else {
        return GateVerdict::MissingResult;
    };
    let replay_profit = r.profit();
    let under = g.profit.saturating_sub(replay_profit);
    let over = replay_profit.saturating_sub(g.profit);
    let input_delta = if r.optimal_input >= g.optimal_input {
        r.optimal_input - g.optimal_input
    } else {
        g.optimal_input - r.optimal_input
    };
    if under > U256::from(PROFIT_EPS) {
        GateVerdict::UnderShoot {
            under,
            over,
            input_delta,
        }
    } else if over > U256::from(OVER_SHOOT_TOLERANCE_WEI) {
        GateVerdict::OverShoot {
            under,
            over,
            input_delta,
        }
    } else {
        GateVerdict::Within {
            under,
            over,
            input_delta,
        }
    }
}

/// The solve is pure integer math: every repetition must agree, and a rep
/// that yields nothing while another solves is disagreement. An empty slice
/// is vacuously consistent.
#[must_use]
pub fn results_consistent(reps: &[Option<ReplaySolution>]) -> bool {
    let Some(first) = reps.first() else {
        return true;
    };
    reps.iter().all(|r| r == first)
}
