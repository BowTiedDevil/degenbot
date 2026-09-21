//! The driver-domain context a command executes against (ADR-051 D8).
//!
//! The database path / chain id / node URIs are resolved through the
//! `degenbot-config` resolvers — the SAME cascades the Python console reads —
//! over an injectable [`EnvVars`] seam (never `std::env`), with the CLI override
//! values the argv facade threaded in.

use std::path::PathBuf;

use degenbot_config::{
    resolve_chain_id, resolve_database_path, resolve_node_http_uri, resolve_node_uris, EnvVars,
    Resolved, ResolvedNodeUris,
};

/// The resolved inputs a console command runs against.
pub struct CliContext<'a> {
    env: &'a dyn EnvVars,
    database: Option<String>,
    chain_id: Option<String>,
    node_http: Option<String>,
    node_ws: Option<String>,
    config: Option<String>,
}

impl<'a> CliContext<'a> {
    /// A context over `env` with no CLI overrides (env + defaults only).
    #[must_use]
    pub fn new(env: &'a dyn EnvVars) -> Self {
        Self {
            env,
            database: None,
            chain_id: None,
            node_http: None,
            node_ws: None,
            config: None,
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

    /// Set the `--node-http` override.
    #[must_use]
    pub fn with_node_http(mut self, uri: impl Into<String>) -> Self {
        self.node_http = Some(uri.into());
        self
    }

    /// Set the `--node-ws` override.
    #[must_use]
    pub fn with_node_ws(mut self, uri: impl Into<String>) -> Self {
        self.node_ws = Some(uri.into());
        self
    }

    /// Set the `--config` override (the typed config file the strategy
    /// verbs read and write).
    #[must_use]
    pub fn with_config(mut self, path: impl Into<String>) -> Self {
        self.config = Some(path.into());
        self
    }

    /// The env seam (the resolvers' only env reader).
    #[must_use]
    pub fn env(&self) -> &'a dyn EnvVars {
        self.env
    }

    /// The `--database` override, if any.
    #[must_use]
    pub fn database_override(&self) -> Option<&str> {
        self.database.as_deref()
    }

    /// Resolve the database path: `--database` > `DEGENBOT_DB_PATH` > the
    /// built-in default. Never fails.
    #[must_use]
    pub fn database_path(&self) -> Resolved<PathBuf> {
        resolve_database_path(self.env, self.database.as_deref())
    }

    /// Resolve the session chain id: `--chain-id` > `DEGENBOT_DEFAULT_CHAIN_ID`.
    ///
    /// # Errors
    ///
    /// [`degenbot_config::ConfigError`] when neither layer supplied a value, or
    /// the winning layer is not an integer.
    pub fn chain_id(&self) -> Result<Resolved<u64>, degenbot_config::ConfigError> {
        resolve_chain_id(self.env, self.chain_id.as_deref())
    }

    /// Resolve the HTTP node URI for `chain_id`: `--node-http` >
    /// `DEGENBOT_RPC_HTTP_CHAINID_<id>`.
    ///
    /// The updater arms resolve the chain they actually operate on (which may
    /// differ from the session chain id, e.g. a validated Aave deployment).
    ///
    /// # Errors
    ///
    /// [`degenbot_config::ConfigError`] when no layer supplied the URI.
    pub fn node_http_uri_for(
        &self,
        chain_id: u64,
    ) -> Result<Resolved<String>, degenbot_config::ConfigError> {
        resolve_node_http_uri(self.env, chain_id, self.node_http.as_deref())
    }

    /// Resolve the HTTP node URI for the session chain id.
    ///
    /// # Errors
    ///
    /// [`degenbot_config::ConfigError`] when the chain id or endpoint is
    /// unresolved.
    pub fn node_http_uri(&self) -> Result<Resolved<String>, degenbot_config::ConfigError> {
        let chain_id = self.chain_id()?;
        self.node_http_uri_for(chain_id.value)
    }

    /// Resolve both node URIs for the session chain id.
    ///
    /// # Errors
    ///
    /// [`degenbot_config::ConfigError`] when the chain id or either endpoint is
    /// unresolved.
    pub fn node_uris(&self) -> Result<ResolvedNodeUris, degenbot_config::ConfigError> {
        let chain_id = self.chain_id()?;
        resolve_node_uris(
            self.env,
            chain_id.value,
            self.node_http.as_deref(),
            self.node_ws.as_deref(),
        )
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
