//! The solver crate's injected runtime config (SU7MAE T4 / Q7b+Q13a): the
//! tunables the perf campaign kept re-reading from the process environment
//! become ONE plain-data config, packed by the outer owner and passed
//! down through the call chain (KAHU5W: the RUNTIME OnceLock is deleted;
//! the config is instance-scoped and threaded). Internals read the
//! passed-in config — data, never the environment — so tests construct
//! the config directly and A/B per run.

//! This is the ADR-021 tripwire pattern ("reads no environment: the owner
//! packs the config stances into one value at construction and threads it
//! to every solve").

#[derive(Clone, Copy, Debug)]
pub enum AnchorSweep {
    Full,
    CenterOnly,
    Off,
}

/// The solver crate's runtime tunables. Defaults match the loop-17
/// production stances; the owner overrides at construction. The bool flags
/// are deliberate — each names one rollout stance from the perf campaign
/// (clippy::struct_excessive_bools accepted: a stance PACK is the point of
/// the value).
#[expect(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug)]
pub struct SolveRuntimeConfig {
    /// Loop-15 event-solver rollout gate: DEGENBOT_WALK_EVENT_SOLVER=0
    /// forces the legacy grow + bisection.
    pub event_solver_legacy: bool,
    /// Loop-15 census gate: DEGENBOT_WALK_EVENT_CENSUS=1.
    pub walk_event_census: bool,
    /// DEGENBOT_WALK_ANCHOR_SWEEP: 0 = off, 2 = center-only, else full.
    pub anchor_sweep: AnchorSweep,
    /// Loop-18 tangent-lines-per-hop cap (default 32).
    pub max_tangent_lines: usize,
    /// Loop-18 composed-survivor line cap (default 48).
    pub sampled_compose_lines: usize,
    /// DEGENBOT_SOLVER_WALK_MEMO (result caching ON/OFF) — the owner builds
    /// its WalkMemo handle from these two stances.
    pub memo_on: bool,
    /// DEGENBOT_SOLVER_WALK_MEMO_STATS (recomposition census).
    pub memo_stats: bool,
    /// Loop-19 EXPERIMENT (`refine_model_anchor`): `walk_refine_window`
    /// brackets its ternary around the piece's model anchor when the anchor
    /// is inside the window (the EVM floor staircase perturbs the top at wei
    /// scale — see the [`crate::cl::active_set`] REFINE_BRACKET_WEI note),
    /// saving the ternary-narrowing probes.
    pub refine_model_anchor: bool,
    /// Loop-20 EXPERIMENT (`tangent_sample_by_mass`): CL tangent sampling
    /// ranks ranges by input capacity (`max_gross_input_in_range`) instead
    /// of even index spacing, keeping the high-volume shelves.
    pub tangent_sample_by_mass: bool,
    /// Loop-21 EXPERIMENT (`envelope_pruned_refine`): the active-set walk
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
