//! Proof gate: a mock third strategy family drives the real host/seam paths.
//!
//! The mock is a third family in the sense ADR-057 fixes: it registers with
//! [`StrategyHost::register`], is admitted by [`StrategyHost::enable`], runs
//! its loop through a [`DriverSpawnFactory`] on [`StrategyHost::start_driving`],
//! consumes its nonce through a [`NonceLane`] over the host authority, records
//! into the host-reconciled [`SubmissionLedger`], subscribes to the hub's
//! typed head tick, and lands on the host's clean-stop fold. No production
//! code names it — that is the proof.
//!
//! The mock deliberately uses only the public seams a real family uses. If a
//! required shape were missing, the test would not paper over it: the gap is
//! recorded in `.scratch/seam-survey/knlj2u.md`.

#![expect(
    clippy::expect_used,
    reason = "integration fixtures and assertions fail loudly"
)]

use std::sync::mpsc;
use std::sync::Arc;

use alloy::primitives::B256;
use degenbot_bot::bot_core::RouteRegistry;
use degenbot_bot::connector_index::V2ConnectorIndex;
use degenbot_bot::nonce_authority::{NonceAuthority, StrategyId};
use degenbot_bot::strategy_host::{
    DriverExit, DriverPose, DriverSpawnFactory, FacetStatus, FsmDecline, HeadReconciler, HostError,
    SessionDecline, SessionPhase, StrategyHost, StrategyNotice,
};
use degenbot_eventhub::{Hub, HubClass, HubError, HubEvent};
use degenbot_submission::{NonceLane, SubmissionLedger, SubmissionState, TargetId};

/// The mock third strategy family. A real family is named by its config facet
/// and the operator's `strategy.name`; the mock keeps only the name the host
/// registers.
struct MockThirdFamily {
    id: StrategyId,
}

/// The shared handles the family's loop borrows from the host. A real family
/// is handed the same services at its driving edge; the mock takes them
/// directly because the test constructs the host.
#[derive(Clone)]
struct MockFamilyServices {
    hub: Arc<Hub>,
    authority: Arc<NonceAuthority>,
    ledger: Arc<SubmissionLedger>,
}

/// The observations the mock's loop reports back to the test: the mock's
/// stand-in for a family's run artifacts, never reached by production.
#[derive(Debug, PartialEq, Eq)]
struct MockRunReport {
    nonce: u64,
    ticked_head: Option<u64>,
}

impl MockThirdFamily {
    /// The registered name. A real family's name is its config facet key.
    const NAME: &'static str = "mock-third-family";

    fn new() -> Self {
        Self {
            id: StrategyId::new(Self::NAME),
        }
    }

    fn id(&self) -> &StrategyId {
        &self.id
    }

    /// The family's runnable loop, driven through the host spawn factory.
    ///
    /// A real family's loop is `BackrunDriver::start`; the mock's loop is the
    /// smallest program that touches each seam the proof gate pins: the typed
    /// hub tick, the authority lane, and the host-reconciled ledger.
    async fn drive(
        self,
        services: MockFamilyServices,
        ready: mpsc::Sender<()>,
        report: mpsc::Sender<MockRunReport>,
    ) -> DriverExit {
        // The reaction is declared by hub class: a family reacts to the head
        // tick without a mempool (`PendingTx`) stream ever being registered.
        assert!(
            matches!(
                services.hub.subscribe(HubClass::PendingTx),
                Err(HubError::NotRegistered(HubClass::PendingTx))
            ),
            "the hub tick must not require a mempool source"
        );
        let mut head = services
            .hub
            .subscribe_head()
            .expect("the host hub carries a head source");

        // Lane consumption over the one process authority: the lease is the
        // lowest free nonce at or above the confirmed chain nonce.
        let lane = NonceLane::new(
            Arc::clone(&services.authority),
            Arc::clone(&services.ledger),
            self.id.clone(),
        );
        let lease = lane.stamp().expect("authority grants a contiguous nonce");
        lane.record_signed(&lease, TargetId::new(B256::ZERO), B256::ZERO, 0)
            .expect("record signed");
        services
            .authority
            .record_broadcast(&lease)
            .expect("authority broadcast");
        services
            .ledger
            .record_broadcast(&self.id, lease.nonce())
            .expect("ledger broadcast");

        // Hand the test the wheel before it publishes the head, then await the
        // tick the hub promises.
        ready.send(()).expect("ready signal");
        head.changed()
            .await
            .expect("head tick published without a mempool stream");

        report
            .send(MockRunReport {
                nonce: lease.nonce(),
                ticked_head: head.head(),
            })
            .expect("run report");
        DriverExit::Stopped
    }
}

/// The one spawn factory a family attaches to the host.
fn mock_family_spawn(
    services: MockFamilyServices,
    ready: mpsc::Sender<()>,
    report: mpsc::Sender<MockRunReport>,
) -> DriverSpawnFactory {
    Box::new(move |_namespace| {
        Box::pin(async move { MockThirdFamily::new().drive(services, ready, report).await })
    })
}

fn registry() -> Arc<RouteRegistry> {
    Arc::new(RouteRegistry::new(V2ConnectorIndex::default()))
}

fn head_event(number: u64) -> HubEvent {
    HubEvent::NewHead {
        number,
        timestamp: 0,
        base_fee_per_gas: None,
        gas_used: 0,
        gas_limit: 0,
    }
}

/// The full lifecycle a real family walks: register -> enable -> run through
/// the host's driving edge -> clean stop, with the loop's own seam checks.
#[tokio::test]
async fn mock_family_registers_enables_runs_and_stops_through_the_host() {
    let hub = Arc::new(Hub::new());
    let head = hub.register_head_source().expect("head source registered");
    let authority = Arc::new(NonceAuthority::new(7));
    let ledger = Arc::new(SubmissionLedger::new());
    let mut host = StrategyHost::new(Arc::clone(&hub), registry(), Arc::clone(&authority));
    host.attach_reconciler(Arc::clone(&ledger) as Arc<dyn HeadReconciler>);

    let family = MockThirdFamily::new();
    host.register(family.id().clone(), FacetStatus::Configured)
        .expect("register the mock family");
    assert_eq!(host.state_of(family.id()), Some(DriverPose::Registered));
    assert_eq!(host.enable(family.id()), Ok(DriverPose::Enabled));

    let (ready_tx, ready_rx) = mpsc::channel();
    let (report_tx, report_rx) = mpsc::channel();
    host.attach_spawn(
        family.id(),
        mock_family_spawn(
            MockFamilyServices {
                hub: Arc::clone(&hub),
                authority: Arc::clone(&authority),
                ledger: Arc::clone(&ledger),
            },
            ready_tx,
            report_tx,
        ),
    )
    .expect("attach the family spawn factory");

    let tasks = host.start_driving().expect("start driving the mock family");
    assert_eq!(host.state_of(family.id()), Some(DriverPose::Running));
    let task = tasks.into_iter().next().expect("one driver task");

    // The loop has subscribed and stamped; publish the head tick now.
    ready_rx.recv().expect("loop ready");
    head.publish(head_event(42));

    // The clean stop funnels through the host-owned fold, never a caller
    // re-implementation.
    host.drive_and_fold(task)
        .await
        .expect("fold the clean stop");
    assert_eq!(host.state_of(family.id()), Some(DriverPose::Stopped));

    let report = report_rx.recv().expect("run report");
    assert_eq!(
        report.nonce, 7,
        "the mock family's lease is the authority's lowest-free nonce"
    );
    assert_eq!(
        report.ticked_head,
        Some(42),
        "the hub head tick fired without a mempool stream"
    );
}

/// Admission declines and one deformed-seam attempt, each a typed refusal.
#[test]
fn mock_family_admission_and_deformed_moves_decline_with_types() {
    let mut host = StrategyHost::new(
        Arc::new(Hub::new()),
        registry(),
        Arc::new(NonceAuthority::new(0)),
    );
    let family = MockThirdFamily::new();
    let ghost = StrategyId::new("ghost-family");

    // A name the host never registered.
    assert_eq!(
        host.enable(&ghost),
        Err(HostError::UnknownStrategy(ghost.clone()))
    );

    // A registered name with no configured facet.
    host.register(ghost.clone(), FacetStatus::Unconfigured)
        .expect("register ghost");
    assert_eq!(
        host.enable(&ghost),
        Err(HostError::UnconfiguredStrategy(ghost))
    );

    host.register(family.id().clone(), FacetStatus::Configured)
        .expect("register the mock family");

    // Registering the same name twice.
    assert_eq!(
        host.register(family.id().clone(), FacetStatus::Configured),
        Err(HostError::AlreadyRegistered(family.id().clone()))
    );

    // start before enable.
    assert_eq!(
        host.start(family.id()),
        Err(HostError::Transition {
            id: family.id().clone(),
            decline: FsmDecline::RunningRequiresEnabled,
        })
    );

    // Deformed-seam attempt: enable twice.
    assert_eq!(host.enable(family.id()), Ok(DriverPose::Enabled));
    assert_eq!(
        host.enable(family.id()),
        Err(HostError::Transition {
            id: family.id().clone(),
            decline: FsmDecline::EnableRequiresRegistered,
        })
    );

    // halt before running.
    assert_eq!(
        host.halt(family.id(), "too early"),
        Err(HostError::Transition {
            id: family.id().clone(),
            decline: FsmDecline::HaltRequiresRunning,
        })
    );

    // Deformed-seam attempt: disable a terminal tombstone.
    host.start(family.id()).expect("start");
    host.halt(family.id(), "local violation").expect("halt");
    assert_eq!(host.state_of(family.id()), Some(DriverPose::Halted));
    assert_eq!(
        host.disable(family.id()),
        Err(HostError::Transition {
            id: family.id().clone(),
            decline: FsmDecline::DisableRejectsTerminal,
        })
    );
    assert_eq!(
        host.enable(family.id()),
        Err(HostError::Transition {
            id: family.id().clone(),
            decline: FsmDecline::EnableRequiresRegistered,
        })
    );
}

/// Two families over one authority keep the outstanding set a contiguous
/// prefix above the confirmed nonce, and a released gap is refilled.
#[test]
fn mock_family_lane_consumption_keeps_authority_contiguity() {
    let authority = Arc::new(NonceAuthority::new(7));
    let ledger = Arc::new(SubmissionLedger::new());
    let family = MockThirdFamily::new();
    let peer = StrategyId::new("mock-peer-family");
    let lane_a = NonceLane::new(
        Arc::clone(&authority),
        Arc::clone(&ledger),
        family.id().clone(),
    );
    let lane_b = NonceLane::new(Arc::clone(&authority), Arc::clone(&ledger), peer);

    let lease_a = lane_a.stamp().expect("family stamp");
    let lease_b = lane_b.stamp().expect("peer stamp");
    assert_eq!(
        (lease_a.nonce(), lease_b.nonce()),
        (7, 8),
        "the second family never re-issues the first family's held nonce"
    );
    assert_eq!(
        authority.outstanding_nonces(),
        vec![7, 8],
        "the outstanding set is a contiguous prefix above the confirmed nonce"
    );

    // A released gap is refilled by the next stamp.
    lane_a.release(&lease_a).expect("release the family lease");
    let refill = lane_a.stamp().expect("refill stamp");
    assert_eq!(refill.nonce(), 7, "the next stamp refills the freed gap");
    assert_eq!(authority.outstanding_nonces(), vec![7, 8]);
}

/// The host-reconciled ledger classifies a terminal miss (`Stale`) and an
/// observed record's orphan event (`Orphaned`, carrying the fillable
/// predecessor).
#[test]
fn mock_family_ledger_classifies_terminal_miss_and_observed_orphan() {
    let authority = Arc::new(NonceAuthority::new(10));
    let ledger = Arc::new(SubmissionLedger::new());
    let mut host = StrategyHost::new(Arc::new(Hub::new()), registry(), Arc::clone(&authority));
    let family = MockThirdFamily::new();
    host.register(family.id().clone(), FacetStatus::Configured)
        .expect("register the mock family");
    host.enable(family.id()).expect("enable");
    host.attach_reconciler(Arc::clone(&ledger) as Arc<dyn HeadReconciler>);

    let lane = NonceLane::new(
        Arc::clone(&authority),
        Arc::clone(&ledger),
        family.id().clone(),
    );

    // Two signed broadcasts; the lower is then vacated without landing, so the
    // upper's immediate predecessor is neither confirmed nor outstanding.
    let lease_10 = lane.stamp().expect("stamp 10");
    assert_eq!(lease_10.nonce(), 10);
    lane.record_signed(&lease_10, TargetId::new(B256::ZERO), B256::ZERO, 10)
        .expect("sign 10");
    authority
        .record_broadcast(&lease_10)
        .expect("authority broadcast 10");
    ledger
        .record_broadcast(family.id(), 10)
        .expect("ledger broadcast 10");

    let lease_11 = lane.stamp().expect("stamp 11");
    assert_eq!(lease_11.nonce(), 11);
    lane.record_signed(&lease_11, TargetId::new(B256::ZERO), B256::ZERO, 10)
        .expect("sign 11");
    authority
        .record_broadcast(&lease_11)
        .expect("authority broadcast 11");
    ledger
        .record_broadcast(family.id(), 11)
        .expect("ledger broadcast 11");

    authority
        .release_broadcast(family.id(), 10)
        .expect("vacate the predecessor broadcast");

    let notices = host.on_head(10);
    let by_nonce = |nonce: u64| {
        notices
            .iter()
            .find(|notice| notice.nonce() == nonce)
            .expect("one notice per classified record")
    };
    assert_eq!(
        by_nonce(10).notice(),
        &StrategyNotice::Stale,
        "a terminal miss is classified Stale"
    );
    assert_eq!(
        by_nonce(11).notice(),
        &StrategyNotice::Orphaned { fillable_nonce: 10 },
        "an observed record's orphan event carries the fillable predecessor"
    );
    assert_eq!(
        ledger.state_of(family.id(), 10),
        Some(SubmissionState::Stale)
    );
    assert_eq!(
        ledger.state_of(family.id(), 11),
        Some(SubmissionState::Orphaned)
    );
}

/// A submitted record lands when the chain's confirmed nonce passes it.
#[test]
fn mock_family_ledger_lands_a_submitted_record() {
    let authority = Arc::new(NonceAuthority::new(10));
    let ledger = Arc::new(SubmissionLedger::new());
    let mut host = StrategyHost::new(Arc::new(Hub::new()), registry(), Arc::clone(&authority));
    let family = MockThirdFamily::new();
    host.register(family.id().clone(), FacetStatus::Configured)
        .expect("register the mock family");
    host.enable(family.id()).expect("enable");
    host.attach_reconciler(Arc::clone(&ledger) as Arc<dyn HeadReconciler>);

    let lane = NonceLane::new(
        Arc::clone(&authority),
        Arc::clone(&ledger),
        family.id().clone(),
    );
    let lease = lane.stamp().expect("stamp 10");
    lane.record_signed(&lease, TargetId::new(B256::ZERO), B256::ZERO, 10)
        .expect("sign 10");
    authority
        .record_broadcast(&lease)
        .expect("authority broadcast");
    ledger
        .record_broadcast(family.id(), 10)
        .expect("ledger broadcast");

    let notices = host.on_head(11);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].notice(), &StrategyNotice::Landed);
    assert_eq!(
        ledger.state_of(family.id(), 10),
        Some(SubmissionState::Landed)
    );
}

/// The post-KDDCQI session-phase surface the Python stop funnels translate:
/// one total Rust table, typed declines for illegal moves, shutdown total from
/// every phase.
#[test]
fn the_session_phase_table_is_the_stop_funnel_contract() {
    assert_eq!(SessionPhase::New.on_start(), Ok(SessionPhase::Started));
    assert_eq!(
        SessionPhase::Started.on_start(),
        Ok(SessionPhase::Started),
        "start re-entry is idempotent"
    );
    assert_eq!(
        SessionPhase::Running.on_start(),
        Err(SessionDecline::StartRequiresNew)
    );
    assert_eq!(SessionPhase::Started.on_run(), Ok(SessionPhase::Running));
    assert_eq!(
        SessionPhase::New.on_run(),
        Err(SessionDecline::RunRequiresStarted)
    );
    assert_eq!(SessionPhase::Running.on_query(), Ok(SessionPhase::Running));
    assert_eq!(
        SessionPhase::New.on_query(),
        Err(SessionDecline::QueryRequiresRunning)
    );
    for phase in [
        SessionPhase::New,
        SessionPhase::Started,
        SessionPhase::Running,
        SessionPhase::Closed,
    ] {
        assert_eq!(
            phase.on_shutdown(),
            SessionPhase::Closed,
            "shutdown funnels every phase to Closed"
        );
    }
    assert_eq!(SessionPhase::New.as_str(), "new");
    assert_eq!(SessionPhase::Closed.as_str(), "closed");
}
