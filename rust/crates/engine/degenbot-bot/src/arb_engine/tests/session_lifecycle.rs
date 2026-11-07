//! The session lifecycle's public-interface acceptance tests (ADR-093 D1/D2/D5/D7).
//!
//! One test per invariant the session's interface is supposed to carry. Every
//! assertion reads the PUBLIC projection (`PumpPhase`), a typed refusal, the
//! ordered `SessionEvent` stream, the terminal fact, the closure, or the
//! teardown report — never a session field.

#![expect(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use degenbot_ingestion::IngestEvent as WsEvent;
use futures_util::StreamExt;

use crate::arb_engine::crawl::controller::CrawlConfig;
use crate::arb_engine::crawl::policy::CrawlPathPolicy;
use crate::arb_engine::driver::{ConnectFault, SessionSeed};
use crate::arb_engine::pump_session::PumpDecline;
use crate::arb_engine::session_events::{ClaimRefusal, SessionEvent};
use crate::arb_engine::{
    BlockMetadata, ConnectRefusal, CrawlRefusal, DriverError, EngineDriver, EngineStages,
    PumpEvent, PumpExit, PumpPhase, TeardownReport,
};
use crate::bot_core::bot::Bot;
use crate::bot_core::{ConstructionRoute, PumpControl};
use degenbot_pathfinding::PoolKind;

/// The runtime every async test drives: the shared one the driver's own
/// `block_on` boundaries use.
fn runtime() -> &'static tokio::runtime::Runtime {
    degenbot_core::runtime::get_runtime()
}

fn driver() -> Arc<EngineDriver> {
    let bot = Arc::new(Bot::new(1));
    let stages = Arc::new(EngineStages::with_core(bot.state_arc(), bot.active_delta()));
    Arc::new(EngineDriver::from_stages(bot, stages))
}

/// Seed the session in `Subscribed` over a local pump and a scripted stream —
/// the pose a live WS handshake would otherwise have to be reached for.
fn seed_connected(
    driver: &EngineDriver,
    stream: futures_util::stream::BoxStream<'static, WsEvent>,
) {
    driver.seed_session_for_test(SessionSeed::Connected {
        pump: driver.local_pump_for_test(),
        first_block: 100,
        stream,
    });
}

/// A stream that never yields: the pump parks in its select, so `Running` is
/// the pose a reader observes rather than a race against a loop that ended.
fn parked_stream() -> futures_util::stream::BoxStream<'static, WsEvent> {
    futures_util::stream::pending::<WsEvent>().boxed()
}

/// An already-ended stream: the pump returns on its own, immediately.
fn ended_stream() -> futures_util::stream::BoxStream<'static, WsEvent> {
    futures_util::stream::empty::<WsEvent>().boxed()
}

/// A crawl configuration nothing in these tests boots: the refusals they assert
/// are answered before any crawl work begins.
fn crawl_config_for_test() -> CrawlConfig {
    CrawlConfig {
        chain_id: 1,
        database_path: std::path::PathBuf::from("/nonexistent/session-lifecycle.sqlite"),
        route: ConstructionRoute::generic_only(),
        pool_kinds: vec![PoolKind::V4],
        pool_type_per_depth: None,
        start_tokens: vec![degenbot_substrate::session_registry::TokenIdentity::native()],
        end_tokens: vec![degenbot_substrate::session_registry::TokenIdentity::native()],
        min_depth: 2,
        max_depth: Some(2),
        discovery_batch_size: 1,
        intake_window: 1,
        path_cap: None,
        retry: crate::bot_core::verification_retry::RetryPolicy {
            max_attempts: 1,
            base_delay: 0.0,
            max_delay: 0.0,
            jitter: 0.0,
        },
        policy: CrawlPathPolicy::default(),
        oracle: None,
        policy_batch_size: 1,
        profit_token: alloy::primitives::Address::ZERO,
        snapshot_block: None,
        state_block: Some(18_000_000),
    }
}

// ── the public projection: 9 poses x 11 events ──────────────────────────────

#[test]
fn the_pump_session_table_is_total_over_nine_poses_by_eleven_events() {
    // The grid's SHAPE is the contract: every pose answers every event, and an
    // edge added or removed anywhere has to be acknowledged here.
    let poses = [
        PumpPhase::Detached,
        PumpPhase::Connecting,
        PumpPhase::Subscribed,
        PumpPhase::Resuming,
        PumpPhase::Running,
        PumpPhase::Ended,
        PumpPhase::Abandoned,
        PumpPhase::Stopping,
        PumpPhase::Stopped,
    ];
    let events = [
        PumpEvent::SubscribeRequested,
        PumpEvent::Subscribed,
        PumpEvent::SubscribeFailed,
        PumpEvent::SubscribeAbandoned,
        PumpEvent::ResumeRequested,
        PumpEvent::ResumeSpawned,
        PumpEvent::ResumeAbandoned,
        PumpEvent::PumpExited,
        PumpEvent::CrawlAttached,
        PumpEvent::Stop,
        PumpEvent::StopCompleted,
    ];
    assert_eq!(poses.len(), 9, "the pose vocabulary");
    assert_eq!(events.len(), 11, "the event vocabulary");
    let mut allowed = 0;
    for pose in poses {
        for event in events {
            if crate::arb_engine::transition(pose, event).is_ok() {
                allowed += 1;
            }
        }
    }
    assert_eq!(allowed, 23, "the graph's allowed edge count changed");
}

// ── invariant: pose and pose-owned data cannot disagree ─────────────────────

#[test]
fn pose_and_pose_owned_data_cannot_disagree() {
    // A pose that OWNS its payload cannot be read as "running" with no loop, or
    // "subscribed" with no pump: the projection is derived from the pose, so
    // there is no second slot to fall out of step with it.
    let driver = driver();
    assert_eq!(driver.current_phase(), PumpPhase::Detached);
    assert!(!driver.current_phase().live_loop_armed());

    seed_connected(&driver, parked_stream());
    assert_eq!(driver.current_phase(), PumpPhase::Subscribed);
    assert!(driver.current_phase().holds_subscription());
    assert!(!driver.current_phase().live_loop_armed());

    let (_stream, claim) = driver
        .claim_consumer()
        .expect("one hand-off per session")
        .split();
    runtime()
        .block_on(driver.run(claim))
        .expect("run over a seeded connected session");
    assert_eq!(driver.current_phase(), PumpPhase::Running);
    assert!(driver.current_phase().live_loop_armed());
    runtime().block_on(driver.close());
}

// ── invariant: one claim, one run ──────────────────────────────────────────

#[test]
fn claim_consumer_is_once_only_and_mints_a_non_cloneable_claim() {
    let driver = driver();
    let handoff = driver.claim_consumer().expect("the first hand-off");
    assert!(
        matches!(driver.claim_consumer(), Err(ClaimRefusal::AlreadyClaimed)),
        "a second claim is refused by name"
    );
    // The claim travels WITH the stream: a consumer cannot hold one without the
    // other, which is what makes attachment a type fact.
    let (_stream, _claim) = handoff.split();
}

// ── invariant: an in-flight move refuses a concurrent one, typed ───────────

#[test]
fn a_concurrent_lifecycle_move_is_refused_as_in_flight() {
    let driver = driver();
    seed_connected(&driver, parked_stream());
    let (_stream, claim) = driver.claim_consumer().expect("the hand-off").split();
    let gate = driver.hold_run_in_flight_for_test();

    let running = {
        let driver = Arc::clone(&driver);
        runtime().spawn(async move { driver.run(claim).await })
    };

    // The in-flight pose is published before the run's first await.
    runtime().block_on(async {
        while driver.current_phase() != PumpPhase::Resuming {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    // A second move while the first is in flight is the table's typed in-flight
    // refusal, not a prose complaint about session state.
    match runtime().block_on(driver.connect("ws://127.0.0.1:1")) {
        Err(ConnectRefusal::Pose(phase)) => {
            assert_eq!(phase.method, "connect");
            assert_eq!(phase.current, PumpPhase::Resuming);
            assert!(
                phase.detail.contains("in flight"),
                "the refusal names the in-flight move: {}",
                phase.detail
            );
        }
        other => panic!("a concurrent connect must be the typed pose refusal, got {other:?}"),
    }

    drop(gate);
    let landed = runtime()
        .block_on(running)
        .expect("the run future resolves");
    assert!(landed.is_ok(), "the held run lands once the gate releases");
    runtime().block_on(driver.close());
}

// ── the two ordering holes, as named regressions ───────────────────────────

#[test]
fn a_resume_landing_after_close_cannot_install_a_live_loop() {
    // Regression (ADR-093 §1.3(a)): `resume` used to land `Running` and then
    // store the handle unconditionally, so a `stop` interleaving in that window
    // landed `Stopped` with a live pump nobody joined.
    let driver = driver();
    seed_connected(&driver, parked_stream());
    let (_stream, claim) = driver.claim_consumer().expect("the hand-off").split();
    let gate = driver.hold_run_in_flight_for_test();

    let running = {
        let driver = Arc::clone(&driver);
        runtime().spawn(async move { driver.run(claim).await })
    };
    runtime().block_on(async {
        while driver.current_phase() != PumpPhase::Resuming {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    // The close requests termination and WAITS for the in-flight move: it lands
    // the teardown pose first, then the held move unwinds into its refusal.
    let closing = {
        let driver = Arc::clone(&driver);
        runtime().spawn(async move { driver.close().await })
    };
    runtime().block_on(async {
        while driver.current_phase() != PumpPhase::Stopping {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    drop(gate);
    let report = runtime().block_on(closing).expect("the close resolves");

    assert_eq!(report.from, PumpPhase::Resuming);
    assert_eq!(driver.current_phase(), PumpPhase::Stopped);
    assert!(
        !driver.current_phase().live_loop_armed(),
        "a live loop must never be armed after the terminal pose"
    );

    // The in-flight move unwinds: its landing declines, and its guard aborts the
    // loop instead of installing it.
    let outcome = runtime()
        .block_on(running)
        .expect("the run future resolves");
    assert!(
        outcome.is_err(),
        "the landing after the close is a typed refusal, not a silent install"
    );
    assert_eq!(driver.current_phase(), PumpPhase::Stopped);
}

#[test]
fn a_crawl_attach_after_close_begins_is_refused() {
    // Regression (ADR-093 §1.3(b)): `begin_crawl_with` guarded only "one crawl
    // per driver", so a crawl booted after `Stopped` installed its run and kept
    // running for the process's lifetime, joined by nobody.
    let driver = driver();
    runtime().block_on(driver.close());
    assert_eq!(driver.current_phase(), PumpPhase::Stopped);

    match driver.begin_crawl(crawl_config_for_test()) {
        Err(CrawlRefusal::Pose(phase)) => {
            assert_eq!(phase.method, "crawl");
            assert_eq!(phase.current, PumpPhase::Stopped);
        }
        Ok(_) => panic!("a crawl attach after close must be refused, not booted"),
        Err(other) => panic!("a crawl attach after close must be the pose refusal, got {other}"),
    }
    assert!(
        driver.crawl_handle().is_none(),
        "a refused attach installs nothing"
    );
}

// ── close: total, asynchronous, shared ─────────────────────────────────────

#[test]
fn close_during_connect_requests_termination_and_waits_for_the_move_to_unwind() {
    let driver = driver();
    let gate = driver.hold_connect_in_flight_for_test();
    let connecting = {
        let driver = Arc::clone(&driver);
        runtime().spawn(async move { driver.connect("ws://127.0.0.1:1").await })
    };
    runtime().block_on(async {
        while driver.current_phase() != PumpPhase::Connecting {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    // The close requests termination and waits for the held move to unwind.
    let closing = {
        let driver = Arc::clone(&driver);
        runtime().spawn(async move { driver.close().await })
    };
    runtime().block_on(async {
        while driver.current_phase() != PumpPhase::Stopping {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    drop(gate);
    let report = runtime().block_on(closing).expect("the close resolves");
    assert_eq!(report.from, PumpPhase::Connecting);
    assert_eq!(driver.current_phase(), PumpPhase::Stopped);

    // The connect unwinds through its guard's abandonment edge (refused, because
    // the close already landed) and reports the refusal rather than succeeding.
    let connected = runtime()
        .block_on(connecting)
        .expect("the connect resolves");
    assert!(connected.is_err(), "a connect cannot commit after close");
}

#[test]
fn concurrent_close_callers_observe_one_shared_report() {
    let driver = driver();
    seed_connected(&driver, parked_stream());

    let a = {
        let driver = Arc::clone(&driver);
        runtime().spawn(async move { driver.close().await })
    };
    let b = {
        let driver = Arc::clone(&driver);
        runtime().spawn(async move { driver.close().await })
    };
    let (first, second) = runtime().block_on(async { tokio::join!(a, b) });
    let first: TeardownReport = first.expect("close a");
    let second: TeardownReport = second.expect("close b");

    assert_eq!(
        first, second,
        "concurrent callers share one teardown report"
    );
    assert_eq!(first.from, PumpPhase::Subscribed);
    assert_eq!(driver.current_phase(), PumpPhase::Stopped);

    // Idempotent: a third call is the same report, not a second teardown.
    let third = runtime().block_on(driver.close());
    assert_eq!(third, first, "close is idempotent");
}

// ── every pump exit: one typed fact, the right landing ─────────────────────

#[test]
fn a_stream_end_lands_ended_and_publishes_one_session_end_fact() {
    let driver = driver();
    seed_connected(&driver, ended_stream());
    let (_stream, claim) = driver.claim_consumer().expect("the hand-off").split();
    runtime()
        .block_on(driver.run(claim))
        .expect("run over an already-ended stream");

    // The loop returns on its own; its exit guard publishes `PumpExited`.
    runtime().block_on(async {
        while driver.current_phase() != PumpPhase::Ended {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    assert_eq!(
        runtime().block_on(driver.wait_session_end()),
        crate::arb_engine::SessionEndCause::PumpFinished
    );
    // Exactly one exit fact: a second observation is the retained fact.
    assert_eq!(
        runtime().block_on(driver.wait_session_end()),
        crate::arb_engine::SessionEndCause::PumpFinished
    );
    runtime().block_on(driver.close());
    assert_eq!(driver.current_phase(), PumpPhase::Stopped);
}

#[test]
fn a_panicking_live_loop_lands_ended_and_reports_the_join_error() {
    let driver = driver();
    // A stand-in pump that panics: the exit shape a real pump's hard abort
    // takes, without a WS fixture to arrange it through.
    driver.seed_session_for_test(SessionSeed::Running(Box::pin(async {
        panic!("simulated pump panic");
    })));
    assert_eq!(driver.current_phase(), PumpPhase::Running);

    // A panic is a completion too: the broadcast closes during the unwind, and
    // the exit guard has published the pose edge by then.
    assert_eq!(
        runtime().block_on(driver.wait_session_end()),
        crate::arb_engine::SessionEndCause::PumpFinished
    );
    assert_eq!(
        driver.current_phase(),
        PumpPhase::Ended,
        "a panic is one pump exit, and it lands the table"
    );
    let report = runtime().block_on(driver.close());
    assert_eq!(
        report.pump,
        PumpExit::JoinError,
        "a panic is reported as the join error it is"
    );
    assert_eq!(driver.current_phase(), PumpPhase::Stopped);
}

#[test]
fn a_shutdown_return_lands_ended() {
    // The pump's own early returns (a too-deep reorg, the shutdown flag) leave
    // the loop the same way a stream end does; the exit edge is one door.
    let driver = driver();
    driver.seed_session_for_test(SessionSeed::Running(Box::pin(async {})));
    // Event-driven, not a spin: the completion broadcast resolves when the loop
    // is gone.
    assert_eq!(
        runtime().block_on(driver.wait_session_end()),
        crate::arb_engine::SessionEndCause::PumpFinished
    );
    assert_eq!(driver.current_phase(), PumpPhase::Ended);
    runtime().block_on(driver.close());
    assert_eq!(driver.current_phase(), PumpPhase::Stopped);
}

// ── backfill and cancellation: Detached versus terminal Abandoned ──────────

#[test]
fn a_failed_backfill_lands_abandoned_and_reports_the_unapplied_gap_as_data() {
    let driver = driver();
    let (_stream, claim) = driver.claim_consumer().expect("the hand-off").split();
    // S = 85 < W = 100: a real gap, and the injected failure makes it unclosed.
    driver.set_snapshot_seed_block(Some(85));
    driver.inject_backfill_fault_for_test();
    seed_connected(&driver, parked_stream());

    let err = runtime().block_on(driver.run(claim)).unwrap_err();
    match err {
        DriverError::Backfill(gap) => {
            assert_eq!((gap.from, gap.to), (86, 100), "the unapplied range is data");
        }
        other @ DriverError::Phase(_) => {
            panic!("a failed backfill must be the typed gap, got {other:?}")
        }
    }
    assert_eq!(driver.current_phase(), PumpPhase::Abandoned);
    assert!(!driver.current_phase().live_loop_armed());
    runtime().block_on(driver.close());
}

#[test]
fn a_cancelled_run_lands_abandoned_and_only_close_is_legal_after() {
    // Not cancel-safe by restoration: the pump and its WS session die with the
    // future, so the honest landing is the terminal `Abandoned`.
    let driver = driver();
    seed_connected(&driver, parked_stream());
    let (_stream, claim) = driver.claim_consumer().expect("the hand-off").split();
    let gate = driver.hold_run_in_flight_for_test();
    let running = {
        let driver = Arc::clone(&driver);
        runtime().spawn(async move { driver.run(claim).await })
    };
    runtime().block_on(async {
        while driver.current_phase() != PumpPhase::Resuming {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    running.abort();
    assert!(
        runtime().block_on(running).is_err(),
        "the future is cancelled"
    );
    drop(gate);
    assert_eq!(driver.current_phase(), PumpPhase::Abandoned);

    // The re-subscribe edge is gone, by name.
    match runtime().block_on(driver.connect("ws://127.0.0.1:1")) {
        Err(ConnectRefusal::Pose(phase)) => {
            assert_eq!(phase.current, PumpPhase::Abandoned);
            assert_eq!(
                phase.detail,
                PumpDecline::SubscribeAfterAbandonment.to_string()
            );
        }
        other => panic!("a re-connect after abandonment must decline by name, got {other:?}"),
    }
    // `close` is the one remaining exit.
    runtime().block_on(driver.close());
    assert_eq!(driver.current_phase(), PumpPhase::Stopped);
}

#[test]
fn a_failed_connect_lands_detached_and_stays_retryable() {
    let driver = driver();
    for fault in [ConnectFault::Handshake, ConnectFault::NoStream] {
        driver.inject_connect_fault_for_test(fault);
        match runtime().block_on(driver.connect("ws://127.0.0.1:1")) {
            Err(ConnectRefusal::Transport(refusal)) => {
                assert_eq!(refusal, ConnectFault::refusal(fault));
            }
            other => panic!("a failed handshake must be the typed transport, got {other:?}"),
        }
        assert_eq!(
            driver.current_phase(),
            PumpPhase::Detached,
            "{fault:?} restores detached, so a retry is legal"
        );
    }
    driver.inject_connect_fault_for_test(ConnectFault::Handshake);
    match runtime().block_on(driver.connect("ws://127.0.0.1:1")) {
        Err(ConnectRefusal::Transport(_)) => {}
        other => panic!("the retry must reach the WS step again, got {other:?}"),
    }
}

// ── the ordered stream ─────────────────────────────────────────────────────

#[test]
fn block_n_precedes_every_batch_n_and_zero_batches_are_legal() {
    let driver = driver();
    let (mut stream, _claim) = driver.claim_consumer().expect("the hand-off").split();

    // The PRODUCER owns the order: a header tick reaches the stream before that
    // block's batches, and a block with no batches is legal.
    let stages = Arc::clone(driver.stages());
    runtime().block_on(async {
        PumpControl::notify_block(&*stages, 7, &BlockMetadata::default());
        PumpControl::notify_block(&*stages, 8, &BlockMetadata::default());
        stages.emit_batch_for_test(8);

        let mut blocks = Vec::new();
        let mut batches = Vec::new();
        for _ in 0..3 {
            match stream.recv().await.expect("three events") {
                SessionEvent::Block(n) => blocks.push(n.number),
                SessionEvent::Batch(b) => batches.push(b.solve_block),
            }
        }
        assert_eq!(blocks, vec![7, 8], "one Block per epoch, ascending");
        assert_eq!(batches, vec![8], "zero or more batches per epoch");
    });
    runtime().block_on(driver.close());
}

#[test]
fn the_session_stream_ends_exactly_once() {
    let driver = driver();
    let (mut stream, _claim) = driver.claim_consumer().expect("the hand-off").split();
    runtime().block_on(driver.close());

    let end = runtime().block_on(async { stream.recv().await });
    assert!(end.is_none(), "the natural end is the stream's closure");
    assert!(
        runtime().block_on(async { stream.recv().await }).is_none(),
        "exactly once: a second read stays ended"
    );
}
