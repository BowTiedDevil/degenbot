//! The engine session's strategy-plane boot: the one home of the composition
//! order that mints the hub, admits the facets, attaches the planes, and
//! starts them.
//!
//! The order is load-bearing, and each step reads the previous one's product:
//!
//! 1. **Mint.** [`StrategyHost::mint`] creates the session's fresh hub, and this
//!    boot CLAIMS that hub's one head source before matching on the hosted
//!    input, so no session shape is ever headless.
//! 2. **Register.** Every row of the caller's facet table enters the driver
//!    FSM in [`DriverPose::Registered`], carrying its admission status and the
//!    loop its driver runs as values. A crate that can read the typed config
//!    resolves that status, so this module authors no activation, stance, or
//!    settlement policy.
//! 3. **Attach.** The submission-truth arm ([`HeadReconciler`]) and the state
//!    root each enabled lane's namespace resolves under, then the hosted
//!    sources (one pending-tx pump per arm) and each arm's spawn factory, which
//!    the boot hands the arm's own typed stream. The loop a facet row names in
//!    [`FacetLoop`] is composed here too, over the driver this boot mints, so
//!    every admitted facet leaves the boot with the factory its `Enabled` pose
//!    promises.
//! 4. **Start** ([`EngineSession::start`]). The hosted sources' deferred I/O
//!    launches before any driver, and each arm's factory already carries the
//!    session's head subscription, so a driver's loop always reads the one clock
//!    the whole session shares.
//!
//! The boot returns handles and runs no consume loop (ADR-050 D9). Admission
//! and lifecycle stay with the host (ADR-057): this module orders calls into
//! it and never authors a legality rule of its own.
//!
//! The plane inputs arrive as values this crate cannot build for itself. The
//! route registry, the facet statuses, the arm feeds, the head trigger, and
//! the reconciler come from crates that depend on this one (the strategy and
//! submission planes), so they are resolved by the composition root and this
//! boot only places them in order.

use std::path::PathBuf;
use std::sync::Arc;

use degenbot_eventhub::{HeadSender, HeadSubscription, HubError, PendingTxSource};
use degenbot_rpc::pending_tx_stream::{PendingTxFeedConfig, PendingTxStream};
use degenbot_substrate::driver_spawn::DriverSpawnFactory;
use degenbot_substrate::nonce::{NonceAuthority, StrategyId};
use degenbot_substrate::RouteRegistry;
use parking_lot::Mutex;

use super::Bot;
use crate::arb_engine::{EngineChannelHandles, EngineDriver, EngineStages};
use crate::hosted_sources::{
    HeadTrigger, HostedClockBinding, HostedHeadClock, HostedSources, HostedStartError,
    StreamRefusal,
};
use crate::strategy_host::{FacetStatus, HeadReconciler, HostError, HostSupervisor, StrategyHost};

/// One facet admitted at boot.
pub struct FacetBoot {
    /// The plane name the facet registers and is enabled under — its
    /// registration id and its typed-config section name are the same string.
    pub id: StrategyId,
    /// The admission status the host serves `enable` from.
    pub facet: FacetStatus,
    /// The loop this facet's driver runs, when the boot composes it.
    ///
    /// A facet whose loop is a hosted pending-tx arm names none: its factory
    /// travels with its [`HostedArmBoot`]. The settlement row names the pump,
    /// whose factory this boot builds over the session's one driver.
    pub driver: Option<FacetLoop>,
}

/// The loop one facet's driver runs, as the boot composes it.
///
/// The distinction is a VALUE on the facet row, not a name the boot matches:
/// the composition root decides which facet the engine's own pump drives, and
/// the boot wires whatever it is handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacetLoop {
    /// The facet's block clock IS the engine pump session, so its driver's
    /// loop is that session's lifetime ([`crate::arb_engine::settlement_driver`]).
    Pump,
}

/// Composes one hosted arm's spawn factory.
///
/// The boot hands it the arm's typed stream and the session hub's head
/// subscription, so the strategy crate that owns the loop builds the factory
/// without this crate naming it. A stream is absent when the arm's pump was not
/// minted; the strategy boot then refuses its live mode, which is the same
/// refusal a feed-less boot always produced. The subscription is never absent:
/// the session claims the hub's one head source before any arm is attached.
pub type ArmSpawnComposer =
    Box<dyn FnOnce(Option<PendingTxStream>, HeadSubscription) -> DriverSpawnFactory + Send>;

/// One hosted arm: the feed its pump dials at the start boundary, and the
/// builder for its driver's spawn factory.
///
/// The two couplings differ, and [`HostedArmFeed`] is the one value that says
/// which: an arm whose endpoint the boot could not resolve provisions no pump
/// (and hands its factory no stream, so the driver halts at its live-mode gate
/// rather than silently never starting), while the factory itself is attached
/// for every arm the boot hosts. A facet with no arm — one the caller left
/// inactive — gets neither, since its admission status already refuses the
/// enable.
pub struct HostedArmBoot {
    /// The facet whose driver loop this arm hosts. It must also appear in the
    /// facet table, and it is the id the host attaches the factory under.
    pub id: StrategyId,
    /// The arm's feed: the resolved config its pump dials, or the source kind
    /// whose endpoint the boot could not resolve.
    pub feed: HostedArmFeed,
    /// Builds the arm's spawn factory from the booted hub and the arm's stream.
    pub spawn: ArmSpawnComposer,
}

/// One hosted arm's feed: the config the boot resolved, or the source kind whose
/// endpoint it could not resolve.
///
/// The source kind and the config cannot disagree: [`Self::Resolved`] derives the
/// kind from the config it carries, and [`Self::Unresolved`] carries a kind with
/// no feed at all. A boot that named the mevblocker arm with the txpool arm's
/// config is therefore unrepresentable rather than a mint-time refusal.
#[derive(Debug, Clone)]
pub enum HostedArmFeed {
    /// The arm's feed, ready for its pump to dial at the start boundary.
    Resolved(PendingTxFeedConfig),
    /// The arm's source kind, whose endpoint the boot could not resolve. No
    /// pump is provisioned, so the arm's driver halts at its live-mode gate.
    Unresolved(PendingTxSource),
}

impl HostedArmFeed {
    /// The source kind this arm hosts.
    #[must_use]
    pub fn source(&self) -> PendingTxSource {
        match self {
            Self::Resolved(cfg) => cfg.source(),
            Self::Unresolved(source) => *source,
        }
    }

    /// The feed config, when the boot resolved one.
    #[must_use]
    pub fn config(&self) -> Option<&PendingTxFeedConfig> {
        match self {
            Self::Resolved(cfg) => Some(cfg),
            Self::Unresolved(_) => None,
        }
    }
}

/// Composes the per-head trigger the hosted head edge fires, given the shared
/// host the boot minted.
///
/// The composition root builds its `HeadReconciliation` over that very host,
/// and the reconciliation type lives in a crate that depends on this one, so
/// the boot hands it the host rather than accepting a finished trigger whose
/// subject would have to exist first.
pub type HeadTriggerComposer = Box<dyn FnOnce(&Arc<Mutex<StrategyHost>>) -> HeadTrigger + Send>;

/// The hosted sources' boot: the arms to provision, the head clock to host, and
/// the trigger composer.
pub struct HostedBoot {
    /// One row per active arm, in host registration order.
    pub arms: Vec<HostedArmBoot>,
    /// The head clock the session hosts. `None` hosts no head edge, so no hub
    /// head source is registered and no clock is published.
    pub clock: Option<HostedHeadClock>,
    /// Composes the per-head trigger once the host exists.
    pub trigger: Option<HeadTriggerComposer>,
}

/// The construction products and shared plane resources a boot composes over.
pub struct SessionConstruction {
    /// The shared `Bot` the driver's pump session runs against.
    pub bot: Arc<Bot>,
    /// The stage surface the driver crosses.
    pub stages: Arc<EngineStages>,
    /// The route registry the host serves admission membership from.
    pub registry: Arc<RouteRegistry>,
    /// The nonce authority every hosted lane stamps through. The caller that
    /// builds per-strategy lanes builds them over this same `Arc`.
    pub nonce: Arc<NonceAuthority>,
    /// The submission-truth arm the head feed reconciles against, when the
    /// caller owns one. A host without one still refreshes the authority per
    /// head; it simply has no submission records to close out.
    pub reconciler: Option<Arc<dyn HeadReconciler>>,
    /// The root each enabled lane's namespace resolves under. A boot with no
    /// resolvable root leaves the drivers on their process-global paths.
    pub state_root: Option<PathBuf>,
}

/// What `provision_hosted` returns: the hosted sources the session owns and the
/// claimed head publisher the driver routes to the clock's owner.
#[derive(Default)]
pub struct HostedProvisioning {
    /// The hosted sources, when the boot hosted any.
    pub hosted: Option<HostedSources>,
    /// The claimed head publisher this session's pump publishes through, when
    /// the pump owns the clock (`PumpOwned`, or a boot that declared no clock).
    /// `None` when a host-side watch or poller owns it.
    pub pump_publisher: Option<HeadSender>,
}

/// Why a session boot refused to compose the plane, or refused to start it.
#[derive(Debug, thiserror::Error)]
pub enum SessionBootError {
    /// The host refused a facet registration or a spawn attachment — two rows
    /// under one name, which no boot may produce.
    #[error(transparent)]
    Host(#[from] HostError),
    /// The hosted sources could not be minted over the booted hub.
    #[error(transparent)]
    Hosted(#[from] HubError),
    /// The hosted sources refused a start transition — an in-flight start, a
    /// start after a failed launch, or a torn-down owner. The typed refusals
    /// reach the caller unchanged.
    #[error(transparent)]
    HostedStart(#[from] HostedStartError),
    /// An arm's stream was already handed out: two arm rows named one source
    /// kind, which the boot may not produce.
    #[error(transparent)]
    Stream(#[from] StreamRefusal),
}

/// A booted engine session: the driver attached to the host's hub, the shared
/// host every consumer reaches the plane through, and the hosted sources the
/// session owns for its whole life.
///
/// The handles are the product; no consume loop runs here (ADR-050 D9). The
/// owning consumer drives the pump, enables facets, and calls [`Self::start`]
/// at its own start boundary.
pub struct EngineSession {
    /// The session's driver, attached to `host`'s hub.
    driver: Arc<EngineDriver>,
    /// The shared host: the hub, the route registry, the nonce authority, and
    /// the driver FSM records.
    host: Arc<Mutex<StrategyHost>>,
    /// The pending-tx pumps and the head clock, when the boot hosted any.
    hosted: Option<HostedSources>,
    /// The host-owned supervisor for the drivers [`Self::start_drivers`]
    /// boots.
    supervisor: HostSupervisor,
}

impl EngineSession {
    /// Boot the plane: mint, register, attach, install, and return the handles.
    ///
    /// `facets` registers in table order; the order is the order `list()`
    /// reports and the order an operator reads. `hosted` is `None` for a
    /// session that hosts no pending-tx source and no head clock — such a
    /// session still admits and drives facets, it just has no feed to pump.
    ///
    /// Nothing here starts: the pumps are provisioned unstarted and the drivers
    /// are attached, not launched.
    ///
    /// # Errors
    ///
    /// [`SessionBootError::Host`] when the host refuses the engine's channel
    /// declaration, a registration, or an attachment (two facet rows, or an arm
    /// under an unregistered id), and [`SessionBootError::Hosted`] when the
    /// hosted sources cannot be minted.
    ///
    /// # Panics
    ///
    /// Panics if the freshly minted session hub's head source is already
    /// claimed. [`StrategyHost::mint`] always returns a fresh hub, so the first
    /// claim is an invariant of this construction and a panic means that
    /// invariant was violated, not a recoverable boot failure.
    pub fn boot(
        construction: SessionConstruction,
        facets: Vec<FacetBoot>,
        hosted: Option<HostedBoot>,
    ) -> Result<Self, SessionBootError> {
        let SessionConstruction {
            bot,
            stages,
            registry,
            nonce,
            reconciler,
            state_root,
        } = construction;

        // Mint: the engine's channel declaration is validated and its named
        // channels are registered inside the exclusive construction window, and
        // the host owns the hub afterwards.
        let (host, attached) =
            StrategyHost::mint(registry, nonce, EngineChannelHandles::registration())
                .map_err(HostError::ChannelRegistration)?;
        let host = Arc::new(Mutex::new(host));

        // Claim the session hub's one head source on the freshly minted hub,
        // before matching on the hosted input: a session whose clock nobody
        // claimed would be headless in every shape. `StrategyHost::mint` always
        // returns a fresh hub, so this first claim cannot fail.
        #[expect(
            clippy::expect_used,
            reason = "StrategyHost::mint always returns a fresh hub, so its head source is unclaimed by construction"
        )]
        let head_sender = host
            .lock()
            .hub()
            .claim_head_source()
            .expect("a freshly minted session hub's head source is unclaimed");

        // Register the facet table in the caller's order, then the shared plane
        // attachments. The rows that name a driver loop are kept for the attach
        // step below, which needs the driver this boot mints last.
        let mut pump_driven: Vec<StrategyId> = Vec::new();
        {
            let mut guard = host.lock();
            for facet in facets {
                if let Some(FacetLoop::Pump) = facet.driver {
                    pump_driven.push(facet.id.clone());
                }
                guard.register(facet.id, facet.facet)?;
            }
            if let Some(reconciler) = reconciler {
                guard.attach_reconciler(reconciler);
            }
            if let Some(root) = state_root {
                guard.set_state_root(root);
            }
        }

        // Provision the hosted sources and attach each arm's factory, which
        // needs the arm's stream the provisioning step hands out. The claimed
        // publisher is routed to exactly one owner: the host-side watch or
        // poller when the boot declares one, otherwise this driver's pump.
        let HostedProvisioning {
            hosted,
            pump_publisher,
        } = match hosted {
            Some(boot) => provision_hosted(&host, boot, head_sender)?,
            None => HostedProvisioning {
                hosted: None,
                pump_publisher: Some(head_sender),
            },
        };

        // Attach the driver to the hub the host minted, then attach the factory
        // of every facet the pump drives: that loop IS this session's lifetime,
        // so its factory is built over the driver it must watch.
        let driver = Arc::new(EngineDriver::from_stages_with_hub(
            bot,
            stages,
            attached,
            pump_publisher,
        ));
        for id in pump_driven {
            let spawn = crate::arb_engine::settlement_driver::settlement_spawn(Arc::clone(&driver));
            host.lock().attach_spawn(&id, spawn)?;
        }
        let supervisor = HostSupervisor::new(Arc::clone(&host));
        Ok(Self {
            driver,
            host,
            hosted,
            supervisor,
        })
    }

    /// The session's driver.
    #[must_use]
    pub fn driver(&self) -> &Arc<EngineDriver> {
        &self.driver
    }

    /// The shared strategy host.
    #[must_use]
    pub fn host(&self) -> &Arc<Mutex<StrategyHost>> {
        &self.host
    }

    /// The hosted sources, when the boot provisioned any.
    #[must_use]
    pub fn hosted(&self) -> Option<&HostedSources> {
        self.hosted.as_ref()
    }

    /// Take the hosted sources out of the session so their teardown runs at the
    /// caller's chosen moment instead of at the session's drop.
    pub fn take_hosted(&mut self) -> Option<HostedSources> {
        self.hosted.take()
    }

    /// The supervisor owning the lane boundaries of every driver the session
    /// started.
    #[must_use]
    pub fn supervisor(&self) -> &HostSupervisor {
        &self.supervisor
    }

    /// Launch the session's deferred I/O and then its drivers — the one start
    /// boundary.
    ///
    /// The hosted sources start first because the head edge must attach to the
    /// session hub's one head source (the pump owns it) before a driver's loop
    /// subscribes that same source. Returns the number of drivers booted.
    ///
    /// # Errors
    ///
    /// [`SessionBootError::HostedStart`] when the hosted sources refuse the
    /// start transition — a miswired boot whose head source cannot be
    /// registered lands half-started (never running), and a retry on that
    /// session is a typed `StartAfterFailure`, so the caller must drop it and
    /// boot again. [`SessionBootError::Host`] when the host refuses a driver's
    /// start.
    pub async fn start(&self) -> Result<usize, SessionBootError> {
        if let Some(hosted) = self.hosted.as_ref() {
            hosted.start().await?;
        }
        self.start_drivers().map_err(SessionBootError::from)
    }

    /// Boot every enabled facet's driver loop and supervise their lane
    /// boundaries, so a terminal exit folds through the host FSM (a self-halt
    /// becomes a tombstone) without exposing the fold.
    ///
    /// Every admitted facet leaves the boot with a factory — a hosted arm's
    /// from its arm row, the pump-driven facet's from the loop this boot
    /// composes over the session driver — so the host refuses an enabled record
    /// with no loop ([`HostError::MissingSpawn`]) instead of leaving it
    /// silently unstarted.
    ///
    /// # Errors
    ///
    /// [`HostError`] when an enabled driver has no loop, or the lifecycle
    /// refuses a driver's start.
    pub fn start_drivers(&self) -> Result<usize, HostError> {
        let tasks = self.host.lock().start_driving()?;
        let count = tasks.len();
        self.supervisor.supervise(tasks);
        Ok(count)
    }
}

/// Provision the hosted sources and attach every arm's spawn factory.
///
/// `head_sender` is the session hub's claimed head publisher. The boot's public
/// [`HostedHeadClock`] DECLARATION is converted here into the post-claim
/// [`HostedClockBinding`] the hosted sources consume: a `Watch`/`Poll` boot
/// moves the publisher into the owner, and a `PumpOwned` boot (or a boot that
/// declared no clock) routes it back to the driver's pump.
fn provision_hosted(
    host: &Arc<Mutex<StrategyHost>>,
    boot: HostedBoot,
    head_sender: HeadSender,
) -> Result<HostedProvisioning, SessionBootError> {
    let HostedBoot {
        arms,
        clock,
        trigger,
    } = boot;
    // Clone the hub outside the guarded statement: no nested host lock may run
    // while the guard is alive (the mutex is not reentrant).
    let hub = Arc::clone(host.lock().hub());
    let trigger = trigger.map(|compose| compose(host));
    let feeds = arms
        .iter()
        .filter_map(|arm| arm.feed.config().cloned())
        .collect::<Vec<_>>();
    let (binding, pump_publisher) = match clock {
        None => (None, Some(head_sender)),
        Some(HostedHeadClock::PumpOwned) => {
            (Some(HostedClockBinding::PumpOwned), Some(head_sender))
        }
        Some(HostedHeadClock::Watch { ws_url, provider }) => (
            Some(HostedClockBinding::Watch {
                ws_url,
                provider,
                sender: head_sender,
            }),
            None,
        ),
        Some(HostedHeadClock::Poll { provider }) => (
            Some(HostedClockBinding::Poll {
                provider,
                sender: head_sender,
            }),
            None,
        ),
    };
    let mut hosted = HostedSources::mint(Arc::clone(&hub), feeds, binding, trigger)?;
    let head_subscription = hub.subscribe_head();
    for arm in arms {
        // An arm with no minted pump is handed no stream (its driver halts at
        // its live-mode gate); a stream already handed out is a miswired boot
        // and fails closed.
        let stream = match hosted.take_stream(arm.feed.source()) {
            Ok(stream) => Some(stream),
            Err(StreamRefusal::NoPump(_)) => None,
            Err(refusal @ StreamRefusal::AlreadyTaken(_)) => {
                return Err(SessionBootError::Stream(refusal))
            }
        };
        let spawn = (arm.spawn)(stream, head_subscription.clone());
        host.lock().attach_spawn(&arm.id, spawn)?;
    }
    Ok(HostedProvisioning {
        hosted: Some(hosted),
        pump_publisher,
    })
}

impl Drop for EngineSession {
    fn drop(&mut self) {
        // The hosted sources stop their pumps and tear down the head edge's
        // runtime, so they go first — before the host and the driver they feed.
        drop(self.hosted.take());
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions fail loudly"
)]
mod tests {
    use super::*;
    use degenbot_eventhub::Hub;
    use degenbot_rpc::backrun_feed::BackrunFeedConfig;
    use degenbot_rpc::txpool_feed::TxpoolFeedConfig;
    use degenbot_substrate::connector_index::V2ConnectorIndex;
    use degenbot_substrate::driver_spawn::DriverExit;
    use degenbot_substrate::session_registry::SessionObjectRegistry;
    use degenbot_substrate::state_lock::StateLock;
    use degenbot_substrate::BotState;
    use std::time::Duration;

    /// A closed local port: every dial is refused immediately, so a feed's
    /// reconnect counter witnesses the dial without a live endpoint. The
    /// module-level dial discipline (zero at mint, deferred to the start
    /// boundary) is pinned against a counting listener by
    /// `crate::hosted_sources::tests::minting_performs_zero_connection_attempts`;
    /// these tests pin the composition that orders it.
    const CLOSED_FEED: &str = "ws://127.0.0.1:1";

    /// The plane names a boot registers, spelled as the strategy plane's
    /// canonical facet names.
    const FACETS: [&str; 3] = ["settlement", "mevblocker_backrun", "txpool_backrun"];

    /// The construction products plus the in-memory route registry and nonce
    /// authority a test session composes over — no database, no provider.
    fn construction() -> SessionConstruction {
        let core = Arc::new(StateLock::new(BotState::new()));
        let bot = Arc::new(Bot::with_core(
            Arc::clone(&core),
            Arc::new(SessionObjectRegistry::new(1)),
        ));
        let stages = Arc::new(EngineStages::with_core(core, bot.active_delta()));
        SessionConstruction {
            bot,
            stages,
            registry: Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
            nonce: Arc::new(NonceAuthority::new(1)),
            reconciler: None,
            state_root: None,
        }
    }

    /// The facet table, with the settlement row naming the pump loop: the
    /// composition root decides which facet the engine's pump drives, and the
    /// boot wires the loop it is handed.
    fn facet_table(status: impl Fn(&str) -> FacetStatus) -> Vec<FacetBoot> {
        FACETS
            .into_iter()
            .map(|name| FacetBoot {
                id: StrategyId::new(name),
                facet: status(name),
                driver: (name == "settlement").then_some(FacetLoop::Pump),
            })
            .collect()
    }

    fn feed_for(source: PendingTxSource) -> PendingTxFeedConfig {
        match source {
            PendingTxSource::Mevblocker => PendingTxFeedConfig::Mevblocker(BackrunFeedConfig {
                url: CLOSED_FEED.to_string(),
                ..BackrunFeedConfig::for_mainnet()
            }),
            PendingTxSource::Txpool => PendingTxFeedConfig::Txpool(TxpoolFeedConfig {
                ws_url: CLOSED_FEED.to_string(),
                ..TxpoolFeedConfig::defaults()
            }),
        }
    }

    /// A fake arm whose driver loop reports what the attach handed it: the
    /// stream kind for its pump and whether the session's head subscription was
    /// already open (a live source, not a closed one) when the loop came up.
    fn fake_arm(id: &str, source: PendingTxSource) -> HostedArmBoot {
        HostedArmBoot {
            id: StrategyId::new(id),
            feed: HostedArmFeed::Resolved(feed_for(source)),
            spawn: Box::new(move |stream, head_subscription| {
                let stream_kind = stream.as_ref().map(PendingTxStream::source);
                Box::new(move |_lane| {
                    Box::pin(async move {
                        let head_source_live = fake_head_liveness(head_subscription).await;
                        DriverExit::Halted(format!(
                            "stream={stream_kind:?} head_source_live={head_source_live}"
                        ))
                    })
                })
            }),
        }
    }

    /// An attached head subscription is live when it neither yields a head nor
    /// reports a closed clock inside a short window: the session's claimed source
    /// still has its publisher.
    async fn fake_head_liveness(mut head: HeadSubscription) -> bool {
        tokio::time::timeout(Duration::from_millis(50), head.changed())
            .await
            .is_err()
    }

    fn hosted_boot() -> HostedBoot {
        HostedBoot {
            arms: vec![
                fake_arm("mevblocker_backrun", PendingTxSource::Mevblocker),
                fake_arm("txpool_backrun", PendingTxSource::Txpool),
            ],
            // The pump owns the hub head source (the driver registered it at
            // composition, without dialing), so the ordering fact is observable
            // with no endpoint at all.
            clock: Some(HostedHeadClock::PumpOwned),
            trigger: None,
        }
    }

    /// Poll the host FSM until `id` reports a tombstone, returning its detail.
    fn await_tombstone(session: &EngineSession, id: &StrategyId) -> String {
        degenbot_core::runtime::get_runtime().block_on(async {
            for _ in 0..1_000 {
                let record = session.host().lock().record(id);
                if let Some(detail) = record.as_ref().and_then(|record| record.halt_detail()) {
                    return detail.to_string();
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            panic!("driver {id} never folded a terminal exit");
        })
    }

    /// Poll one pump's status until its feed dialed, returning the status.
    fn await_first_dial(
        session: &EngineSession,
        source: PendingTxSource,
    ) -> degenbot_rpc::backrun_feed::BackrunFeedStatus {
        degenbot_core::runtime::get_runtime().block_on(async {
            for _ in 0..1_000 {
                let status = session
                    .hosted()
                    .expect("the boot hosted sources")
                    .pump_status(source)
                    .expect("one pump per arm");
                if status.reconnects > 0 {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("the {source:?} arm's pump never dialed after the start boundary");
        })
    }

    /// The boot provisions one pump per arm and dials nothing, then the
    /// start boundary launches each pump's feed and boots one driver per arm
    /// against a live head source.
    #[test]
    fn the_boot_mints_one_pump_per_arm_and_the_start_boundary_launches_them() {
        let session = EngineSession::boot(
            construction(),
            facet_table(|_| FacetStatus::Configured),
            Some(hosted_boot()),
        )
        .expect("boot composes the plane");

        // Mint: one pump per arm, none launched, and no head edge registered.
        // A dial would have been refused well inside this window on loopback, so
        // the counters read below prove the mint launched nothing.
        degenbot_core::runtime::get_runtime()
            .block_on(async { tokio::time::sleep(Duration::from_millis(100)).await });
        {
            let hosted = session.hosted().expect("the boot hosted sources");
            assert_eq!(
                hosted.pump_sources(),
                vec![PendingTxSource::Mevblocker, PendingTxSource::Txpool],
                "exactly one pump per hosted arm, in arm order"
            );
            for source in [PendingTxSource::Mevblocker, PendingTxSource::Txpool] {
                let status = hosted.pump_status(source).expect("one pump per arm");
                assert!(
                    !status.connected && status.accepted == 0 && status.reconnects == 0,
                    "the mint dialed nothing for {source:?}: {status:?}"
                );
            }
        }
        let hub = Arc::clone(session.host().lock().hub());
        let head = hub.subscribe_head();
        assert!(
            head.head().is_none(),
            "the mint published no head: no head read was dialed before the start boundary"
        );
        for id in ["mevblocker_backrun", "txpool_backrun"] {
            let id = StrategyId::new(id);
            assert!(
                session.host().lock().has_spawn(&id),
                "the boot attaches {id}'s spawn factory"
            );
            assert_eq!(
                session.host().lock().state_of(&id),
                Some(crate::strategy_host::DriverPose::Registered),
                "an attached driver stays registered until the start boundary"
            );
        }

        // A driver's loop starts only for an ENABLED facet: admission stays the
        // operator's verb, and the boot never enables on its own.
        for id in ["mevblocker_backrun", "txpool_backrun"] {
            session
                .host()
                .lock()
                .enable(&StrategyId::new(id))
                .expect("the configured arm admits");
        }

        // Start: the hosted sources launch before the drivers, so each driver's
        // loop finds a registered head source and its own arm's stream.
        let started = degenbot_core::runtime::get_runtime()
            .block_on(session.start())
            .expect("start the session");
        assert_eq!(started, 2, "one driver per hosted arm starts");
        for (id, source) in [
            ("mevblocker_backrun", PendingTxSource::Mevblocker),
            ("txpool_backrun", PendingTxSource::Txpool),
        ] {
            let detail = await_tombstone(&session, &StrategyId::new(id));
            assert!(
                detail.contains("head_source_live=true"),
                "{id} starts only after the head edge registered its source: {detail}"
            );
            assert!(
                detail.contains(&format!("stream=Some({source:?})")),
                "{id} is handed its own arm's stream: {detail}"
            );
        }
        for source in [PendingTxSource::Mevblocker, PendingTxSource::Txpool] {
            let status = await_first_dial(&session, source);
            assert!(
                status.reconnects > 0,
                "the start boundary launched {source:?}'s feed loop: {status:?}"
            );
        }

        // The start boundary is once-only: the lifecycle is `Running` (so the
        // hosted re-entry is an idempotent no-op) and no driver is left in a
        // startable pose.
        let restarted = degenbot_core::runtime::get_runtime()
            .block_on(session.start())
            .expect("a second start is a no-op");
        assert_eq!(restarted, 0, "no driver starts twice");
    }

    /// The settlement-versus-backrun asymmetry is a facet VALUE: the settlement
    /// row names the pump loop, so the boot composes that loop's factory over
    /// the session's own driver and the start boundary boots it through the same
    /// FSM a hosted arm crosses. The backrun facets register unconfigured, and
    /// the host refuses an enable for them without the boot branching on a name.
    #[test]
    fn the_facet_table_carries_the_asymmetry_as_a_value() {
        let session = EngineSession::boot(
            construction(),
            facet_table(|name| {
                if name == "settlement" {
                    FacetStatus::Configured
                } else {
                    FacetStatus::Unconfigured
                }
            }),
            None,
        )
        .expect("boot composes the plane");

        let ids: Vec<String> = session
            .host()
            .lock()
            .list()
            .iter()
            .map(|record| record.id().as_str().to_string())
            .collect();
        assert_eq!(
            ids, FACETS,
            "the facets register in the table's order, without a name match"
        );
        assert!(
            session.hosted().is_none(),
            "a boot with no hosted inputs provisions no pump and no head clock"
        );
        let settlement = StrategyId::new("settlement");
        assert!(
            session.host().lock().has_spawn(&settlement),
            "the pump-driven row leaves the boot with the factory its Enabled pose promises"
        );
        let peer = StrategyId::new("mevblocker_backrun");
        assert!(
            !session.host().lock().has_spawn(&peer),
            "a hosted arm's factory travels with its arm row, not the facet table"
        );
        assert!(
            matches!(
                session.host().lock().enable(&peer),
                Err(HostError::UnconfiguredStrategy(id)) if id == peer
            ),
            "an unconfigured facet value is refused at admission"
        );
        assert_eq!(
            session.host().lock().enable(&settlement),
            Ok(crate::strategy_host::DriverPose::Enabled),
            "the configured facet admits"
        );

        let started = degenbot_core::runtime::get_runtime()
            .block_on(session.start())
            .expect("start the session");
        assert_eq!(
            started, 1,
            "the settlement arm crosses the host's one driving edge like any other driver"
        );

        // The arm's loop is the pump session's lifetime, and this boot armed no
        // block clock: the record is a tombstone carrying that fact, never a
        // `Running` claim over a clock nobody armed.
        let detail = await_tombstone(&session, &settlement);
        assert!(
            detail.contains("the block clock is not armed"),
            "an unarmed clock is a tombstone: {detail}"
        );
        assert_eq!(
            session.host().lock().state_of(&settlement),
            Some(crate::strategy_host::DriverPose::Halted)
        );
    }

    /// An active arm whose endpoint the boot could not resolve keeps its spawn
    /// factory and provisions no pump, so its driver boots and halts at its
    /// live-mode gate instead of silently never starting. Its source kind is the
    /// unresolved arm's own name — never a config that would have to agree with
    /// it.
    #[test]
    fn an_unresolved_arm_has_a_source_without_a_feed() {
        let mut boot = hosted_boot();
        boot.arms.truncate(1);
        boot.arms[0].feed = HostedArmFeed::Unresolved(PendingTxSource::Txpool);
        assert_eq!(
            boot.arms[0].feed.source(),
            PendingTxSource::Txpool,
            "an unresolved arm still names the kind its stream would have had"
        );
        assert!(
            boot.arms[0].feed.config().is_none(),
            "an unresolved arm carries no feed to dial"
        );
        let session = EngineSession::boot(
            construction(),
            facet_table(|_| FacetStatus::Configured),
            Some(boot),
        )
        .expect("boot composes the plane");
        assert!(
            session
                .hosted()
                .expect("the boot hosted sources")
                .pump_sources()
                .is_empty(),
            "an arm with no feed provisions no pump"
        );
        let id = StrategyId::new("mevblocker_backrun");
        assert!(
            session.host().lock().has_spawn(&id),
            "the arm's spawn factory is attached regardless of its feed"
        );
        session
            .host()
            .lock()
            .enable(&id)
            .expect("the configured arm admits");
        let started = degenbot_core::runtime::get_runtime()
            .block_on(session.start())
            .expect("start the session");
        assert_eq!(started, 1, "the driver boots with no pump to drain");
        let detail = await_tombstone(&session, &id);
        assert!(
            detail.contains("stream=None"),
            "the factory is handed no stream: {detail}"
        );
    }

    /// A settlement-only boot reports head liveness through the SAME registry a
    /// mixed boot uses: the engine's block pump owns the hub head source, so the
    /// boot's head edge fires the reconcile trigger per accepted head with no
    /// backrun arm, no second `newHeads` subscription, and no dial.
    #[test]
    fn a_settlement_only_boot_reports_heads_through_the_pumps_shared_source() {
        let (fire_tx, mut fire_rx) = tokio::sync::mpsc::unbounded_channel::<u64>();
        let trigger: HeadTriggerComposer = Box::new(move |_host| {
            Arc::new(move |head: u64| {
                let fire_tx = fire_tx.clone();
                Box::pin(async move {
                    let _ = fire_tx.send(head);
                })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            }) as HeadTrigger
        });
        let session = EngineSession::boot(
            construction(),
            facet_table(|name| {
                if name == "settlement" {
                    FacetStatus::Configured
                } else {
                    FacetStatus::Unconfigured
                }
            }),
            Some(HostedBoot {
                arms: Vec::new(),
                clock: Some(HostedHeadClock::PumpOwned),
                trigger: Some(trigger),
            }),
        )
        .expect("a settlement-only boot composes the plane");

        // The pump owns the boot's ONE head source: registered at composition,
        // not deferred to a start, and shared by every reader on the hub.
        let hub = Arc::clone(session.host().lock().hub());
        assert!(
            hub.head_source_is_claimed(),
            "the session boot claimed the hub's one head source"
        );
        assert!(
            session
                .hosted()
                .expect("the boot hosts the session's sources")
                .pump_sources()
                .is_empty(),
            "a settlement-only boot admits no backrun arm, so it mints no pending-tx pump"
        );
        assert!(
            hub.subscribe_head().head().is_none(),
            "the mint publishes nothing: no head has been dialed or read"
        );

        degenbot_core::runtime::get_runtime().block_on(async {
            session
                .start()
                .await
                .expect("the start boundary attaches the head edge");
            assert_eq!(
                session
                    .hosted()
                    .expect("the boot hosts the session's sources")
                    .head_edge()
                    .mode(),
                crate::hosted_sources::HeadEdgeMode::PumpHosted,
                "the edge consumes the pump-owned source instead of dialing its own"
            );
            session
                .driver()
                .head_publisher()
                .expect("the pump owns the session's claimed head source")
                .publish(degenbot_eventhub::Head {
                    number: 55,
                    metadata: None,
                });
            let fired = tokio::time::timeout(Duration::from_secs(5), fire_rx.recv())
                .await
                .expect("the head edge fires inside the window")
                .expect("the recorder lives");
            assert_eq!(
                fired, 55,
                "the reconcile trigger carries the pump's accepted head"
            );
        });
    }

    /// A boot that hosts NO clock still claims the session hub's one head
    /// source and routes the publisher to its pump: a hostless session is never
    /// headless.
    #[test]
    fn a_hostless_boot_claims_the_pump_owned_head_source() {
        let session = EngineSession::boot(
            construction(),
            facet_table(|_| FacetStatus::Configured),
            None,
        )
        .expect("a hostless boot composes the plane");
        assert!(
            session.hosted().is_none(),
            "a boot with no hosted input provisions no pump and no head clock"
        );
        let hub = Arc::clone(session.host().lock().hub());
        assert!(
            hub.head_source_is_claimed(),
            "the boot claims the hub's head source before matching the hosted input"
        );
        let mut head = hub.subscribe_head();
        assert_eq!(head.head(), None, "the mint published no head");

        degenbot_core::runtime::get_runtime().block_on(async {
            session
                .driver()
                .head_publisher()
                .expect("a hostless boot routes the claimed publisher to the pump")
                .publish(degenbot_eventhub::Head {
                    number: 41,
                    metadata: None,
                });
            assert!(head.changed().await.is_ok(), "the publisher is live");
            assert_eq!(
                head.borrow_and_update().map(|head| head.number),
                Some(41),
                "the pump's publisher reaches every reader of the session's one source"
            );
        });
    }

    /// The arm's spawn factory receives the session's head subscription at
    /// attach, so a driver's loop reads the one clock the session claimed — the
    /// head published after the arm came up is the edge it awaits.
    #[test]
    fn drivers_receive_the_head_subscription_at_attach() {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let boot = HostedBoot {
            arms: vec![HostedArmBoot {
                id: StrategyId::new("mevblocker_backrun"),
                feed: HostedArmFeed::Resolved(feed_for(PendingTxSource::Mevblocker)),
                spawn: Box::new(move |_stream, mut head| {
                    Box::new(move |_lane| {
                        Box::pin(async move {
                            let attached_head = head.head();
                            ready_tx.send(()).expect("the test waits for this arm");
                            let observed =
                                tokio::time::timeout(Duration::from_secs(5), head.changed()).await;
                            let number = match observed {
                                Ok(Ok(())) => head.borrow_and_update().map(|head| head.number),
                                _ => None,
                            };
                            DriverExit::Halted(format!(
                                "attached={:?} observed={number:?}",
                                attached_head.map(|head| head.number)
                            ))
                        })
                    })
                }),
            }],
            clock: Some(HostedHeadClock::PumpOwned),
            trigger: None,
        };
        let session = EngineSession::boot(
            construction(),
            facet_table(|_| FacetStatus::Configured),
            Some(boot),
        )
        .expect("boot composes the plane");
        let id = StrategyId::new("mevblocker_backrun");
        session
            .host()
            .lock()
            .enable(&id)
            .expect("the configured arm admits");
        degenbot_core::runtime::get_runtime()
            .block_on(session.start())
            .expect("start the session");
        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the arm's loop comes up before the head is published");

        degenbot_core::runtime::get_runtime().block_on(async {
            session
                .driver()
                .head_publisher()
                .expect("the pump owns the session's claimed head source")
                .publish(degenbot_eventhub::Head {
                    number: 64,
                    metadata: None,
                });
        });

        let detail = await_tombstone(&session, &id);
        assert!(
            detail.contains("attached=None"),
            "the subscription is attached at the clock's current value: {detail}"
        );
        assert!(
            detail.contains("observed=Some(64)"),
            "the driver's loop awaited the head its own session published: {detail}"
        );
    }

    /// A directly minted pump-owned owner over an unclaimed hub half-starts; the
    /// recovery is a WHOLE fresh session and hub — whose boot claims the source
    /// and registers the same pending arms — never an owner-level re-mint.
    #[test]
    fn a_half_started_direct_owner_recovers_via_fresh_session_and_hub() {
        use crate::hosted_sources::HostedPhase;

        let hub = Arc::new(Hub::new());
        let direct = HostedSources::mint(
            Arc::clone(&hub),
            [feed_for(PendingTxSource::Mevblocker)],
            Some(HostedClockBinding::PumpOwned),
            None,
        )
        .expect("the direct owner mints");
        let refused = degenbot_core::runtime::get_runtime().block_on(direct.start());
        assert!(
            matches!(refused, Err(HostedStartError::HeadSourceUnclaimed)),
            "an unclaimed hub half-starts the direct owner: {refused:?}"
        );
        assert_eq!(direct.phase(), HostedPhase::HalfStarted);
        drop(direct);

        let session = EngineSession::boot(
            construction(),
            facet_table(|_| FacetStatus::Configured),
            Some(hosted_boot()),
        )
        .expect("a fresh session and hub compose the same arms");
        let hub = Arc::clone(session.host().lock().hub());
        assert!(
            hub.head_source_is_claimed(),
            "the fresh session's boot claims its own hub's head source"
        );
        assert_eq!(
            session
                .hosted()
                .expect("the fresh boot hosts the arms")
                .phase(),
            HostedPhase::Provisioned,
            "the recovered session substitutes a fresh owner, not the half-started one"
        );
        for id in ["mevblocker_backrun", "txpool_backrun"] {
            session
                .host()
                .lock()
                .enable(&StrategyId::new(id))
                .expect("the configured arm admits");
        }
        degenbot_core::runtime::get_runtime()
            .block_on(session.start())
            .expect("the recovered session starts");
        assert_eq!(
            session
                .hosted()
                .expect("the fresh boot hosts the arms")
                .phase(),
            HostedPhase::Running,
            "the whole-session recovery reaches Running"
        );
    }
}
