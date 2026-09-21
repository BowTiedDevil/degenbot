//! The strategy readiness gate: activation settles an endpoint posture or
//! refuses.
//!
//! An activated facet must carry a non-empty `endpoints` set — anything
//! less would let signed bytes reach the public mempool by omission (the
//! reverted-broadcast incident class). The `--endpoints-default` CLI
//! switch stamps the pinned allowlist into that set at activation time. Explicit settlement endpoints are restricted to the
//! pinned revert-protecting relay allowlist; the audit trail behind it
//! lives in `docs/autonomous-user-journey/RELAYS_AND_GUARDRAILS.md`.
//!
//! The module never reads env or files; pure config -> result.

use crate::schema::BotConfig;

/// The pinned revert-protecting relay allowlist (the settlement default
/// endpoint set). CLOSED on purpose: an endpoint joins only through the
/// audit doc, and the readiness gate rejects everything else.
pub const SETTLEMENT_DEFAULT_ENDPOINTS: &[&str] = &[
    "https://rpc.flashbots.net?hint=hash",
    "https://rpc.mevblocker.io/noreverts",
    "https://rpc.mevblocker.io/fullprivacy",
];

/// The `MEVBlocker` searcher WS the default backrun bundle channel resolves
/// to. The feed crate re-exports this constant so the default has one
/// home.
pub const DEFAULT_BACKRUN_STREAM_URL: &str = "wss://searchers.mevblocker.io";

/// One strategy arm's settled endpoint posture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arm {
    /// The facet is not activated; no requirement applies.
    Inactive,
    /// The facet is activated with its settled endpoint URLs (the persisted
    /// `endpoints` list — `--endpoints-default` stamps the pinned allowlist
    /// URLs in at activation time, so there is no deferred "default" mode
    /// in the resolved state).
    Active(Vec<String>),
}

/// The readiness of both strategy arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyReadiness {
    /// The settled-block arm (settlement).
    pub settlement: Arm,
    /// The pending-transaction arm (backrun).
    pub backrun: Arm,
}

/// A refused readiness. Every refusal names a remediation in its
/// `Display`, so a boot failure tells the operator exactly what to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrategyReadinessError {
    /// An activated facet carries an empty endpoint set (unset, blank, or
    /// all-blank CSV entries).
    UnsetEndpoints {
        /// The activated facet that lacks an endpoint set.
        facet: &'static str,
    },
    /// An explicit endpoint is refused (off the settlement allowlist, or a
    /// malformed backrun channel URL).
    EndpointRefused {
        /// The facet the endpoint was declared on.
        facet: &'static str,
        /// The refused URL.
        url: String,
        /// Why: the settlement allowlist gate or the backrun channel shape.
        reason: EndpointRefusal,
    },
}

impl std::fmt::Display for StrategyReadinessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsetEndpoints { facet } => write!(
                f,
                "strategy \"{facet}\" is active but its endpoint set is empty: set one with `degenbot strategy activate {facet} --endpoints <urls>` \
                 or adopt the documented default with \
                 `degenbot strategy activate {facet} --endpoints-default`"
            ),
            Self::EndpointRefused { facet, url, reason } => write!(
                f,
                "endpoint {url} for strategy \"{facet}\" refused: {reason}; the closed \
                 allowlist is pinned in docs/autonomous-user-journey/RELAYS_AND_GUARDRAILS.md"
            ),
        }
    }
}

impl std::error::Error for StrategyReadinessError {}

/// Why an endpoint was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointRefusal {
    /// The URL is not in the settlement allowlist.
    OffSettlementAllowlist,
    /// The URL is not a single searcher WebSocket channel.
    NotAWsChannel,
}

impl std::fmt::Display for EndpointRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OffSettlementAllowlist => f.write_str(
                "not on the pinned revert-protecting relay allowlist \
                 (MEV-Blocker /fast and /nochecks hold no revert protection)",
            ),
            Self::NotAWsChannel => f.write_str("expected exactly one ws:// or wss:// channel URL"),
        }
    }
}

/// Split a comma-separated endpoint list: trims, drops empties. A list
/// with nothing left counts as unset.
fn split_endpoints(raw: Option<&str>) -> Option<Vec<String>> {
    let urls: Vec<String> = raw?
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (!urls.is_empty()).then_some(urls)
}

/// Resolve one facet's endpoint arm.
fn settle_arm(
    facet: &'static str,
    active: bool,
    endpoints: Option<&str>,
    validate: impl Fn(&str) -> Option<EndpointRefusal>,
) -> Result<Arm, StrategyReadinessError> {
    if !active {
        return Ok(Arm::Inactive);
    }
    let Some(urls) = split_endpoints(endpoints) else {
        return Err(StrategyReadinessError::UnsetEndpoints { facet });
    };
    for url in &urls {
        if let Some(reason) = validate(url) {
            return Err(StrategyReadinessError::EndpointRefused {
                facet,
                url: url.clone(),
                reason,
            });
        }
    }
    Ok(Arm::Active(urls))
}

/// Validate an explicit settlement endpoint against the closed allowlist.
fn settlement_refusal(url: &str) -> Option<EndpointRefusal> {
    (!SETTLEMENT_DEFAULT_ENDPOINTS.contains(&url))
        .then_some(EndpointRefusal::OffSettlementAllowlist)
}

/// Validate an explicit backrun channel: a searcher WS URL.
fn backrun_refusal(url: &str) -> Option<EndpointRefusal> {
    let ws = url.starts_with("wss://") || url.starts_with("ws://");
    (!ws).then_some(EndpointRefusal::NotAWsChannel)
}

/// Resolve the strategy readiness of the loaded config. Pure; no env, no
/// file.
///
/// # Errors
///
/// A typed refusal naming the facet and the remediation (see
/// [`StrategyReadinessError`]).
pub fn strategy_readiness(cfg: &BotConfig) -> Result<StrategyReadiness, StrategyReadinessError> {
    let settlement = &cfg.strategy.settlement;
    let backrun = &cfg.strategy.backrun;
    Ok(StrategyReadiness {
        settlement: settle_arm(
            "settlement",
            settlement.active,
            settlement.endpoints.as_deref(),
            settlement_refusal,
        )?,
        backrun: settle_arm(
            "backrun",
            backrun.active,
            backrun.endpoints.as_deref(),
            backrun_refusal,
        )?,
    })
}
