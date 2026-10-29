//! The engine session's strategy-plane boot: the one home of the composition
//! order that mints the hub, admits the facets, attaches the planes, and
//! starts them.
//!
//! The order is load-bearing, and each step reads the previous one's product:
//!
//! 1. **Mint.** [`StrategyHost::mint`] registers the engine's named source
//!    channels on a fresh hub inside the exclusive construction window the hub
//!    type requires, and the host owns that hub from then on. Provisioning
//!    only: nothing is started and no socket is opened.
//! 2. **Register.** Every row of the caller's facet table enters the driver
//!    FSM in [`DriverPose::Registered`], carrying its admission status as a
//!    value. A crate that can read the typed config resolves that status, so
//!    this module authors no activation, stance, or settlement policy.
//! 3. **Attach.** The submission-truth arm ([`HeadReconciler`]) and the state
//!    root each enabled lane's namespace resolves under, then the hosted
//!    sources (one pending-tx pump per arm) and each arm's spawn factory, which
//!    the boot hands the arm's own typed stream.
//! 4. **Start** ([`EngineSession::start`]). The hosted sources' deferred I/O
//!    launches before any driver: a driver's loop subscribes the hub head
//!    source that the head edge registers as it starts, so a driver started
//!    first would poll a clock with no source.
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

use degenbot_eventhub::{Hub, HubError, PendingTxSource};
use degenbot_rpc::pending_tx_stream::{PendingTxFeedConfig, PendingTxStream};
use degenbot_substrate::driver_spawn::DriverSpawnFactory;
use degenbot_substrate::nonce::{NonceAuthority, StrategyId};
use degenbot_substrate::RouteRegistry;
use parking_lot::Mutex;

use super::Bot;
use crate::arb_engine::{EngineChannelHandles, EngineDriver, EngineStages};
use crate::hosted_sources::{HeadTrigger, HostedHeadClock, HostedSources};
use crate::strategy_host::{FacetStatus, HeadReconciler, HostError, HostSupervisor, StrategyHost};

/// One facet admitted at boot.
pub struct FacetBoot {
    /// The plane name the facet registers and is enabled under — its
    /// registration id and its typed-config section name are the same string.
    pub id: StrategyId,
    /// The admission status the host serves `enable` from.
    pub facet: FacetStatus,
}

/// Composes one hosted arm's spawn factory.
///
/// The boot hands it the hub the host minted (the arm's stream lives on that
/// hub's ring) and the arm's typed stream, so the strategy crate that owns the
/// loop builds the factory without this crate naming it. A stream is absent
/// when the arm's pump was not minted; the strategy boot then refuses its live
/// mode, which is the same refusal a feed-less boot always produced.
pub type ArmSpawnComposer =
    Box<dyn FnOnce(&Arc<Hub>, Option<PendingTxStream>) -> DriverSpawnFactory + Send>;

/// One hosted arm: the pending-tx kind its pump provisions, the feed that pump
/// dials at the start boundary, and the builder for its driver's spawn factory.
///
/// The feed is optional because the two couplings differ: an active arm whose
/// endpoint the boot could not resolve provisions no pump (and hands its
/// factory no stream, so the driver halts at its live-mode gate rather than
/// silently never starting), while the factory itself is attached for every
/// arm the boot hosts. A facet with no arm — one the caller left inactive — gets
/// neither, since its admission status already refuses the enable.
pub struct HostedArmBoot {
    /// The facet whose driver loop this arm hosts. It must also appear in the
    /// facet table, and it is the id the host attaches the factory under.
    pub id: StrategyId,
    /// The source kind. One pump per kind.
    pub source: PendingTxSource,
    /// The feed the pump dials — at the start boundary, never at mint. `None`
    /// provisions no pump for this kind.
    pub feed: Option<PendingTxFeedConfig>,
    /// Builds the arm's spawn factory from the booted hub and the arm's stream.
    pub spawn: ArmSpawnComposer,
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

/// Why a session boot refused to compose the plane.
#[derive(Debug, thiserror::Error)]
pub enum SessionBootError {
    /// The host refused a facet registration or a spawn attachment — two rows
    /// under one name, which no boot may produce.
    #[error(transparent)]
    Host(#[from] HostError),
    /// The hosted sources could not be minted over the booted hub.
    #[error(transparent)]
    Hosted(#[from] HubError),
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
    /// [`SessionBootError::Host`] when the host refuses a registration or an
    /// attachment (two facet rows, or an arm under an unregistered id), and
    /// [`SessionBootError::Hosted`] when the hosted sources cannot be minted.
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

        // Mint: the hub's named channels are registered inside the exclusive
        // construction window, and the host owns the hub afterwards.
        let (host, attached) =
            StrategyHost::mint(registry, nonce, EngineChannelHandles::register_on);
        let host = Arc::new(Mutex::new(host));

        // Register the facet table in the caller's order, then the shared plane
        // attachments.
        {
            let mut guard = host.lock();
            for facet in facets {
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
        // needs the arm's stream the provisioning step hands out.
        let hosted = match hosted {
            Some(boot) => Some(provision_hosted(&host, boot)?),
            None => None,
        };

        // Attach the driver to the hub the host minted.
        let driver = Arc::new(EngineDriver::from_stages_with_hub(bot, stages, attached));
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
    /// The hosted sources start first because a driver's loop subscribes the
    /// hub head source the head edge registers as it starts; a driver started
    /// first would poll a clock with no source. Returns the number of drivers
    /// booted.
    ///
    /// # Errors
    ///
    /// [`SessionBootError::Hosted`] when the hosted head source cannot be
    /// registered — a miswired boot fails closed rather than driving a
    /// clockless fleet, and the caller must drop the session (the deferred-I/O
    /// latch is already consumed, so a retry is a no-op), and
    /// [`SessionBootError::Host`] when the host refuses a driver's start.
    pub async fn start(&self) -> Result<usize, SessionBootError> {
        if let Some(hosted) = self.hosted.as_ref() {
            hosted.start().await?;
        }
        self.start_drivers().map_err(SessionBootError::from)
    }

    /// Boot every enabled facet that attached a spawn factory and supervise
    /// their lane boundaries, so a terminal exit folds through the host FSM
    /// (a self-halt becomes a tombstone) without exposing the fold.
    ///
    /// A facet with no factory is skipped, not failed: the settlement arm is
    /// driven by the engine's own pump, so it is not a hosted loop.
    ///
    /// # Errors
    ///
    /// [`HostError`] when the lifecycle refuses a driver's start.
    pub fn start_drivers(&self) -> Result<usize, HostError> {
        let tasks = self.host.lock().start_driving()?;
        let count = tasks.len();
        self.supervisor.supervise(tasks);
        Ok(count)
    }
}

/// Provision the hosted sources and attach every arm's spawn factory.
fn provision_hosted(
    host: &Arc<Mutex<StrategyHost>>,
    boot: HostedBoot,
) -> Result<HostedSources, SessionBootError> {
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
        .filter_map(|arm| arm.feed.clone().map(|feed| (arm.source, feed)))
        .collect::<Vec<_>>();
    let mut hosted = HostedSources::mint(Arc::clone(&hub), feeds, clock, trigger)?;
    for arm in arms {
        let stream = hosted.take_stream(arm.source);
        let spawn = (arm.spawn)(&hub, stream);
        host.lock().attach_spawn(&arm.id, spawn)?;
    }
    Ok(hosted)
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

    fn facet_table(status: impl Fn(&str) -> FacetStatus) -> Vec<FacetBoot> {
        FACETS
            .into_iter()
            .map(|name| FacetBoot {
                id: StrategyId::new(name),
                facet: status(name),
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

    /// A fake arm whose driver loop reports what the start boundary handed it:
    /// the stream kind for its pump and whether the hub head source was already
    /// registered when the loop came up.
    fn fake_arm(id: &str, source: PendingTxSource) -> HostedArmBoot {
        HostedArmBoot {
            id: StrategyId::new(id),
            source,
            feed: Some(feed_for(source)),
            spawn: Box::new(move |hub: &Arc<Hub>, stream| {
                let hub = Arc::clone(hub);
                let stream_kind = stream.as_ref().map(PendingTxStream::source);
                Box::new(move |_lane| {
                    let hub = Arc::clone(&hub);
                    Box::pin(async move {
                        let head_source_live = hub.subscribe_head().is_ok();
                        DriverExit::Halted(format!(
                            "stream={stream_kind:?} head_source_live={head_source_live}"
                        ))
                    })
                })
            }),
        }
    }

    fn hosted_boot() -> HostedBoot {
        HostedBoot {
            arms: vec![
                fake_arm("mevblocker_backrun", PendingTxSource::Mevblocker),
                fake_arm("txpool_backrun", PendingTxSource::Txpool),
            ],
            // A hub clock registers the hub head source without dialing, so the
            // ordering fact is observable with no endpoint at all.
            clock: Some(HostedHeadClock::Hub),
            trigger: None,
        }
    }

    /// Poll the host FSM until `id` reports a tombstone, returning its detail.
    fn await_tombstone(session: &EngineSession, id: &StrategyId) -> String {
        degenbot_core::runtime::get_runtime().block_on(async {
            for _ in 0..1_000 {
                let record = session.host().lock().record(id).cloned();
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
        assert!(
            hub.subscribe_head().is_err(),
            "the head edge is not up before the start boundary"
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

        // The start boundary is once-only: the pumps' latch is consumed and no
        // driver is left in a startable pose.
        let restarted = degenbot_core::runtime::get_runtime()
            .block_on(session.start())
            .expect("a second start is a no-op");
        assert_eq!(restarted, 0, "no driver starts twice");
    }

    /// The settlement-versus-backrun asymmetry is a facet VALUE: the settlement
    /// arm registers configured with no spawn factory (the engine's own pump
    /// drives it), the backrun facets register unconfigured, and the host
    /// refuses an enable for them without the boot branching on a name.
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
            !session.host().lock().has_spawn(&settlement),
            "the settlement arm is host-driven, so the boot attaches no factory"
        );
        let peer = StrategyId::new("mevblocker_backrun");
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
            started, 0,
            "the settlement arm has no hosted loop, so the driving edge boots nothing"
        );
    }
    /// An active arm whose endpoint the boot could not resolve keeps its spawn
    /// factory and provisions no pump, so its driver boots and halts at its
    /// live-mode gate instead of silently never starting.
    #[test]
    fn an_arm_without_a_feed_attaches_its_factory_and_provisions_no_pump() {
        let mut boot = hosted_boot();
        boot.arms.truncate(1);
        boot.arms[0].feed = None;
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
}
