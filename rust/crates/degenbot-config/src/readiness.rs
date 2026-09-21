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
//! The two per-ecosystem backrun facets each carry their own endpoint
//! semantics: `mevblocker_backrun.endpoints` is the `MEVBlocker` searcher
//! WebSocket that carries the private bundle, and its bid mode additionally
//! requires the operator key file and the private `mevblocker_url`.
//! `peer_backrun.endpoints` is the public relay fan-out, validated as
//! http(s) relay URLs.
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

/// The `MEVBlocker` searcher WS the default mevblocker-backrun bundle
/// channel (and the shared pending-tx feed) resolves to. The feed crate
/// re-exports this constant so the default has one home.
pub const DEFAULT_BACKRUN_STREAM_URL: &str = "wss://searchers.mevblocker.io";

/// The default public relay fan-out for the peer-backrun composition: the
/// Flashbots relay, with the read provider as the fallback relay.
pub const DEFAULT_PEER_BACKRUN_RELAYS: &[&str] = &["https://rpc.flashbots.net?hint=hash"];

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

/// The readiness of the three strategy arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyReadiness {
    /// The settled-block arm (settlement).
    pub settlement: Arm,
    /// The MEVBlocker-ecosystem pending-transaction arm.
    pub mevblocker_backrun: Arm,
    /// The public-mempool pending-transaction arm.
    pub peer_backrun: Arm,
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
    /// An activated bid-mode facet is missing a required key (the operator
    /// key file or the private broadcast URL).
    MissingRequiredKey {
        /// The activated facet that lacks the key.
        facet: &'static str,
        /// The missing key's dotted TOML path.
        key: &'static str,
    },
    /// An explicit endpoint is refused (off the settlement allowlist, a
    /// malformed backrun channel URL, or a non-http peer relay).
    EndpointRefused {
        /// The facet the endpoint was declared on.
        facet: &'static str,
        /// The refused URL.
        url: String,
        /// Why: the allowlist gate, the channel shape, or the peer shape.
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
            Self::MissingRequiredKey { facet, key } => write!(
                f,
                "strategy \"{facet}\" is in bid mode but the required key {key} is unset: \
                 set it with `degenbot strategy set {facet} <key> <value>`"
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
    /// The URL is not a public http(s) relay.
    NotAnHttpRelay,
}

impl std::fmt::Display for EndpointRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OffSettlementAllowlist => f.write_str(
                "not on the pinned revert-protecting relay allowlist \
                 (MEV-Blocker /fast and /nochecks hold no revert protection)",
            ),
            Self::NotAWsChannel => f.write_str("expected exactly one ws:// or wss:// channel URL"),
            Self::NotAnHttpRelay => f.write_str("expected an http:// or https:// relay URL"),
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

/// Validate an explicit `MEVBlocker` backrun channel: a searcher WS URL.
fn backrun_refusal(url: &str) -> Option<EndpointRefusal> {
    let ws = url.starts_with("wss://") || url.starts_with("ws://");
    (!ws).then_some(EndpointRefusal::NotAWsChannel)
}

/// Validate an explicit peer relay: a public http(s) URL.
fn peer_relay_refusal(url: &str) -> Option<EndpointRefusal> {
    let http = url.starts_with("https://") || url.starts_with("http://");
    (!http).then_some(EndpointRefusal::NotAnHttpRelay)
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
    let mevblocker = &cfg.strategy.mevblocker_backrun;
    let peer = &cfg.strategy.peer_backrun;
    let mevblocker_arm = settle_arm(
        "mevblocker_backrun",
        mevblocker.active,
        mevblocker.endpoints.as_deref(),
        backrun_refusal,
    )?;
    // Bid mode needs signing material and the private endpoint: an
    // activated observe-only facet is allowed to lack both.
    if matches!(mevblocker_arm, Arm::Active(_)) && mevblocker.bid_mode {
        if mevblocker.key_file.is_none() {
            return Err(StrategyReadinessError::MissingRequiredKey {
                facet: "mevblocker_backrun",
                key: "strategy.mevblocker_backrun.key_file",
            });
        }
        if mevblocker.mevblocker_url.is_none() {
            return Err(StrategyReadinessError::MissingRequiredKey {
                facet: "mevblocker_backrun",
                key: "strategy.mevblocker_backrun.mevblocker_url",
            });
        }
    }
    Ok(StrategyReadiness {
        settlement: settle_arm(
            "settlement",
            settlement.active,
            settlement.endpoints.as_deref(),
            settlement_refusal,
        )?,
        mevblocker_backrun: mevblocker_arm,
        peer_backrun: settle_arm(
            "peer_backrun",
            peer.active,
            peer.endpoints.as_deref(),
            peer_relay_refusal,
        )?,
    })
}
