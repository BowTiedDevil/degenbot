//! `WsIngestor` — the transport handle: connect, subscribe + handshake,
//! fetch backfill ranges.

use degenbot_core::{op_error, op_info, op_warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::rpc::types::{Filter, Log};
use degenbot_core::errors::ProviderResult;
use degenbot_rpc::provider::AlloyProvider;
use futures_util::{stream, Stream, StreamExt};
use tokio::time::timeout;

use crate::events::IngestEvent;
use crate::events::PoolEvent;
use crate::topics::{is_relevant_log, RELEVANT_TOPICS};

/// How long to wait with no activity before assuming the connection is dead
/// (the runtime's settle window AND the handshake's degraded-path deadline).
pub const BACKFILL_TIMEOUT_SECS: u64 = 60;

/// Default window (seconds) for the subscribe handshake's log-catchup settle:
/// after the head is header-confirmed, wait (bounded) for the logs stream to
/// deliver its first log before falling back to the header-confirmed
/// boundary. Bounds startup latency on a quiet/log-free chain while still
/// capturing the log stream's true live-from block on active ones (DFQYM5).
pub const LOG_CATCHUP_SETTLE_SECS: u64 = 15;

/// Default chunk size (blocks per `eth_getLogs` request) for the
/// snapshot→WS gap backfill. Mirrors the historical
/// `backfill_from_snapshot` default (`chunk_size` = 2000): the per-chunk
/// response size stays under `eth_getLogs` payload caps.
pub const DEFAULT_BACKFILL_CHUNK_SIZE: u64 = 2000;

/// The handshake result: boundary block + its timestamp + the fused stream
/// (re-injected with any logs the handshake consumed — MJXP5Z, one WS, one
/// handoff, a structurally lost log is impossible).
pub struct SubscribeBoundary {
    /// The first COMPLETE block observed (header + log) or the
    /// header-confirmed fallback — the backfill boundary `W`.
    pub first_block: u64,
    /// Block timestamp from the last confirmed header.
    pub first_timestamp: u64,
    /// The live merged stream (`pending` re-injected at its head).
    pub stream: stream::BoxStream<'static, IngestEvent>,
}

/// The WS transport handle. One per chain: owns the provider, the live merged
/// stream, and the backfill fetch path. Knows nothing about pool state, the
/// stage machine, or Python — the runtime consumes [`IngestEvent`]s.
#[derive(Clone)]
pub struct WsIngestor {
    provider: std::sync::Arc<AlloyProvider>,
}

impl WsIngestor {
    /// Connect an `AlloyProvider` (the same construction the `block_pump` used
    /// pre-extraction: 3 retries).
    ///
    /// # Errors
    /// Provider construction/WS connect failure (message includes the cause).
    pub async fn connect(rpc_url: &str) -> Result<Self, String> {
        let provider = AlloyProvider::new(rpc_url, 3)
            .await
            .map_err(|e| format!("WsIngestor: failed to create provider: {e}"))?;
        Ok(Self::with_provider(std::sync::Arc::new(provider)))
    }

    /// Wrap an existing provider (the test seam + standalone consumers that
    /// already own a transport).
    #[must_use]
    pub fn with_provider(provider: std::sync::Arc<AlloyProvider>) -> Self {
        Self { provider }
    }

    /// Subscribe to block headers + UNFILTERED logs and merge them into one
    /// [`IngestEvent`] stream (fair interleave; all filtering happens in
    /// Rust).
    ///
    /// # Errors
    /// WS subscription failure for either arm (message includes the cause).
    pub async fn subscribe_events(
        &self,
    ) -> Result<stream::BoxStream<'static, IngestEvent>, String> {
        let provider_arc = self.provider.provider_arc();

        // Subscribe to block headers
        let block_stream = provider_arc
            .subscribe_blocks()
            .await
            .map_err(|e| format!("WsIngestor: failed to subscribe to blocks: {e}"))?
            .into_stream();

        // Subscribe to logs — unfiltered. All filtering happens in Rust.
        let log_filter = Filter::new();
        let log_stream = provider_arc
            .subscribe_logs(&log_filter)
            .await
            .map_err(|e| format!("WsIngestor: failed to subscribe to logs: {e}"))?
            .into_stream();

        Ok(stream_select(block_stream, log_stream))
    }

    /// MJXP5Z single-stream handshake + delivery-hole-free boundary (DFQYM5).
    /// Connects, merges both subscriptions, confirms the boundary from the
    /// LOG STREAM's actual liveness (two consecutive headers + first-delivered
    /// log, header fallback past the settle window), and returns the boundary
    /// + the SAME stream with any handshake-consumed logs re-injected.
    ///
    /// # Errors
    /// Provider connect + subscribe failures.
    pub async fn subscribe_with_handshake(
        &self,
        shutdown: std::sync::Arc<AtomicBool>,
    ) -> Result<SubscribeBoundary, String> {
        let combined = self.subscribe_events().await?;
        Ok(self.handshake(shutdown, combined).await)
    }

    /// Run the handshake against a caller-owned merged stream (pure-Rust
    /// consumers that already hold a live fused stream — the test seam).
    /// Same MJXP5Z/DFQYM5 contract as [`Self::subscribe_with_handshake`]
    /// minus the connect.
    #[must_use]
    pub async fn handshake(
        &self,
        shutdown: std::sync::Arc<AtomicBool>,
        mut combined: stream::BoxStream<'static, IngestEvent>,
    ) -> SubscribeBoundary {
        let (first_block, first_timestamp, pending) =
            self.observe_complete_block(&shutdown, &mut combined).await;
        // Re-inject any logs the handshake consumed while polling for
        // headers (preserving arrival order).
        let combined = if pending.is_empty() {
            combined
        } else {
            stream::iter(pending).chain(combined).boxed()
        };
        SubscribeBoundary {
            first_block,
            first_timestamp,
            stream: combined,
        }
    }

    /// The authoritative head (the degraded handshake path + the runtime's
    /// timeout catch-up both poll `eth_blockNumber`).
    ///
    /// # Errors
    /// Provider error (transport / rate-limit).
    pub async fn latest_block(&self) -> ProviderResult<u64> {
        self.provider.get_block_number().await
    }

    /// Fetch the relevant-topic logs of `[from, to]` INCLUSIVE via
    /// `eth_getLogs` (server-side topic[0] OR-list — the
    /// [`crate::build_backfill_filter`] semantics, through the provider's
    /// retry/backoff path).
    ///
    /// # Errors
    /// Provider error (transport / rate-limit / payload cap).
    pub async fn fetch_logs(&self, from: u64, to: u64) -> ProviderResult<Vec<Log>> {
        let topic0: Vec<String> = RELEVANT_TOPICS.iter().map(|t| format!("{t:#x}")).collect();
        let filter = degenbot_rpc::provider::LogFilter::new(from, to, None, Some(vec![topic0]))?;
        self.provider.get_logs(&filter).await
    }

    /// Handshake (MJXP5Z / Alternative B) that confirms the boundary from the
    /// LOG STREAM's actual liveness, not headers alone (DFQYM5). Polls the
    /// fused stream until (a) two consecutive distinct headers confirm the
    /// head is near/finalized AND (b) the `logs` sub has delivered at least
    /// one log — the block of that first log (`first_log_block`) is where the
    /// log stream is PROVABLY live. The boundary `W` returned is
    /// `first_log_block` (falls back to the header-confirmed head if the log
    /// stream stays silent past [`LOG_CATCHUP_SETTLE_SECS`]).
    #[expect(clippy::too_many_lines)] // ADR-043 domain-target args widen the emit calls
    async fn observe_complete_block(
        &self,
        shutdown: &AtomicBool,
        combined: &mut stream::BoxStream<'static, IngestEvent>,
    ) -> (u64, u64, Vec<IngestEvent>) {
        let mut prev_header: Option<u64> = None;
        let mut prev_timestamp: u64 = 0;
        // The block of the FIRST log the WS `logs` sub delivers — the earliest
        // proof the log stream is provably LIVE. The resume boundary + backfill
        // inclusive target = this block (DFQYM5).
        let mut first_log_block: Option<u64> = None;
        // The highest header-confirmed-finalized block (two consecutive
        // headers). Advances as headers flow.
        let mut confirmed_head: Option<u64> = None;
        // Deadline to keep waiting for the log stream's first log after the
        // head is header-confirmed.
        let mut settle_deadline: Option<tokio::time::Instant> = None;
        let mut pending: Vec<IngestEvent> = Vec::new();

        loop {
            if shutdown.load(Ordering::Relaxed) {
                op_info!(
                    domain = ingest,
                    "WsIngestor: shutting down during subscribe phase"
                );
                return (0, 0, pending);
            }

            let event = timeout(Duration::from_secs(BACKFILL_TIMEOUT_SECS), combined.next()).await;

            match event {
                Err(_) => {
                    // Timeout — fall back to eth_blockNumber RPC (degraded path).
                    op_warn!(
                        domain = ingest,
                        "WsIngestor: timeout during subscribe, fetching current block"
                    );
                    match self.provider.get_block_number().await {
                        Ok(block) => {
                            op_info!(domain = ingest, block,
                                "WsIngestor: subscribe observed block via RPC (degraded - no two-header confirmation)"
                            );
                            return (block, 0, pending);
                        }
                        Err(e) => {
                            op_error!(domain = ingest, %e, "WsIngestor: can't get block number during subscribe");
                        }
                    }
                }

                Ok(Some(IngestEvent::BlockHeader {
                    number,
                    timestamp,
                    base_fee_per_gas: _,
                    gas_used: _,
                    gas_limit: _,
                })) => {
                    if let Some(prev) = prev_header {
                        if number == prev + 1 {
                            // Two consecutive headers: `prev` confirmed
                            // finalized. Advance the confirmed head (and arm
                            // the log-catch-up settle deadline on first
                            // confirmation).
                            if confirmed_head.is_none() {
                                settle_deadline = Some(
                                    tokio::time::Instant::now()
                                        + Duration::from_secs(LOG_CATCHUP_SETTLE_SECS),
                                );
                            }
                            confirmed_head = Some(prev);
                            prev_timestamp = timestamp;
                            op_info!(
                                domain = ingest,
                                prev,
                                number,
                                "WsIngestor: subscribe confirmed head at {prev} (header {number})"
                            );
                        } else if number > prev {
                            // Gap or jump - re-anchor on the newer header.
                            prev_header = Some(number);
                            prev_timestamp = timestamp;
                        }
                        // else: duplicate/stale header for the same block - ignore.
                    } else {
                        // First header ever observed.
                        prev_header = Some(number);
                        prev_timestamp = timestamp;
                    }
                }

                Ok(Some(IngestEvent::Pool(pe))) => {
                    if first_log_block.is_none() {
                        if let Some(lb) = pe.payload.block_number {
                            first_log_block = Some(lb);
                        }
                    }
                    // Collect every log observed during the handshake; the
                    // handshake never touches the data plane (some may be for
                    // the boundary block and are already backfilled).
                    pending.push(IngestEvent::Pool(pe));
                }

                Ok(None) => {
                    op_warn!(
                        domain = ingest,
                        "WsIngestor: subscription streams ended during subscribe"
                    );
                    return (prev_header.unwrap_or(0), prev_timestamp, pending);
                }
            }

            // Finalize once we're near the head (headers confirmed) AND we know
            // the log stream's live-from block — or the settle window elapsed.
            if let Some(head) = confirmed_head {
                let deadline_passed =
                    settle_deadline.is_some_and(|d| tokio::time::Instant::now() >= d);
                let boundary_ok = match first_log_block {
                    // Boundary (first_log_block) is finalizable once the
                    // confirmed head reaches it; accept past the deadline.
                    Some(l) => l <= head || deadline_passed,
                    None => deadline_passed,
                };
                if boundary_ok {
                    let boundary = first_log_block.unwrap_or(head);
                    op_info!(
                        domain = ingest,
                        confirmed_head = head,
                        boundary,
                        source = if first_log_block.is_some() {
                            "first-delivered-log"
                        } else {
                            "header-fallback"
                        },
                        "WsIngestor: subscribe boundary set to {boundary}"
                    );
                    return (boundary, prev_timestamp, pending);
                }
            }
        }
    }
}

/// Fetch the exact-topic relevant logs' indices for a single block — the
/// client-side exact-topic pre-filter the runtime's WS-completeness check
/// needs (`build_backfill_filter`'s server-side OR-list over-matches on some
/// nodes, inflating the "missing" set into FALSE drop positives).
#[must_use]
pub fn exact_relevant_indices(logs: &[Log]) -> std::collections::HashSet<u64> {
    logs.iter()
        .filter(|l| is_relevant_log(l))
        .filter_map(|l| l.log_index)
        .collect()
}

/// Merge a block header stream and a log stream into a single
/// [`IngestEvent`] stream.
///
/// Uses `stream::Select` to fairly interleave events from both subscriptions.
/// Returns a boxed stream for storage in the subscribe state.
#[must_use]
pub fn stream_select(
    block_stream: impl Stream<Item = alloy::rpc::types::Header> + Unpin + Send + 'static,
    log_stream: impl Stream<Item = Log> + Unpin + Send + 'static,
) -> stream::BoxStream<'static, IngestEvent> {
    let block_events = block_stream.map(|header| IngestEvent::BlockHeader {
        number: header.number,
        timestamp: header.timestamp,
        base_fee_per_gas: header.base_fee_per_gas,
        gas_used: header.gas_used,
        gas_limit: header.gas_limit,
    });
    let log_events = log_stream.map(|log| IngestEvent::Pool(PoolEvent::from_log(log)));

    stream::select(block_events, log_events).boxed()
}

#[cfg(test)]
mod handshake_tests {
    //! MJXP5Z/DFQYM5 handshake tests (migrated from the `block_pump` with the
    //! transport they exercise — 5WTYYQ). Offline: a mock transport under the
    //! ingestor, synthetic fused streams. The handshake polls headers /
    //! collects logs and never touches the data plane.

    use super::*;
    use alloy::primitives::{Address, Bytes, U256};
    use alloy::rpc::types::Log;
    use degenbot_decoders::v3_mint_burn_decoder::{V3_BURN_TOPIC, V3_MINT_TOPIC};
    use degenbot_rpc::provider::AlloyProvider;
    use std::sync::Arc;

    /// An ingestor over a mock transport (never hit on the no-timeout
    /// handshake paths).
    fn ingestor_for_test() -> (WsIngestor, Arc<AtomicBool>) {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::{Asserter, MockTransport};

        let asserter = Asserter::new();
        let client = ClientBuilder::default().transport(MockTransport::new(asserter), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        let provider = Arc::new(AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
        ));
        let ingestor = WsIngestor::with_provider(provider);
        (ingestor, Arc::new(AtomicBool::new(false)))
    }

    fn header(number: u64) -> IngestEvent {
        IngestEvent::BlockHeader {
            number,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        }
    }

    /// A raw V3 `Mint` log with `block_number` set (topics = [MINT, owner,
    /// tickLower, tickUpper]; 128-byte data matching the ABI word shape).
    fn make_v3_mint_log_with_block(
        pool: Address,
        tick_lower: i32,
        tick_upper: i32,
        block: u64,
    ) -> Log {
        use alloy::primitives::{I256, U128};
        let tick_to_topic = |tick: i32| {
            let i = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
            alloy::primitives::B256::from(i.to_be_bytes::<32>())
        };
        let owner = Address::from([0xccu8; 20]);
        let sender = Address::from([0xddu8; 20]);
        let mut amount_word = [0u8; 32];
        amount_word[16..32].copy_from_slice(&U128::from(1u128).to_be_bytes::<16>());
        let mut data = Vec::with_capacity(128);
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(sender.as_slice());
        data.extend_from_slice(&amount_word);
        data.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
        data.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![
                V3_MINT_TOPIC,
                owner.into_word(),
                tick_to_topic(tick_lower),
                tick_to_topic(tick_upper),
            ],
            Bytes::from(data),
        );
        Log {
            inner,
            block_hash: None,
            block_number: Some(block),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    /// A raw V3 `Burn` log with `block_number` set (96-byte data :=
    /// abi.encode(uint128,uint256,uint256)).
    fn make_v3_burn_log_with_block(
        pool: Address,
        tick_lower: i32,
        tick_upper: i32,
        block: u64,
    ) -> Log {
        use alloy::primitives::{I256, U128};
        let tick_to_topic = |tick: i32| {
            let i = I256::try_from(i128::from(tick)).unwrap_or(I256::ZERO);
            alloy::primitives::B256::from(i.to_be_bytes::<32>())
        };
        let owner = Address::from([0xccu8; 20]);
        let mut amount_word = [0u8; 32];
        amount_word[16..32].copy_from_slice(&U128::from(1u128).to_be_bytes::<16>());
        let mut data = Vec::with_capacity(96);
        data.extend_from_slice(&amount_word);
        data.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
        data.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());
        let inner = alloy::primitives::Log::new_unchecked(
            pool,
            vec![
                V3_BURN_TOPIC,
                owner.into_word(),
                tick_to_topic(tick_lower),
                tick_to_topic(tick_upper),
            ],
            Bytes::from(data),
        );
        Log {
            inner,
            block_hash: None,
            block_number: Some(block),
            block_timestamp: None,
            transaction_hash: None,
            transaction_index: None,
            log_index: None,
            removed: false,
        }
    }

    /// MJXP5Z (GREEN): the single-stream handshake does NOT drop block-W
    /// logs. The handshake polls headers ONLY (two consecutive headers W,
    /// W+1 confirm the boundary), collecting any `IngestEvent::Pool` the
    /// fused stream interleaves and re-injecting it. With the OLD
    /// drop+resubscribe, the Mint/Burn queued after the confirming log were
    /// lost (XBQNJ5 RED). Under Alternative B they survive in `pending` and
    /// reach the resume stream.
    #[tokio::test]
    async fn handshake_preserves_w_logs() {
        let (ingestor, shutdown) = ingestor_for_test();
        let w = 21_500_000u64;
        let pool = Address::from([0xaau8; 20]);

        let combined = stream::iter(vec![
            header(w),
            IngestEvent::Pool(PoolEvent::from_log(make_v3_mint_log_with_block(
                pool, -100, 100, w,
            ))),
            IngestEvent::Pool(PoolEvent::from_log(make_v3_burn_log_with_block(
                pool, -100, 100, w,
            ))),
            header(w + 1),
        ])
        .boxed();

        let boundary = ingestor.handshake(shutdown, combined).await;
        assert_eq!(
            boundary.first_block, w,
            "handshake must anchor on block W (confirmed by W+1)"
        );

        let mut got_mint = false;
        let mut got_burn = false;
        let mut stream = boundary.stream;
        while let Some(ev) = stream.next().await {
            if let IngestEvent::Pool(pe) = ev {
                match pe.payload.topics().first().copied() {
                    Some(t) if t == V3_MINT_TOPIC => got_mint = true,
                    Some(t) if t == V3_BURN_TOPIC => got_burn = true,
                    _ => {}
                }
            }
        }
        assert!(
            got_mint,
            "block-W Mint MUST survive the handshake (Alternative B re-injects it)"
        );
        assert!(
            got_burn,
            "block-W Burn MUST survive the handshake (Alternative B re-injects it)"
        );
    }

    /// MJXP5Z: the handshake consumes ONLY headers (and collects logs); it
    /// never matches or interprets a log. Both W logs arrive between
    /// header(W) and header(W+1) and must be re-injected into `pending` for
    /// the resume stream.
    #[tokio::test]
    async fn handshake_does_not_consume_logs() {
        let (ingestor, shutdown) = ingestor_for_test();
        let w = 42u64;
        let pool = Address::from([0xbbu8; 20]);

        let combined = stream::iter(vec![
            header(w),
            IngestEvent::Pool(PoolEvent::from_log(make_v3_mint_log_with_block(
                pool, -10, 10, w,
            ))),
            IngestEvent::Pool(PoolEvent::from_log(make_v3_burn_log_with_block(
                pool, -10, 10, w,
            ))),
            header(w + 1),
        ])
        .boxed();

        let boundary = ingestor.handshake(shutdown, combined).await;
        assert_eq!(boundary.first_block, w);

        let mut logs = 0u32;
        let mut stream = boundary.stream;
        while let Some(ev) = stream.next().await {
            if matches!(ev, IngestEvent::Pool(_)) {
                logs += 1;
            }
        }
        assert_eq!(
            logs, 2,
            "both Mint and Burn must be re-injected from pending"
        );
    }
}
