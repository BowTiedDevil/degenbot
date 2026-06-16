//! WebSocket subscriber to Arbitrum RPC.
//!
//! Maintains an `eth_subscribe("logs", filter)` subscription against the
//! configured WS endpoint; reconnects with exponential backoff on
//! disconnect; emits raw `alloy_rpc_types::eth::Log` values onto an internal
//! channel for `sync_event::decode_log` to interpret.

use std::time::Duration;

use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::eth::{Filter, Log};
use alloy_transport_ws::WsConnect;
use eyre::{eyre, Result, WrapErr};
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::warn;

use crate::monitor::sync_event::WATCHED_EVENT_SIGNATURES;

/// Initial reconnect delay; doubles per failure up to [`MAX_BACKOFF`].
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Log filter built from the registered pool addresses.
///
/// Restricts the subscription to the watched pools AND to the pool-state
/// event signatures (UniV2 `Sync`, UniV3/V4 `Swap`), so the node streams
/// only state-bearing logs. `sync_event::decode_log` still dispatches by
/// topic0 as a defensive re-check.
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    pub addresses: Vec<Address>,
}

impl LogFilter {
    fn to_filter(&self) -> Filter {
        Filter::new()
            .address(self.addresses.clone())
            .event_signature(WATCHED_EVENT_SIGNATURES.to_vec())
    }
}

/// WS log subscriber. Holds the endpoint URL and rebuilds the provider on
/// every (re)connect so a dropped socket is transparently recovered.
pub struct Subscriber {
    ws_url: String,
}

impl Subscriber {
    /// Connect to the configured WS endpoint. Performs one connect up front
    /// so a misconfigured URL fails fast at boot.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        ProviderBuilder::new()
            .connect_ws(WsConnect::new(ws_url))
            .await
            .wrap_err("WS provider connect failed")?;
        Ok(Self {
            ws_url: ws_url.to_string(),
        })
    }

    /// Subscribe to the supplied `LogFilter` and forward `Log`s to `tx`.
    /// Reconnects on transport error with exponential backoff; returns
    /// `Ok(())` only when the consumer drops `tx`.
    pub async fn run(self, filter: LogFilter, tx: mpsc::Sender<Log>) -> Result<()> {
        let mut backoff = INITIAL_BACKOFF;
        loop {
            match self.subscribe_once(&filter, &tx).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    warn!(
                        target: "engine::monitor::ws",
                        error = ?err,
                        backoff_ms = backoff.as_millis(),
                        "WS subscription dropped; reconnecting"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// One connect → subscribe → forward cycle. Returns `Ok(())` when `tx`
    /// closes (consumer gone — caller stops); `Err` on any transport fault
    /// (caller reconnects).
    async fn subscribe_once(&self, filter: &LogFilter, tx: &mpsc::Sender<Log>) -> Result<()> {
        let provider = ProviderBuilder::new()
            .connect_ws(WsConnect::new(&self.ws_url))
            .await
            .wrap_err("WS provider connect failed")?;
        let subscription = provider
            .subscribe_logs(&filter.to_filter())
            .await
            .wrap_err("eth_subscribe(logs) failed")?;
        let mut stream = subscription.into_stream();

        while let Some(log) = stream.next().await {
            if tx.send(log).await.is_err() {
                return Ok(());
            }
        }
        Err(eyre!("WS log stream ended"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_filter_carries_its_addresses() {
        let filter = LogFilter {
            addresses: vec![Address::ZERO, Address::from([1u8; 20])],
        };
        // Smoke check: filter construction does not panic and is reusable.
        let _ = filter.to_filter();
        assert_eq!(filter.addresses.len(), 2);
    }

    #[tokio::test]
    async fn connect_smoke_test() {
        // Integration check — runs only when a WS endpoint is configured.
        if std::env::var("RUN_MONITOR_WS_TESTS").is_err() {
            return;
        }
        let Ok(url) = std::env::var("ARB_RPC_WS") else {
            return;
        };
        if url.trim().is_empty() {
            return;
        }
        assert!(
            matches!(
                tokio::time::timeout(Duration::from_secs(5), Subscriber::connect(&url)).await,
                Ok(Ok(_))
            ),
            "WS integration test failed to connect to configured ARB_RPC_WS"
        );
    }
}
