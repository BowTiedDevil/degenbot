//! The engine retune value — the typed operator re-parameterization crossing
//! the driver seam (epic 5TBT7L, task 3WI4EO).
//!
//! The engine's twin of the fleet's centralized posture feeders + wake
//! (43121b9): one typed value carries every config-derived knob the engine
//! packs at construction and the operator can re-apply at runtime through
//! [`EngineStages::apply_retune`](super::EngineStages::apply_retune).
//!
//! NOT named "stance" (the fleet-migration stance, ADR-042, and the KAHU5W
//! per-construction construction-stance values) and NOT "posture" (the
//! fleet's cordon concept) — see CONTEXT.md's "Engine retune" glossary entry.

use alloy::primitives::U256;
use hashbrown::HashSet;

use super::detached_cycle::DETACHED_INFLIGHT_CAP;

/// The typed operator re-parameterization value crossing the driver seam.
///
/// Construction packs the config-derived knobs ONCE ([`Self::from_config`] —
/// the J4HN66 / KAHU5W construction-stance trajectory completed for the
/// engine); a runtime operator retune overwrites the live values through
/// `ArbitrageEngine::apply_retune`, reached from [`EngineStages::apply_retune`](super::EngineStages::apply_retune).
///
/// `set_result_channel` is deliberately NOT represented here: a channel
/// install is wiring, not a retune.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineRetune {
    /// V3/V4 buffered-event max age. `None` disables expiry (the cockpit
    /// default — the hot-path expiry scan is gated off).
    pub event_buffer_max_age: Option<u64>,
    /// QTZGFL capacity-modulated admission: `true` draws `max(0, target -
    /// in-flight)` and SHEDS zero-budget cycles; `false` is the take-all
    /// no-shed operator stance.
    pub solve_admission: bool,
    /// QTZGFL admission target depth (un-merged-result pipe depth, in keys),
    /// clamped at application to `1..=DETACHED_INFLIGHT_CAP`.
    pub admission_target_depth: u64,
    /// QTZGFL retained (carried) key retention window `W` in blocks.
    pub admission_retention_blocks: u64,
    /// Registered-path cap (`None` = unlimited).
    pub path_cap: Option<usize>,
    /// Minimum result profit (in wei) for the delivery policy window (strict
    /// `>`).
    pub min_profit: U256,
    /// Maximum result profit (in wei) for the delivery policy window
    /// (inclusive).
    pub max_profit: U256,
    /// KJWIK5 diagnostic override: force the future-price deferral for these
    /// path ids. Production leaves it empty.
    pub force_deferred: Option<HashSet<u64>>,
}

impl EngineRetune {
    /// Pack the config-derived retune from the caller's typed [`BotConfig`]
    /// (the ONE parse point for the engine's construction knobs).
    ///
    /// Only construction/config-derived writes are folded here: the admission
    /// trio (packed like every other KAHU5W construction-stance value),
    /// `min_profit_wei`, and the schema defaults for the operator-only knobs
    /// ([`Self::path_cap`] and [`Self::event_buffer_max_age`] have no Rust
    /// config key — the `PyO3` driver sets them, T5's scope).
    ///
    /// [`BotConfig`]: ::degenbot_config::BotConfig
    #[must_use]
    pub fn from_config(cfg: &::degenbot_config::BotConfig) -> Self {
        Self {
            // No Rust config key: the cockpit disables expiry until the
            // driver configures a max age.
            event_buffer_max_age: None,
            // `DEGENBOT_SOLVE_ADMISSION` parse matrix (supervisor-confirmed
            // conservative default): unset/0/false => OFF (byte-identical
            // degrade); 1/true/on => the capacity-modulated draw. Test builds
            // always take the OFF arm.
            solve_admission: !cfg!(test) && cfg.solve.admission_shed,
            admission_target_depth: u64::try_from(cfg.solve.admission_target_depth)
                .unwrap_or(DETACHED_INFLIGHT_CAP)
                .clamp(1, DETACHED_INFLIGHT_CAP),
            admission_retention_blocks: cfg.solve.admission_retention_blocks,
            // No Rust config key: the driver installs the path cap.
            path_cap: None,
            min_profit: U256::from(cfg.solve.min_profit_wei),
            max_profit: U256::MAX,
            force_deferred: None,
        }
    }
}

impl Default for EngineRetune {
    fn default() -> Self {
        Self {
            event_buffer_max_age: None,
            solve_admission: false,
            admission_target_depth: DETACHED_INFLIGHT_CAP,
            admission_retention_blocks: 0,
            path_cap: None,
            min_profit: U256::ZERO,
            max_profit: U256::MAX,
            force_deferred: None,
        }
    }
}

#[cfg(test)]
#[expect(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::arb_engine::{ArbitrageEngine, EngineStages};
    use crate::bot_core::state_lock::StateLock;
    use crate::bot_core::{BotState, EpochDelta};
    use degenbot_config::BotConfigLoader;
    use std::sync::Arc;

    /// Build an engine from an EXPLICIT local config (no env/file/process
    /// layer), wrapped for the stage surface. Only `solve.admission_*` is
    /// overridden — the fleet-boot hash stays byte-identical to the schema
    /// default, so the construction is a legal boot rider, and the
    /// process-global solver floor is untouched.
    fn engine_and_stages(
        cli: &[(&str, &str)],
    ) -> (Arc<parking_lot::Mutex<ArbitrageEngine>>, EngineStages) {
        let mut loader = BotConfigLoader::new().without_env();
        for (key, value) in cli {
            loader = loader.with_cli(*key, *value);
        }
        let parsed = loader.load().expect("the local config must load");
        let cfg = Arc::new(parsed.config);
        let core = Arc::new(StateLock::new(BotState::new()));
        let engine = Arc::new(parking_lot::Mutex::new(ArbitrageEngine::with_core_cfg(
            core, &cfg,
        )));
        let stages = EngineStages::new(Arc::clone(&engine), Arc::new(EpochDelta::new(0u64)));
        (engine, stages)
    }

    /// Construction applies the value from the `BotConfig` fields (the
    /// KAHU5W/J4HN66 packing): the admission trio reflects the config, the
    /// profit window takes `min_profit_wei`, and the operator-only knobs keep
    /// their schema defaults.
    #[test]
    fn construction_applies_the_config_derived_retune() {
        let (engine, _stages) = engine_and_stages(&[
            ("solve.admission_target_depth", "3"),
            ("solve.admission_retention_blocks", "7"),
        ]);
        let e = engine.lock();
        assert_eq!(e.cycle.admission_target_depth, 3);
        assert_eq!(e.cycle.admission_retention_blocks, 7);
        // Defaults: no config key for the cap / max age, min_profit_wei = 0.
        assert_eq!(e.registry.cap(), None);
        assert!(!e.event_buffer_expiry_enabled);
        assert_eq!(e.delivery.min_profit, U256::ZERO);
        assert_eq!(e.delivery.max_profit, U256::MAX);
    }

    /// `EngineStages::apply_retune` overwrites every live value — the runtime
    /// operator entry (the engine's twin of the fleet's posture feeder).
    #[test]
    fn apply_retune_overwrites_the_live_values() {
        let (engine, stages) = engine_and_stages(&[]);
        let retune = EngineRetune {
            event_buffer_max_age: Some(99),
            solve_admission: true,
            admission_target_depth: 2,
            admission_retention_blocks: 5,
            path_cap: Some(11),
            min_profit: U256::from(7u64),
            max_profit: U256::from(9u64),
            force_deferred: Some(HashSet::from([42u64])),
        };
        stages.apply_retune(&retune);

        let e = engine.lock();
        assert!(e.event_buffer_expiry_enabled);
        assert!(e.cycle.solve_admission);
        assert_eq!(e.cycle.admission_target_depth, 2);
        assert_eq!(e.cycle.admission_retention_blocks, 5);
        assert_eq!(e.registry.cap(), Some(11));
        assert_eq!(e.delivery.min_profit, U256::from(7u64));
        assert_eq!(e.delivery.max_profit, U256::from(9u64));
        assert_eq!(e.cycle.force_deferred, Some(HashSet::from([42u64])));
    }
}
