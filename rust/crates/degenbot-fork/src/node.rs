//! The `AnvilFork` handle — lifecycle + `Provider` + dev-RPC surface.
//!
//! A Rust-owned fork handle wrapping [`alloy::node_bindings::Anvil`] (subprocess
//! lifecycle) + a connected alloy [`Provider`] (over IPC) + the anvil dev-RPC
//! surface as typed methods delegating to [`alloy::providers::ext::AnvilApi`].
//!
//! Mirrors the public config + dev-method surface of the legacy Python
//! `AnvilFork` (see `src/degenbot/anvil_fork.py`); the Python class becomes a
//! thin `PyO3` shell in task FF4 (`WXRNHH`).
//!
//! [`Provider`]: alloy::providers::Provider

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use alloy::{
    node_bindings::Anvil,
    primitives::{Address, Bytes, U256},
    providers::{ext::AnvilApi, DynProvider, Provider, ProviderBuilder},
    rpc::types::anvil::{Forking, NodeInfo},
    transports::ipc::IpcConnect,
};
use thiserror::Error;

/// Errors raised by [`AnvilFork`] lifecycle + dev-RPC operations.
#[derive(Debug, Error)]
pub enum ForkError {
    /// `anvil` binary not found or failed to spawn.
    #[error("anvil spawn failed: {0}")]
    Spawn(String),

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

impl From<alloy::transports::TransportError> for ForkError {
    fn from(err: alloy::transports::TransportError) -> Self {
        Self::Rpc(err.to_string())
    }
}

/// Mining mode — mirrors the Python `AnvilFork(mining_mode=...)` parameter.
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
/// Mirrors the Python `AnvilFork.__init__` config surface; call
/// [`AnvilForkBuilder::try_spawn`] to spawn the subprocess + connect the
/// Provider + apply any post-spawn state overrides.
#[derive(Debug, Clone)]
pub struct AnvilForkBuilder {
    fork_url: Option<String>,
    fork_block: Option<u64>,
    fork_transaction_hash: Option<String>,
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
}

impl Default for AnvilForkBuilder {
    fn default() -> Self {
        Self {
            fork_url: None,
            fork_block: None,
            fork_transaction_hash: None,
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
        }
    }
}

impl AnvilForkBuilder {
    /// Create a new builder with the default config.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
    /// - [`ForkError::Spawn`] if the `anvil` binary is missing or fails to start.
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
        let instance = anvil
            .try_spawn()
            .map_err(|e| ForkError::Spawn(e.to_string()))?;

        // Use the instance's resolved ipc_path (may differ from the requested
        // default if anvil picked its own).
        let resolved_ipc = instance.ipc_path().to_string();
        let resolved_ipc_display = resolved_ipc.clone();
        // The IPC provider's pubsub service + backend tasks live on a
        // DEDICATED single-worker runtime, not the ambient one: teardown
        // must be able to drive that service deterministically (see
        // `Drop`), and only the fork knows which runtime the service is
        // on.
        let pubsub_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| ForkError::Spawn(e.to_string()))?;
        // Connect INSIDE the dedicated runtime via spawn + oneshot: a
        // nested `block_on` here would panic (tokio forbids blocking from
        // within any async execution environment, `try_spawn` being one).
        let (connect_tx, connect_rx) = tokio::sync::oneshot::channel::<
            Result<DynProvider, alloy::transports::TransportError>,
        >();
        pubsub_runtime.spawn(async move {
            let provider = match ProviderBuilder::default()
                .connect_ipc(IpcConnect::new(resolved_ipc))
                .await
            {
                Ok(p) => Ok(p.erased()),
                Err(source) => Err(source),
            };
            let _ = connect_tx.send(provider);
        });
        let provider = match connect_rx.await {
            Ok(Ok(provider)) => provider,
            Ok(Err(source)) => {
                return Err(ForkError::Connect {
                    path: resolved_ipc_display,
                    source,
                })
            }
            Err(_) => {
                return Err(ForkError::Connect {
                    path: resolved_ipc_display,
                    source: alloy::transports::TransportErrorKind::custom_str(
                        "IPC connect task aborted",
                    ),
                })
            }
        };

        let fork = AnvilFork {
            instance,
            provider: Some(provider),
            pubsub_runtime: Some(pubsub_runtime),
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

    /// Build the `alloy::node_bindings::Anvil` builder from the configured
    /// params (mirrors the Python command construction).
    fn build_anvil(&self, ipc_path: &str) -> Anvil {
        let mut anvil = Anvil::new()
            // Python default args:
            .arg("--auto-impersonate")
            .host(self.host.clone())
            .mnemonic(self.mnemonic.clone())
            .ipc_path(ipc_path.to_string());

        if let Some(port) = self.port {
            anvil = anvil.port(port);
        }
        if let Some(chain_id) = self.chain_id {
            anvil = anvil.arg(format!("--chain-id={chain_id}"));
        }
        if let Some(ref url) = self.fork_url {
            anvil = anvil.fork(url.clone()).arg("--no-rate-limit");
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
/// Owns the spawned `anvil` subprocess (via the embedded `AnvilInstance`) +
/// a connected alloy [`Provider`] (over IPC) + the dedicated runtime that
/// runs the provider's pubsub service/backend tasks. Dropping the handle
/// shuts the pubsub service down deterministically (see [`Drop`]), kills the
/// subprocess, and removes the Unix-domain IPC socket file so `/tmp` doesn't
/// accumulate leftover `.ipc` files.
///
/// Call [`AnvilForkBuilder::try_spawn`] to construct.
pub struct AnvilFork {
    /// The alloy Provider wired to the anvil node over IPC.
    ///
    /// Stored as an `Option` so [`Drop`] can cleanly shut it down while the
    /// anvil subprocess is still alive: dropping the provider drops the
    /// alloy pubsub frontend, which closes the pubsub service's request
    /// channel. The graced teardown (below) then lets the service observe
    /// that closure and exit BEFORE the socket file is unlinked / anvil is
    /// killed — dropping it *after* the subprocess dies leaves the service
    /// to enter the reconnect loop (``WARN alloy_pubsub::service:
    /// Reconnection attempt …``) against the dead socket. Callers must also
    /// not hold `DynProvider` clones (`provider().clone()`) past drop: a
    /// leaked frontend keeps the request channel open and will reconnect
    /// into the dead socket regardless of teardown order.
    provider: Option<DynProvider>,
    /// Dedicated single-worker runtime that owns the IPC provider's alloy
    /// pubsub service + backend tasks (spawned ambient-at-connect, i.e. the
    /// connect inside [`AnvilForkBuilder::try_spawn`] runs on THIS runtime).
    /// Steady state: one parked OS thread (~0 CPU); RPC round-trips and
    /// teardown wake it on demand. [`Drop`] drives it across
    /// `PUBSUB_SHUTDOWN_GRACE` so the pubsub service exits via the clean
    /// "request channel closed" path before the anvil child can die —
    /// deterministically, without depending on which shared-runtime worker
    /// gets to poll a woken task first. Dropping this runtime is
    /// non-blocking, so even a leaked `DynProvider` clone (caller contract
    /// violation) can only keep one parked worker alive until the fork
    /// itself is dropped, at which point any residual reconnect task is
    /// discarded with the runtime.
    ///
    /// Field drop order is load-bearing for teardown: `provider` is
    /// already `None` (taken in the explicit `Drop` body), then
    /// `pubsub_runtime` is torn down BEFORE `instance` kills the anvil
    /// child, so a residual (leaked or starving) pubsub service is
    /// cancelled while the backend is still alive — the backend-death
    /// path (and its reconnect storm) is never observed at all.
    ///
    /// `Option` so [`Drop`] can pull it out and control WHERE it is
    /// dropped: tokio forbids both driving (`block_on`) and dropping a
    /// multi-thread runtime from within an async execution environment,
    /// and a pure-Rust consumer can legitimately drop the fork from a task.
    pubsub_runtime: Option<tokio::runtime::Runtime>,
    /// Owns the anvil subprocess lifecycle (drop = kill).
    instance: alloy::node_bindings::AnvilInstance,
}

/// Short grace the `Drop` impl drives the fork's dedicated transport
/// runtime for, after dropping the IPC provider (see below).
const PUBSUB_SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_millis(2);

impl Drop for AnvilFork {
    fn drop(&mut self) {
        // Teardown ORDER matters and is intentional:
        //
        // 1. Take (drop) the IPC provider FIRST, while the anvil subprocess
        //    is still alive and the socket is still connected. That drops
        //    the pubsub frontend, closes the service's request channel, and
        //    synchronously wakes the pubsub service task on
        //    `pubsub_runtime`.
        self.provider.take();
        // 2. Bounded grace for the service to take the clean exit path.
        //    Step 1 synchronously WOKEN the pubsub service task, parked in
        //    the local queue of the `pubsub_runtime`'s DEDICATED worker —
        //    an OS thread that has nothing else to do. Driving it matters
        //    because the alternative (the old design, service on the SHARED
        //    runtime) made clean shutdown depend on some contended shared
        //    worker getting a CPU slice before anvil could die — a race the
        //    CI flake lost: `WARN alloy_pubsub::service: Reconnection
        //    attempt N/10 failed: No such file or directory …`.
        //
        //    Off an async context (the Python path: CPython refcounting
        //    drops this handle on the main thread), `block_on` the
        //    dedicated runtime across the grace: that single worker polls
        //    the already-READY service task first, then the pending sleep
        //    future. Inside an async execution environment a nested
        //    `block_on` would PANIC — and so would dropping the runtime —
        //    so there we sleep the same bound on this thread (the
        //    dedicated worker is an independent OS thread woken by its
        //    waker) and drop the runtime detached, where blocking (the
        //    worker join) is allowed.
        if let Some(rt) = self.pubsub_runtime.take() {
            if tokio::runtime::Handle::try_current().is_ok() {
                std::thread::sleep(PUBSUB_SHUTDOWN_GRACE);
                std::thread::spawn(move || drop(rt));
            } else {
                rt.block_on(async { tokio::time::sleep(PUBSUB_SHUTDOWN_GRACE).await });
            }
        }
        // 3. Unlink the IPC socket file (safe while the fd is open), then
        //    the field drops kill the anvil child (`instance`) and tear
        //    down the transport runtime (`pubsub_runtime` — a non-blocking
        //    drop that discards any residual reconnect task, bounding the
        //    damage of a leaked provider clone).
        let _ = std::fs::remove_file(self.instance.ipc_path());
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
        self.provider
            .as_ref()
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
        self.instance.ipc_path()
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

    // -- Teardown: pubsub service shutdown observation ---------------------

    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Events (level + formatted message) captured from the alloy pubsub
    /// service, so teardown tests can observe its backend/reconnect decisions.
    static CAPTURED: std::sync::OnceLock<Arc<Mutex<Vec<String>>>> = std::sync::OnceLock::new();

    struct CaptureLayer {
        events: Arc<Mutex<Vec<String>>>,
    }

    /// Extracts the (unrendered) `message` field from an event.
    ///
    /// Tracing stores the format TEMPLATE plus separate field values, so the
    /// message field is e.g. `Reconnection attempt {retry_count}...` — the
    /// literal prefix is what the assertions key on.
    #[derive(Default)]
    struct MsgVisitor(Option<String>);

    impl tracing::field::Visit for MsgVisitor {
        fn record_str(&mut self, field: &tracing_core::field::Field, value: &str) {
            if field.name() == "message" {
                self.0 = Some(value.to_string());
            }
        }

        fn record_debug(
            &mut self,
            field: &tracing_core::field::Field,
            value: &dyn std::fmt::Debug,
        ) {
            if field.name() == "message" {
                self.0 = Some(format!("{value:?}"));
            }
        }
    }

    impl<S> tracing_subscriber::Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let meta = event.metadata();
            if meta.target() != "alloy_pubsub::service" {
                return;
            }
            let mut visitor = MsgVisitor::default();
            event.record(&mut visitor);
            let msg = format!(
                "[{}] {}",
                meta.level(),
                visitor.0.unwrap_or_else(|| "<no message>".into())
            );
            self.events
                .lock()
                .expect("capture mutex poisoned")
                .push(msg);
        }
    }

    fn install_capture_subscriber() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let events = Arc::new(Mutex::new(Vec::new()));
            CAPTURED
                .set(events.clone())
                .expect("capture state set twice");
            let _ = tracing_subscriber::registry()
                .with(CaptureLayer { events })
                .try_init();
        });
    }

    fn captured_events() -> Vec<String> {
        CAPTURED
            .get()
            .map(|c| c.lock().expect("capture mutex poisoned").clone())
            .unwrap_or_default()
    }

    fn clear_captured_events() {
        if let Some(c) = CAPTURED.get() {
            c.lock().expect("capture mutex poisoned").clear();
        }
    }

    /// The teardown tests share one process-wide tracing subscriber + capture
    /// vec (global `try_init` wins once), and the test harness runs tests on
    /// parallel threads — their clear/observe windows would erase each
    /// other's events (seen as `captured: []` flakes). Serialize the whole
    /// capture window per test. A `tokio` mutex (not `std`) because the
    /// guard is held across `await`s.
    static TEARDOWN_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    async fn teardown_lock() -> tokio::sync::MutexGuard<'static, ()> {
        TEARDOWN_TEST_LOCK.lock().await
    }

    /// A caller contract violation — holding an `Arc` clone of the IPC
    /// provider across fork teardown — must NOT produce a reconnection
    /// storm either: the pubsub service lives on the fork's dedicated
    /// transport runtime, so it is discarded with the fork (a short
    /// backend-death window can at most log a single event pair before the
    /// runtime drop). RPCs issued on the leaked clone thereafter simply
    /// hang (bounded by the caller's own call timeout).
    ///
    /// Pre-fix, both a lost teardown race AND a leaked clone left a 10x
    /// 3s/6s/12s/30s... `Reconnection attempt N/10 failed: No such file or
    /// directory ...` storm on the SHARED runtime for ~2 minutes — the
    /// warning observed at the tail of CI `just test-python` runs.
    #[tokio::test]
    async fn leaked_provider_clone_emits_no_reconnect_storm() {
        ensure_anvil_available();
        let _guard = teardown_lock().await;
        install_capture_subscriber();
        let ipc = test_ipc_path();
        let _ = std::fs::remove_file(&ipc);

        let fork = AnvilForkBuilder::new()
            .ipc_path(&ipc)
            .try_spawn()
            .await
            .expect("spawn");
        let leaked = fork.provider().clone();
        clear_captured_events();
        drop(fork); // kills anvil + unlinks the socket + drops the runtime

        tokio::time::sleep(Duration::from_millis(1500)).await;
        drop(leaked);

        let events = captured_events();
        let bad: Vec<String> = events
            .iter()
            .filter(|e| {
                e.contains("Reconnection attempt") || e.contains("Pubsub service backend error")
            })
            .cloned()
            .collect();
        assert!(
            bad.is_empty(),
            "leaked provider clone left a pubsub reconnect storm: {bad:?}"
        );
    }

    /// Normal teardown (no external provider clones) must NOT enter the
    /// reconnect path: dropping the fork closes the pubsub service's
    /// request channel while anvil is still up, and `Drop` drives the
    /// fork's dedicated transport runtime so the service exits via the
    /// clean "request channel closed" path before the anvil child can die.
    ///
    /// A lost teardown race (the pre-fix CI flake) would log
    /// `Reconnection attempt 1/10 failed: No such file or directory ...`
    /// almost immediately — the first attempt precedes any backoff — so a
    /// short observation window suffices. The positive assertion (clean
    /// shutdown event observed) additionally proves the service actually
    /// ran its exit path rather than merely being unscheduled.
    ///
    /// The drop happens on a bare std thread, mirroring the Python process
    /// (`CPython` reference counting drops the `PyAnvilFork` handle on the
    /// main thread, never on a runtime worker).
    #[tokio::test]
    async fn drop_shuts_down_pubsub_cleanly() {
        ensure_anvil_available();
        let _guard = teardown_lock().await;
        install_capture_subscriber();
        let ipc = test_ipc_path();
        let _ = std::fs::remove_file(&ipc);

        let fork = AnvilForkBuilder::new()
            .ipc_path(&ipc)
            .try_spawn()
            .await
            .expect("spawn");
        clear_captured_events();
        // Drop on a bare thread, mirroring the Python main-thread drop.
        std::thread::spawn(move || drop(fork))
            .join()
            .expect("drop thread");

        tokio::time::sleep(Duration::from_millis(1500)).await;

        let events = captured_events();
        let bad: Vec<String> = events
            .iter()
            .filter(|e| {
                e.contains("Reconnection attempt") || e.contains("Pubsub service backend error")
            })
            .cloned()
            .collect();
        assert!(
            bad.is_empty(),
            "fork drop triggered a pubsub reconnect/backend error: {bad:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e.contains("Pubsub service request channel closed")),
            "expected the clean 'request channel closed' shutdown event; captured: {events:?}"
        );
    }
}
