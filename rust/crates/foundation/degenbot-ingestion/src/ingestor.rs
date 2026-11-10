//! `WsIngestor` — the transport handle: connect, subscribe + handshake,
//! fetch backfill ranges.

use degenbot_core::{op_error, op_info, op_warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::rpc::types::{Filter, Log};
use degenbot_core::errors::ProviderResult;
use degenbot_rpc::provider::{AlloyProvider, EthBlock};
use futures_util::{stream, Stream, StreamExt};
use tokio::time::timeout;

use crate::events::IngestEvent;
use crate::events::PoolEvent;
use crate::handshake::{HandshakeCompletion, HandshakeEffect, HandshakeEvent, HandshakeMachine};
use crate::topics::{is_relevant_log, RELEVANT_TOPICS};

/// How long to wait with no activity before assuming the connection is dead
/// (the runtime's settle window AND the handshake's degraded-path deadline).
pub const BACKFILL_TIMEOUT_SECS: u64 = 60;

/// Default window (seconds) for the subscribe handshake's log-catchup settle.
/// Bounds startup latency on a quiet/log-free chain while still capturing
/// the log stream's true live-from block on active ones.
pub const LOG_CATCHUP_SETTLE_SECS: u64 = 15;

/// Default chunk size (blocks per `eth_getLogs` request) for the
/// snapshot→WS gap backfill. Mirrors the historical
/// `backfill_from_snapshot` default (`chunk_size` = 2000): the per-chunk
/// response size stays under `eth_getLogs` payload caps.
pub const DEFAULT_BACKFILL_CHUNK_SIZE: u64 = 2000;

/// The handshake result: boundary block + its timestamp + the fused stream
/// (re-injected with any logs the handshake consumed — one WS, one
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

    /// Single-stream handshake + delivery-hole-free boundary.
    /// Connects, merges both subscriptions, runs the
    /// [`crate::handshake::HandshakeMachine`] to pick the boundary, and
    /// returns it + the SAME stream with any handshake-consumed logs
    /// re-injected.
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
    /// Same contract as [`Self::subscribe_with_handshake`]
    /// minus the connect.
    #[must_use]
    pub async fn handshake(
        &self,
        shutdown: std::sync::Arc<AtomicBool>,
        mut combined: stream::BoxStream<'static, IngestEvent>,
    ) -> SubscribeBoundary {
        self.run_handshake(&shutdown, &mut combined)
            .await
            .into_boundary(combined)
    }

    /// The authoritative head (the degraded handshake path + the runtime's
    /// timeout catch-up both poll `eth_blockNumber`).
    ///
    /// # Errors
    /// Provider error (transport / rate-limit).
    pub async fn latest_block(&self) -> ProviderResult<u64> {
        self.provider.get_block_number().await
    }

    /// Fetch a full block through the same provider that owns the live
    /// subscriptions. The Tauri feed uses this to enrich `newHeads` with the
    /// transaction count, which is not part of an Ethereum block header.
    ///
    /// # Errors
    ///
    /// Provider error (transport / rate-limit).
    pub async fn get_block(&self, block_number: u64) -> ProviderResult<Option<EthBlock>> {
        self.provider.get_block(block_number).await
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

    /// The async adapter around [`crate::handshake::HandshakeMachine`]: stream
    /// polling, the `eth_blockNumber` fallback, the injected clock, and
    /// loop-head shutdown observation. Boundary legality stays in the machine.
    async fn run_handshake(
        &self,
        shutdown: &AtomicBool,
        combined: &mut stream::BoxStream<'static, IngestEvent>,
    ) -> HandshakeCompletion {
        let clock = tokio::time::Instant::now();
        let mut machine = HandshakeMachine::new();
        loop {
            let now_ms = millis(clock.elapsed());
            if shutdown.load(Ordering::Relaxed) {
                op_info!(
                    domain = ingest,
                    "WsIngestor: shutting down during subscribe phase"
                );
                return terminal(machine.handle(now_ms, HandshakeEvent::Shutdown));
            }

            let budget = poll_budget(now_ms, machine.settle_deadline());
            let polled = timeout(budget, combined.next()).await;
            let now_ms = millis(clock.elapsed());
            match polled {
                Ok(Some(event)) => {
                    if let HandshakeEffect::Complete(completion) =
                        machine.handle(now_ms, HandshakeEvent::from(event))
                    {
                        log_boundary(&completion, &machine);
                        return completion;
                    }
                }

                Ok(None) => {
                    op_warn!(
                        domain = ingest,
                        "WsIngestor: subscription streams ended during subscribe"
                    );
                    return terminal(machine.handle(now_ms, HandshakeEvent::StreamEnded));
                }

                Err(_) => {
                    if machine.settle_deadline().is_some_and(|d| now_ms >= d) {
                        return terminal(machine.handle(now_ms, HandshakeEvent::Tick));
                    }
                    // Stream silence: the degraded path asks `eth_blockNumber`.
                    op_warn!(
                        domain = ingest,
                        "WsIngestor: timeout during subscribe, fetching current block"
                    );
                    match machine.handle(now_ms, HandshakeEvent::StreamTimeout) {
                        HandshakeEffect::FetchLatestBlock => {
                            match self.provider.get_block_number().await {
                                Ok(block) => {
                                    op_info!(domain = ingest, block,
                                        "WsIngestor: subscribe observed block via RPC (degraded - no two-header confirmation)"
                                    );
                                    return terminal(machine.handle(
                                        millis(clock.elapsed()),
                                        HandshakeEvent::LatestBlock(block),
                                    ));
                                }
                                Err(e) => {
                                    op_error!(domain = ingest, %e, "WsIngestor: can't get block number during subscribe");
                                    if let HandshakeEffect::Complete(completion) = machine.handle(
                                        millis(clock.elapsed()),
                                        HandshakeEvent::LatestBlockFailed,
                                    ) {
                                        log_boundary(&completion, &machine);
                                        return completion;
                                    }
                                }
                            }
                        }
                        HandshakeEffect::Complete(completion) => {
                            log_boundary(&completion, &machine);
                            return completion;
                        }
                        HandshakeEffect::Continue => {}
                    }
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
        // The identity the header already carries: downstream head consumers
        // (the canonical-head sink) fence reads by the whole anchor, and
        // re-deriving it would cost one header read per head.
        hash: header.hash,
        parent_hash: header.parent_hash,
    });
    let log_events = log_stream.map(|log| IngestEvent::Pool(PoolEvent::from_log(log)));

    stream::select(block_events, log_events).boxed()
}

/// Injected-clock milliseconds measured from the handshake's start.
fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// The stream-poll budget: the silence timeout, cut short by an armed settle
/// deadline so its fallback rules run at expiry rather than at the next
/// stream event.
fn poll_budget(now_ms: u64, settle_deadline: Option<u64>) -> Duration {
    let silence_ms = BACKFILL_TIMEOUT_SECS * 1_000;
    let ms = settle_deadline.map_or(silence_ms, |deadline| {
        deadline.saturating_sub(now_ms).min(silence_ms)
    });
    Duration::from_millis(ms)
}

/// Shutdown, stream end, `eth_blockNumber` success, and a passed deadline
/// each have exactly one transition-table outcome: [`HandshakeEffect::Complete`].
fn terminal(effect: HandshakeEffect) -> HandshakeCompletion {
    match effect {
        HandshakeEffect::Complete(completion) => completion,
        HandshakeEffect::Continue | HandshakeEffect::FetchLatestBlock => {
            unreachable!("terminal handshake events only complete")
        }
    }
}

/// The boundary chosen by the finalize rules. The degraded, stream-end, and
/// shutdown paths log their own outcomes.
fn log_boundary(completion: &HandshakeCompletion, machine: &HandshakeMachine) {
    let boundary = completion.first_block;
    op_info!(
        domain = ingest,
        boundary,
        source = if machine.first_log_block().is_some() {
            "first-delivered-log"
        } else {
            "header-fallback"
        },
        "WsIngestor: subscribe boundary set to {boundary}"
    );
}

#[cfg(test)]
mod handshake_tests {
    //! The public seam: scripted merged streams through
    //! [`WsIngestor::handshake`]. Offline by construction — a mock transport
    //! under the ingestor, synthetic fused streams, and a handshake that never
    //! touches the data plane.

    use super::*;
    use alloy::primitives::{Address, Bytes, B256, U256};
    use alloy::rpc::types::Log;
    use alloy::transports::mock::Asserter;
    use degenbot_decoders::v3_mint_burn_decoder::{V3_BURN_TOPIC, V3_MINT_TOPIC};
    use degenbot_rpc::provider::AlloyProvider;
    use std::sync::Arc;

    /// An ingestor over a mock transport whose [`Asserter`] answers (or fails
    /// to answer) the degraded path's `eth_blockNumber`.
    fn ingestor_for_test() -> (WsIngestor, Arc<AtomicBool>, Asserter) {
        use alloy::network::Ethereum as NetEth;
        use alloy::providers::{Provider, ProviderBuilder};
        use alloy::rpc::client::ClientBuilder;
        use alloy::transports::mock::MockTransport;

        let asserter = Asserter::new();
        let client = ClientBuilder::default().transport(MockTransport::new(asserter.clone()), true);
        let dyn_provider = ProviderBuilder::new().connect_client(client).erased();
        let provider = Arc::new(AlloyProvider::from_provider(
            Arc::new(dyn_provider) as Arc<dyn alloy::providers::Provider<NetEth>>
        ));
        let ingestor = WsIngestor::with_provider(provider);
        (ingestor, Arc::new(AtomicBool::new(false)), asserter)
    }

    fn header(number: u64) -> IngestEvent {
        IngestEvent::BlockHeader {
            number,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
            hash: B256::ZERO,
            parent_hash: B256::ZERO,
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

    fn header_ts(number: u64, timestamp: u64) -> IngestEvent {
        IngestEvent::BlockHeader {
            number,
            timestamp,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
            hash: B256::ZERO,
            parent_hash: B256::ZERO,
        }
    }

    /// First topic of every re-injected pool event, in delivery order.
    async fn drained_topics(
        mut s: stream::BoxStream<'static, IngestEvent>,
    ) -> Vec<alloy::primitives::B256> {
        let mut topics = Vec::new();
        while let Some(ev) = s.next().await {
            if let IngestEvent::Pool(pe) = ev {
                topics.push(pe.payload.topics().first().copied().unwrap_or_default());
            }
        }
        topics
    }

    /// Logs before, between, and after the confirming headers are re-injected
    /// exactly once, in arrival order, and the boundary is the first log's
    /// block once the confirmed head reaches it.
    #[tokio::test]
    async fn handshake_reinjects_pending_logs_once_in_arrival_order() {
        let (ingestor, shutdown, _asserter) = ingestor_for_test();
        let w = 500u64;
        let pool = Address::from([0xccu8; 20]);

        let combined = stream::iter(vec![
            IngestEvent::Pool(PoolEvent::from_log(make_v3_mint_log_with_block(
                pool,
                -100,
                100,
                w + 2,
            ))),
            header(w),
            IngestEvent::Pool(PoolEvent::from_log(make_v3_burn_log_with_block(
                pool, -100, 100, w,
            ))),
            header(w + 1),
            IngestEvent::Pool(PoolEvent::from_log(make_v3_mint_log_with_block(
                pool, -100, 100, w,
            ))),
            header(w + 2),
            header(w + 3),
        ])
        .boxed();

        let boundary = ingestor.handshake(shutdown, combined).await;
        assert_eq!(
            boundary.first_block,
            w + 2,
            "boundary is the first log's block once the confirmed head reaches it"
        );
        assert_eq!(
            drained_topics(boundary.stream).await,
            vec![V3_MINT_TOPIC, V3_BURN_TOPIC, V3_MINT_TOPIC],
            "every pending log is re-injected once, in arrival order"
        );
    }

    /// Shutdown at the adapter loop head takes the existing zero boundary and
    /// still hands the pending logs onward.
    #[tokio::test]
    async fn handshake_shutdown_returns_zero_boundary_and_preserves_pending() {
        let (ingestor, shutdown, _asserter) = ingestor_for_test();
        let w = 700u64;
        let pool = Address::from([0xddu8; 20]);

        let shutdown_flag = Arc::clone(&shutdown);
        let flip_then_header = stream::once(async move {
            shutdown_flag.store(true, Ordering::Relaxed);
            header(w + 3)
        });
        let combined = stream::iter(vec![
            IngestEvent::Pool(PoolEvent::from_log(make_v3_mint_log_with_block(
                pool, -100, 100, w,
            ))),
            header(w),
        ])
        .chain(flip_then_header)
        .boxed();

        let boundary = ingestor.handshake(shutdown, combined).await;
        assert_eq!(boundary.first_block, 0);
        assert_eq!(boundary.first_timestamp, 0);
        assert_eq!(
            drained_topics(boundary.stream).await,
            vec![V3_MINT_TOPIC],
            "pending logs survive shutdown"
        );
    }

    /// A stream that ends before completion takes the anchor header as the
    /// fallback boundary, keeping the pending logs.
    #[tokio::test]
    async fn handshake_stream_end_falls_back_to_the_anchor_and_preserves_pending() {
        let (ingestor, shutdown, _asserter) = ingestor_for_test();
        let w = 900u64;
        let pool = Address::from([0xeeu8; 20]);

        let combined = stream::iter(vec![
            IngestEvent::Pool(PoolEvent::from_log(make_v3_mint_log_with_block(
                pool,
                -100,
                100,
                w + 3,
            ))),
            header_ts(w, 50),
        ])
        .boxed();

        let boundary = ingestor.handshake(shutdown, combined).await;
        assert_eq!(boundary.first_block, w);
        assert_eq!(boundary.first_timestamp, 50);
        assert_eq!(drained_topics(boundary.stream).await, vec![V3_MINT_TOPIC]);
    }

    #[tokio::test]
    async fn handshake_stream_end_without_headers_completes_at_zero() {
        let (ingestor, shutdown, _asserter) = ingestor_for_test();
        let pool = Address::from([0xffu8; 20]);

        let combined = stream::iter(vec![IngestEvent::Pool(PoolEvent::from_log(
            make_v3_mint_log_with_block(pool, -100, 100, 1),
        ))])
        .boxed();

        let boundary = ingestor.handshake(shutdown, combined).await;
        assert_eq!(boundary.first_block, 0);
        assert_eq!(boundary.first_timestamp, 0);
        assert_eq!(drained_topics(boundary.stream).await, vec![V3_MINT_TOPIC]);
    }

    /// A silent stream times out into the `eth_blockNumber` degraded path.
    #[tokio::test(start_paused = true)]
    async fn handshake_stream_timeout_completes_via_the_rpc_degraded_path() {
        let (ingestor, shutdown, asserter) = ingestor_for_test();
        asserter.push_success(&"0x1234");

        let boundary = ingestor
            .handshake(shutdown, stream::pending().boxed())
            .await;
        assert_eq!(
            boundary.first_block, 0x1234,
            "the degraded boundary is the RPC block"
        );
        assert_eq!(
            boundary.first_timestamp, 0,
            "the degraded path invents no confirmed-head timestamp"
        );
    }

    /// A failed `eth_blockNumber` is not terminal: the handshake keeps
    /// waiting and the existing rules still pick the boundary.
    #[tokio::test(start_paused = true)]
    async fn handshake_timeout_rpc_failure_keeps_waiting_then_completes() {
        let (ingestor, shutdown, _asserter) = ingestor_for_test();
        let w = 321u64;
        let pool = Address::from([0x11u8; 20]);

        // silence past the stream-timeout first; then the stream wakes
        let items = vec![
            IngestEvent::Pool(PoolEvent::from_log(make_v3_mint_log_with_block(
                pool, -100, 100, w,
            ))),
            header_ts(w, 50),
            header_ts(w + 1, 60),
        ];
        let combined = stream::once(async move {
            tokio::time::sleep(Duration::from_secs(61)).await;
            stream::iter(items).boxed()
        })
        .flatten()
        .boxed();

        let boundary = ingestor.handshake(shutdown, combined).await;
        assert_eq!(
            boundary.first_block, w,
            "after the failed fallback the normal rules still choose the boundary"
        );
        assert_eq!(boundary.first_timestamp, 60);
        assert_eq!(drained_topics(boundary.stream).await, vec![V3_MINT_TOPIC]);
    }

    /// The settle deadline is a real trigger: once it expires with no log,
    /// the header-confirmed fallback is selected without waiting for the next
    /// stream event (the queued RPC answer is never consumed).
    #[tokio::test(start_paused = true)]
    async fn handshake_deadline_wake_completes_with_the_header_fallback() {
        let (ingestor, shutdown, asserter) = ingestor_for_test();
        asserter.push_success(&"0x1234");

        let combined = stream::iter(vec![header_ts(5, 50), header_ts(6, 60)])
            .chain(stream::pending())
            .boxed();

        let boundary = ingestor.handshake(shutdown, combined).await;
        assert_eq!(
            (boundary.first_block, boundary.first_timestamp),
            (5, 60),
            "deadline expiry selects the confirmed header fallback"
        );
        assert_eq!(
            asserter.read_q().len(),
            1,
            "the degraded RPC path is not taken once the deadline owns completion"
        );
    }

    /// Block-`W` logs interleaved between the confirming headers survive the
    /// handshake in `pending` and reach the resume stream.
    #[tokio::test]
    async fn handshake_preserves_w_logs() {
        let (ingestor, shutdown, _asserter) = ingestor_for_test();
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
            "block-W Mint MUST survive the handshake (re-injected from pending)"
        );
        assert!(
            got_burn,
            "block-W Burn MUST survive the handshake (re-injected from pending)"
        );
    }

    /// The handshake never interprets a log: both block-`W` logs it collects
    /// between the confirming headers are re-injected into the resume stream.
    #[tokio::test]
    async fn handshake_does_not_consume_logs() {
        let (ingestor, shutdown, _asserter) = ingestor_for_test();
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
