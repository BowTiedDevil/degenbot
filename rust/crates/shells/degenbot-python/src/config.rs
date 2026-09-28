//! The resolved configuration verdict the Python driver reads.
//!
//! `degenbot-config` owns the operator file, the environment, and the
//! resolution order end to end; the loader is the only environment reader and
//! the FFI module init installs the process-wide typed config it produced.
//! What crosses the seam is ONE object — [`ResolvedConfig`] — built from that
//! one load and installed once at module init.
//!
//! The verdict answers three kinds of question, and the difference matters:
//! a `#[getter]` for a value that is settled when the load lands (the file,
//! the declared keys, the layer each key came from), a schema-driven
//! [`ResolvedConfig::values`] projection for every declared key, and a
//! `#[pymethods]` entry for a resolution that takes an argument or can refuse
//! (which chain's endpoint, the activation gate). A getter that could refuse
//! would make the verdict unconstructible in exactly the processes that need
//! it most, and an argument-taking resolution is not a value the load settled.
//!
//! The projection is what keeps the seam from growing with the schema: it
//! walks `degenbot_config::SCHEMA` through the generated `BotConfig::value`
//! reader, so declaring a key is the only edit a new key needs anywhere in
//! the workspace.

use crate::prelude::*;

/// The shared core verification-retry policy defaults as a SELF-DESCRIBING
/// value — a positional 4-tuple would silently mis-assign on a Rust-side
/// field reorder.
///
/// The Python `VerificationRetryPolicy` dataclass reads these instead of
/// carrying its own literal set, so the Rust `RetryPolicy` is the one
/// declaration site for both the driver shell and the pure-Rust example.
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
/// Built from the same typed config the verdict holds, through the SAME
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

/// Build the retry-policy defaults the verdict carries.
fn verification_retry_defaults() -> RetryPolicyDefaults {
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

/// The one load's layers plus the file it read.
struct Verdict {
    layers: &'static ::degenbot_config::LoadedConfig,
    file: Option<String>,
}

static VERDICT: ::std::sync::OnceLock<Verdict> = ::std::sync::OnceLock::new();

/// The installed verdict, built from the layers [`publish_loaded`] recorded.
///
/// Both `OnceLock`s resolve to the same load whichever is read first, so the
/// verdict is a value rather than a race: the eager install only makes the
/// module-init ordering a property instead of a coincidence.
fn verdict() -> &'static Verdict {
    VERDICT.get_or_init(|| Verdict {
        layers: loaded(),
        file: ::degenbot_config::standard_file_path()
            .map(|path| path.to_string_lossy().into_owned()),
    })
}

/// Build and install the verdict. Module init calls this right after
/// publishing the layers, so the verdict cannot be built from a load the
/// holder never received.
pub(crate) fn install_verdict() {
    let _ = verdict();
}

/// Every declared key's value, keyed by its dotted TOML path.
///
/// Walked out of `degenbot_config::SCHEMA` through the generated
/// `BotConfig::value` reader, so this is the schema and nothing beside it: a
/// key declared in `config_schema!` appears here with no FFI edit, which is
/// the property that keeps the seam from growing one accessor per key.
fn values_by_path(
    cfg: &::degenbot_config::BotConfig,
) -> ::std::collections::BTreeMap<String, Option<::degenbot_config::ConfigValue<'_>>> {
    ::degenbot_config::SCHEMA
        .iter()
        .map(|key| (key.toml_path.to_string(), cfg.value(key.section, key.field)))
        .collect()
}

/// The winning layer per DECLARED key, keyed by its dotted TOML path.
///
/// The projection is schema-driven so the vocabulary a caller reads is the
/// operator's rather than the loader's internal env names. A key no layer
/// supplied is ABSENT, and a layer recorded against a name no key declares has
/// no dotted path to appear under. Both absences are the answer, because
/// filling them in with the floor layer is how an `env` winner gets reported
/// as a `file` — the divergence a cross-language oracle exists to catch.
fn provenance_by_path(
    layers: &::degenbot_config::LoadedConfig,
) -> ::std::collections::BTreeMap<String, String> {
    ::degenbot_config::SCHEMA
        .iter()
        .filter_map(|key| {
            layers
                .source_of(key.env)
                .map(|source| (key.toml_path.to_string(), source.to_string()))
        })
        .collect()
}

/// Per-ENTRY provenance for the table-shaped keys, keyed by the key's env name
/// (the family prefix) and then by the operator-chosen entry — a per-chain
/// endpoint table is overridden one entry at a time, so the key-level map
/// cannot describe it.
fn entry_provenance_by_env(
    layers: &::degenbot_config::LoadedConfig,
) -> ::std::collections::BTreeMap<String, ::std::collections::BTreeMap<String, String>> {
    layers
        .entry_provenance
        .iter()
        .map(|(env, entries)| {
            (
                (*env).to_string(),
                entries
                    .iter()
                    .map(|(entry, source)| (entry.clone(), source.to_string()))
                    .collect(),
            )
        })
        .collect()
}

/// One declared value as the Python object its kind names. An unset optional
/// key is `None`, never its declared default: "the operator said nothing" and
/// "the operator chose this" are different facts and a caller must be able to
/// tell them apart.
fn value_into_py<'py>(
    py: ::pyo3::Python<'py>,
    value: Option<&::degenbot_config::ConfigValue<'_>>,
) -> ::pyo3::PyResult<::pyo3::prelude::Bound<'py, ::pyo3::PyAny>> {
    use ::degenbot_config::ConfigValue as V;
    Ok(match value {
        None => py.None().into_bound(py),
        Some(V::Bool(inner)) => (*inner).into_pyobject(py)?.to_owned().into_any(),
        Some(V::Text(inner) | V::Enum(inner)) => inner.as_ref().into_pyobject(py)?.into_any(),
        Some(V::Path(inner)) => inner
            .to_string_lossy()
            .into_owned()
            .into_pyobject(py)?
            .into_any(),
        Some(V::Uint(inner)) => (*inner).into_pyobject(py)?.into_any(),
        Some(V::Int(inner)) => (*inner).into_pyobject(py)?.into_any(),
        Some(V::Float(inner)) => (*inner).into_pyobject(py)?.into_any(),
        Some(V::Map(entries)) => {
            let table = ::pyo3::types::PyDict::new(py);
            for (key, entry) in entries {
                table.set_item(key, entry)?;
            }
            table.into_any()
        }
    })
}

/// The whole resolved configuration for this process: the values, the layer
/// each came from, and the resolutions that need a capability or an override.
///
/// Frozen, so a Python driver cannot edit a verdict the console also read, and
/// constructed only from the load published at module init — one process, one
/// cascade, one answer.
#[pyclass(frozen, module = "degenbot._ffi")]
pub struct ResolvedConfig {
    verdict: &'static Verdict,
}

impl ResolvedConfig {
    /// The installed verdict.
    #[must_use]
    pub fn installed() -> Self {
        Self { verdict: verdict() }
    }
}

#[pymethods]
impl ResolvedConfig {
    /// The operator file the loader selected: the `DEGENBOT_CONFIG` override
    /// when set, else the XDG/HOME config file when it exists, else `None` (an
    /// absent user file is contractually defaults).
    ///
    /// A raw-table reader (the deployment registry, the failure-policy table)
    /// resolves the SAME file the typed load read instead of re-deriving the
    /// discovery rule.
    #[getter]
    fn config_file_path(&self) -> Option<String> {
        self.verdict.file.clone()
    }

    /// The database path through the full cascade, with `~` and the state home
    /// expanded by the resolver.
    #[getter]
    fn database_path(&self) -> ResolvedDatabasePath {
        database_path_in(self.verdict.layers, None)
    }

    /// The DECLARED `database.path` key — the value the operator wrote, with no
    /// cascade and no `~` expansion.
    ///
    /// A caller that wants the path a session will actually open reads
    /// [`Self::database_path`]; this is the declared key behind it, for the
    /// readers that need to know what the config said rather than what won.
    #[getter]
    fn declared_database_path(&self) -> String {
        declared_database_path_of(&self.verdict.layers.config)
    }

    /// The database path through the full cascade, with an explicit override
    /// in the same slot a `--database` flag occupies.
    #[pyo3(signature = (database=None))]
    fn resolve_database_path(&self, database: Option<&str>) -> ResolvedDatabasePath {
        database_path_in(self.verdict.layers, database)
    }

    /// The typed `pathfinding.discovery_batch_size` value (env
    /// `DEGENBOT_DISCOVERY_BATCH_SIZE`), positive-clamped to `>= 1` so a zero /
    /// garbage value degrades to the legacy per-path delivery instead of a busy
    /// loop.
    #[getter]
    fn discovery_batch_size(&self) -> usize {
        self.verdict
            .layers
            .config
            .pathfinding
            .discovery_batch_size
            .max(1)
    }

    /// Every declared key's typed value, keyed by its dotted TOML path.
    ///
    /// The seam's whole schema surface in one object: a key added to
    /// `config_schema!` appears here with no change to the FFI interface.
    #[getter]
    fn values<'py>(
        &self,
        py: ::pyo3::Python<'py>,
    ) -> ::pyo3::PyResult<
        ::std::collections::BTreeMap<String, ::pyo3::prelude::Bound<'py, ::pyo3::PyAny>>,
    > {
        values_by_path(&self.verdict.layers.config)
            .iter()
            .map(|(path, value)| Ok((path.clone(), value_into_py(py, value.as_ref())?)))
            .collect()
    }

    /// The layer that supplied each declared key, keyed by its dotted TOML path
    /// (`default`, `file`, `env`, or `cli`).
    ///
    /// A key no layer supplied is absent rather than reported as the floor, so
    /// an unrecorded provenance map is visible as an unrecorded one.
    #[getter]
    fn provenance(&self) -> ::std::collections::BTreeMap<String, String> {
        provenance_by_path(self.verdict.layers)
    }

    /// Per-entry layers for the table-shaped keys, keyed by the key's env name
    /// and then by the operator-chosen entry (a chain id for `[nodes.*]`).
    #[getter]
    fn entry_provenance(
        &self,
    ) -> ::std::collections::BTreeMap<String, ::std::collections::BTreeMap<String, String>> {
        entry_provenance_by_env(self.verdict.layers)
    }

    /// The node endpoint a consumer with `scope` capability resolves for
    /// `chain_id`, through the layers `degenbot-config` owns.
    ///
    /// `node` is the explicit-override layer: one endpoint, classified by its
    /// own value the way the console's `--node` is, so a driver passes a URL
    /// and the transport is not a second thing to get right. A method rather
    /// than a getter because the chain and the capability are the CALLER's,
    /// not facts this load settled.
    ///
    /// # Errors
    ///
    /// `ValueError` when `scope` is not a capability, when `node` names no
    /// transport, or when no layer supplied an endpoint for the chain (the
    /// refusal names the scope, the transports, and the layers each was read
    /// through).
    #[pyo3(signature = (chain_id, scope, node=None))]
    fn node_uri(
        &self,
        chain_id: u64,
        scope: &str,
        node: Option<&str>,
    ) -> PyResult<ResolvedNodeUri> {
        node_uri_in(self.verdict.layers, chain_id, scope, node)
    }

    /// The session chain id: the explicit override when given, else the layers
    /// `degenbot-config` owns.
    ///
    /// # Errors
    ///
    /// `ValueError` when no layer named a chain, or when the explicit value is
    /// not an integer.
    #[pyo3(signature = (chain_id=None))]
    fn resolve_chain_id(&self, chain_id: Option<&str>) -> PyResult<ResolvedChainId> {
        chain_id_in(self.verdict.layers, chain_id)
    }

    /// The resolved strategy readiness of this process's config.
    ///
    /// # Errors
    ///
    /// `ValueError` carrying the typed refusal's remediation message (e.g. the
    /// activation/endpoint remedies from the console verbs) — a live boot that
    /// cannot settle STRATEGY endpoints, or that has no active facet at all,
    /// refuses instead of degrading to the public mempool.
    fn strategy_readiness(&self) -> PyResult<StrategyReadinessView> {
        ::degenbot_config::validate_hosted_strategy_readiness(&self.verdict.layers.config)
            .map(|readiness| StrategyReadinessView::from_readiness(&readiness))
            .map_err(|error| ::pyo3::exceptions::PyValueError::new_err(error.to_string()))
    }

    /// The resolved settlement broadcast endpoints (this process's settlement
    /// arm).
    ///
    /// The endpoints are the settlement arm's settled list from
    /// `degenbot-config`'s readiness resolution — the one splitter and
    /// allowlist gate. The settlement strategy composition keeps its role for
    /// the strategy plane; this driver path does not re-derive the list
    /// through it.
    ///
    /// # Errors
    ///
    /// `ValueError` when the settlement facet is not active, or when its
    /// endpoint set is unsettled or refused by the readiness gate — a hosted
    /// runner is the settlement arm, so its broadcast posture is never
    /// optional.
    fn settlement_broadcast_endpoints(&self) -> PyResult<Vec<String>> {
        settlement_broadcast_endpoints_in(&self.verdict.layers.config)
    }
}

/// [`ResolvedConfig::settlement_broadcast_endpoints`] over an explicit typed
/// config: the settlement readiness arm's settled URLs, which the one readiness
/// resolution already split and allowlist-validated.
fn settlement_broadcast_endpoints_in(
    config: &::degenbot_config::BotConfig,
) -> PyResult<Vec<String>> {
    let readiness = ::degenbot_config::strategy_readiness(config)
        .map_err(|error| ::pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    match readiness.settlement {
        ::degenbot_config::Arm::Active(urls) => Ok(urls),
        ::degenbot_config::Arm::Inactive => Err(::pyo3::exceptions::PyValueError::new_err(
            "strategy settlement is not active: this hosted runner IS the settlement arm; \
             activate it first (degenbot strategy activate settlement --endpoints-default)",
        )),
    }
}

/// The installed verdict, as the Python driver reads it.
///
/// # Errors
///
/// `PyErr` when the interpreter refuses the allocation. The verdict itself
/// cannot fail: a process that named no chain or refused an endpoint still has
/// a configuration to report.
#[pyfunction]
pub fn resolved_config(py: ::pyo3::Python<'_>) -> PyResult<::pyo3::Py<ResolvedConfig>> {
    ::pyo3::Py::new(py, ResolvedConfig::installed())
}

// =============================================================================
// Hypothetical resolution (ADR-013 private seam): a pure function of a
// captured environment and an operator file. It installs NOTHING and is
// reachable only from the raw FFI, never from `degenbot.config`, so a caller
// that asks HOW the cascade resolves an input cannot be reading the answer
// this process happened to install.
// =============================================================================

/// The layered load a hypothetical resolution reads: the captured environment
/// (never the process environment) over the named file (or the standard file
/// the CAPTURED env selects when `file` is `None`).
///
/// A pure function of its inputs: it reads no `OnceLock`, publishes nothing,
/// and a refusal is the loader's own typed [`::degenbot_config::ConfigError`]
/// rather than a process exit.
fn hypothetical_layers(
    env: ::std::collections::BTreeMap<String, String>,
    file: Option<&str>,
) -> Result<::degenbot_config::LoadedConfig, ::degenbot_config::ConfigError> {
    let captured = ::degenbot_config::MapEnv::new(env);
    let selected = match file.filter(|path| !path.is_empty()) {
        Some(path) => Some(::std::path::PathBuf::from(path)),
        None => ::degenbot_config::standard_file_path_with(&captured),
    };
    let mut loader = ::degenbot_config::BotConfigLoader::new().with_env(Box::new(captured));
    if let Some(path) = selected {
        loader = loader.with_config_path(path);
    }
    loader.load()
}

/// The verdict a hypothetical load produces, as the Python driver reads it:
/// the same three schema-driven projections [`ResolvedConfig`] exposes, with
/// no install and no `&'static` borrow.
///
/// A `ResolvedConfig` CANNOT be built here — it holds `&'static Verdict` from
/// the process-wide `OnceLock` — so a hypothetical is its own value type. The
/// name carries the warning: this object answers what a cascade WOULD resolve
/// for the inputs given, never what this process installed.
#[pyclass(frozen, module = "degenbot._ffi")]
pub struct HypotheticalConfig {
    layers: ::degenbot_config::LoadedConfig,
}

#[pymethods]
impl HypotheticalConfig {
    /// Every declared key's typed value, keyed by its dotted TOML path.
    #[getter]
    fn values<'py>(
        &self,
        py: ::pyo3::Python<'py>,
    ) -> ::pyo3::PyResult<
        ::std::collections::BTreeMap<String, ::pyo3::prelude::Bound<'py, ::pyo3::PyAny>>,
    > {
        values_by_path(&self.layers.config)
            .iter()
            .map(|(path, value)| Ok((path.clone(), value_into_py(py, value.as_ref())?)))
            .collect()
    }

    /// The layer that supplied each declared key, keyed by its dotted TOML path.
    #[getter]
    fn provenance(&self) -> ::std::collections::BTreeMap<String, String> {
        provenance_by_path(&self.layers)
    }

    /// Per-entry layers for the table-shaped keys.
    #[getter]
    fn entry_provenance(
        &self,
    ) -> ::std::collections::BTreeMap<String, ::std::collections::BTreeMap<String, String>> {
        entry_provenance_by_env(&self.layers)
    }
}

/// The whole resolved configuration a cascade WOULD produce for `env` + `file`,
/// installing nothing and reachable only through the raw FFI (ADR-013).
///
/// The comparison door for claims about HOW the cascade resolves inputs; the
/// installed [`ResolvedConfig`] is the door for claims about WHAT this process
/// installed. Using the wrong one is a tautology, so the name says
/// "hypothetical" and the Python home (`degenbot.config`) deliberately does not
/// re-export it.
///
/// # Errors
///
/// `ValueError` carrying the loader's aggregated refusal — the typed error the
/// module-init boot path turns into an exit(2) wrapper.
#[pyfunction]
#[pyo3(signature = (env, file=None))]
pub fn resolve_hypothetical(
    py: ::pyo3::Python<'_>,
    env: ::std::collections::BTreeMap<String, String>,
    file: Option<&str>,
) -> PyResult<::pyo3::Py<HypotheticalConfig>> {
    let layers = hypothetical_layers(env, file).map_err(|error| refusal(&error))?;
    ::pyo3::Py::new(py, HypotheticalConfig { layers })
}

/// [`ResolvedConfig::node_uri`] over a hypothetical load: the standalone
/// sibling for the argument-taking cascade method (envy's `from_env` /
/// `from_iter` split). Installs nothing.
///
/// # Errors
///
/// `ValueError` on a load refusal or the resolution's own refusal.
#[pyfunction]
#[pyo3(signature = (env, file, chain_id, scope, node=None))]
pub fn resolve_hypothetical_node_uri(
    env: ::std::collections::BTreeMap<String, String>,
    file: Option<&str>,
    chain_id: u64,
    scope: &str,
    node: Option<&str>,
) -> PyResult<ResolvedNodeUri> {
    let layers = hypothetical_layers(env, file).map_err(|error| refusal(&error))?;
    node_uri_in(&layers, chain_id, scope, node)
}

/// [`ResolvedConfig::resolve_chain_id`] over a hypothetical load. Installs
/// nothing.
///
/// # Errors
///
/// `ValueError` on a load refusal or the resolution's own refusal.
#[pyfunction]
#[pyo3(signature = (env, file, chain_id=None))]
pub fn resolve_hypothetical_chain_id(
    env: ::std::collections::BTreeMap<String, String>,
    file: Option<&str>,
    chain_id: Option<&str>,
) -> PyResult<ResolvedChainId> {
    let layers = hypothetical_layers(env, file).map_err(|error| refusal(&error))?;
    chain_id_in(&layers, chain_id)
}

/// [`ResolvedConfig::resolve_database_path`] over a hypothetical load.
/// Installs nothing.
///
/// # Errors
///
/// `ValueError` on a load refusal; the resolution itself is fallible by
/// contract but cannot refuse today.
#[pyfunction]
#[pyo3(signature = (env, file, database=None))]
pub fn resolve_hypothetical_database_path(
    env: ::std::collections::BTreeMap<String, String>,
    file: Option<&str>,
    database: Option<&str>,
) -> PyResult<ResolvedDatabasePath> {
    let layers = hypothetical_layers(env, file).map_err(|error| refusal(&error))?;
    Ok(database_path_in(&layers, database))
}

/// The shared core verification-retry policy defaults: the numbers a driver
/// seeds its retry policy from instead of carrying its own literal set.
///
/// A module function rather than a verdict member, because it answers a
/// different question: nothing here was configured, so there is no layer to
/// report and nothing for a cross-language comparison to compare.
#[pyfunction]
#[must_use]
pub fn verification_retry_policy_defaults() -> RetryPolicyDefaults {
    verification_retry_defaults()
}

/// [`ResolvedConfig::node_uri`] over an explicit set of layers (the shape a
/// caller outside the process boot, or a test, resolves against).
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

/// [`ResolvedConfig::resolve_chain_id`] over an explicit set of layers.
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

/// [`ResolvedConfig::database_path`] over an explicit set of layers.
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

/// The declared `database.path` key over an explicit typed config.
fn declared_database_path_of(cfg: &::degenbot_config::BotConfig) -> String {
    cfg.database.path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, reason = "test assertions fail loudly")]

    use std::collections::{BTreeMap, BTreeSet};

    use ::degenbot_config::{Arm, BotConfig, BotConfigLoader, MapEnv, NodeScope, Source};
    use ::degenbot_strategy::Settlement;

    use super::{
        chain_id_in, database_path_in, declared_database_path_of, entry_provenance_by_env,
        node_uri_in, provenance_by_path, settlement_broadcast_endpoints_in, values_by_path,
        ResolvedConfig,
    };

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

    /// The value projection carries every declared key and nothing else, so a
    /// key added to `config_schema!` reaches the driver with no FFI edit — the
    /// property that makes the verdict, not a per-key accessor, the interface.
    #[test]
    fn the_projection_carries_every_declared_key_and_nothing_else() {
        let cfg = ::degenbot_config::BotConfig::default();
        let projected: BTreeSet<String> = values_by_path(&cfg).into_keys().collect();
        let declared: BTreeSet<String> = ::degenbot_config::SCHEMA
            .iter()
            .map(|key| key.toml_path.to_string())
            .collect();
        assert_eq!(
            projected, declared,
            "the projection is walked out of SCHEMA, so a key declared without \
             reaching it means the walk is no longer the schema"
        );
    }

    /// A loaded key names the layer that supplied it, in the operator's dotted
    /// path vocabulary rather than the loader's internal env names.
    #[test]
    fn provenance_names_a_layer_for_every_key_the_load_recorded() {
        let cfg = loaded(&[("DEGENBOT_DEFAULT_CHAIN_ID", "8453")]);
        let provenance = provenance_by_path(&cfg);
        assert_eq!(
            provenance.get("session.chain_id").map(String::as_str),
            Some("env")
        );
        assert_eq!(
            provenance.get("telemetry.otel").map(String::as_str),
            Some("default"),
            "an untouched key still names the layer that won it"
        );
        let declared: BTreeSet<&str> = ::degenbot_config::SCHEMA
            .iter()
            .map(|key| key.toml_path)
            .collect();
        assert_eq!(
            provenance
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            declared
        );
    }

    /// A load that recorded no layer reports NO layer. Filling the gap with
    /// the floor would report an `env` winner as a `file` winner, which is
    /// exactly the divergence a cross-language oracle exists to catch.
    #[test]
    fn an_unrecorded_layer_is_absent_rather_than_the_floor() {
        let mut cfg = loaded(&[("DEGENBOT_DEFAULT_CHAIN_ID", "8453")]);
        cfg.provenance.clear();
        assert!(
            provenance_by_path(&cfg).is_empty(),
            "a map with no recorded layer must project to no layers at all"
        );
    }

    /// A layer recorded against a name no key declares has no dotted path to
    /// appear under, so it cannot masquerade as a declared key's layer.
    #[test]
    fn a_foreign_layer_name_never_becomes_a_declared_key() {
        // Named without the `DEGENBOT_` prefix on purpose: a prefixed
        // literal here would enter the schema-inventory sweep, which is a
        // different gate answering a different question.
        let mut cfg = loaded(&[]);
        cfg.provenance.insert("NOT_A_DECLARED_KEY", Source::Env);
        let provenance = provenance_by_path(&cfg);
        assert!(
            provenance.keys().all(|path| ::degenbot_config::SCHEMA
                .iter()
                .any(|k| k.toml_path == path)),
            "every projected path must be a declared key"
        );
    }

    /// A per-chain endpoint table is overridden one entry at a time, so the
    /// per-entry layers are carried beside the key-level one.
    #[test]
    fn entry_provenance_reports_the_layer_of_each_table_entry() {
        let cfg = loaded(&[("DEGENBOT_RPC_WS_CHAINID_1", "wss://node.example/ws")]);
        let entries = entry_provenance_by_env(&cfg);
        assert_eq!(
            entries
                .get("DEGENBOT_RPC_WS_CHAINID_")
                .and_then(|table| table.get("1"))
                .map(String::as_str),
            Some("env")
        );
    }

    /// The verdict Python receives is the installed one, and its value
    /// projection is built under the GIL from that same load — the two halves
    /// of the interface cannot describe different configurations.
    #[test]
    fn the_installed_verdict_projects_the_installed_load() {
        ::pyo3::Python::attach(|py| {
            let verdict = ResolvedConfig::installed();
            let values = verdict
                .values(py)
                .expect("every declared key projects into a Python object");
            assert_eq!(
                values.keys().cloned().collect::<BTreeSet<_>>(),
                ::degenbot_config::SCHEMA
                    .iter()
                    .map(|key| key.toml_path.to_string())
                    .collect::<BTreeSet<_>>()
            );
            // This binary never runs the module init that publishes a load, so
            // the verdict stands on the floor: every declared key is still
            // projected, and no key claims a layer nobody recorded.
            assert!(
                verdict.provenance().keys().all(|path| {
                    ::degenbot_config::SCHEMA
                        .iter()
                        .any(|key| key.toml_path == path)
                }),
                "every projected layer must belong to a declared key"
            );
            assert!(
                verdict.discovery_batch_size() >= 1,
                "the forwarded batch size must never collapse to a busy loop"
            );
        });
    }

    /// The readiness resolution's settled settlement URLs, or `None` when the
    /// resolution refuses or the arm is inactive.
    fn readiness_settlement_endpoints(cfg: &BotConfig) -> Option<Vec<String>> {
        match ::degenbot_config::strategy_readiness(cfg) {
            Ok(readiness) => match readiness.settlement {
                Arm::Active(urls) => Some(urls),
                Arm::Inactive => None,
            },
            Err(_) => None,
        }
    }

    /// Precondition for returning the readiness arm directly: for every
    /// representative settlement config the readiness arm's settled URLs equal
    /// the composition's, so the lift cannot change the returned list. A blank
    /// or whitespace-only set is the refusal shape both splitters agree on:
    /// the composition settles to empty, and the readiness resolution refuses
    /// it outright because an active facet may not carry an unsettled set.
    #[test]
    fn readiness_and_composition_settle_the_same_settlement_endpoints() {
        let explicit = {
            let mut cfg = BotConfig::default();
            cfg.strategy.settlement.active = true;
            cfg.strategy.settlement.endpoints = Some(String::from(
                "https://rpc.mevblocker.io/noreverts, https://rpc.flashbots.net?hint=hash",
            ));
            cfg
        };
        let default_stamped = {
            let mut cfg = BotConfig::default();
            cfg.strategy.settlement.active = true;
            cfg.strategy.settlement.endpoints =
                Some(::degenbot_config::SETTLEMENT_DEFAULT_ENDPOINTS.join(","));
            cfg
        };
        for cfg in [&explicit, &default_stamped] {
            let readiness = readiness_settlement_endpoints(cfg)
                .expect("the settlement arm is active with an allowlisted set");
            assert_eq!(
                readiness,
                Settlement::from_config(cfg).into_config().endpoints,
                "the readiness arm and the composition must settle one list"
            );
        }
        for raw in ["", "   ", " , , "] {
            let mut cfg = BotConfig::default();
            cfg.strategy.settlement.active = true;
            cfg.strategy.settlement.endpoints = Some(String::from(raw));
            assert!(
                ::degenbot_config::strategy_readiness(&cfg).is_err(),
                "an active settlement facet with a blank set must refuse"
            );
            assert!(
                Settlement::from_config(&cfg)
                    .into_config()
                    .endpoints
                    .is_empty(),
                "the composition's split of a blank set is empty, matching the refusal"
            );
        }
    }

    /// The returned list is pinned here in Rust: the Python relay-posture pins
    /// monkeypatch the wrapper, so the lift's return home needs a lower-level
    /// pin. Splitting, trimming, and order come from the readiness resolution.
    #[test]
    fn settlement_broadcast_endpoints_returns_the_readiness_arm_list() {
        let mut cfg = BotConfig::default();
        cfg.strategy.settlement.active = true;
        cfg.strategy.settlement.endpoints = Some(String::from(
            " https://rpc.mevblocker.io/noreverts , https://rpc.flashbots.net?hint=hash ",
        ));
        assert_eq!(
            settlement_broadcast_endpoints_in(&cfg).expect("the settlement arm is active"),
            vec![
                String::from("https://rpc.mevblocker.io/noreverts"),
                String::from("https://rpc.flashbots.net?hint=hash"),
            ]
        );
    }

    /// An inactive settlement facet refuses with the settlement-arm message,
    /// not an empty list.
    #[test]
    fn settlement_broadcast_endpoints_refuses_an_inactive_facet() {
        let error = settlement_broadcast_endpoints_in(&BotConfig::default())
            .expect_err("an inactive settlement arm refuses");
        assert!(
            error
                .to_string()
                .contains("this hosted runner IS the settlement arm"),
            "the refusal must name the settlement-arm remedy, got: {error}"
        );
    }
}
