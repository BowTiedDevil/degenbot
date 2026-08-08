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
        let provider: DynProvider = ProviderBuilder::default()
            .connect_ipc(IpcConnect::new(resolved_ipc.clone()))
            .await
            .map_err(|source| ForkError::Connect {
                path: resolved_ipc,
                source,
            })?
            .erased();

        let fork = AnvilFork {
            instance,
            provider: Some(provider),
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
/// a connected alloy [`Provider`] (over IPC). Dropping the handle kills the
/// subprocess + closes the transport, and removes the Unix-domain IPC socket
/// file so `/tmp` doesn't accumulate leftover `.ipc` files.
///
/// Call [`AnvilForkBuilder::try_spawn`] to construct.
pub struct AnvilFork {
    /// The alloy Provider wired to the anvil node over IPC.
    ///
    /// Stored as an `Option` so `Drop` can cleanly shut it down BEFORE the
    /// `instance` kills the spawn anvil subprocess. The IPC provider's
    /// pubsub backend takes the clean alloy "pubsub service request channel
    /// closed" shutdown path only while the socket is still alive; dropping
    /// it *after* the subprocess is killed leaves the backend to enter the
    /// reconnect loop (``WARN alloy_pubsub::service: Reconnection attempt …``)
    /// against the dead socket. See [`Drop`] for the ordering.
    provider: Option<DynProvider>,
    /// Owns the anvil subprocess lifecycle (drop = kill).
    instance: alloy::node_bindings::AnvilInstance,
}

impl Drop for AnvilFork {
    fn drop(&mut self) {
        // Teardown ORDER matters and is intentional:
        //
        // 1. Take (drop) the IPC provider FIRST, while the anvil subprocess
        //    is still alive and the socket fd is still open. Dropping the
        //    pubsub frontend closes the service's request channel, so alloy
        //    exits via the clean "pubsub service request channel closed"
        //    path. (A blocked select! is biased to the `reqs.recv()` branch,
        //    which resolves the instant the frontend is dropped, beating the
        //    socket-close branch because the peer is still up.) This avoids
        //    the reconnect loop that would otherwise log repeated
        //    ``alloy_pubsub::service`` reconnection warnings when the socket
        //    is torn down beneath the open provider.
        self.provider.take();
        // 2. Unlink the IPC socket file (safe while the fd is open), then
        //    drop `instance`, which kills the anvil child process and closes
        //    the transport.
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
}
