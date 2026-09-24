//! The settlement strategy composition: sealed-block atomic arbitrage.
//!
//! Settlement is the settled-block reaction kind. It reacts to sealed blocks
//! through the arb engine's block pump rather than a hosted driver loop, so
//! this type is the strategy plane's *config* view of the arm — the values the
//! engine pump resolves before it drives a block — not a second driver.
//!
//! | Slot | Binding |
//! |---|---|
//! | **source** | the block pump's accepted-sealed-block feed |
//! | **infrastructure** | the engine's live pool registry + planning sandbox |
//! | **calculation** | the engine's arbitrage solver |
//! | **encoder** | the composed executor calldata |
//! | **simulator** | the engine's block simulation |
//! | **submission** | the revert-protecting relay fan-out |
//!
//! # Special-case ledger
//!
//! Every place settlement remains special, each with why it stays:
//!
//! - **Pump self-driving (no spawn factory).** The engine's arb pump owns the
//!   settlement loop, so the host registers no [`DriverSpawnFactory`] for it and
//!   `start_driving` skips it. Why: sealed-block work is driven by the block
//!   clock the pump already owns; generalizing the pump into a hosted loop is
//!   ADR-018's on-demand work, deferred until a second settled-block strategy
//!   exists.
//! - **Boot registration is unconditionally configured.** The hosted Python
//!   boot is the settlement arm, so it registers the settlement facet as
//!   [`FacetStatus::Configured`] even when `strategy.settlement.active` is
//!   false. Why: the runner resolves its live relay posture *after* the engine
//!   exists, and a dry-run boot that never broadcasts must still enable the
//!   arm (the live-mode gate lives in the runner, not in registration).
//! - **Process-global submission lane.** The boot installs the settlement
//!   [`NonceLane`] process-wide instead of scoping it under a host
//!   `LaneNamespace`. Why: the settlement seam is the Python-driven arm of the
//!   one hosted process, and every settlement submission must stamp through the
//!   shared operator-account authority no matter which engine instance signs.
//! - **Sole channel registrar.** `EngineChannelHandles::register_on` is the
//!   closure [`StrategyHost::mint`] hands the exclusive `&mut Hub`, so the
//!   engine's named channels register before the hub is shared. Why: named
//!   channels are `&mut self`; the host cannot mint them after sharing.
//! - **ADR-057 head-liveness coupling.** The hosted head feed runs from the
//!   Python settlement consumer's accepted-header clock, and
//!   `has_hosted_activity` short-circuits a settlement-only boot so it pays no
//!   new chain read. Why: the settlement pump already watches heads, so a
//!   second head source would double-read the chain nonce.
//!
//! [`DriverSpawnFactory`]: degenbot_bot::strategy_host::DriverSpawnFactory
//! [`FacetStatus`]: degenbot_bot::strategy_host::FacetStatus
//! [`NonceLane`]: degenbot_submission::NonceLane
//! [`StrategyHost::mint`]: degenbot_bot::strategy_host::StrategyHost::mint

use degenbot_config::BotConfig;

use crate::strategy_plane::{Strategy, StrategyName};

/// The settlement facet's resolved config: the activation flag and the
/// resolved broadcast endpoint set, mapped (not duplicated) from
/// `strategy.settlement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementConfig {
    /// Whether the settlement facet is active.
    pub active: bool,
    /// The broadcast endpoint URLs, in fan-out order. Empty when the facet
    /// names none; the readiness gate owns refusing an active-but-unsettled
    /// facet.
    pub endpoints: Vec<String>,
}

/// The settlement strategy composition.
#[derive(Debug, Clone)]
pub struct Settlement {
    config: SettlementConfig,
}

impl Settlement {
    /// Build the composition from the loaded config's `strategy.settlement`
    /// facet.
    #[must_use]
    pub fn from_config(cfg: &BotConfig) -> Self {
        let facet = &cfg.strategy.settlement;
        let endpoints: Vec<String> = facet
            .endpoints
            .as_deref()
            .into_iter()
            .flat_map(|raw| raw.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        Self {
            config: SettlementConfig {
                active: facet.active,
                endpoints,
            },
        }
    }

    /// The composed strategy config.
    #[must_use]
    pub fn config(&self) -> &SettlementConfig {
        &self.config
    }

    /// Consume the composition into its config.
    #[must_use]
    pub fn into_config(self) -> SettlementConfig {
        self.config
    }

    /// The plane name this composition selects under.
    #[must_use]
    pub const fn name(&self) -> StrategyName {
        StrategyName::Settlement
    }
}

impl Strategy for Settlement {
    const NAME: StrategyName = StrategyName::Settlement;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_config_maps_the_facet_and_splits_the_endpoint_set() {
        let mut cfg = BotConfig::default();
        cfg.strategy.settlement.active = true;
        cfg.strategy.settlement.endpoints =
            Some(String::from(" https://a.local , https://b.local ,, "));
        let settlement = Settlement::from_config(&cfg);
        assert_eq!(
            settlement.config(),
            &SettlementConfig {
                active: true,
                endpoints: vec![
                    String::from("https://a.local"),
                    String::from("https://b.local"),
                ],
            }
        );
    }

    #[test]
    fn an_unset_endpoint_set_resolves_to_empty() {
        let settlement = Settlement::from_config(&BotConfig::default());
        assert!(!settlement.config().active);
        assert!(settlement.config().endpoints.is_empty());
    }
}
