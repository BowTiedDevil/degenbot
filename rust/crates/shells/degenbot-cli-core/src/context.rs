//! The driver-domain context a command executes against (ADR-051 D8,
//! ADR-062 D7).
//!
//! The database path / chain id / node URIs are resolved through the
//! `degenbot-config` resolvers — the SAME cascades the Python console reads —
//! over a config loaded ONCE from the context's injectable [`EnvVars`] seam
//! (never `std::env`) plus the CLI override values the argv facade threaded
//! in. One load, four layers, every value tagged with the layer that supplied
//! it.

use std::path::PathBuf;
use std::sync::OnceLock;

use degenbot_config::{
    resolve_chain_id, resolve_database_path_with, resolve_node_request_uri,
    resolve_node_subscription_uri, EnvVars, LoadedConfig, NodeOverrides, NodeTransport, Resolved,
};

/// The resolved inputs a console command runs against.
pub struct CliContext<'a> {
    env: &'a dyn EnvVars,
    database: Option<String>,
    chain_id: Option<String>,
    nodes: NodeOverrides,
    config: Option<String>,
    loaded: OnceLock<LoadedConfig>,
}

impl<'a> CliContext<'a> {
    /// A context over `env` with no CLI overrides (file + env + defaults only).
    #[must_use]
    pub fn new(env: &'a dyn EnvVars) -> Self {
        Self {
            env,
            database: None,
            chain_id: None,
            nodes: NodeOverrides::new(),
            config: None,
            loaded: OnceLock::new(),
        }
    }

    /// Set the `--database` override (highest-precedence layer).
    #[must_use]
    pub fn with_database(mut self, database: impl Into<String>) -> Self {
        self.database = Some(database.into());
        self
    }

    /// Set the `--chain-id` override.
    #[must_use]
    pub fn with_chain_id(mut self, chain_id: impl Into<String>) -> Self {
        self.chain_id = Some(chain_id.into());
        self
    }

    /// Set one `--node` override: an explicit endpoint for the transport its
    /// own value classified as (ADR-062 D6). The flag cannot name a transport
    /// its value does not carry, so each occurrence fills exactly one slot.
    #[must_use]
    pub fn with_node(mut self, uri: impl Into<String>, transport: NodeTransport) -> Self {
        self.nodes = self.nodes.with_transport(transport, uri);
        self
    }

    /// Set every explicit endpoint at once (a caller that classified a whole
    /// argv vector itself, e.g. the repeatable `--node`).
    #[must_use]
    pub fn with_node_overrides(mut self, nodes: NodeOverrides) -> Self {
        self.nodes = nodes;
        self
    }

    /// Set the `--config` override (the typed config file the strategy
    /// verbs read and write).
    #[must_use]
    pub fn with_config(mut self, path: impl Into<String>) -> Self {
        self.config = Some(path.into());
        self
    }

    /// The env seam (the loaders' only env reader).
    #[must_use]
    pub fn env(&self) -> &'a dyn EnvVars {
        self.env
    }

    /// The `--database` override, if any.
    #[must_use]
    pub fn database_override(&self) -> Option<&str> {
        self.database.as_deref()
    }

    /// The explicit node endpoints the argv facade threaded in.
    #[must_use]
    pub fn node_overrides(&self) -> &NodeOverrides {
        &self.nodes
    }

    /// The config file the driver-domain resolvers read: the `--config`
    /// override when given, else the standard path — `DEGENBOT_CONFIG` (honored
    /// even when the file is missing: the operator asked for it) else the
    /// XDG/HOME config file when it exists. `None` means the context has no
    /// file layer at all, which is contractually the schema defaults.
    #[must_use]
    pub fn config_file(&self) -> Option<PathBuf> {
        match &self.config {
            Some(path) => Some(PathBuf::from(path)),
            None => degenbot_config::standard_file_path_with(self.env),
        }
    }

    /// The four layers loaded once per context: the file layer
    /// ([`Self::config_file`]) over the env seam, plus the declared defaults.
    /// Every driver-domain resolver reads this value, so a command resolves
    /// its database path, chain id, and endpoints against ONE load.
    ///
    /// # Errors
    ///
    /// [`crate::error::CliError::Config`] carrying the loader's fail-closed
    /// [`degenbot_config::ConfigError`] when a file the operator named is
    /// unreadable, unparsable, or holds an invalid value.
    pub fn loaded_config(&self) -> Result<&LoadedConfig, crate::error::CliError> {
        if let Some(loaded) = self.loaded.get() {
            return Ok(loaded);
        }
        let loaded = match self.config_file() {
            Some(file) => degenbot_config::BotConfigLoader::new()
                .with_config_path(file)
                .with_env_ref(self.env)
                .load(),
            None => degenbot_config::BotConfigLoader::new()
                .with_env_ref(self.env)
                .load(),
        }
        .map_err(crate::error::CliError::Config)?;
        Ok(self.loaded.get_or_init(|| loaded))
    }

    /// Resolve the database path: `--database` > `DEGENBOT_DB_PATH` >
    /// `database.path` > the state-home default. The env seam supplies
    /// `HOME` / `$XDG_STATE_HOME` for the `~` expansion of the default.
    ///
    /// # Errors
    ///
    /// [`crate::error::CliError::Config`] when the context's config layers do
    /// not load.
    pub fn database_path(&self) -> Result<Resolved<PathBuf>, crate::error::CliError> {
        let resolved =
            resolve_database_path_with(self.loaded_config()?, self.database.as_deref(), self.env);
        Ok(resolved)
    }

    /// Resolve the session chain id: `--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID`
    /// > `session.chain_id`.
    ///
    /// # Errors
    ///
    /// [`crate::error::CliError::Config`] when no layer named a chain, when
    /// the explicit value is not an integer, or when the config layers do not
    /// load.
    pub fn chain_id(&self) -> Result<Resolved<u64>, crate::error::CliError> {
        Ok(resolve_chain_id(
            self.loaded_config()?,
            self.chain_id.as_deref(),
        )?)
    }

    /// Resolve the node URI a REQUEST consumer uses for `chain_id`: ipc, then
    /// ws, then http (ADR-062 D3).
    ///
    /// The updater arms resolve the chain they actually operate on (which may
    /// differ from the session chain id, e.g. a validated Aave deployment).
    ///
    /// # Errors
    ///
    /// [`crate::error::CliError::Config`] when no layer supplied an endpoint
    /// for the chain, or when the config layers do not load.
    pub fn node_request_uri_for(
        &self,
        chain_id: u64,
    ) -> Result<Resolved<String>, crate::error::CliError> {
        Ok(resolve_node_request_uri(
            self.loaded_config()?,
            chain_id,
            self.node_overrides(),
        )?)
    }

    /// Resolve the node URI a REQUEST consumer uses for the session chain id.
    ///
    /// # Errors
    ///
    /// [`crate::error::CliError::Config`] when the chain id or the endpoint is
    /// unresolved, or when the config layers do not load.
    pub fn node_request_uri(&self) -> Result<Resolved<String>, crate::error::CliError> {
        let chain_id = self.chain_id()?;
        self.node_request_uri_for(chain_id.value)
    }

    /// Resolve the node URI a SUBSCRIPTION consumer uses for the session
    /// chain id: ipc or ws, never http (ADR-062 D3 — a feed never polls).
    ///
    /// # Errors
    ///
    /// [`crate::error::CliError::Config`] when the chain id or the feed
    /// endpoint is unresolved, or when the config layers do not load.
    pub fn node_subscription_uri(&self) -> Result<Resolved<String>, crate::error::CliError> {
        let chain_id = self.chain_id()?;
        Ok(resolve_node_subscription_uri(
            self.loaded_config()?,
            chain_id.value,
            self.node_overrides(),
        )?)
    }

    /// Resolve the config file the strategy verbs write to: the `--config`
    /// override, else the `DEGENBOT_CONFIG` env var (honored even when the
    /// file is absent — the operator asked for it), else the XDG config home
    /// (a write there creates the file). No home and no override is a typed
    /// refusal, never a silent skip.
    ///
    /// # Errors
    ///
    /// [`CliError::Config`]-carrying [`crate::error::CliError`] when no
    /// config location resolves at all.
    pub fn resolve_config_file(&self) -> Result<std::path::PathBuf, crate::error::CliError> {
        use crate::error::CliError;

        if let Some(path) = &self.config {
            return Ok(std::path::PathBuf::from(path));
        }
        if let Some(path) = degenbot_config::standard_file_path_with(self.env) {
            return Ok(path);
        }
        if let Some(home) = degenbot_config::config_home(self.env) {
            return Ok(home.join("degenbot").join("config.toml"));
        }
        Err(CliError::InvalidArgument(
            "no config file location: pass --config or set DEGENBOT_CONFIG".to_string(),
        ))
    }

    /// Load the typed config over this context's env + the resolved file.
    ///
    /// # Errors
    ///
    /// The loader's fail-closed [`degenbot_config::ConfigError`] wrapped in
    /// [`crate::error::CliError::InvalidArgument`].
    pub fn load_bot_config(&self) -> Result<degenbot_config::LoadedConfig, crate::error::CliError> {
        let file = self.resolve_config_file()?;
        self.load_bot_config_at(&file)
    }

    /// Load the typed config over this context's env + an explicit file.
    /// An absent file is the empty default config (a first write creates it);
    /// an existing-but-unreadable file surfaces the loader's refusal.
    ///
    /// # Errors
    ///
    /// The loader's fail-closed [`degenbot_config::ConfigError`] wrapped in
    /// [`crate::error::CliError::InvalidArgument`].
    pub fn load_bot_config_at(
        &self,
        file: &std::path::Path,
    ) -> Result<degenbot_config::LoadedConfig, crate::error::CliError> {
        if !file.exists() {
            return degenbot_config::BotConfigLoader::new()
                .with_env_ref(self.env)
                .load()
                .map_err(|error| crate::error::CliError::InvalidArgument(error.to_string()));
        }
        degenbot_config::BotConfigLoader::new()
            .with_config_path(file)
            .with_env_ref(self.env)
            .load()
            .map_err(|error| crate::error::CliError::InvalidArgument(error.to_string()))
    }
}
