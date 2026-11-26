//! The solve tree's injected runtime config. The outer owner packs one
//! plain-data value and threads it through the call chain; internals read
//! that value rather than the process environment, and tests construct it
//! directly for per-run comparisons.

//! This follows ADR-021: the owner packs every runtime stance at construction
//! and passes the same configuration to every solve.

#[derive(Clone, Copy, Debug)]
pub enum AnchorSweep {
    Full,
    CenterOnly,
    Off,
}

/// The solve tree's runtime tunables. Defaults are the production stances;
/// the owner overrides them at construction. The bool flags are deliberate —
/// each names one runtime stance (`clippy::struct_excessive_bools` accepted:
/// a stance pack is the point of the value).
#[expect(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug)]
pub struct SolveRuntimeConfig {
    /// `DEGENBOT_WALK_EVENT_SOLVER=0` forces legacy grow + bisection
    /// instead of the event solver.
    pub event_solver_legacy: bool,
    /// `DEGENBOT_WALK_EVENT_CENSUS=1` enables the event census.
    pub walk_event_census: bool,
    /// `DEGENBOT_WALK_ANCHOR_SWEEP`: 0 = off, 2 = center-only, else full.
    pub anchor_sweep: AnchorSweep,
    /// Tangent-lines-per-hop cap (default 32).
    pub max_tangent_lines: usize,
    /// Composed-survivor line cap (default 48).
    pub sampled_compose_lines: usize,
    /// `DEGENBOT_SOLVER_WALK_MEMO` (result caching ON/OFF) — the owner builds
    /// its `WalkMemo` handle from these two stances.
    pub memo_on: bool,
    /// `DEGENBOT_SOLVER_WALK_MEMO_STATS` (recomposition census).
    pub memo_stats: bool,
    /// Opt-in model-anchor refinement (`refine_model_anchor`):
    /// brackets its ternary around the piece's model anchor when the anchor
    /// is inside the window (the EVM floor staircase perturbs the top at wei
    /// scale — see the `crate::solve::concentrated_liquidity::active_set` `REFINE_BRACKET_WEI` note),
    /// saving the ternary-narrowing probes.
    pub refine_model_anchor: bool,
    /// Opt-in mass-weighted sampling (`tangent_sample_by_mass`): concentrated liquidity tangent sampling
    /// ranks ranges by input capacity (`max_gross_input_in_range`) instead
    /// of even index spacing, keeping the high-volume shelves.
    pub tangent_sample_by_mass: bool,
    /// Envelope-pruned refinement (`envelope_pruned_refine`): the active-set walk
    /// intersects each refine window with the composed envelope bound's
    /// undisproved region — inputs the bound proves cannot beat the walk's
    /// best candidate are skipped without a simulation.
    pub envelope_pruned_refine: bool,
}

impl Default for SolveRuntimeConfig {
    fn default() -> Self {
        Self {
            event_solver_legacy: false,
            walk_event_census: false,
            anchor_sweep: AnchorSweep::Full,
            max_tangent_lines: 32,
            sampled_compose_lines: 48,
            memo_on: false,
            memo_stats: false,
            refine_model_anchor: false,
            tangent_sample_by_mass: false,
            envelope_pruned_refine: true,
        }
    }
}
