//! Hosted sources: the process-lifetime owner of the strategies' pending-tx
//! source provisioning and the head clock.
//!
//! One typed stream per SOURCE KIND, minted once at boot: each active arm's
//! pump registers its own [`HubClass::PendingTx`] ring under its
//! [`PendingTxSource`], so two backrun arms can run in one process without a
//! registration tombstone and each arm's frame corpus is exactly its own
//! feed's. The head clock is also process-lifetime: ONE `newHeads` watch
//! registered on the hub head source, ONE stale-fallback `eth_blockNumber`
//! poller, and ONE feed-telemetry sampler — drivers never spawn feeds, watch
//! heads, or scrape status; they drain their arm's typed stream and read the
//! hub's latest head ([`Hub::subscribe_head`] is multi-subscriber).
//!
//! The head edge drives the once-per-head reconciliation through an injected
//! trigger: this crate cannot depend on `degenbot-submission` (the
//! composition root owns `HeadReconciliation`), so the boot wires
//! `HeadReconciliation::reconcile_head` in as the [`HeadTrigger`] closure and
//! this module only knows "a head was observed at the source edge, fire the
//! trigger" — once per observed head; the reconciliation's once-per-head
//! dedupe makes redundant triggers free.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use degenbot_eventhub::{HeadSubscription, Hub, HubError, HubEvent, PendingTxSource};
use degenbot_rpc::backrun_feed::BackrunFeedStatus;
use degenbot_rpc::head_watch::{HeadWatch, HeadWatchConfig};
use degenbot_rpc::pending_tx_stream::{PendingTxFeedConfig, PendingTxPump, PendingTxStream};
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};
use futures_util::future::BoxFuture;

/// How long a head await may run before the edge re-checks staleness. The
/// watch resolves the instant a header arrives (~12s apart), so a healthy
/// watch leaves this to time out most iterations.
const HEAD_WAIT: Duration = Duration::from_secs(2);

/// A watch silent this long is treated as dead: the edge polls for that
/// iteration while the watch's watchdog reconnects in the background. Must
/// exceed the chain's block interval — mainnet blocks arrive ~12s apart, so
/// a threshold at or below that would poll on every block and defeat the
/// point of the subscription. Kept below the watch's 48s watchdog so the
/// fallback poll covers the reconnect window.
const HEAD_STALE: Duration = Duration::from_secs(30);

/// The fallback head-poll cadence.
const HEAD_POLL_TICK: Duration = Duration::from_millis(200);

/// The feed-telemetry sample cadence (the per-driver `<=2s` scrape it
/// replaces).
const FEED_SAMPLE_TICK: Duration = Duration::from_secs(2);

/// The per-head trigger the source head edge fires. The composition root
/// wires `HeadReconciliation::reconcile_head` in here; the module never
/// names the reconciliation type.
pub type HeadTrigger = Arc<dyn Fn(u64) -> BoxFuture<'static, ()> + Send + Sync>;

/// The head clock the module hosts.
pub enum HostedHeadClock {
    /// Watch the chain node's `newHeads` subscription, falling back to the
    /// poller when the subscribe fails or the source goes stale.
    Watch {
        /// The chain node's subscription-scope endpoint.
        ws_url: String,
        /// The chain-node provider the fallback poller reads.
        provider: Arc<AlloyProvider>,
    },
    /// No WS feed: the poller is the primary clock.
    Poll {
        /// The chain-node provider the poller reads.
        provider: Arc<AlloyProvider>,
    },
    /// A head source hosted elsewhere already publishes into the hub; this
    /// module's edge only fires the trigger per observed head.
    Hub,
}

/// The process-lifetime pending-tx pumps and head clock.
///
/// Constructed once at the boot ([`Self::mint`]); each arm's driver stream is
/// handed out once via [`Self::take_stream`]. Dropping the owner stops the
/// pumps' transports; the hub rings outlive the handles.
pub struct HostedSources {
    hub: Arc<Hub>,
    pumps: Arc<parking_lot::Mutex<Vec<PendingTxPump>>>,
    streams: Vec<(PendingTxSource, PendingTxStream)>,
}

impl HostedSources {
    /// Mint the hosted sources: one pump per named arm kind, the head watch,
    /// the fallback poller, and the telemetry sampler.
    ///
    /// `arms` names every ACTIVE arm's kind; an arm whose kind is absent gets
    /// no pump and no stream (its driver halts at its live-mode gate, the
    /// same refusal a boot with no resolvable feed produced).
    ///
    /// The head watch subscribes synchronously so a failed subscribe is known
    /// at boot and the clock falls back to the poller immediately.
    ///
    /// # Errors
    ///
    /// [`HubError`] when a head-source registration or a pump provisioning
    /// fails — a miswired boot (two mints over one hub) fails closed rather
    /// than half-hosting.
    pub async fn mint(
        hub: Arc<Hub>,
        arms: impl IntoIterator<Item = (PendingTxSource, PendingTxFeedConfig)>,
        clock: Option<HostedHeadClock>,
        trigger: Option<HeadTrigger>,
    ) -> Result<Self, HubError> {
        let mut pumps = Vec::new();
        let mut streams = Vec::new();
        for (source, cfg) in arms {
            let (pump, stream) = PendingTxPump::spawn_on_ring(&hub, source, cfg)?;
            pumps.push(pump);
            streams.push((source, stream));
        }
        let pumps = Arc::new(parking_lot::Mutex::new(pumps));
        spawn_feed_sampler(Arc::clone(&pumps));
        if let Some(clock) = clock {
            spawn_head_task(Arc::clone(&hub), clock, trigger).await?;
        }
        Ok(Self {
            hub,
            pumps,
            streams,
        })
    }

    /// Hand out one arm's typed stream — once. `None` when no pump was
    /// minted for the kind (the arm was not active at boot).
    pub fn take_stream(&mut self, source: PendingTxSource) -> Option<PendingTxStream> {
        let index = self.streams.iter().position(|(kind, _)| *kind == source)?;
        Some(self.streams.remove(index).1)
    }

    /// Subscribe a reader of the hub's latest head. Multi-subscriber: every
    /// driver reads the same hosted clock.
    ///
    /// # Errors
    ///
    /// [`HubError::NotRegistered`] while no head source is registered (a
    /// joinless boot hosts no clock); callers pace on the refusal.
    pub fn subscribe_head(&self) -> Result<HeadSubscription, HubError> {
        self.hub.subscribe_head()
    }
}

impl Drop for HostedSources {
    fn drop(&mut self) {
        for pump in self.pumps.lock().iter() {
            pump.stop();
        }
    }
}

/// Subscribe the transport and spawn the head edge task.
///
/// The task owns the head edge for the process lifetime: it fires the
/// trigger once per observed head (from the watch, or from the fallback
/// poller, which also publishes polled heads into the hub so every driver
/// sees them), and on a stale or closed watch it polls for that iteration.
async fn spawn_head_task(
    hub: Arc<Hub>,
    clock: HostedHeadClock,
    trigger: Option<HeadTrigger>,
) -> Result<(), HubError> {
    let mut watch_live = false;
    if let HostedHeadClock::Watch { ws_url, .. } = &clock {
        match AlloyProvider::new(ws_url, DEFAULT_MAX_RETRIES).await {
            Ok(ws_provider) => {
                match HeadWatch::subscribe(
                    &hub,
                    ws_provider.provider_arc(),
                    HeadWatchConfig::default(),
                )
                .await
                {
                    Ok(_) => watch_live = true,
                    Err(error) => {
                        tracing::error!(
                            %error,
                            "head watch subscribe failed - falling back to 200ms head poll"
                        );
                    }
                }
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    "head watch WS connect failed - falling back to 200ms head poll"
                );
            }
        }
    }
    // The fallback poller's publish handle beside the transport's own: the
    // hub's head source is registered by the watch when it subscribed, and
    // by this module otherwise. `Hub` mode reuses (or registers) the source
    // the external publisher hosts.
    let sender = match hub.head_sender() {
        Ok(sender) => sender,
        Err(HubError::NotRegistered(_)) => hub.register_head_source()?,
        Err(error) => return Err(error),
    };
    let mut head = hub.subscribe_head()?;
    let poll_provider: Option<Arc<AlloyProvider>> = match &clock {
        HostedHeadClock::Watch { provider, .. } | HostedHeadClock::Poll { provider } => {
            Some(Arc::clone(provider))
        }
        HostedHeadClock::Hub => None,
    };
    let poll_only =
        matches!(clock, HostedHeadClock::Poll { .. }) || (!watch_live && poll_provider.is_some());
    let mut last_published = head.head().unwrap_or(0);
    degenbot_core::runtime::get_runtime().spawn(async move {
        loop {
            if poll_only {
                // The poll is the primary clock: publish + trigger each NEW
                // head at the poll cadence.
                tokio::time::sleep(HEAD_POLL_TICK).await;
                if let Some(number) = poll(poll_provider.as_ref()).await {
                    if number > last_published {
                        publish_polled(&sender, number);
                        last_published = number;
                        if let Some(trigger) = trigger.as_ref() {
                            trigger(number).await;
                        }
                    }
                }
                continue;
            }
            // Watch (or external-hub) mode: the head edge resolves on a
            // header's arrival; the 2s bound keeps the edge live while the
            // head is quiet.
            let observed: Option<(u64, bool)> =
                match tokio::time::timeout(HEAD_WAIT, head.changed()).await {
                    Ok(Ok(())) => head.borrow_and_update().map(|number| (number, false)),
                    Ok(Err(_)) => {
                        tracing::warn!("head source closed - polling");
                        tokio::time::sleep(HEAD_POLL_TICK).await;
                        poll(poll_provider.as_ref())
                            .await
                            .map(|number| (number, true))
                    }
                    Err(_) => {
                        if watch_live && head.stale(HEAD_STALE) {
                            tracing::warn!("head source stale - polling");
                            tokio::time::sleep(HEAD_POLL_TICK).await;
                            poll(poll_provider.as_ref())
                                .await
                                .map(|number| (number, true))
                        } else {
                            None
                        }
                    }
                };
            if let Some((number, from_poll)) = observed {
                if from_poll && number > last_published {
                    publish_polled(&sender, number);
                    last_published = number;
                }
                if let Some(trigger) = trigger.as_ref() {
                    trigger(number).await;
                }
            }
        }
    });
    Ok(())
}

async fn poll(provider: Option<&Arc<AlloyProvider>>) -> Option<u64> {
    let provider = provider?;
    provider.get_block_number().await.ok()
}

/// Publish a polled head into the hub head source (zeroed header fields —
/// the consumers key on the block number).
fn publish_polled(sender: &degenbot_eventhub::HeadSender, number: u64) {
    sender.publish(HubEvent::NewHead {
        number,
        timestamp: 0,
        base_fee_per_gas: None,
        gas_used: 0,
        gas_limit: 0,
    });
}

/// Spawn the one feed-telemetry sampler: scrape every hosted pump's status
/// into the engine instruments on a `<=2s` tick, pushing counters as deltas.
fn spawn_feed_sampler(pumps: Arc<parking_lot::Mutex<Vec<PendingTxPump>>>) {
    let mut last: Vec<BackrunFeedStatus> = pumps.lock().iter().map(PendingTxPump::status).collect();
    degenbot_core::runtime::get_runtime().spawn(async move {
        loop {
            tokio::time::sleep(FEED_SAMPLE_TICK).await;
            let Some(pipeline) = degenbot_substrate::telemetry_port::pipeline() else {
                continue;
            };
            let guard = pumps.lock();
            if guard.is_empty() {
                continue;
            }
            let current: Vec<BackrunFeedStatus> = guard.iter().map(PendingTxPump::status).collect();
            drop(guard);
            let pairs: Vec<(BackrunFeedStatus, BackrunFeedStatus)> = current
                .iter()
                .zip(
                    last.iter()
                        .chain(std::iter::repeat(&BackrunFeedStatus {
                            connected: false,
                            accepted: 0,
                            dropped_ring: 0,
                            rejected_chain_id: 0,
                            rejected_parse: 0,
                            reconnects: 0,
                            last_event_unix_ms: 0,
                        }))
                        .take(current.len()),
                )
                .map(|(cur, prev)| (*cur, *prev))
                .collect();
            if let Some((
                connected,
                seconds_since_event,
                frames,
                dropped,
                parse,
                chain_id,
                reconnects,
            )) = aggregate_feed_sample(&pairs)
            {
                pipeline.record_backrun_feed(
                    connected,
                    seconds_since_event,
                    frames,
                    dropped,
                    parse,
                    chain_id,
                    reconnects,
                );
            }
            last = current;
        }
    });
}

/// One sampler record: `(connected, seconds_since_event, frames, dropped,
/// parse, chain-id rejects, reconnects)` — the positional shape the engine
/// instruments' `record_backrun_feed` consumes.
type FeedSample = (bool, Option<f64>, u64, u64, u64, u64, u64);

/// One sampler record from every pump's (current, previous) status pair:
/// `connected` is ANY arm live, `seconds_since_event` the most recent event
/// across arms, and the counters the summed per-arm deltas. A single arm
/// records exactly what the per-driver scrape it replaced recorded.
fn aggregate_feed_sample(samples: &[(BackrunFeedStatus, BackrunFeedStatus)]) -> Option<FeedSample> {
    if samples.is_empty() {
        return None;
    }
    let now_ms = now_unix_ms();
    let mut connected = false;
    let mut seconds_since_event: Option<f64> = None;
    let mut frames = 0;
    let mut dropped = 0;
    let mut parse = 0;
    let mut chain_id = 0;
    let mut reconnects = 0;
    #[expect(clippy::cast_precision_loss)]
    for (current, previous) in samples {
        connected |= current.connected;
        if current.last_event_unix_ms != 0 {
            let secs = (now_ms.saturating_sub(current.last_event_unix_ms)) as f64 / 1_000.0;
            seconds_since_event = Some(match seconds_since_event {
                Some(shortest) if shortest < secs => shortest,
                _ => secs,
            });
        }
        frames += current.accepted.saturating_sub(previous.accepted);
        dropped += current.dropped_ring.saturating_sub(previous.dropped_ring);
        parse += current
            .rejected_parse
            .saturating_sub(previous.rejected_parse);
        chain_id += current
            .rejected_chain_id
            .saturating_sub(previous.rejected_chain_id);
        reconnects += current.reconnects.saturating_sub(previous.reconnects);
    }
    Some((
        connected,
        seconds_since_event,
        frames,
        dropped,
        parse,
        chain_id,
        reconnects,
    ))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "hosted-source tests assert mint and head-edge outcomes"
)]
mod tests {
    use super::*;
    use degenbot_rpc::backrun_feed::BackrunFeedConfig;
    use degenbot_rpc::txpool_feed::TxpoolFeedConfig;

    /// Both arms' kinds mint on ONE hub, and each arm's stream is handed out
    /// exactly its own feed's frames: no second-arm tombstone, per-kind
    /// corpus split.
    #[tokio::test]
    async fn both_arms_mint_and_drain_their_own_corpus() {
        let hub = Arc::new(Hub::new());
        let mut hosted = HostedSources::mint(
            Arc::clone(&hub),
            [
                (
                    PendingTxSource::Mevblocker,
                    PendingTxFeedConfig::Mevblocker(BackrunFeedConfig::for_mainnet()),
                ),
                (
                    PendingTxSource::Txpool,
                    PendingTxFeedConfig::Txpool(TxpoolFeedConfig::defaults()),
                ),
            ],
            None,
            None,
        )
        .await
        .expect("both kinds mint without a tombstone");

        let mev = hosted
            .take_stream(PendingTxSource::Mevblocker)
            .expect("the mevblocker arm's stream");
        let tx = hosted
            .take_stream(PendingTxSource::Txpool)
            .expect("the txpool arm's stream");
        assert_eq!(mev.source(), PendingTxSource::Mevblocker);
        assert_eq!(tx.source(), PendingTxSource::Txpool);
        assert!(
            hosted.take_stream(PendingTxSource::Mevblocker).is_none(),
            "a stream is handed out exactly once"
        );

        // Push a frame per kind (test support) and drain each stream: each
        // arm's corpus is exactly its own feed's.
        {
            let pumps = hosted.pumps.lock();
            for pump in pumps.iter() {
                pump.push(frame(pump.source(), 7));
            }
        }
        let mev_frames = mev.drain();
        let tx_frames = tx.drain();
        assert_eq!(
            mev_frames.len(),
            1,
            "the mevblocker stream is its feed's corpus"
        );
        assert_eq!(
            mev_frames[0].hash,
            mev_hash(),
            "the mevblocker frame is the mevblocker feed's"
        );
        assert_eq!(tx_frames.len(), 1, "the txpool stream is its feed's corpus");
        assert_eq!(
            tx_frames[0].hash,
            txpool_hash(),
            "the txpool frame is the txpool feed's"
        );
    }

    fn mev_hash() -> alloy::primitives::B256 {
        alloy::primitives::B256::repeat_byte(0x11)
    }

    fn txpool_hash() -> alloy::primitives::B256 {
        alloy::primitives::B256::repeat_byte(0x22)
    }

    fn frame(source: PendingTxSource, nonce: u64) -> degenbot_eventhub::HubEvent {
        degenbot_eventhub::HubEvent::PendingTx(degenbot_eventhub::PendingTx {
            raw_signed_tx: None,
            chain_id: 1,
            from: alloy::primitives::Address::ZERO,
            to: None,
            value: alloy::primitives::U256::ZERO,
            data: alloy::primitives::Bytes::new(),
            gas: 0,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            nonce,
            hash: match source {
                PendingTxSource::Mevblocker => mev_hash(),
                PendingTxSource::Txpool => txpool_hash(),
            },
            access_list: serde_json::Value::Null,
            tx_type: 2,
            received_unix_ms: 0,
        })
    }

    /// The head edge fires the trigger once per observed head — the source
    /// edge, not a driver loop, owns the reconciliation trigger.
    #[tokio::test]
    async fn the_head_edge_fires_the_trigger_per_observed_head() {
        let hub = Arc::new(Hub::new());
        hub.register_head_source()
            .expect("the test hosts the head source");
        let (fire_tx, mut fire_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let trigger: HeadTrigger = Arc::new(move |head| {
            let fire_tx = fire_tx.clone();
            Box::pin(async move {
                fire_tx.send(head).expect("the recorder lives");
            })
        });
        let _hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedHeadClock::Hub),
            Some(trigger),
        )
        .await
        .expect("the hub clock mints");

        let sender = hub.head_sender().expect("the head source is registered");
        sender.publish(HubEvent::NewHead {
            number: 42,
            timestamp: 0,
            base_fee_per_gas: None,
            gas_used: 0,
            gas_limit: 0,
        });
        let fired = fire_rx.recv().await.expect("the edge fired");
        assert_eq!(fired, 42, "the trigger carries the observed head");
    }

    /// One head registration per process: a second mint reuses the hosted
    /// head source instead of colliding with it.
    #[tokio::test]
    async fn a_second_mint_reuses_the_head_source() {
        let hub = Arc::new(Hub::new());
        let _first = HostedSources::mint(Arc::clone(&hub), [], Some(HostedHeadClock::Hub), None)
            .await
            .expect("first mint");
        let _second = HostedSources::mint(Arc::clone(&hub), [], Some(HostedHeadClock::Hub), None)
            .await
            .expect("second mint reuses the head source");
        assert!(hub.head_sender().is_ok(), "exactly one head source");
    }

    /// The sampler record: one arm aggregates to exactly what the per-driver
    /// scrape recorded; two arms sum their deltas and keep the freshest
    /// event age.
    #[test]
    fn the_sampler_aggregates_arms() {
        let status = |connected, accepted, last_ms| BackrunFeedStatus {
            connected,
            accepted,
            dropped_ring: 0,
            rejected_chain_id: 0,
            rejected_parse: 0,
            reconnects: 0,
            last_event_unix_ms: last_ms,
        };
        let prev = status(false, 10, 0);
        let one = status(true, 15, 1_000);
        let (connected, secs, frames, dropped, parse, chain, reconnects) =
            aggregate_feed_sample(&[(one, prev)]).expect("one arm");
        assert!(connected);
        assert_eq!((frames, dropped, parse, chain, reconnects), (5, 0, 0, 0, 0));
        assert!(secs.is_some(), "the event age is carried");

        let two_prev = status(false, 100, 0);
        let two = status(false, 103, 2_000);
        let (connected, secs, frames, ..) =
            aggregate_feed_sample(&[(one, prev), (two, two_prev)]).expect("two arms");
        assert!(connected, "any arm live reads connected");
        assert_eq!(frames, 8, "the counters sum across arms");
        assert!(
            secs.is_some_and(|s| s
                <= aggregate_feed_sample(&[(status(true, 15, 1_000), status(false, 10, 0))])
                    .expect("one arm")
                    .1
                    .expect("age")),
            "the freshest event age wins"
        );
    }
}
