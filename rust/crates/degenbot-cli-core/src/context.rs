//! The driver-domain context a command executes against (ADR-051 D8).
//!
//! The database path / chain id / node URIs are resolved through the
//! `degenbot-config` resolvers — the SAME cascades the Python console reads —
//! over an injectable [`EnvVars`] seam (never `std::env`), with the CLI override
//! values the argv facade threaded in.

use std::path::PathBuf;

use degenbot_config::{
    resolve_chain_id, resolve_database_path, resolve_node_uris, EnvVars, Resolved, ResolvedNodeUris,
};

/// The resolved inputs a console command runs against.
pub struct CliContext<'a> {
    env: &'a dyn EnvVars,
    database: Option<String>,
    chain_id: Option<String>,
    node_http: Option<String>,
    node_ws: Option<String>,
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
}
