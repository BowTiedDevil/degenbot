//! Typed `BotConfig` accessors for the Python driver shell (4IOEVT).
//!
//! The 12-factor loader is the ONLY environment reader; it installs the
//! process-wide typed config through `degenbot_config::holder::install` at
//! `_ffi` import. These thin `#[pyfunction]` getters expose a typed field to
//! Python without introducing a second (parallel) declaration site.

use crate::prelude::*;

/// The typed `pathfinding.discovery_batch_size` value (env
/// `DEGENBOT_DISCOVERY_BATCH_SIZE`), positive-clamped to `>= 1` so a zero /
/// garbage value degrades to the legacy per-path delivery instead of a busy
/// loop.
#[pyfunction]
#[must_use]
pub fn discovery_batch_size() -> usize {
    ::degenbot_config::holder::config()
        .pathfinding
        .discovery_batch_size
        .max(1)
}

/// The shared core verification-retry policy defaults as a SELF-DESCRIBING
/// value — a positional 4-tuple would silently mis-assign on a Rust-side
/// field reorder:
/// in seconds for the float fields.
///
/// The Python `VerificationRetryPolicy` dataclass reads these instead of
/// carrying its own literal set, so the Rust `RetryPolicy`  is
/// the one declaration site for both the driver shell and the pure-Rust
/// example.
#[pyclass(frozen, get_all, module = "degenbot._ffi")]
pub struct RetryPolicyDefaults {
    pub max_attempts: u32,
    pub base_delay: f64,
    pub max_delay: f64,
    pub jitter: f64,
}

/// The resolved strategy readiness, exposed as a self-describing Python
/// view: the settlement and two per-ecosystem backrun arms with their settled
/// endpoint posture.
///
/// Built from the process-wide typed config through the SAME
/// `strategy_readiness` authority the operators' `degenbot strategy` verbs
/// and the backrun driver boot use, so the Python driver shell cannot disagree
/// with the console about what "settled" means.
#[pyclass(frozen, module = "degenbot._ffi")]
pub struct StrategyReadinessView {
    #[pyo3(get)]
    pub settlement_active: bool,
    #[pyo3(get)]
    pub settlement_endpoints: Vec<String>,
    #[pyo3(get)]
    pub mevblocker_backrun_active: bool,
    #[pyo3(get)]
    pub mevblocker_backrun_endpoints: Vec<String>,
    #[pyo3(get)]
    pub txpool_backrun_active: bool,
    #[pyo3(get)]
    pub txpool_backrun_endpoints: Vec<String>,
}

impl StrategyReadinessView {
    /// Build from the resolved arms (activity + resolved URLs).
    fn from_readiness(readiness: &::degenbot_config::StrategyReadiness) -> Self {
        fn arm(arm: &::degenbot_config::Arm) -> (bool, Vec<String>) {
            match arm {
                ::degenbot_config::Arm::Inactive => (false, Vec::new()),
                ::degenbot_config::Arm::Active(urls) => (true, urls.clone()),
            }
        }
        let (settlement_active, settlement_endpoints) = arm(&readiness.settlement);
        let (mevblocker_backrun_active, mevblocker_backrun_endpoints) =
            arm(&readiness.mevblocker_backrun);
        let (txpool_backrun_active, txpool_backrun_endpoints) = arm(&readiness.txpool_backrun);
        Self {
            settlement_active,
            settlement_endpoints,
            mevblocker_backrun_active,
            mevblocker_backrun_endpoints,
            txpool_backrun_active,
            txpool_backrun_endpoints,
        }
    }
}

/// Resolve the strategy readiness of the installed typed config.
///
/// # Errors
///
/// `ValueError` carrying the typed refusal's remediation message (e.g. the
/// activation/endpoint remedies from the console verbs) — a live boot that
/// cannot settle STRATEGY endpoints refuses instead of degrading to the
/// public mempool.
#[pyfunction]
pub fn validate_strategy_readiness() -> PyResult<StrategyReadinessView> {
    ::degenbot_config::strategy_readiness(::degenbot_config::holder::config())
        .map(|readiness| StrategyReadinessView::from_readiness(&readiness))
        .map_err(|error| ::pyo3::exceptions::PyValueError::new_err(error.to_string()))
}

/// The resolved settlement broadcast endpoints (this process's settlement
/// arm). Raises `ValueError` when the settlement facet is not active — a
/// hosted runner is the settlement arm, so its broadcast posture is never
/// optional.
///
/// # Errors
///
/// `ValueError` when the facet is inactive.
#[pyfunction]
pub fn settlement_broadcast_endpoints() -> PyResult<Vec<String>> {
    let config = ::degenbot_config::holder::config();
    let readiness = ::degenbot_config::strategy_readiness(config)
        .map_err(|error| ::pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    if matches!(readiness.settlement, ::degenbot_config::Arm::Inactive) {
        return Err(::pyo3::exceptions::PyValueError::new_err(
            "strategy settlement is not active: this hosted runner IS the settlement arm; \
             activate it first (degenbot strategy activate settlement --endpoints-default)",
        ));
    }
    // The endpoints come from the settlement strategy composition, so the
    // broadcast posture and the strategy plane read one config surface.
    Ok(::degenbot_strategy::Settlement::from_config(config)
        .into_config()
        .endpoints)
}
/// Read the shared core verification-retry policy defaults.
#[pyfunction]
#[must_use]
pub fn verification_retry_policy_defaults() -> RetryPolicyDefaults {
    let policy = ::degenbot_core::retry::RetryPolicy::verification_default();
    RetryPolicyDefaults {
        max_attempts: policy.max_attempts,
        base_delay: policy.base_delay,
        max_delay: policy.max_delay,
        jitter: policy.jitter,
    }
}

// =============================================================================
// Driver-domain resolution (ADR-062 D7/D10): thin readers of the layers the
// boot load published, so the Python driver resolves a value through the same
// cascade as the console instead of owning a second config model.
// =============================================================================

/// The layers behind the installed typed config: the standard operator file
/// the loader selected over the process environment, plus the per-key and
/// per-entry provenance the driver-domain resolvers tag a winner with.
///
/// Published at module init from the SAME load the typed-config holder
/// received, so the typed getters above and the resolvers below cannot
/// disagree about what this process configured.
static LOADED: ::std::sync::OnceLock<::degenbot_config::LoadedConfig> =
    ::std::sync::OnceLock::new();

/// The layers [`publish_loaded`] recorded.
///
/// Before any owner published (a construction that never booted a driver), the
/// floor stands: the installed typed config with no provenance recorded, so
/// every value reports the file layer — the bottom of the cascade — rather
/// than inventing a winner.
pub(crate) fn loaded() -> &'static ::degenbot_config::LoadedConfig {
    LOADED.get_or_init(floor_loaded)
}

/// The installed typed config carrying no provenance: the shape a config
/// installed without a layered load has (the typed accessors above read the
/// holder directly, so the values agree).
fn floor_loaded() -> ::degenbot_config::LoadedConfig {
    ::degenbot_config::LoadedConfig {
        config: ::degenbot_config::holder::config().clone(),
        provenance: ::std::collections::BTreeMap::new(),
        entry_provenance: ::std::collections::BTreeMap::new(),
    }
}

/// Publish the layers a boot load produced, next to the holder install it fed.
///
/// `installed` is the holder's first-wins verdict: when another owner
/// installed a config before this module loaded one, the resolvers must read
/// THAT config, so the published layers carry it — with no provenance, which
/// the resolvers already read as the floor layer.
pub(crate) fn publish_loaded(loaded: ::degenbot_config::LoadedConfig, installed: bool) {
    let _ = LOADED.set(if installed { loaded } else { floor_loaded() });
}

/// A node endpoint with the layer that supplied it, as the Python driver reads
/// it: two named fields, so a driver cannot mis-assign the pair the way a
/// positional tuple would.
#[pyclass(frozen, get_all, module = "degenbot._ffi")]
pub struct ResolvedNodeUri {
    /// The effective endpoint.
    pub uri: String,

    /// Which layer supplied it: `default`, `file`, `env`, or `cli`.
    pub source: String,
}

/// A session chain id with the layer that supplied it.
#[pyclass(frozen, get_all, module = "degenbot._ffi")]
pub struct ResolvedChainId {
    /// The effective chain id.
    pub chain_id: u64,

    /// Which layer supplied it: `default`, `file`, `env`, or `cli`.
    pub source: String,
}

/// A database path with the layer that supplied it.
#[pyclass(frozen, get_all, module = "degenbot._ffi")]
pub struct ResolvedDatabasePath {
    /// The effective path, with `~` and the state home already expanded.
    pub path: String,

    /// Which layer supplied it: `default`, `file`, `env`, or `cli`.
    pub source: String,
}

/// A config-layer refusal as `ValueError`: the loaders and resolvers fail
/// closed, and a Python driver sees the same refusal a console command prints.
fn refusal(error: &::degenbot_config::ConfigError) -> ::pyo3::PyErr {
    ::pyo3::exceptions::PyValueError::new_err(error.to_string())
}

/// The node endpoint a consumer with `scope` capability resolves for
/// `chain_id`, through the layers `degenbot-config` owns.
///
/// `node` is the explicit-override layer: one endpoint, classified by its own
/// value the way the console's `--node` is, so a driver passes a URL and the
/// transport is not a second thing to get right.
///
/// # Errors
///
/// `ValueError` when `scope` is not a capability, when `node` names no
/// transport, or when no layer supplied an endpoint for the chain (the
/// refusal names the scope, the transports, and the layers each was read
/// through).
#[pyfunction]
#[pyo3(signature = (chain_id, scope, node=None))]
pub fn resolve_node_uri(
    chain_id: u64,
    scope: &str,
    node: Option<&str>,
) -> PyResult<ResolvedNodeUri> {
    node_uri_in(loaded(), chain_id, scope, node)
}

/// [`resolve_node_uri`] over an explicit set of layers (the shape a caller
/// outside the process boot, or a test, resolves against).
fn node_uri_in(
    cfg: &::degenbot_config::LoadedConfig,
    chain_id: u64,
    scope: &str,
    node: Option<&str>,
) -> PyResult<ResolvedNodeUri> {
    let scope = scope
        .parse::<::degenbot_config::NodeScope>()
        .map_err(|error| refusal(&error))?;
    let overrides = match node.filter(|value| !value.is_empty()) {
        Some(uri) => {
            let transport = ::degenbot_config::NodeTransport::classify(uri).ok_or_else(|| {
                let forms: Vec<String> = ::degenbot_config::NodeTransport::ALL
                    .iter()
                    .map(|transport| format!("{} — {}", transport.key_path(), transport.expected()))
                    .collect();
                ::pyo3::exceptions::PyValueError::new_err(format!(
                    "{uri:?} names no node transport; accepted forms: {}",
                    forms.join("; ")
                ))
            })?;
            ::degenbot_config::NodeOverrides::new().with_transport(transport, uri)
        }
        None => ::degenbot_config::NodeOverrides::new(),
    };
    let resolved = ::degenbot_config::resolve_node_uri(cfg, chain_id, scope, &overrides)
        .map_err(|error| refusal(&error))?;
    Ok(ResolvedNodeUri {
        uri: resolved.value,
        source: resolved.source.to_string(),
    })
}

/// The session chain id: the explicit override when given, else the layers
/// `degenbot-config` owns.
///
/// # Errors
///
/// `ValueError` when no layer named a chain, or when the explicit value is not
/// an integer.
#[pyfunction]
#[pyo3(signature = (chain_id=None))]
pub fn resolve_chain_id(chain_id: Option<&str>) -> PyResult<ResolvedChainId> {
    chain_id_in(loaded(), chain_id)
}

/// [`resolve_chain_id`] over an explicit set of layers.
fn chain_id_in(
    cfg: &::degenbot_config::LoadedConfig,
    chain_id: Option<&str>,
) -> PyResult<ResolvedChainId> {
    let resolved =
        ::degenbot_config::resolve_chain_id(cfg, chain_id).map_err(|error| refusal(&error))?;
    Ok(ResolvedChainId {
        chain_id: resolved.value,
        source: resolved.source.to_string(),
    })
}

/// The database path through the full cascade, with `~` and the state home
/// expanded by the resolver.
///
/// # Errors
///
/// Never today, but the resolver is fallible by contract, so the refusal path
/// exists rather than being invented when a layer grows one.
#[pyfunction]
#[pyo3(signature = (database=None))]
pub fn resolve_database_path(database: Option<&str>) -> PyResult<ResolvedDatabasePath> {
    Ok(database_path_in(loaded(), database))
}

/// [`resolve_database_path`] over an explicit set of layers.
fn database_path_in(
    cfg: &::degenbot_config::LoadedConfig,
    database: Option<&str>,
) -> ResolvedDatabasePath {
    let resolved = ::degenbot_config::resolve_database_path(cfg, database);
    ResolvedDatabasePath {
        path: resolved.value.to_string_lossy().into_owned(),
        source: resolved.source.to_string(),
    }
}

/// The operator file the loader selected: the `DEGENBOT_CONFIG` override when
/// set, else the XDG/HOME config file when it exists, else `None` (an absent
/// user file is contractually defaults).
///
/// A raw-table reader (the deployment registry, the failure-policy table)
/// resolves the SAME file the typed load read instead of re-deriving the
/// discovery rule.
#[pyfunction]
#[must_use]
pub fn config_file_path() -> Option<String> {
    ::degenbot_config::standard_file_path().map(|path| path.to_string_lossy().into_owned())
}

/// The DECLARED `database.path` key of the installed typed config — the value
/// the operator wrote, with no cascade and no `~` expansion.
///
/// A caller that wants the path a session will actually open asks
/// [`resolve_database_path`]; this is the declared key behind it, for the
/// readers that need to know what the config said rather than what won.
#[pyfunction]
#[must_use]
pub fn declared_database_path() -> String {
    declared_database_path_of(::degenbot_config::holder::config())
}

/// [`declared_database_path`] over an explicit typed config.
fn declared_database_path_of(cfg: &::degenbot_config::BotConfig) -> String {
    cfg.database.path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, reason = "test assertions fail loudly")]

    use std::collections::BTreeMap;

    use ::degenbot_config::{BotConfigLoader, MapEnv, NodeScope, Source};

    use super::{chain_id_in, database_path_in, declared_database_path_of, node_uri_in};

    /// The layers a `MapEnv` supplies, with no file layer and no process
    /// environment, so every case names its own winning layer.
    fn loaded(pairs: &[(&str, &str)]) -> ::degenbot_config::LoadedConfig {
        BotConfigLoader::new()
            .with_env(Box::new(MapEnv::new(
                pairs
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                    .collect::<BTreeMap<_, _>>(),
            )))
            .load()
            .expect("a map env carries no unreadable file layer")
    }

    /// The winning layer travels with the value: a driver that reports where an
    /// endpoint came from would otherwise have to re-derive the cascade.
    #[test]
    fn node_uri_reports_the_layer_that_won() {
        let cfg = loaded(&[("DEGENBOT_RPC_WS_CHAINID_1", "wss://node.example/ws")]);
        let resolved = node_uri_in(&cfg, 1, NodeScope::Request.as_str(), None)
            .expect("the env layer names a request endpoint");
        assert_eq!(resolved.uri, "wss://node.example/ws");
        assert_eq!(resolved.source, Source::Env.to_string());
    }

    /// An explicit endpoint is classified by its own value and outranks every
    /// layer, so a driver that passes `node=` cannot be overruled by an
    /// exported endpoint for another transport.
    #[test]
    fn explicit_node_override_outranks_the_exported_endpoint() {
        let cfg = loaded(&[("DEGENBOT_RPC_WS_CHAINID_1", "wss://node.example/ws")]);
        let resolved = node_uri_in(
            &cfg,
            1,
            NodeScope::Request.as_str(),
            Some("ipc:///tmp/geth.ipc"),
        )
        .expect("an ipc endpoint serves a request");
        assert_eq!(resolved.uri, "ipc:///tmp/geth.ipc");
        assert_eq!(resolved.source, Source::Cli.to_string());
    }

    /// A value that names no transport is refused with the accepted forms, not
    /// guessed into a slot.
    #[test]
    fn node_override_naming_no_transport_is_refused() {
        let cfg = loaded(&[("DEGENBOT_RPC_WS_CHAINID_1", "wss://node.example/ws")]);
        let error = node_uri_in(&cfg, 1, NodeScope::Request.as_str(), Some("node.example"))
            .err()
            .expect("a bare host is not an endpoint");
        let message = error.to_string();
        for transport in ::degenbot_config::NodeTransport::ALL {
            assert!(
                message.contains(transport.key_path()),
                "the refusal must name {}, got: {message}",
                transport.key_path()
            );
        }
    }

    /// The scope is the caller's capability, so a scope the driver cannot
    /// resolve refuses here rather than degrading to another transport.
    #[test]
    fn unknown_scope_is_refused() {
        let cfg = loaded(&[("DEGENBOT_RPC_WS_CHAINID_1", "wss://node.example/ws")]);
        assert!(node_uri_in(&cfg, 1, "polling", None).is_err());
    }

    /// The chain id resolves through the same layers, and an unparsable
    /// explicit value is a refusal that names the layer that supplied it.
    #[test]
    fn chain_id_resolves_and_refuses_with_the_layer_named() {
        let cfg = loaded(&[("DEGENBOT_DEFAULT_CHAIN_ID", "8453")]);
        let resolved = chain_id_in(&cfg, None).expect("the env layer names a chain");
        assert_eq!(resolved.chain_id, 8453);
        assert_eq!(resolved.source, Source::Env.to_string());

        let error = chain_id_in(&cfg, Some("mainnet"))
            .err()
            .expect("a name is not a chain id");
        assert!(
            error.to_string().contains("--chain-id"),
            "the refusal must name the layer that refused, got: {error}"
        );
    }

    /// The declared key and the resolved path are different questions: the
    /// first reads the installed typed config, the second runs the cascade.
    #[test]
    fn declared_database_path_is_the_installed_key_not_the_cascade() {
        let cfg = loaded(&[("DEGENBOT_DB_PATH", "/var/lib/degenbot.db")]);
        assert_eq!(
            declared_database_path_of(&cfg.config),
            "/var/lib/degenbot.db"
        );
        assert_eq!(database_path_in(&cfg, None).path, "/var/lib/degenbot.db");
        assert_eq!(
            database_path_in(&cfg, Some("/tmp/session.db")).path,
            "/tmp/session.db"
        );
    }
}
