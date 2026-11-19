//! The `AnvilFork` handle — lifecycle + `Provider` + dev-RPC surface.
//!
//! A Rust-owned fork handle wrapping [`alloy::node_bindings::Anvil`] (subprocess
//! lifecycle) + a connected alloy [`Provider`] (over IPC) + the anvil dev-RPC
//! surface as typed methods delegating to [`alloy::providers::ext::AnvilApi`].
//!
//! The public config + dev-method surface of a fork: the Python `AnvilFork`
//! any driver sees is a thin `PyO3` shell over this crate.
//!
//! [`Provider`]: alloy::providers::Provider

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use alloy::{
    node_bindings::{Anvil, NodeError},
    primitives::{Address, Bytes, U256},
    providers::{ext::AnvilApi, DynProvider},
    rpc::types::anvil::{Forking, NodeInfo},
};
use thiserror::Error;

use crate::transport::ForkTransport;

/// Where the spawned node's chain state comes from.
///
/// The spawn/readiness diagnostics name it: a forked child's startup waits on
/// an upstream and an in-memory child's does not, so an operator reading a
/// failure needs to know which shape they are looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkTarget {
    /// No `fork_url` — a self-contained node, no upstream in the path.
    InMemory,
    /// `fork_url` — the upstream JSON-RPC endpoint the child forks from.
    Upstream(String),
}

impl std::fmt::Display for ForkTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InMemory => f.write_str("in-memory node"),
            Self::Upstream(url) => write!(f, "fork target {url}"),
        }
    }
}

/// Errors raised by [`AnvilFork`] lifecycle + dev-RPC operations.
///
/// Startup failures are one variant per cause the node-binding distinguishes —
/// the program never ran, the child exited before readiness, or the readiness
/// deadline expired — because each needs a different operator response.
#[derive(Debug, Error)]
pub enum ForkError {
    /// The node program could not be executed at all: no such file (a missing
    /// `anvil`), a permission error, a bad working directory. Nothing ran.
    #[error("anvil spawn failed ({fork_target}): could not execute the node program: {source}")]
    Spawn {
        fork_target: ForkTarget,
        #[source]
        source: std::io::Error,
    },

    /// The child process exited before it reported readiness.
    ///
    /// A forked child whose upstream refuses the fork's genesis fetch (a hosted
    /// endpoint answering HTTP 429, for instance) fails this way. The child's
    /// own reason is on the stderr it inherits, which the node-binding does not
    /// capture into this error.
    #[error("{detail} ({fork_target}); the child's own reason is on the stderr above")]
    ChildExit {
        fork_target: ForkTarget,
        detail: String,
    },

    /// The readiness deadline expired while the child was still alive.
    #[error("anvil did not report readiness within {deadline_ms}ms ({fork_target})")]
    SpawnTimeout {
        fork_target: ForkTarget,
        deadline_ms: u64,
    },

    /// The readiness wait failed for a reason that is neither the child exiting
    /// nor the deadline expiring: a stdout read error, a missing stdout pipe,
    /// an unparsable startup line.
    #[error("anvil readiness failed ({fork_target}): {source}")]
    Readiness {
        fork_target: ForkTarget,
        #[source]
        source: NodeError,
    },

    /// The fork's dedicated transport runtime could not be built.
    #[error("fork transport runtime failed to start ({fork_target}): {source}")]
    Runtime {
        fork_target: ForkTarget,
        #[source]
        source: std::io::Error,
    },

    /// IPC connection to the spawned anvil node failed.
    #[error("IPC connect failed at {path}: {source}")]
    Connect {
        path: String,
        source: alloy::transports::TransportError,
    },

    /// An anvil dev-RPC call returned an error.
    #[error("RPC error: {0}")]
    Rpc(String),
}

impl ForkError {
    /// Classify a node-binding startup failure into the cause an operator acts on.
    ///
    /// The locked binding already separates the causes: `SpawnError` means the
    /// program never ran, `Fatal` means the child exited before readiness (the
    /// binding detects stdout EOF and waits for the child to reap its status),
    /// and `Timeout` means the deadline passed with the child alive. Wrapping
    /// those types and forwarding their own text preserves each cause; what the
    /// binding cannot see — which fork target the child was fetching from, and
    /// the deadline it was given — is carried here.
    fn spawn_failure(source: NodeError, fork_target: ForkTarget, deadline_ms: u64) -> Self {
        match source {
            NodeError::SpawnError(source) => Self::Spawn {
                fork_target,
                source,
            },
            NodeError::Fatal(detail) => Self::ChildExit {
                fork_target,
                detail,
            },
            NodeError::Timeout => Self::SpawnTimeout {
                fork_target,
                deadline_ms,
            },
            source => Self::Readiness {
                fork_target,
                source,
            },
        }
    }
}

impl From<alloy::transports::TransportError> for ForkError {
    fn from(err: alloy::transports::TransportError) -> Self {
        Self::Rpc(err.to_string())
    }
}

/// Mining mode — auto-mine on each transaction, or interval-mine, or none.
#[derive(Debug, Clone, Copy, Default)]
pub enum MiningMode {
    /// `mining_mode="auto"` — anvil's default (mine on each tx).
    #[default]
    Auto,
    /// `mining_mode="interval"` + `mining_interval=<secs>` → `--block-time=<secs>`.
    Interval(u64),
    /// `mining_mode="none"` → `--no-mining --order=fifo`.
    None,
}

/// Generate a process-unique default IPC socket path under the OS temp
/// directory so concurrent `AnvilFork` instances don't collide on anvil's
/// default `/tmp/anvil.ipc`.
fn default_ipc_path() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    std::env::temp_dir()
        .join(format!("degenbot-anvil-{}-{n}.ipc", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

/// Builder for an [`AnvilFork`].
///
/// The fork's config surface; call
/// [`AnvilForkBuilder::try_spawn`] to spawn the subprocess + connect the
/// Provider + apply any post-spawn state overrides.
#[derive(Debug, Clone)]
pub struct AnvilForkBuilder {
    fork_url: Option<String>,
    fork_block: Option<u64>,
    fork_transaction_hash: Option<String>,
    fork_compute_units_per_second: Option<u64>,
    mining_mode: MiningMode,
    storage_caching: bool,
    base_fee: Option<u128>,
    ipc_path: Option<PathBuf>,
    port: Option<u16>,
    host: String,
    mnemonic: String,
    chain_id: Option<u64>,
    extra_args: Vec<String>,
    balance_overrides: Vec<(Address, U256)>,
    code_overrides: Vec<(Address, Bytes)>,
    nonce_overrides: Vec<(Address, u64)>,
    storage_overrides: Vec<(Address, U256, U256)>,
    /// Node program to spawn, overriding `anvil` in `$PATH` (test-only).
    #[cfg(test)]
    anvil_program: Option<PathBuf>,
    /// Readiness deadline in milliseconds (test-only).
    #[cfg(test)]
    spawn_deadline_override: Option<u64>,
}

impl Default for AnvilForkBuilder {
    fn default() -> Self {
        Self {
            fork_url: None,
            fork_block: None,
            fork_transaction_hash: None,
            fork_compute_units_per_second: None,
            mining_mode: MiningMode::Auto,
            storage_caching: true,
            base_fee: None,
            ipc_path: None,
            port: None,
            host: "127.0.0.1".to_string(),
            // The default mnemonic used by Brownie for Ganache forks (matches
            // the Python AnvilFork default).
            mnemonic: "patient rude simple dog close planet oval animal hunt sketch suspect slim"
                .to_string(),
            chain_id: None,
            extra_args: Vec::new(),
            balance_overrides: Vec::new(),
            code_overrides: Vec::new(),
            nonce_overrides: Vec::new(),
            storage_overrides: Vec::new(),
            #[cfg(test)]
            anvil_program: None,
            #[cfg(test)]
            spawn_deadline_override: None,
        }
    }
}

impl AnvilForkBuilder {
    /// Create a new builder with the default config.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cap the fork upstream's request rate in compute units per second
    /// (`--compute-units-per-second`). `None` (default) spawns with
    /// `--no-rate-limit`: uncapped suits a local node, while a configured
    /// cap keeps a hosted free-tier endpoint under its per-IP CU budget —
    /// an uncapped hosted endpoint answers the fork's genesis fetches with
    /// HTTP 429, which kills anvil before it ever reports readiness.
    #[must_use]
    pub fn fork_compute_units_per_second(mut self, cups: u64) -> Self {
        self.fork_compute_units_per_second = Some(cups);
        self
    }

    /// Set the fork URL (`fork_url`). `None` (default) = in-memory, no fork.
    #[must_use]
    pub fn fork_url(mut self, fork_url: impl Into<String>) -> Self {
        self.fork_url = Some(fork_url.into());
        self
    }

    /// Set the fork block number (`fork_block`).
    #[must_use]
    pub fn fork_block(mut self, block: u64) -> Self {
        self.fork_block = Some(block);
        self
    }

    /// Set the fork transaction hash (`fork_transaction_hash`).
    #[must_use]
    pub fn fork_transaction_hash(mut self, hash: impl Into<String>) -> Self {
        self.fork_transaction_hash = Some(hash.into());
        self
    }

    /// Set the mining mode (`mining_mode` + `mining_interval`).
    #[must_use]
    pub fn mining_mode(mut self, mode: MiningMode) -> Self {
        self.mining_mode = mode;
        self
    }

    /// Enable/disable storage caching (`storage_caching`). Default `true`;
    /// `false` → `--no-storage-caching`.
    #[must_use]
    pub const fn storage_caching(mut self, enable: bool) -> Self {
        self.storage_caching = enable;
        self
    }

    /// Set the next-block base fee (`base_fee`).
    #[must_use]
    pub fn base_fee(mut self, fee: u128) -> Self {
        self.base_fee = Some(fee);
        self
    }

    /// Set the IPC socket path (`ipc_path`). Defaults to a process-unique
    /// file under the OS temp directory (e.g. `/tmp` on POSIX).
    #[must_use]
    pub fn ipc_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.ipc_path = Some(path.into());
        self
    }

    /// Set the HTTP/port (`port`). `None` = anvil picks a free port.
    #[must_use]
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set the bind host (`localhost`). Default `127.0.0.1`.
    #[must_use]
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = host.into();
        self
    }

    /// Set the mnemonic (`mnemonic`).
    #[must_use]
    pub fn mnemonic(mut self, mnemonic: impl Into<String>) -> Self {
        self.mnemonic = mnemonic.into();
        self
    }

    /// Set the chain id (`chain_id`).
    #[must_use]
    pub fn chain_id(mut self, chain_id: u64) -> Self {
        self.chain_id = Some(chain_id);
        self
    }

    /// Append raw anvil CLI args (`anvil_opts`).
    #[must_use]
    pub fn extra_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extra_args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Queue a post-spawn balance override (`balance_overrides`).
    #[must_use]
    pub fn balance_override(mut self, address: Address, balance: U256) -> Self {
        self.balance_overrides.push((address, balance));
        self
    }

    /// Queue a post-spawn code override (`bytecode_overrides`).
    #[must_use]
    pub fn code_override(mut self, address: Address, code: Bytes) -> Self {
        self.code_overrides.push((address, code));
        self
    }

    /// Queue a post-spawn nonce override (`nonce_overrides`).
    #[must_use]
    pub fn nonce_override(mut self, address: Address, nonce: u64) -> Self {
        self.nonce_overrides.push((address, nonce));
        self
    }

    /// Queue a post-spawn storage override (`storage_overrides`).
    #[must_use]
    pub fn storage_override(mut self, address: Address, slot: U256, value: U256) -> Self {
        self.storage_overrides.push((address, slot, value));
        self
    }

    /// Spawn the anvil subprocess + connect the Provider over IPC + apply any
    /// queued state overrides.
    ///
    /// # Errors
    /// - [`ForkError::Spawn`] if the node program cannot be executed (a missing
    ///   `anvil`, a permission error).
    /// - [`ForkError::ChildExit`] if the child exits before reporting readiness
    ///   (a fork child whose upstream refuses the genesis fetch).
    /// - [`ForkError::SpawnTimeout`] if the readiness deadline expires with the
    ///   child still alive.
    /// - [`ForkError::Readiness`] for any other readiness-wait failure.
    /// - [`ForkError::Runtime`] if the fork's transport runtime cannot start.
    /// - [`ForkError::Connect`] if the IPC connection fails.
    /// - [`ForkError::Rpc`] if a queued state-override call fails.
    ///
    /// # Panics
    ///
    /// Does not panic: the IPC connect failure (including the dedicated
    /// runtime's connect task aborting without delivering a result) maps to
    /// [`ForkError::Connect`].
    pub async fn try_spawn(self) -> Result<AnvilFork, ForkError> {
        // Use a process-unique IPC path by default (avoids anvil's default
        // socket collisions when multiple anvil forks spawn concurrently — e.g.
        // pytest-xdist parallel workers, or multiple `AnvilFork` instances in
        // one process). Caller-supplied `ipc_path` overrides this.
        let ipc_path = self
            .ipc_path
            .as_ref()
            .map_or_else(default_ipc_path, |p| p.to_string_lossy().into_owned());

        let anvil = self.build_anvil(&ipc_path);
        let instance = anvil.try_spawn().map_err(|source| {
            ForkError::spawn_failure(source, self.fork_target(), self.readiness_deadline_ms())
        })?;

        // Use the instance's resolved ipc_path (may differ from the requested
        // default if anvil picked its own).
        let resolved_ipc = instance.ipc_path().to_string();
        // The IPC provider's pubsub service + backend tasks live on a
        // DEDICATED single-worker runtime, not the ambient one: the session
        // disposes of that runtime as one unit, so the teardown never depends
        // on which shared-runtime worker gets to poll a woken task first.
        let pubsub_runtime = degenbot_core::pubsub_runtime::OwnedPubsubRuntime::build(
            degenbot_core::pubsub_runtime::OwnedPubsubRuntimeSpec {
                resource: "fork_pubsub_workers",
                kind: "tokio single-worker runtime (Anvil fork IPC pubsub — session-owned)",
                thread_name: "degenbot-fork-pubsub",
                sizing:
                    "exactly one per AnvilFork (fork-scoped IPC transport); fixed single worker",
                binding: "pinned",
            },
        )
        .map_err(|source| ForkError::Runtime {
            fork_target: self.fork_target(),
            source,
        })?;

        let transport = ForkTransport::open(pubsub_runtime, resolved_ipc.clone())
            .await
            .map_err(|source| ForkError::Connect {
                path: resolved_ipc.clone(),
                source,
            })?;

        let fork = AnvilFork {
            transport,
            instance,
            ipc_path: resolved_ipc,
        };

        // Apply queued state overrides post-spawn.
        for (addr, bal) in &self.balance_overrides {
            fork.set_balance(*addr, *bal).await?;
        }
        for (addr, code) in &self.code_overrides {
            fork.set_code(*addr, code.clone()).await?;
        }
        for (addr, nonce) in &self.nonce_overrides {
            fork.set_nonce(*addr, *nonce).await?;
        }
        for (addr, slot, val) in &self.storage_overrides {
            fork.set_storage_at(*addr, *slot, *val).await?;
        }

        Ok(fork)
    }

    /// Spawn deadline for a forked anvil (alloy's default is 10s, tuned
    /// for in-memory spawns).
    ///
    /// A forked child's startup includes fetching fork metadata and genesis
    /// state from the upstream, so the deadline is an upstream-latency budget,
    /// not a local process-spawn budget: a hosted endpoint with a configured
    /// compute-unit cap stretches every fetch, and a deep fork (a pinned block
    /// or transaction hash) issues many of them. It is therefore deliberately
    /// far above any plausible local spawn cost, while staying under the test
    /// suite's per-test budget (300s) that the fork lane spawns into. Widening
    /// it costs nothing on the failure path: a child that dies during startup
    /// is reported as [`ForkError::ChildExit`] the moment it exits, not when
    /// this deadline expires.
    const FORK_SPAWN_TIMEOUT_MS: u64 = 180_000;

    /// The readiness deadline this builder spawns under, in milliseconds.
    ///
    /// A forked spawn gets [`Self::FORK_SPAWN_TIMEOUT_MS`]; an in-memory spawn
    /// keeps the node-binding's own default
    /// ([`alloy::node_bindings::NODE_STARTUP_TIMEOUT`]), so its behavior is
    /// unchanged.
    fn readiness_deadline_ms(&self) -> u64 {
        #[cfg(test)]
        if let Some(deadline_ms) = self.spawn_deadline_override {
            return deadline_ms;
        }

        if self.fork_url.is_some() {
            Self::FORK_SPAWN_TIMEOUT_MS
        } else {
            u64::try_from(alloy::node_bindings::NODE_STARTUP_TIMEOUT.as_millis())
                .unwrap_or(u64::MAX)
        }
    }

    /// Where the spawned child reads its chain state from.
    fn fork_target(&self) -> ForkTarget {
        self.fork_url
            .clone()
            .map_or(ForkTarget::InMemory, ForkTarget::Upstream)
    }

    /// Spawn `program` instead of `anvil` from `$PATH` (test-only).
    ///
    /// The readiness diagnostics are proven against scripted children through
    /// this, so they do not depend on a real binary's startup behavior.
    #[cfg(test)]
    #[must_use]
    pub fn anvil_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.anvil_program = Some(program.into());
        self
    }

    /// Override the readiness deadline in milliseconds (test-only).
    #[cfg(test)]
    #[must_use]
    pub const fn spawn_deadline_ms(mut self, deadline_ms: u64) -> Self {
        self.spawn_deadline_override = Some(deadline_ms);
        self
    }

    /// The anvil CLI flags governing the fork upstream's request rate.
    ///
    /// Uncapped (`--no-rate-limit`) suits a local node, which has no per-IP
    /// budget to protect; a configured cap
    /// (`--compute-units-per-second`) keeps a hosted free-tier endpoint
    /// under its budget (dRPC's free floor is 50,400 CU/min).
    fn fork_rate_args(&self) -> Vec<String> {
        match self.fork_compute_units_per_second {
            Some(cups) => vec![format!("--compute-units-per-second={cups}")],
            None => vec!["--no-rate-limit".to_string()],
        }
    }

    /// Build the `alloy::node_bindings::Anvil` builder from the configured
    /// params.
    fn build_anvil(&self, ipc_path: &str) -> Anvil {
        #[cfg(test)]
        let program = self.anvil_program.as_ref();
        #[cfg(not(test))]
        let program: Option<&PathBuf> = None;

        let mut anvil = program
            .map_or_else(Anvil::new, Anvil::at)
            // Python default args:
            .arg("--auto-impersonate")
            .host(self.host.clone())
            .mnemonic(self.mnemonic.clone())
            .ipc_path(ipc_path.to_string())
            // The deadline is a property of the spawn shape, and the failure
            // diagnostic reports the deadline the wait actually ran under.
            .timeout(self.readiness_deadline_ms());

        if let Some(port) = self.port {
            anvil = anvil.port(port);
        }
        if let Some(chain_id) = self.chain_id {
            anvil = anvil.arg(format!("--chain-id={chain_id}"));
        }
        if let Some(ref url) = self.fork_url {
            anvil = anvil.fork(url.clone());
            for arg in self.fork_rate_args() {
                anvil = anvil.arg(arg);
            }
        }
        if let Some(block) = self.fork_block {
            anvil = anvil.arg(format!("--fork-block-number={block}"));
        }
        if let Some(ref hash) = self.fork_transaction_hash {
            anvil = anvil.arg(format!("--fork-transaction-hash={hash}"));
        }
        if let Some(fee) = self.base_fee {
            anvil = anvil.arg(format!("--base-fee={fee}"));
        }
        match self.mining_mode {
            MiningMode::Auto => {}
            MiningMode::Interval(secs) => anvil = anvil.arg(format!("--block-time={secs}")),
            MiningMode::None => {
                anvil = anvil.arg("--no-mining").arg("--order=fifo");
            }
        }
        if !self.storage_caching {
            anvil = anvil.arg("--no-storage-caching");
        }
        for arg in &self.extra_args {
            anvil = anvil.arg(arg.clone());
        }
        anvil
    }
}

/// A Rust-owned Anvil fork handle.
///
/// Owns the spawned `anvil` subprocess (via the embedded `AnvilInstance`) and
/// the [`ForkTransport`] session wired to it over IPC. Dropping the handle
/// closes the session, removes the Unix-domain IPC socket file so `/tmp`
/// doesn't accumulate leftover `.ipc` files, and kills the subprocess.
///
/// Ownership invariant: the child outlives the transport. [`Drop`] closes the
/// session while the child is still running, so the pubsub service observes a
/// closed request channel instead of a dead backend; the session's reconnect
/// gate — not this ordering — is what makes a service that does outlive the
/// child unable to dial its socket again.
///
/// Call [`AnvilForkBuilder::try_spawn`] to construct.
pub struct AnvilFork {
    /// The IPC transport session: the provider frontend handed out by
    /// [`AnvilFork::provider`], the dedicated runtime its pubsub service runs
    /// on, and the reconnect gate the session shuts when it closes. `close` is
    /// the one operation that ends it (see [`ForkTransport`]).
    transport: ForkTransport,
    /// Owns the anvil subprocess lifecycle (drop = kill).
    ///
    /// Killed by the field drop that follows [`Drop::drop`], i.e. after the
    /// transport session closed — the child is the last thing to go.
    instance: alloy::node_bindings::AnvilInstance,
    /// The IPC socket file this fork owns (removed on drop).
    ipc_path: String,
}

impl Drop for AnvilFork {
    fn drop(&mut self) {
        // Close the session while the child is alive and its socket is still
        // connected, so the pubsub service can take the clean "request channel
        // closed" exit instead of observing a dead backend.
        self.transport.close();
        // Safe while the fd is open; the `instance` field drop kills the child
        // after this body returns.
        let _ = std::fs::remove_file(&self.ipc_path);
    }
}

impl AnvilFork {
    /// Borrow the connected alloy [`Provider`] for general RPC
    /// (`get_block`, `get_balance`, `eth_call`, etc.) over the same IPC
    /// transport. This replaces the legacy Python `self.w3` handle.
    ///
    /// # Panics
    ///
    /// Panics if the fork is closed (its provider was taken by `Drop`).
    #[must_use]
    pub fn provider(&self) -> &DynProvider {
        // Targeted expect (fulfilled): a closed fork has had its provider taken
        // by `Drop`; calling `provider()` on it then is a programmer error, and
        // panicing loudly (as the `# Panics` doc documents) beats returning a
        // dangling reference.
        #[expect(clippy::expect_used)]
        self.transport
            .provider()
            .expect("AnvilFork provider dropped (fork closed)")
    }

    /// The resolved IPC socket path the spawned anvil process is listening on
    /// (anvil's `--ipc` arg, or its default socket path).
    ///
    /// Python companions + standalone Rust consumers use this to construct a
    /// second `Provider` over the same IPC socket (e.g. `AlloyProvider` for
    /// retry-aware `eth_call`/`get_logs` against the in-memory fork).
    #[must_use]
    pub fn ipc_path(&self) -> &str {
        &self.ipc_path
    }

    /// HTTP endpoint of the spawned anvil node (e.g. `http://127.0.0.1:PORT`).
    ///
    /// Companion shells use this to construct their own `Web3`/`HTTPProvider`
    /// over the rust-owned subprocess when they cannot use the IPC-bound
    /// `DynProvider` (e.g. test code still on web3.py contract patterns).
    #[must_use]
    pub fn http_url(&self) -> String {
        self.instance.endpoint()
    }

    /// WebSocket endpoint of the spawned anvil node (e.g. `ws://127.0.0.1:PORT`).
    #[must_use]
    pub fn ws_url(&self) -> String {
        self.instance.ws_endpoint()
    }

    /// TCP port the spawned anvil node listens on.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.instance.port()
    }

    /// `evm_mine` — mine a single block.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn mine(&self) -> Result<(), ForkError> {
        self.provider()
            .evm_mine(None)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    /// `anvil_reset` — reset the fork, optionally to a new block number.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn reset(&self, block_number: Option<u64>) -> Result<(), ForkError> {
        let forking = block_number.map(|bn| Forking {
            json_rpc_url: None,
            block_number: Some(bn),
        });
        self.provider()
            .anvil_reset(forking)
            .await
            .map_err(Into::into)
    }

    /// `anvil_snapshot` — take a snapshot, returning the snapshot id.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn snapshot(&self) -> Result<U256, ForkError> {
        self.provider().anvil_snapshot().await.map_err(Into::into)
    }

    /// `anvil_revert` — revert to a snapshot. Returns `true` if reverted.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn revert(&self, id: U256) -> Result<bool, ForkError> {
        self.provider().anvil_revert(id).await.map_err(Into::into)
    }

    /// `anvil_set_balance`.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn set_balance(&self, address: Address, balance: U256) -> Result<(), ForkError> {
        self.provider()
            .anvil_set_balance(address, balance)
            .await
            .map_err(Into::into)
    }

    /// `anvil_set_code`.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn set_code(&self, address: Address, code: Bytes) -> Result<(), ForkError> {
        self.provider()
            .anvil_set_code(address, code)
            .await
            .map_err(Into::into)
    }

    /// `anvil_set_nonce`.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn set_nonce(&self, address: Address, nonce: u64) -> Result<(), ForkError> {
        self.provider()
            .anvil_set_nonce(address, nonce)
            .await
            .map_err(Into::into)
    }

    /// `anvil_set_storage_at`.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn set_storage_at(
        &self,
        address: Address,
        slot: U256,
        value: U256,
    ) -> Result<(), ForkError> {
        self.provider()
            .anvil_set_storage_at(address, slot, value.into())
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    /// `anvil_set_next_block_base_fee_per_gas`.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn set_next_block_base_fee(&self, basefee: u128) -> Result<(), ForkError> {
        self.provider()
            .anvil_set_next_block_base_fee_per_gas(basefee)
            .await
            .map_err(Into::into)
    }

    /// `anvil_set_next_block_timestamp`.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn set_next_block_timestamp(&self, timestamp: u64) -> Result<(), ForkError> {
        self.provider()
            .anvil_set_next_block_timestamp(timestamp)
            .await
            .map_err(Into::into)
    }

    /// `anvil_set_block_timestamp_interval`.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn set_block_timestamp_interval(&self, seconds: u64) -> Result<(), ForkError> {
        self.provider()
            .anvil_set_block_timestamp_interval(seconds)
            .await
            .map_err(Into::into)
    }

    /// `anvil_set_coinbase`.
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn set_coinbase(&self, address: Address) -> Result<(), ForkError> {
        self.provider()
            .anvil_set_coinbase(address)
            .await
            .map_err(Into::into)
    }

    /// `anvil_node_info` (bonus — not in the current Python surface).
    ///
    /// # Errors
    /// [`ForkError::Rpc`] if the call fails.
    pub async fn node_info(&self) -> Result<NodeInfo, ForkError> {
        self.provider().anvil_node_info().await.map_err(Into::into)
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    //! In-process tests on an in-memory (no-fork) anvil. No external RPC.

    use super::*;
    use alloy::primitives::address;
    use alloy::providers::Provider;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    /// The canonical "Vitalik" address, used as a test fixture target.
    const VITALIK: Address = address!("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045");

    fn ensure_anvil_available() {
        if std::process::Command::new("anvil")
            .arg("--version")
            .output()
            .is_err()
        {
            panic!(
                "`anvil` binary not found in $PATH — required for degenbot-fork tests. \
                 Install Foundry: https://book.getfoundry.sh/getting-started/installation"
            );
        }
    }

    fn test_ipc_path() -> String {
        // Use a process-unique IPC path to avoid clashing with any other
        // concurrent anvil instance during the test run.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        std::env::temp_dir()
            .join(format!("degenbot-fork-test-{}-{n}.ipc", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn fork_rate_args_default_uncapped() {
        let builder = AnvilForkBuilder::new().fork_url("http://localhost:8545");
        assert_eq!(
            builder.fork_rate_args(),
            vec!["--no-rate-limit".to_string()]
        );
    }

    #[test]
    fn fork_rate_args_configured_cap_replaces_no_rate_limit() {
        let builder = AnvilForkBuilder::new()
            .fork_url("http://localhost:8545")
            .fork_compute_units_per_second(350);
        assert_eq!(
            builder.fork_rate_args(),
            vec!["--compute-units-per-second=350".to_string()]
        );
    }

    #[tokio::test]
    async fn spawn_mine_snapshot_revert_set_balance() {
        ensure_anvil_available();
        let ipc = test_ipc_path();
        let _ = std::fs::remove_file(&ipc);

        let fork = AnvilForkBuilder::new()
            .ipc_path(&ipc)
            .try_spawn()
            .await
            .expect("anvil should spawn + connect over IPC");

        // node_info works over IPC (chain_id = anvil default 31337, no fork).
        let info = fork.node_info().await.expect("anvil_node_info");
        assert_eq!(info.environment.chain_id, 31337);

        // mine a block.
        let bn0 = fork.provider().get_block_number().await.unwrap();
        fork.mine().await.expect("evm_mine");
        let bn1 = fork.provider().get_block_number().await.unwrap();
        assert_eq!(bn1, bn0 + 1);

        // snapshot → mine → revert → block restored.
        let snap = fork.snapshot().await.expect("anvil_snapshot");
        fork.mine().await.expect("evm_mine post-snapshot");
        let bn2 = fork.provider().get_block_number().await.unwrap();
        assert_eq!(bn2, bn0 + 2);
        let reverted = fork.revert(snap).await.expect("anvil_revert");
        assert!(reverted);
        let bn3 = fork.provider().get_block_number().await.unwrap();
        assert_eq!(bn3, bn0 + 1);

        // set_balance over IPC.
        fork.set_balance(VITALIK, U256::from(1_000_000_u64))
            .await
            .expect("anvil_set_balance");
        let bal = fork.provider().get_balance(VITALIK).await.unwrap();
        assert_eq!(bal, U256::from(1_000_000_u64));
    }

    #[tokio::test]
    async fn balance_override_applied_post_spawn() {
        ensure_anvil_available();
        let ipc = test_ipc_path();
        let _ = std::fs::remove_file(&ipc);

        let fork = AnvilForkBuilder::new()
            .ipc_path(&ipc)
            .balance_override(VITALIK, U256::from(42_u64))
            .try_spawn()
            .await
            .expect("spawn with balance override");

        let bal = fork.provider().get_balance(VITALIK).await.unwrap();
        assert_eq!(bal, U256::from(42_u64));
    }

    #[tokio::test]
    async fn set_code_then_get_code() {
        ensure_anvil_available();
        let ipc = test_ipc_path();
        let _ = std::fs::remove_file(&ipc);

        let code = Bytes::from_static(&[0x60, 0x80, 0x60, 0x40, 0x52]);
        let fork = AnvilForkBuilder::new()
            .ipc_path(&ipc)
            .try_spawn()
            .await
            .expect("spawn");

        fork.set_code(VITALIK, code.clone())
            .await
            .expect("anvil_set_code");
        let got = fork.provider().get_code_at(VITALIK).await.unwrap();
        assert_eq!(got, code);
    }

    #[tokio::test]
    async fn set_storage_then_read() {
        ensure_anvil_available();
        let ipc = test_ipc_path();
        let _ = std::fs::remove_file(&ipc);

        let fork = AnvilForkBuilder::new()
            .ipc_path(&ipc)
            .try_spawn()
            .await
            .expect("spawn");

        let slot = U256::ZERO;
        let value = U256::from(0xdead_beef_u64);
        fork.set_storage_at(VITALIK, slot, value)
            .await
            .expect("anvil_set_storage_at");
        // anvil exposes storage reads via `eth_getStorageAt` on the Provider.
        let got = fork
            .provider()
            .get_storage_at(VITALIK, slot)
            .await
            .expect("get_storage_at");
        assert_eq!(got, value);
    }

    #[tokio::test]
    async fn ipc_file_removed_on_drop() {
        ensure_anvil_available();
        let ipc = test_ipc_path();
        let _ = std::fs::remove_file(&ipc);

        let fork = AnvilForkBuilder::new()
            .ipc_path(&ipc)
            .try_spawn()
            .await
            .expect("spawn");

        // The socket file must exist while the instance is alive.
        assert!(
            std::fs::metadata(&ipc).is_ok(),
            "IPC socket should exist while AnvilFork is alive"
        );

        drop(fork);

        // After dropping the handle the file must be gone.
        assert!(
            std::fs::metadata(&ipc).is_err(),
            "IPC socket should be removed after AnvilFork is dropped"
        );
    }

    // -- Transport session: teardown and reconnect gating ------------------

    use std::time::Duration;

    /// A teardown that closes the session, joins its runtime, reaps the child,
    /// and unlinks the socket still settles inside this: the fork owns one
    /// parked worker, not a retry budget.
    const TEARDOWN_BOUND: Duration = Duration::from_secs(5);

    /// Spawn an in-memory fork on a socket path unique to the test.
    async fn spawn_fork() -> AnvilFork {
        ensure_anvil_available();
        let ipc = test_ipc_path();
        let _ = std::fs::remove_file(&ipc);
        AnvilForkBuilder::new()
            .ipc_path(&ipc)
            .try_spawn()
            .await
            .expect("anvil should spawn + connect over IPC")
    }

    /// Whether nothing accepts TCP connections on `port` — i.e. the child is
    /// gone, not merely unlinked.
    fn child_stopped(port: u16) -> bool {
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(500),
        )
        .is_err()
    }

    /// Dropping from a bare thread (the shape a `PyO3` handle takes when
    /// `CPython` refcounting releases it on the main thread) must close the
    /// session, remove the socket, and stop the child within the bound.
    #[tokio::test]
    async fn drop_on_a_bare_thread_tears_down_within_the_bound() {
        let fork = spawn_fork().await;
        let ipc = fork.ipc_path().to_string();
        let port = fork.port();

        let started = Instant::now();
        std::thread::spawn(move || drop(fork))
            .join()
            .expect("drop thread");
        let elapsed = started.elapsed();

        assert!(elapsed < TEARDOWN_BOUND, "teardown took {elapsed:?}");
        assert!(
            std::fs::metadata(&ipc).is_err(),
            "IPC socket left behind: {ipc}"
        );
        assert!(
            child_stopped(port),
            "the child still answers on port {port}"
        );
    }

    /// Dropping inside an async context takes the runtime's inside-async
    /// teardown path — the one a Rust consumer that drops the handle in a task
    /// takes — and must still be bounded and complete.
    #[tokio::test]
    async fn drop_inside_an_async_context_tears_down_within_the_bound() {
        let fork = spawn_fork().await;
        let ipc = fork.ipc_path().to_string();
        let port = fork.port();

        let started = Instant::now();
        drop(fork);
        let elapsed = started.elapsed();

        assert!(elapsed < TEARDOWN_BOUND, "teardown took {elapsed:?}");
        assert!(
            std::fs::metadata(&ipc).is_err(),
            "IPC socket left behind: {ipc}"
        );
        assert!(
            child_stopped(port),
            "the child still answers on port {port}"
        );
    }

    /// Teardown shuts the reconnect gate, and nothing a service does afterwards
    /// may reach the socket: a refused attempt parks instead of dialling, so
    /// the retry loop can never advance into a dead path.
    #[tokio::test]
    async fn teardown_shuts_the_reconnect_gate() {
        let fork = spawn_fork().await;
        let gate = fork.transport.gate();
        assert!(
            !gate.is_closed(),
            "the gate must be open while the fork is live"
        );

        drop(fork);

        assert!(
            gate.is_closed(),
            "teardown must refuse every later reconnect"
        );
        let (attempts, refused) = gate.stats();
        assert_eq!(
            attempts, refused,
            "a reconnect attempt after teardown reached the socket \
             (attempts={attempts}, refused={refused})"
        );
    }

    /// A provider clone may outlive the fork handle and must not be able to
    /// restart the pubsub service against the dead socket: a call on the clone
    /// fails instead of waiting out a reconnect budget.
    #[tokio::test]
    async fn an_outstanding_provider_clone_fails_instead_of_reconnecting() {
        let fork = spawn_fork().await;
        let gate = fork.transport.gate();
        let leaked = fork.provider().clone();
        assert!(
            fork.provider().get_block_number().await.is_ok(),
            "the session's own provider must be live before teardown"
        );

        drop(fork);

        let outcome = tokio::time::timeout(Duration::from_secs(5), leaked.get_block_number()).await;
        match outcome {
            Ok(Err(_)) => {}
            other => panic!(
                "a leaked provider clone must fail promptly, not reconnect into the dead \
                 socket: {other:?}"
            ),
        }
        let (attempts, refused) = gate.stats();
        assert_eq!(
            attempts, refused,
            "a leaked clone woke the reconnect path (attempts={attempts}, refused={refused})"
        );
    }

    /// Closing the session is transport-only: the child keeps running until the
    /// fork drops it, which is what lets the pubsub service take its clean exit
    /// while the socket is still connected.
    #[tokio::test]
    async fn closing_the_session_leaves_the_child_running() {
        let mut fork = spawn_fork().await;
        let ipc = fork.ipc_path().to_string();
        let port = fork.port();

        fork.transport.close();

        assert!(
            std::net::TcpStream::connect(("127.0.0.1", port)).is_ok(),
            "closing the transport must not stop the child"
        );
        assert!(
            std::fs::metadata(&ipc).is_ok(),
            "the socket file belongs to the fork, not the transport"
        );
        assert!(fork.transport.provider().is_none());

        drop(fork);
        assert!(std::fs::metadata(&ipc).is_err());
        assert!(child_stopped(port));
    }

    /// Teardown is one operation, not a schedule: closing twice repeats
    /// nothing, and the drop that follows finds an already-closed session.
    #[tokio::test]
    async fn session_close_is_idempotent() {
        let mut fork = spawn_fork().await;
        let gate = fork.transport.gate();

        fork.transport.close();
        fork.transport.close();

        assert!(gate.is_closed());
        assert!(fork.transport.provider().is_none());
        drop(fork);
    }

    // -- Spawn and readiness failure classification ----------------------
    //
    // A missing binary, a dead child, and an expired readiness deadline are
    // three unrelated operator problems behind one spawn call, and each maps
    // to its own variant: the mapping is proven directly on the upstream
    // `NodeError` it classifies, and end-to-end against scripted children
    // spawned through the node-binding's `program` capability (no external
    // RPC, no dependence on a real anvil's startup timing).

    use std::time::Instant;

    use alloy::node_bindings::NodeError;

    const FORK_URL: &str = "https://hosted.example/v1/key";

    fn upstream_target() -> ForkTarget {
        ForkTarget::Upstream(FORK_URL.to_string())
    }

    #[test]
    fn process_spawn_failure_is_a_typed_spawn_not_a_timeout() {
        let err = ForkError::spawn_failure(
            NodeError::SpawnError(std::io::Error::from(std::io::ErrorKind::NotFound)),
            upstream_target(),
            AnvilForkBuilder::FORK_SPAWN_TIMEOUT_MS,
        );

        let ForkError::Spawn {
            fork_target,
            source,
        } = &err
        else {
            panic!("a spawn failure must stay ForkError::Spawn, got {err:?}");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(fork_target, &upstream_target());

        let rendered = err.to_string();
        assert!(rendered.contains("anvil spawn failed"), "{rendered}");
        assert!(rendered.contains(FORK_URL), "{rendered}");
        assert!(!rendered.contains("timed out"), "{rendered}");
    }

    #[test]
    fn child_exit_before_readiness_is_neither_missing_binary_nor_timeout() {
        let err = ForkError::spawn_failure(
            NodeError::Fatal("anvil exited before it was ready (exit status: 1)".to_string()),
            upstream_target(),
            AnvilForkBuilder::FORK_SPAWN_TIMEOUT_MS,
        );

        let ForkError::ChildExit {
            fork_target,
            detail,
        } = &err
        else {
            panic!("a dead child must map to ForkError::ChildExit, got {err:?}");
        };
        assert_eq!(fork_target, &upstream_target());
        assert!(detail.contains("exit status: 1"), "{detail}");

        let rendered = err.to_string();
        assert!(
            rendered.contains("exited before it was ready"),
            "{rendered}"
        );
        assert!(rendered.contains("stderr"), "{rendered}");
        assert!(!rendered.contains("timed out"), "{rendered}");
        assert!(!rendered.contains("binary installed"), "{rendered}");
    }

    #[test]
    fn readiness_deadline_is_a_typed_spawn_timeout() {
        let err = ForkError::spawn_failure(
            NodeError::Timeout,
            upstream_target(),
            AnvilForkBuilder::FORK_SPAWN_TIMEOUT_MS,
        );

        let ForkError::SpawnTimeout {
            fork_target,
            deadline_ms,
        } = &err
        else {
            panic!("the readiness deadline must map to ForkError::SpawnTimeout, got {err:?}");
        };
        assert_eq!(fork_target, &upstream_target());
        assert_eq!(*deadline_ms, AnvilForkBuilder::FORK_SPAWN_TIMEOUT_MS);
        assert!(err.to_string().contains("180000ms"), "{err}");
    }

    #[test]
    fn readiness_read_failure_keeps_the_alloy_source() {
        let err = ForkError::spawn_failure(
            NodeError::ReadLineError(std::io::Error::other("pipe closed mid-line")),
            ForkTarget::InMemory,
            u64::try_from(alloy::node_bindings::NODE_STARTUP_TIMEOUT.as_millis())
                .expect("alloy's default startup deadline fits in u64"),
        );

        let ForkError::Readiness {
            fork_target,
            source,
        } = &err
        else {
            panic!("a readiness I/O failure must map to ForkError::Readiness, got {err:?}");
        };
        assert_eq!(fork_target, &ForkTarget::InMemory);
        assert!(matches!(source, NodeError::ReadLineError(_)), "{source:?}");
        assert!(std::error::Error::source(&err).is_some(), "{err:?}");
        assert!(err.to_string().contains("pipe closed mid-line"), "{err}");
    }

    #[test]
    fn readiness_deadline_is_bounded_per_spawn_shape() {
        assert_eq!(
            AnvilForkBuilder::new().readiness_deadline_ms(),
            u64::try_from(alloy::node_bindings::NODE_STARTUP_TIMEOUT.as_millis())
                .expect("alloy's default startup deadline fits in u64"),
            "an in-memory spawn keeps the node-binding's own startup deadline"
        );
        assert_eq!(
            AnvilForkBuilder::new()
                .fork_url(FORK_URL)
                .readiness_deadline_ms(),
            AnvilForkBuilder::FORK_SPAWN_TIMEOUT_MS,
            "a forked spawn widens the deadline to the upstream-latency budget"
        );
        // The lane's per-test budget: a deadline above it would be the test's
        // own timeout rather than a fork-spawn verdict.
        const { assert!(AnvilForkBuilder::FORK_SPAWN_TIMEOUT_MS < 300_000) };
    }

    /// Write an executable `#!/bin/sh` program standing in for the node binary.
    ///
    /// The body is written by a CHILD, not by this process. `execve` refuses a
    /// program any process holds open for writing (`ETXTBSY`), and a sibling
    /// test's concurrent `fork` captures this thread's in-flight descriptor
    /// until its own exec — a window no write-then-exec inside this process can
    /// close. A descriptor the child owns dies with the child, which is reaped
    /// here before the script is ever spawned.
    fn scripted_node(body: &str) -> std::path::PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "degenbot-fork-scripted-node-{}-{n}.sh",
            std::process::id()
        ));

        let mut writer = Command::new("sh")
            .arg("-c")
            .arg("cat > \"$1\"")
            .arg("sh")
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn()
            .expect("start the scripted node's writer");
        writer
            .stdin
            .take()
            .expect("the writer's stdin is piped")
            .write_all(body.as_bytes())
            .expect("hand the script body to the writer");
        assert!(
            writer.wait().expect("reap the writer").success(),
            "the writer must land the script at {}",
            path.display()
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make scripted node executable");
        path
    }

    #[tokio::test]
    async fn missing_node_binary_fails_without_waiting_for_the_deadline() {
        let missing = std::env::temp_dir().join("degenbot-fork-absent-anvil-binary");
        let _ = std::fs::remove_file(&missing);

        let started = Instant::now();
        let Err(err) = AnvilForkBuilder::new()
            .ipc_path(test_ipc_path())
            .anvil_program(&missing)
            .try_spawn()
            .await
        else {
            panic!("a missing node binary cannot spawn");
        };
        let elapsed = started.elapsed();

        let ForkError::Spawn { source, .. } = &err else {
            panic!("a missing node binary must map to ForkError::Spawn, got {err:?}");
        };
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        assert!(
            elapsed < Duration::from_secs(1),
            "nothing ran, so nothing should be waited for (took {elapsed:?})"
        );
    }

    #[tokio::test]
    async fn child_exiting_on_a_refused_genesis_fetch_is_a_child_exit() {
        // A forked child whose upstream refuses the genesis fetch (HTTP 429)
        // exits at once. The binding sees stdout EOF and reports the child's
        // exit status, while the child's own reason goes to the stderr it
        // inherits — so the classification must not depend on reading that
        // sentence.
        let script = scripted_node(
            "#!/bin/sh\n\
             echo 'Error: server returned an error response: error code 429' >&2\n\
             exit 1\n",
        );

        let started = Instant::now();
        let Err(err) = AnvilForkBuilder::new()
            .ipc_path(test_ipc_path())
            // The forked shape: this child owns the full fork deadline.
            .fork_url(FORK_URL)
            .anvil_program(&script)
            .try_spawn()
            .await
        else {
            panic!("a child that exits during startup cannot spawn");
        };
        let elapsed = started.elapsed();
        let _ = std::fs::remove_file(&script);

        let ForkError::ChildExit {
            fork_target,
            detail,
        } = &err
        else {
            panic!("a dead child must map to ForkError::ChildExit, got {err:?}");
        };
        assert_eq!(fork_target, &upstream_target());
        assert!(detail.contains("exit status"), "{detail}");
        assert!(
            elapsed < Duration::from_secs(10),
            "a dead child is reported when it dies, not at the {}-second fork deadline \
             (took {elapsed:?})",
            AnvilForkBuilder::FORK_SPAWN_TIMEOUT_MS / 1000
        );
        let rendered = err.to_string();
        assert!(rendered.contains("stderr"), "{rendered}");
        assert!(!rendered.contains("timed out"), "{rendered}");
    }

    #[tokio::test]
    async fn child_that_never_reports_readiness_hits_the_deadline() {
        // The child must keep emitting complete stdout lines: the binding
        // checks its deadline between lines, so a child that prints nothing
        // blocks the wait instead of letting the deadline fire.
        let script = scripted_node("#!/bin/sh\nwhile true; do echo .; sleep 0.05; done\n");

        let started = Instant::now();
        let Err(err) = AnvilForkBuilder::new()
            .ipc_path(test_ipc_path())
            .anvil_program(&script)
            .spawn_deadline_ms(1_000)
            .try_spawn()
            .await
        else {
            panic!("a child that never reports readiness cannot spawn");
        };
        let elapsed = started.elapsed();
        let _ = std::fs::remove_file(&script);

        let ForkError::SpawnTimeout { deadline_ms, .. } = &err else {
            panic!("the readiness deadline must map to ForkError::SpawnTimeout, got {err:?}");
        };
        assert_eq!(*deadline_ms, 1_000);
        assert!(
            elapsed < Duration::from_secs(10),
            "the deadline must bound the wait (took {elapsed:?})"
        );
    }
}
