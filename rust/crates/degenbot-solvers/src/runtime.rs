//! The solver crate's injected runtime config (SU7MAE T4 / Q7b+Q13a): the
//! tunables the perf campaign kept re-reading from the process environment
//! become ONE plain-data config, packed by the outer owner (the engine
//! parses env at construction; the engine crate owns the env→config
//! mapping). Internals read the set-once holder — data, never the
//! environment — so tests construct the config directly and A/B per run.

//! This is the ADR-021 tripwire pattern ("reads no environment: the pump
//! packs the env stances into one config value at construction").

#[derive(Clone, Copy, Debug)]
pub enum AnchorSweep {
    Full,
    CenterOnly,
    Off,
}

/// The solver crate's runtime tunables. Defaults match the loop-17
/// production stances; the owner overrides at construction.
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
        }
    }
}

static RUNTIME: std::sync::OnceLock<SolveRuntimeConfig> = std::sync::OnceLock::new();

/// Install the runtime config (the engine calls this ONCE at construction;
/// the first caller wins and later calls are ignored — a second engine in
/// the same process reuses the process-wide stance).
pub fn set_runtime(config: SolveRuntimeConfig) {
    let _ = RUNTIME.set(config);
}

/// The process-wide config (defaults until [`set_runtime`]).
#[must_use]
pub fn runtime() -> &'static SolveRuntimeConfig {
    RUNTIME.get_or_init(SolveRuntimeConfig::default)
}

/// True while no owner has installed a config (the engine checks so its
/// construction-time env parse runs exactly once per process).
#[must_use]
pub fn runtime_is_default() -> bool {
    RUNTIME.get().is_none()
}
