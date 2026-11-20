//! The settlement strategy composition: sealed-block atomic arbitrage.
//!
//! Settlement is the settled-block reaction kind. Its block clock is the arb
//! engine's block pump, so this type is the strategy plane's *config* view of
//! the arm — the values the engine pump resolves before it drives a block — not
//! a driver of its own. The arm's hosted driver is that pump session's
//! lifetime (`degenbot_bot::arb_engine::settlement_driver`), which the session
//! boot attaches as this facet's spawn factory; the loop it boots adds no pump
//! around the one the engine already runs.
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
