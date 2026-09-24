//! The strategy plane's shared selection surface.
//!
//! A strategy is a top-level label for one kind of profit opportunity. Three
//! concrete compositions exist — [`Settlement`], [`MevblockerBackrun`], and
//! [`TxpoolBackrun`] — and the surface they demonstrably share is their identity:
//! a facet name, a registration id (the same string), and selection through
//! [`StrategyName`]. That is the whole contract; a slot with one consumer stays
//! out of it.
//!
//! [`Settlement`]: crate::settlement::Settlement
//! [`MevblockerBackrun`]: crate::backrun::MevblockerBackrun
//! [`TxpoolBackrun`]: crate::backrun::TxpoolBackrun

use degenbot_config::BotConfig;

use crate::backrun::{MevblockerBackrun, TxpoolBackrun};
use crate::settlement::Settlement;

/// One strategy's identity on the plane: its facet name and registration id,
/// and the selection key a boot or console resolves an arm through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StrategyName {
    /// The settled-block settlement arm.
    Settlement,
    /// The `MEVBlocker`-ecosystem pending-transaction arm.
    MevblockerBackrun,
    /// The public-mempool pending-transaction arm.
    TxpoolBackrun,
}

impl StrategyName {
    /// Every strategy, in registration order.
    pub const ALL: [Self; 3] = [
        Self::Settlement,
        Self::MevblockerBackrun,
        Self::TxpoolBackrun,
    ];

    /// The canonical spelling: the facet name and the host registration id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Settlement => "settlement",
            Self::MevblockerBackrun => "mevblocker_backrun",
            Self::TxpoolBackrun => "txpool_backrun",
        }
    }

    /// The facet's dotted TOML section path.
    #[must_use]
    pub const fn config_section(self) -> &'static str {
        match self {
            Self::Settlement => "strategy.settlement",
            Self::MevblockerBackrun => "strategy.mevblocker_backrun",
            Self::TxpoolBackrun => "strategy.txpool_backrun",
        }
    }

    /// Parse a strategy name.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        Self::ALL.into_iter().find(|name| name.as_str() == raw)
    }

    /// Whether this strategy's facet is active in the loaded config.
    #[must_use]
    pub fn is_active(self, cfg: &BotConfig) -> bool {
        match self {
            Self::Settlement => cfg.strategy.settlement.active,
            Self::MevblockerBackrun => cfg.strategy.mevblocker_backrun.active,
            Self::TxpoolBackrun => cfg.strategy.txpool_backrun.active,
        }
    }

    /// The facet's declared endpoint list, raw.
    #[must_use]
    pub fn endpoints(self, cfg: &BotConfig) -> Option<&str> {
        match self {
            Self::Settlement => cfg.strategy.settlement.endpoints.as_deref(),
            Self::MevblockerBackrun => cfg.strategy.mevblocker_backrun.endpoints.as_deref(),
            Self::TxpoolBackrun => cfg.strategy.txpool_backrun.endpoints.as_deref(),
        }
    }

    /// Select the named composition from the loaded config: the one code path
    /// all three strategies share.
    ///
    /// `rpc_url` is the chain-node join a hosted lane resolves at its driving
    /// edge; the settlement composition does not carry it, because the engine
    /// pump supplies its own node.
    #[must_use]
    pub fn select(self, cfg: &BotConfig, rpc_url: String) -> SelectedStrategy {
        match self {
            Self::Settlement => SelectedStrategy::Settlement(Settlement::from_config(cfg)),
            Self::MevblockerBackrun => {
                SelectedStrategy::MevblockerBackrun(MevblockerBackrun::from_config(cfg, rpc_url))
            }
            Self::TxpoolBackrun => {
                SelectedStrategy::TxpoolBackrun(TxpoolBackrun::from_config(cfg, rpc_url))
            }
        }
    }
}

/// The plane contract, extracted by subtraction: every concrete strategy names
/// itself, and nothing else is shared.
pub trait Strategy {
    /// This strategy's plane name.
    const NAME: StrategyName;
}

/// The composition [`StrategyName::select`] builds.
#[derive(Debug, Clone)]
pub enum SelectedStrategy {
    /// The settlement composition.
    Settlement(Settlement),
    /// The `MEVBlocker`-ecosystem backrun composition.
    MevblockerBackrun(MevblockerBackrun),
    /// The public-mempool backrun composition.
    TxpoolBackrun(TxpoolBackrun),
}

impl SelectedStrategy {
    /// The plane name of the selected composition.
    #[must_use]
    pub const fn name(&self) -> StrategyName {
        match self {
            Self::Settlement(_) => StrategyName::Settlement,
            Self::MevblockerBackrun(_) => StrategyName::MevblockerBackrun,
            Self::TxpoolBackrun(_) => StrategyName::TxpoolBackrun,
        }
    }
}
