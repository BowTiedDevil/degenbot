//! Hosted sources: the ENGINE-lifetime owner of the strategies' pending-tx
//! source provisioning and the head clock.
//!
//! One typed stream per source kind, minted once at boot: each active arm's
//! pump registers its own [`HubClass::PendingTx`] ring under its
//! [`PendingTxSource`], so two backrun arms can run in one process without a
//! registration tombstone and each arm's frame corpus is exactly its own
//! feed's. The head clock is also engine-lifetime: ONE `newHeads` watch
//! registered on the hub head source, ONE stale-fallback `eth_blockNumber`
//! poller, and ONE feed-telemetry sampler — drivers never spawn feeds, watch
//! heads, or scrape status; they drain their arm's typed stream and read the
//! hub's latest head ([`Hub::subscribe_head`] is multi-subscriber).
//!
//! # Deferred start
//!
//! [`HostedSources::mint`] PROVISIONS (the hub rings, the driver streams, the
//! dedicated head-watch runtime, the clock and trigger) and dials nothing;
//! [`HostedSources::start`] launches the live I/O at the engine's start
//! boundary — each arm's feed pump, the feed-telemetry sampler, and the
//! head-watch WS connect. A process that mints an engine without resuming
//! performs ZERO network dials.
//!
//! The head edge drives the once-per-head reconciliation through an injected
//! trigger: this crate cannot depend on `degenbot-submission` (the
//! composition root owns `HeadReconciliation`), so the boot wires
//! `HeadReconciliation::reconcile_head` in as the [`HeadTrigger`] closure and
//! this module only knows "a head was observed at the source edge, fire the
//! trigger" — once per observed head; the reconciliation's once-per-head
//! dedupe makes redundant triggers free.
//!
//! # Lifetime & teardown
//!
//! The boot mints one `HostedSources` and the engine holds it for its whole
//! life, so `Drop for HostedSources` runs when the ENGINE drops — not at
//! boot-host end. Drop stops every pump's transport, fires a `watch` shutdown
//! signal the head edge and the feed sampler select on, and tears down a
//! DEDICATED single-worker runtime (the shared `OwnedPubsubRuntime`), the same
//! type `AnvilFork` uses, that
//! hosts the head-watch WS transport and its alloy pubsub service. Tearing that
//! runtime down CANCELS the pubsub service and the head watch's driving task
//! deterministically, with no dependence on a contended shared-runtime worker —
//! the head edge also drops its WS transport Arc while the endpoint may still
//! be alive (the `AnvilFork` ordering discipline).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy::network::Ethereum;
use alloy::providers::Provider;
use degenbot_core::pubsub_runtime::{OwnedPubsubRuntime, OwnedPubsubRuntimeSpec};
use degenbot_eventhub::{HeadSubscription, Hub, HubError, HubEvent, PendingTxSource};
use degenbot_rpc::backrun_feed::BackrunFeedStatus;
use degenbot_rpc::head_watch::{HeadWatch, HeadWatchConfig};
use degenbot_rpc::pending_tx_stream::{PendingTxFeedConfig, PendingTxPump, PendingTxStream};
use degenbot_rpc::provider::{AlloyProvider, DEFAULT_MAX_RETRIES};
use futures_util::future::BoxFuture;
use tokio::sync::watch;

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

/// The engine-lifetime pending-tx pumps and head clock.
///
/// Constructed once at the boot ([`Self::mint`]), which PROVISIONS everything
/// (the hub rings, the driver streams, the dedicated head-watch runtime, the
/// clock and trigger) but starts nothing and dials nothing. [`Self::start`] —
/// the engine's start boundary — launches the deferred I/O: each arm's feed
/// pump, the feed-telemetry sampler, and the head-watch WS connect. Each arm's
/// driver stream is handed out once via [`Self::take_stream`]. The boot hands
/// the owner to the engine, which holds it for its whole life, so [`Drop`] runs
/// when the ENGINE drops: it stops the pumps' transports, fires the shutdown
/// signal the head edge and the feed sampler exit on, and tears down the
/// dedicated head-watch runtime (so its pubsub service + driving task are
/// cancelled). The hub rings outlive the handles.
pub struct HostedSources {
    hub: Arc<Hub>,
    pumps: Arc<parking_lot::Mutex<Vec<PendingTxPump>>>,
    streams: Vec<(PendingTxSource, PendingTxStream)>,
    /// The shutdown signal fired by [`Drop`]: `true` stops the head edge and
    /// the feed sampler (the graceful stop for the plain loops on the shared
    /// runtime; the dedicated runtime teardown is the deterministic cancel).
    /// [`Self::start`] subscribes the receivers; a never-started owner has
    /// none, so its drop send is a plain no-op.
    shutdown: watch::Sender<bool>,
    /// Set by the head edge task just before it exits, after it drops its WS
    /// transport. Read through [`Self::head_edge_stopped`]; a test waits on a
    /// clone for the positive half of the clean-shutdown guard.
    head_edge_stopped: Arc<AtomicBool>,
    /// The head-watch WS transport Arc [`Self::start`] created, held for the
    /// owner's life so it is never moved away unaccounted: when [`Drop`] tears
    /// the dedicated runtime down, no frontend clone keeps the pubsub request
    /// channel open. `None` until [`Self::start`] connects (a never-started
    /// owner never dials).
    head_ws_provider: parking_lot::Mutex<Option<Arc<dyn Provider<Ethereum>>>>,
    /// Dedicated single-worker runtime the head-watch WS transport (and the
    /// alloy pubsub service it spawns) live on — the shared
    /// [`OwnedPubsubRuntime`] discipline (the same type `AnvilFork` uses).
    /// [`Drop`] tears it down, so the pubsub service and the head watch's
    /// driving task are CANCELLED deterministically rather than left to a
    /// contended shared-runtime worker. `Option` so [`Drop`] can pull it out
    /// and control WHERE it is dropped. Built at mint (empty); [`Self::start`]
    /// is the only thing that spawns onto it.
    pubsub_runtime: Option<OwnedPubsubRuntime>,
    /// The head clock [`Self::start`] hosts. [`Self::mint`] only stores it: the
    /// WS connect is the deferred dial, so it happens at the start boundary.
    clock: parking_lot::Mutex<Option<HostedHeadClock>>,
    /// The per-head trigger [`Self::start`] hands the head edge.
    trigger: parking_lot::Mutex<Option<HeadTrigger>>,
    /// The deferred-I/O latch: [`Self::start`] launches the pumps, the feed
    /// sampler, and the head edge exactly once.
    started: AtomicBool,
}

impl HostedSources {
    /// Mint the hosted sources: one pump per named arm kind (provisioned, not
    /// started), the dedicated head-watch runtime, and the deferred head clock.
    ///
    /// `arms` names every ACTIVE arm's kind; an arm whose kind is absent gets
    /// no pump and no stream (its driver halts at its live-mode gate, the
    /// same refusal a boot with no resolvable feed produced).
    ///
    /// Nothing here dials: the feed pumps and the head-watch WS connect are
    /// launched by [`Self::start`] at the engine's start boundary, so an
    /// engine that is constructed and never started performs zero network
    /// I/O. The rings, the driver streams, the dedicated runtime, and the
    /// clock are all provisioned here, so `take_stream` and the live-gate
    /// semantics are unchanged.
    ///
    /// # Panics
    ///
    /// When a WS `Watch` clock is hosted and the dedicated single-worker
    /// runtime cannot be built (OS resource exhaustion): the boot fails closed
    /// rather than hosting a half-live head watch.
    ///
    /// # Errors
    ///
    /// [`HubError`] when a pump provisioning fails — a miswired boot (two
    /// mints over one hub) fails closed rather than half-hosting.
    pub fn mint(
        hub: Arc<Hub>,
        arms: impl IntoIterator<Item = (PendingTxSource, PendingTxFeedConfig)>,
        clock: Option<HostedHeadClock>,
        trigger: Option<HeadTrigger>,
    ) -> Result<Self, HubError> {
        let mut pumps = Vec::new();
        let mut streams = Vec::new();
        for (source, cfg) in arms {
            let (pump, stream) = PendingTxPump::provision_on_ring(&hub, source, cfg)?;
            pumps.push(pump);
            streams.push((source, stream));
        }
        let pumps = Arc::new(parking_lot::Mutex::new(pumps));
        // The one shutdown channel the head edge and the feed sampler select
        // on; `Drop` fires it and `start` subscribes the receivers. No receiver
        // exists until `start`, so a never-started owner's drop send is a plain
        // no-op.
        let (shutdown, _shutdown_rx) = watch::channel(false);
        let head_edge_stopped = Arc::new(AtomicBool::new(false));
        // A WS watch is hosted on a DEDICATED single-worker runtime, so the
        // alloy pubsub service it spawns (and the head watch's driving task) are
        // cancellable by tearing that runtime down — the deterministic teardown
        // `Drop` performs (the shared `OwnedPubsubRuntime` discipline). Built
        // here (empty); `start` is the only thing that spawns onto it.
        let pubsub_runtime = if matches!(&clock, Some(HostedHeadClock::Watch { .. })) {
            #[expect(
                clippy::expect_used,
                reason = "a fresh single-worker runtime build fails only on OS resource exhaustion; the boot fails closed rather than hosting a half-live head watch"
            )]
            let runtime = OwnedPubsubRuntime::build(OwnedPubsubRuntimeSpec {
                resource: "hosted_pubsub_workers",
                kind: "tokio single-worker runtime (hosted head-watch WS pubsub — deterministic teardown)",
                thread_name: "degenbot-hosted-pubsub",
                sizing: "exactly one per HostedSources owner (one hosted owner per engine); fixed single worker",
                binding: "pinned",
            })
            .expect("build the hosted head-watch pubsub runtime");
            Some(runtime)
        } else {
            None
        };
        Ok(Self {
            hub,
            pumps,
            streams,
            shutdown,
            head_edge_stopped,
            head_ws_provider: parking_lot::Mutex::new(None),
            pubsub_runtime,
            clock: parking_lot::Mutex::new(clock),
            trigger: parking_lot::Mutex::new(trigger),
            started: AtomicBool::new(false),
        })
    }

    /// Launch the deferred hosted I/O — the engine's start boundary.
    ///
    /// Exactly once: starts every arm's feed pump (its first dial), spawns the
    /// feed-telemetry sampler, connects the head-watch WS transport on the
    /// owned runtime (the second dial), registers the hub head source, and
    /// spawns the head edge. [`Self::mint`] performed none of this, so an
    /// engine that is minted and never started dials nothing; a started engine
    /// behaves exactly as before (a failed WS connect still falls back to the
    /// poller).
    ///
    /// # Errors
    ///
    /// [`HubError`] when the hub head-source registration fails — a miswired
    /// boot fails closed rather than hosting a clock with no source.
    ///
    /// # Half-failure contract
    ///
    /// The deferred-I/O latch is consumed BEFORE any pump launches, so a
    /// `start` that returns an error is NOT retryable: a second call is a
    /// no-op (the latch is already taken) and cannot re-attempt the failed
    /// registration. When the head-source registration fails AFTER the pumps
    /// launched, the owner is left HALF-STARTED — the pumps are live but the
    /// clock has no source. The boot is fail-closed: the caller MUST drop the
    /// engine (which drops this owner, stopping the pumps and tearing the
    /// dedicated head-watch runtime down). There is deliberately no retry
    /// path.
    pub async fn start(&self) -> Result<(), HubError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // 1. Each arm's feed pump: the deferred dial.
        for pump in self.pumps.lock().iter() {
            pump.start();
        }
        // 2. The one feed-telemetry sampler.
        spawn_feed_sampler(Arc::clone(&self.pumps), self.shutdown.subscribe());
        // 3. The head edge: connect the WS transport on the owned runtime,
        //    register the hub head source, and spawn the edge loop. The clock
        //    and trigger guards are dropped before the await.
        let clock = self.clock.lock().take();
        let trigger = self.trigger.lock().take();
        if let Some(clock) = clock {
            let transport = spawn_head_task(
                Arc::clone(&self.hub),
                clock,
                trigger,
                self.shutdown.subscribe(),
                Arc::clone(&self.head_edge_stopped),
                self.pubsub_runtime.as_ref(),
            )
            .await?;
            *self.head_ws_provider.lock() = transport;
        }
        Ok(())
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

    /// The head edge's completion flag: set to `true` just before the edge
    /// task exits, after it drops its WS transport. A clone lets a caller
    /// observe the edge's clean shutdown across the owner's drop (the positive
    /// half of the teardown guard); the owner itself never reads it.
    #[must_use]
    pub fn head_edge_stopped(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.head_edge_stopped)
    }
}

impl Drop for HostedSources {
    fn drop(&mut self) {
        // Teardown ORDER is load-bearing (the `AnvilFork` discipline):
        //
        // 1. Stop every pump's transport.
        for pump in self.pumps.lock().iter() {
            pump.stop();
        }
        // 2. Fire the graceful stop the head edge and the feed sampler select
        //    on (plain loops on the shared runtime). The receiver side may
        //    already be gone (a mint that failed after the sampler spawned, or
        //    a task that aborted): a send error is expected and harmless — a
        //    dropped sender ends both actors too.
        let _ = self.shutdown.send(true);
        // 3. Drop the WS transport frontend WHILE the endpoint may still be
        //    alive, closing the pubsub request channel. `None` on a
        //    never-started owner, which never dialed.
        self.head_ws_provider.lock().take();
        // 4. Tear the dedicated runtime down via the shared `OwnedPubsubRuntime`
        //    discipline — the DETERMINISTIC cancellation: the alloy pubsub
        //    service and the head watch's driving task live on it and are
        //    discarded with it, with no dependence on a contended shared-runtime
        //    worker. The off-async `block_on` / detached inside-async drop split
        //    lives in that type.
        drop(self.pubsub_runtime.take());
    }
}

/// Subscribe the transport and spawn the head edge task.
///
/// The task owns the head edge while its [`HostedSources`] owner lives: it
/// fires the trigger once per observed head (from the watch, or from the
/// fallback poller, which also publishes polled heads into the hub so every
/// driver sees them), and on a stale or closed watch it polls for that
/// iteration. It exits when the owner's shutdown signal fires (or the owner's
/// sender drops), dropping the WS transport it holds WHILE the endpoint may
/// still be alive — the `AnvilFork` teardown discipline, so the alloy pubsub
/// service exits through the clean "request channel closed" path rather than a
/// reconnect loop against a dead endpoint.
///
/// The WS connect and the head watch's driving task run on `pubsub_runtime`
/// (the owner's dedicated runtime), so `Drop for HostedSources` cancels them
/// deterministically by tearing that runtime down. Returns the WS transport Arc
/// the owner must hold (`None` when no WS watch was hosted).
/// Connect the head-watch WS transport on the owner's dedicated runtime and
/// subscribe, returning the transport Arc when the watch went live (`None` when
/// the connect or subscribe failed, so the clock falls back to the poller).
///
/// Both the connect and the subscribe run on `runtime`, so the alloy pubsub
/// service and the head watch's driving task land there and are cancelled with
/// it. The initial work runs in a task on that runtime (a nested `block_on`
/// would panic off the async caller) and reports back over a oneshot.
async fn connect_head_watch(
    hub: &Arc<Hub>,
    ws_url: &str,
    runtime: &OwnedPubsubRuntime,
) -> Option<Arc<dyn Provider<Ethereum>>> {
    let handle = runtime.handle();
    let hub_for_connect = Arc::clone(hub);
    let ws_url = ws_url.to_string();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    handle.spawn(async move {
        let transport = match AlloyProvider::new(&ws_url, DEFAULT_MAX_RETRIES).await {
            Ok(ws_provider) => {
                let transport = ws_provider.provider_arc();
                match HeadWatch::subscribe(
                    &hub_for_connect,
                    Arc::clone(&transport),
                    HeadWatchConfig::default(),
                    tokio::runtime::Handle::current(),
                )
                .await
                {
                    Ok(_) => Some(transport),
                    Err(error) => {
                        tracing::error!(
                            %error,
                            "head watch subscribe failed - falling back to 200ms head poll"
                        );
                        None
                    }
                }
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    "head watch WS connect failed - falling back to 200ms head poll"
                );
                None
            }
        };
        let _ = result_tx.send(transport);
    });
    result_rx.await.unwrap_or_else(|_| {
        tracing::error!("head watch connect task aborted - falling back to 200ms head poll");
        None
    })
}

async fn spawn_head_task(
    hub: Arc<Hub>,
    clock: HostedHeadClock,
    trigger: Option<HeadTrigger>,
    mut shutdown: watch::Receiver<bool>,
    head_edge_stopped: Arc<AtomicBool>,
    pubsub_runtime: Option<&OwnedPubsubRuntime>,
) -> Result<Option<Arc<dyn Provider<Ethereum>>>, HubError> {
    let mut watch_live = false;
    // The WS transport the edge owns for its lifetime; the owner keeps its own
    // clone and this one is dropped on exit (see this function's doc). `None`
    // when the boot hosts no WS watch (a poll or hub clock, or a WS connect
    // that failed at boot).
    let mut head_transport: Option<Arc<dyn Provider<Ethereum>>> = None;
    if let HostedHeadClock::Watch { ws_url, .. } = &clock {
        // The WS connect runs on the DEDICATED runtime, so the alloy pubsub
        // service it spawns lives there too — and is cancelled when `Drop`
        // tears that runtime down; the head watch's driving task spawns on the
        // same runtime for the same reason. The runtime is `Some` here by
        // construction: `mint` builds it exactly when the clock is a WS watch.
        if let Some(runtime) = pubsub_runtime {
            if let Some(transport) = connect_head_watch(&hub, ws_url, runtime).await {
                watch_live = true;
                head_transport = Some(transport);
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
    // The owner keeps its own Arc clone (criterion 2b); the edge keeps this one
    // and drops it on exit.
    let owned_transport = head_transport.clone();
    degenbot_core::runtime::get_runtime().spawn(async move {
        loop {
            // Select the stop signal against ONE head iteration so the edge
            // leaves promptly on drop instead of waiting out a poll/watch
            // bound — and drops its WS transport right after.
            let observed = tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                observed = head_iteration(
                    &mut head,
                    poll_only,
                    watch_live,
                    poll_provider.as_ref(),
                    last_published,
                ) => observed,
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
        // Teardown discipline: drop the WS transport WHILE the endpoint may
        // still be alive, so the pubsub service observes the closed request
        // channel and exits cleanly (never the reconnect loop). Then record the
        // clean-shutdown fact the teardown guard asserts.
        drop(head_transport);
        head_edge_stopped.store(true, Ordering::Relaxed);
        degenbot_core::op_info!(
            domain = pump,
            "hosted head edge stopped; WS transport dropped"
        );
    });
    Ok(owned_transport)
}

/// One head-edge iteration: resolve the next observed head, or `None` when the
/// watch was quiet inside its staleness bound. Split out so the spawned loop
/// can `select!` the stop signal against it.
///
/// In poll mode the poll is the primary clock and only a NEW head is returned
/// (`from_poll = true`); in watch/hub mode a header resolution is returned
/// directly, and a closed or stale watch falls back to one polled read.
async fn head_iteration(
    head: &mut HeadSubscription,
    poll_only: bool,
    watch_live: bool,
    poll_provider: Option<&Arc<AlloyProvider>>,
    last_published: u64,
) -> Option<(u64, bool)> {
    if poll_only {
        // The poll is the primary clock: publish + trigger each NEW head at
        // the poll cadence.
        tokio::time::sleep(HEAD_POLL_TICK).await;
        return match poll(poll_provider).await {
            Some(number) if number > last_published => Some((number, true)),
            _ => None,
        };
    }
    // Watch (or external-hub) mode: the head edge resolves on a header's
    // arrival; the 2s bound keeps the edge live while the head is quiet.
    match tokio::time::timeout(HEAD_WAIT, head.changed()).await {
        Ok(Ok(())) => head.borrow_and_update().map(|number| (number, false)),
        Ok(Err(_)) => {
            tracing::warn!("head source closed - polling");
            tokio::time::sleep(HEAD_POLL_TICK).await;
            poll(poll_provider).await.map(|number| (number, true))
        }
        Err(_) => {
            if watch_live && head.stale(HEAD_STALE) {
                tracing::warn!("head source stale - polling");
                tokio::time::sleep(HEAD_POLL_TICK).await;
                poll(poll_provider).await.map(|number| (number, true))
            } else {
                None
            }
        }
    }
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
fn spawn_feed_sampler(
    pumps: Arc<parking_lot::Mutex<Vec<PendingTxPump>>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut last: Vec<BackrunFeedStatus> = pumps.lock().iter().map(PendingTxPump::status).collect();
    degenbot_core::runtime::get_runtime().spawn(async move {
        loop {
            // A stop signal ends the sampler promptly; otherwise it ticks.
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                () = tokio::time::sleep(FEED_SAMPLE_TICK) => {}
            }
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
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

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
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedHeadClock::Hub),
            Some(trigger),
        )
        .expect("the hub clock mints");
        // The head edge is deferred: it must be started before it observes a
        // head (mint alone spawns nothing).
        hosted.start().await.expect("the hub clock starts");

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

    /// One head registration per process: a second started mint reuses the
    /// hosted head source instead of colliding with it. The registration is
    /// deferred to `start`, so both owners must start before the source exists.
    #[tokio::test]
    async fn a_second_mint_reuses_the_head_source() {
        let hub = Arc::new(Hub::new());
        let first = HostedSources::mint(Arc::clone(&hub), [], Some(HostedHeadClock::Hub), None)
            .expect("first mint");
        first.start().await.expect("first start");
        let second = HostedSources::mint(Arc::clone(&hub), [], Some(HostedHeadClock::Hub), None)
            .expect("second mint reuses the head source");
        second
            .start()
            .await
            .expect("second start reuses the head source");
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

    // ---- Engine-lifetime ownership + clean head-edge shutdown --------------

    /// A bounded poll over the head edge's completion flag: the flag flips on
    /// a detached task on the shared runtime, so the test waits a window
    /// instead of assuming one scheduler turn.
    async fn await_head_edge_stopped(flag: &Arc<AtomicBool>) -> bool {
        for _ in 0..1_000 {
            if flag.load(Ordering::Relaxed) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        flag.load(Ordering::Relaxed)
    }

    /// The `Watch` clock's fallback poll provider: a lazy HTTP endpoint (built
    /// without connecting), so the edge holds a provider handle with no live
    /// node behind it.
    async fn dead_loopback_poll_provider() -> Arc<AlloyProvider> {
        Arc::new(
            AlloyProvider::new("http://127.0.0.1:9", DEFAULT_MAX_RETRIES)
                .await
                .expect("a lazy HTTP provider builds without connecting"),
        )
    }

    /// A connection-recording listener: a blocking `std::net::TcpListener` on
    /// an ephemeral loopback port plus a thread that accepts and counts. A
    /// dial that reaches the port increments the counter, so a test can assert
    /// the counter stayed zero (no dial) or grew (a dial landed). `std`, not
    /// `tokio::net`, so the probe does not depend on a tokio `net` feature.
    pub(crate) fn counting_listener() -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicU64>) {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
        let addr = listener.local_addr().expect("the bound loopback address");
        let accepted = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = Arc::clone(&accepted);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stream.is_err() {
                    break;
                }
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        (addr, accepted)
    }

    /// A minimal WS server that serves the head watch's INITIAL connect
    /// (handshake + `eth_subscribe` reply) and then FAILS every later
    /// connection, so the alloy pubsub service spawns, the watch goes live, and
    /// the backend death drives the service's reconnect loop (the storm). This is
    /// the non-vacuous counterpart to [`counting_listener`]: a dead endpoint
    /// (`ws://127.0.0.1:9`) fails the eager connect BEFORE the service spawns
    /// (`alloy-pubsub service.rs`: `connector.connect()?` runs before
    /// `this.spawn()`), so no service -- and no storm -- can exist to observe.
    ///
    /// The accepted-connection COUNT is the observable. The first connection
    /// completes the handshake and answers the `eth_subscribe` (so
    /// `HeadWatch::subscribe` resolves and the watch goes live); every later
    /// connection is accepted then dropped WITHOUT a handshake, so the service's
    /// reconnect attempt errors and it stays in `reconnect_with_retries` (never
    /// returning to the request-channel `select!` arm). Counting accepts is
    /// independent of the best-effort global tracing subscriber (see the sibling
    /// `dropping_the_owner_leaves_no_pubsub_reconnect_storm` note), so the guard
    /// built on this server is non-vacuous even when another test owns the
    /// process-wide subscriber slot.
    ///
    /// Returns the `ws://host:port` URL, the accepted-connection counter, and the
    /// accept task's handle. Promoted `pub(crate)` beside [`counting_listener`]
    /// (the crate-local test-helper convention; see `arb_engine::tests::test_keys`).
    pub(crate) async fn accept_then_die_ws_server() -> (
        String,
        Arc<std::sync::atomic::AtomicU64>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind an ephemeral loopback port");
        let addr = listener.local_addr().expect("the bound loopback address");
        let connections = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = Arc::clone(&connections);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let attempt = counter.fetch_add(1, Ordering::Relaxed) + 1;
                if attempt == 1 {
                    // The head watch's initial connect: complete the handshake,
                    // answer the `eth_subscribe` so the watch goes live, then die.
                    tokio::spawn(serve_initial_connect(stream));
                } else {
                    // A reconnect attempt: fail the handshake so the service's
                    // `reconnect()` errors and it re-enters the retry loop.
                    drop(stream);
                }
            }
        });
        (format!("ws://{addr}"), connections, handle)
    }

    /// Serve the head watch's initial WS connect, then close so the backend dies.
    async fn serve_initial_connect(stream: tokio::net::TcpStream) {
        let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        // Answer every request the client sends until the `eth_subscribe`
        // (provider construction may issue a preliminary request first), then
        // give the client time to process the subscription-id reply BEFORE the
        // backend dies -- otherwise `connect_head_watch` falls back to the poller
        // and no storm exists to observe.
        while let Some(Ok(frame)) = ws.next().await {
            let Ok(text) = frame.to_text() else {
                continue;
            };
            let Ok(request) = serde_json::from_str::<serde_json::Value>(text) else {
                continue;
            };
            let id = request
                .get("id")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let method = request
                .get("method")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let result = if method == "eth_subscribe" {
                serde_json::Value::String("0x1".to_string())
            } else {
                serde_json::Value::Null
            };
            let reply = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            })
            .to_string();
            let _ = ws.send(WsMessage::Text(reply.into())).await;
            if method == "eth_subscribe" {
                tokio::time::sleep(Duration::from_millis(500)).await;
                break;
            }
        }
        drop(ws);
    }

    /// The dial-free construction probe: minting with a `Watch` clock AND a
    /// feed arm performs zero connection attempts. Both the head watch and the
    /// feed point at a live listener that counts acceptances, so a dial at mint
    /// would land here — the test is red if either one connects before start.
    #[tokio::test]
    async fn minting_performs_zero_connection_attempts() {
        let (addr, accepted) = counting_listener();
        let hub = Arc::new(Hub::new());
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [(
                PendingTxSource::Mevblocker,
                PendingTxFeedConfig::Mevblocker(BackrunFeedConfig {
                    url: format!("ws://{addr}"),
                    ..BackrunFeedConfig::for_mainnet()
                }),
            )],
            Some(HostedHeadClock::Watch {
                ws_url: format!("ws://{addr}"),
                provider: dead_loopback_poll_provider().await,
            }),
            None,
        )
        .expect("the watch clock and the feed arm mint");

        // A window long enough for any (wrong) dial to land.
        tokio::time::sleep(Duration::from_millis(750)).await;
        assert_eq!(
            accepted.load(Ordering::Relaxed),
            0,
            "mint must perform zero connection attempts"
        );
        drop(hosted);
    }

    /// The deferred dials launch at the start boundary: after `start`, the
    /// head-watch WS connect and the feed pump both reach the listener.
    #[tokio::test]
    async fn starting_performs_the_deferred_dials() {
        let (addr, accepted) = counting_listener();
        let hub = Arc::new(Hub::new());
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [(
                PendingTxSource::Mevblocker,
                PendingTxFeedConfig::Mevblocker(BackrunFeedConfig {
                    url: format!("ws://{addr}"),
                    ..BackrunFeedConfig::for_mainnet()
                }),
            )],
            Some(HostedHeadClock::Watch {
                ws_url: format!("ws://{addr}"),
                provider: dead_loopback_poll_provider().await,
            }),
            None,
        )
        .expect("the watch clock and the feed arm mint");
        assert_eq!(
            accepted.load(Ordering::Relaxed),
            0,
            "no dial before the start boundary"
        );

        hosted.start().await.expect("the deferred I/O starts");

        for _ in 0..500 {
            if accepted.load(Ordering::Relaxed) >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            accepted.load(Ordering::Relaxed) >= 2,
            "start must dial both the head watch and the feed pump"
        );
        drop(hosted);
    }

    /// Dropping a never-started owner is clean: nothing dialed, no actor ever
    /// ran on the owned runtime, and the head edge's completion flag never
    /// flips (there was no edge to stop).
    #[tokio::test]
    async fn dropping_a_never_started_owner_tears_down_cleanly() {
        let (addr, accepted) = counting_listener();
        let hub = Arc::new(Hub::new());
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [(
                PendingTxSource::Mevblocker,
                PendingTxFeedConfig::Mevblocker(BackrunFeedConfig {
                    url: format!("ws://{addr}"),
                    ..BackrunFeedConfig::for_mainnet()
                }),
            )],
            Some(HostedHeadClock::Watch {
                ws_url: format!("ws://{addr}"),
                provider: dead_loopback_poll_provider().await,
            }),
            None,
        )
        .expect("the watch clock and the feed arm mint");
        let stopped = hosted.head_edge_stopped();

        drop(hosted);

        // The owned runtime dropped empty: no pump, sampler, or head edge ever
        // spawned, so nothing dials after the drop either.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            accepted.load(Ordering::Relaxed),
            0,
            "a never-started owner must dial nothing, before or after its drop"
        );
        assert!(
            !stopped.load(Ordering::Relaxed),
            "the head edge never ran, so its completion flag must not flip"
        );
    }

    /// Dropping the owner stops the head edge: the edge's completion flag
    /// flips, proving the clean-shutdown path ran (the edge dropped its WS
    /// transport and recorded the fact) rather than merely being unscheduled.
    ///
    /// The WS endpoint is the suite's dead loopback: the WS connect fails at
    /// start, the watch falls back to the poller, and the guard asserts the
    /// shutdown path itself — the dead endpoint is never required to connect.
    /// The head edge is deferred to `start`, so the guard starts the owner
    /// before it drops it.
    #[tokio::test]
    async fn dropping_the_owner_stops_the_head_edge() {
        let hub = Arc::new(Hub::new());
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedHeadClock::Watch {
                ws_url: "ws://127.0.0.1:9".to_string(),
                provider: dead_loopback_poll_provider().await,
            }),
            None,
        )
        .expect("the watch clock mints (the dead WS falls back to the poller)");
        hosted
            .start()
            .await
            .expect("the deferred head edge starts (the dead WS falls back to the poller)");

        let stopped = hosted.head_edge_stopped();
        assert!(
            !stopped.load(Ordering::Relaxed),
            "the edge is live before the owner drops"
        );

        drop(hosted);

        assert!(
            await_head_edge_stopped(&stopped).await,
            "dropping the owner must stop the head edge (the clean-shutdown path ran)"
        );
    }

    /// The teardown guard, mirroring `degenbot-fork` node.rs's
    /// `leaked_provider_clone_emits_no_reconnect_storm` /
    /// `drop_shuts_down_pubsub_cleanly`: a capture subscriber records the alloy
    /// pubsub service's own events, the owner is dropped, and the observation
    /// window must hold no `Reconnection attempt` /
    /// `Pubsub service backend error` event.
    ///
    /// The global subscriber slot is best-effort (another test in this binary
    /// may own it, and under `--features otel` this test is excluded so it
    /// never competes for it); the positive half — the head edge's clean
    /// shutdown flag — is independent of the subscriber, so the assertion that
    /// the shutdown path ran is never vacuous.
    #[cfg(not(feature = "otel"))]
    #[tokio::test]
    async fn dropping_the_owner_leaves_no_pubsub_reconnect_storm() {
        let _guard = capture::lock().await;
        capture::install();
        let hub = Arc::new(Hub::new());
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedHeadClock::Watch {
                ws_url: "ws://127.0.0.1:9".to_string(),
                provider: dead_loopback_poll_provider().await,
            }),
            None,
        )
        .expect("the watch clock mints (the dead WS falls back to the poller)");
        hosted
            .start()
            .await
            .expect("the deferred head edge starts (the dead WS falls back to the poller)");
        let stopped = hosted.head_edge_stopped();
        capture::clear();
        drop(hosted);
        assert!(
            await_head_edge_stopped(&stopped).await,
            "the clean-shutdown path must run"
        );
        // A short window past the drop: a reconnect loop's first attempt
        // precedes any backoff, so it would surface almost immediately.
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        let bad: Vec<String> = capture::events()
            .into_iter()
            .filter(|event| {
                event.contains("Reconnection attempt")
                    || event.contains("Pubsub service backend error")
            })
            .collect();
        assert!(
            bad.is_empty(),
            "dropping the owner left a pubsub reconnect storm: {bad:?}"
        );
    }

    /// Bounded poll for the accepted-connection counter to reach `want`.
    /// `false` on timeout: the caller's non-vacuity assertion then trips instead
    /// of the guard passing vacuously.
    async fn wait_for_connections(
        connections: &Arc<std::sync::atomic::AtomicU64>,
        want: u64,
        window: Duration,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + window;
        while tokio::time::Instant::now() < deadline {
            if connections.load(Ordering::Relaxed) >= want {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        connections.load(Ordering::Relaxed) >= want
    }

    /// The NON-VACUOUS teardown guard. Its sibling
    /// `dropping_the_owner_leaves_no_pubsub_reconnect_storm` runs against a dead
    /// endpoint (`ws://127.0.0.1:9`), so the eager WS connect FAILS before
    /// `PubSubService::connect` spawns its service (`alloy-pubsub service.rs`:
    /// `connector.connect()?` runs first, then `this.spawn()`) -- no service
    /// exists and the no-storm half is vacuously true. This guard points the
    /// head watch at `accept_then_die_ws_server`: the initial connect +
    /// `eth_subscribe` SUCCEED (the service spawns on the owner's dedicated
    /// runtime and the watch goes live), the backend dies, and the service's
    /// reconnect loop is OBSERVED RUNNING before the drop (the non-vacuity
    /// assertion: a SECOND accepted connection proves a reconnect attempt). The
    /// owner is then dropped mid-storm and the guard asserts NO further accepted
    /// connection lands.
    ///
    /// The storm is observed through the server's accepted-connection COUNT, not
    /// the process-wide tracing subscriber: that slot is best-effort (the sibling
    /// guard documents the same), so a capture-based assertion would be silently
    /// vacuous whenever another test in the binary owns the subscriber -- exactly
    /// the defect this guard exists to remove.
    ///
    /// RED-capability: neutralizing the `OwnedPubsubRuntime` teardown in
    /// `Drop for HostedSources` (leaking the runtime instead of dropping it)
    /// leaves the service's reconnect loop alive past the drop, so another
    /// reconnect attempt lands on the server and the connection count grows.
    #[tokio::test]
    async fn dropping_the_owner_mid_storm_leaves_no_new_pubsub_reconnect() {
        let (ws_url, connections, server) = accept_then_die_ws_server().await;
        let hub = Arc::new(Hub::new());
        let hosted = HostedSources::mint(
            Arc::clone(&hub),
            [],
            Some(HostedHeadClock::Watch {
                ws_url,
                provider: dead_loopback_poll_provider().await,
            }),
            None,
        )
        .expect("the watch clock mints");
        hosted
            .start()
            .await
            .expect("the head watch connects and subscribes (the server accepts then dies)");

        // Non-vacuity: the initial backend died, so the pubsub service's
        // reconnect loop must land a SECOND connection before the drop.
        let reconnected = wait_for_connections(&connections, 2, Duration::from_secs(10)).await;
        assert!(
            reconnected,
            "vacuous guard: the service never reconnected before the drop (it never spawned, or its backend never died)"
        );

        let stopped = hosted.head_edge_stopped();
        drop(hosted);
        assert!(
            await_head_edge_stopped(&stopped).await,
            "the clean-shutdown path must run"
        );
        // Let any reconnect accepted at the drop instant land and the dedicated
        // runtime teardown settle, then take the baseline.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let before = connections.load(Ordering::Relaxed);
        // A live reconnect loop (3s base backoff, then exponential) lands
        // another accepted connection inside this window; a torn-down one lands
        // none.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let after = connections.load(Ordering::Relaxed);
        assert_eq!(
            after,
            before,
            "dropping the owner mid-storm left {} new reconnect connections",
            after.saturating_sub(before)
        );
        server.abort();
    }

    /// Criterion 2e: the JOIN provider (`AnyRequestTransport::request_provider`
    /// over the request-scoped URI) takes the SAME `build_provider` WS path as
    /// the head-watch provider. Empirically a WS connect to a dead endpoint
    /// FAILS at `WsConnect::connect` BEFORE `PubSubService::connect` spawns its
    /// service task (alloy-pubsub 2.5.0 `service.rs`: `connector.connect()?`
    /// runs first, then `this.spawn()`), so a dead endpoint leaks NOTHING —
    /// there is no service to retry. This pins that: the connect fails and the
    /// capture subscriber records no reconnect/backend-error event, so no
    /// dedicated-runtime reroute is needed for the join provider.
    ///
    /// (It is green here because the leak does not exist; it would go red if the
    /// connect path ever started spawning a service before a successful
    /// connect.)
    #[cfg(not(feature = "otel"))]
    #[tokio::test]
    async fn a_dead_endpoint_ws_connect_leaks_no_pubsub_service() {
        let _guard = capture::lock().await;
        capture::install();
        capture::clear();
        let outcome = AlloyProvider::new("ws://127.0.0.1:9", DEFAULT_MAX_RETRIES).await;
        assert!(
            outcome.is_err(),
            "a dead loopback WS connect must fail, not connect"
        );
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        let bad: Vec<String> = capture::events()
            .into_iter()
            .filter(|event| {
                event.contains("Reconnection attempt")
                    || event.contains("Pubsub service backend error")
            })
            .collect();
        assert!(
            bad.is_empty(),
            "a failed WS connect leaked a pubsub reconnect storm: {bad:?}"
        );
    }

    /// A capture subscriber mirroring `degenbot-fork` node.rs: it records the
    /// alloy pubsub service's events so a teardown test can assert no
    /// reconnect/backend-error storm. `install` is best-effort (first-wins on
    /// the process-wide subscriber slot).
    #[cfg(not(feature = "otel"))]
    mod capture {
        use std::sync::{Arc, OnceLock};

        type Events = Arc<parking_lot::Mutex<Vec<String>>>;

        static CAPTURED: OnceLock<Events> = OnceLock::new();

        /// Serializes the clear/observe window across tests (one process-wide
        /// capture vec); a `tokio` mutex because the guard is held across
        /// `await`s.
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

        struct CaptureLayer {
            events: Events,
        }

        #[derive(Default)]
        struct MsgVisitor(Option<String>);

        impl tracing::field::Visit for MsgVisitor {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "message" {
                    self.0 = Some(value.to_string());
                }
            }

            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
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
                let message = format!(
                    "[{}] {}",
                    meta.level(),
                    visitor.0.unwrap_or_else(|| "<no message>".into())
                );
                self.events.lock().push(message);
            }
        }

        pub(super) fn install() {
            static INIT: std::sync::Once = std::sync::Once::new();
            INIT.call_once(|| {
                use tracing_subscriber::layer::SubscriberExt as _;
                use tracing_subscriber::util::SubscriberInitExt as _;
                let events: Events = Arc::new(parking_lot::Mutex::new(Vec::new()));
                // A re-install (or a slot another test already holds) is fine:
                // the capture is best-effort and the positive assertion does
                // not depend on it.
                let _ = CAPTURED.set(Arc::clone(&events));
                let _ = tracing_subscriber::registry()
                    .with(CaptureLayer { events })
                    .try_init();
            });
        }

        pub(super) async fn lock() -> tokio::sync::MutexGuard<'static, ()> {
            LOCK.lock().await
        }

        pub(super) fn clear() {
            if let Some(events) = CAPTURED.get() {
                events.lock().clear();
            }
        }

        #[must_use]
        pub(super) fn events() -> Vec<String> {
            CAPTURED
                .get()
                .map(|events| events.lock().clone())
                .unwrap_or_default()
        }
    }
}
