//! Lifecycle, read-path, and pump tests for [`SubscriptionHandle`].
//!
//! The seams are the ones a pump and a reader use in production: lifecycle events
//! through [`SubscriptionHandle::apply`], items through [`drive_items`] (or
//! [`buffer_item`]), reads through [`SubscriptionHandle::read_next`] and
//! [`SubscriptionHandle::drain_raw`]. No live transport is involved; the fork lane
//! covers real reth subscription behaviour.

#![expect(clippy::unwrap_used, reason = "test assertions fail loudly")]
#![expect(clippy::panic, reason = "test assertions fail loudly")]

use super::{
    buffer_item, drive_items, pump_header_stream, DrainNotice, LifecycleDecline, LifecycleEvent,
    RawSubItem, ReadInProgress, ReadOutcome, StartupResult, SubscriptionHandle,
    SubscriptionLifecycle, SubscriptionPhase, TerminalNotice,
};
use futures_util::stream;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::time::timeout;

/// A handle with no pump attached: the lifecycle starts at
/// [`SubscriptionPhase::Starting`].
fn make_handle() -> Arc<SubscriptionHandle> {
    Arc::new(SubscriptionHandle::new())
}

/// A handle in the ordinary mid-life phase.
fn streaming_handle() -> Arc<SubscriptionHandle> {
    let handle = make_handle();
    handle.apply(LifecycleEvent::Started).unwrap();
    handle
}

fn header() -> RawSubItem {
    RawSubItem::Header(alloy::rpc::types::Header::default())
}

fn log() -> RawSubItem {
    RawSubItem::Log(alloy::rpc::types::Log::default())
}

fn every_phase() -> Vec<SubscriptionPhase> {
    vec![
        SubscriptionPhase::Starting,
        SubscriptionPhase::Streaming,
        SubscriptionPhase::Reconnecting {
            message: "stalled".to_string(),
        },
        SubscriptionPhase::Ended,
        SubscriptionPhase::Failed {
            message: "boom".to_string(),
        },
        SubscriptionPhase::Unsubscribed,
    ]
}

fn every_event() -> Vec<LifecycleEvent> {
    vec![
        LifecycleEvent::Started,
        LifecycleEvent::StartupFailed {
            message: "no endpoint".to_string(),
        },
        LifecycleEvent::Stall {
            message: "stalled".to_string(),
        },
        LifecycleEvent::Resumed,
        LifecycleEvent::End,
        LifecycleEvent::Fail {
            message: "boom".to_string(),
        },
        LifecycleEvent::Unsubscribe,
    ]
}

fn lifecycle_in(phase: &SubscriptionPhase) -> SubscriptionLifecycle {
    let mut lifecycle = SubscriptionLifecycle::starting();
    loop {
        if lifecycle.phase() == phase {
            return lifecycle;
        }
        // Reach the requested phase through admissible transitions only.
        let event = match *phase {
            SubscriptionPhase::Starting => break,
            SubscriptionPhase::Streaming => LifecycleEvent::Started,
            SubscriptionPhase::Reconnecting { .. } => LifecycleEvent::Stall {
                message: "stalled".to_string(),
            },
            SubscriptionPhase::Ended => LifecycleEvent::End,
            SubscriptionPhase::Failed { .. } => LifecycleEvent::Fail {
                message: "boom".to_string(),
            },
            SubscriptionPhase::Unsubscribed => LifecycleEvent::Unsubscribe,
        };
        let Some(next) = lifecycle.next(&event) else {
            break;
        };
        lifecycle = next;
    }
    assert_eq!(lifecycle.phase(), phase, "could not reach {phase:?}");
    lifecycle
}

fn next_phase(phase: &SubscriptionPhase, event: &LifecycleEvent) -> Option<SubscriptionPhase> {
    lifecycle_in(phase)
        .next(event)
        .map(|lifecycle| lifecycle.phase().clone())
}

/// A test-fed item stream, so a pump can be observed mid-life (items without a
/// terminal end yet).
struct TestStream {
    receiver: tokio::sync::mpsc::UnboundedReceiver<Result<RawSubItem, String>>,
}

impl futures_util::Stream for TestStream {
    type Item = Result<RawSubItem, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

fn test_stream() -> (
    tokio::sync::mpsc::UnboundedSender<Result<RawSubItem, String>>,
    TestStream,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    (sender, TestStream { receiver })
}

// ---------------------------------------------------------------------------
// The lifecycle matrix
// ---------------------------------------------------------------------------

/// A cell where the event is admissible but changes nothing: either a duplicate
/// ack for a phase that already published it, or a re-stall that repeats the
/// message already carried.
fn is_documented_noop(phase: &SubscriptionPhase, event: &LifecycleEvent) -> bool {
    use LifecycleEvent as E;
    use SubscriptionPhase as P;

    match (phase, event) {
        // A handle that never started has no resubscribe to resume. A running or
        // re-subscribing handle has already published its ack, so a duplicate ack
        // or a startup failure changes nothing.
        (P::Starting, E::Resumed)
        | (P::Streaming, E::Started | E::Resumed | E::StartupFailed { .. })
        | (P::Reconnecting { .. }, E::StartupFailed { .. }) => true,
        // A re-stall that repeats the message already carried changes nothing.
        (P::Reconnecting { message }, E::Stall { message: stalled }) => message == stalled,
        _ => false,
    }
}

/// Every (phase, event) cell has a defined outcome, terminal phases absorb every
/// event, and the startup latch resolves exactly once.
#[test]
fn the_startup_stream_event_matrix_is_total() {
    for phase in every_phase() {
        for event in every_event() {
            let lifecycle = lifecycle_in(&phase);
            let next = lifecycle.next(&event);

            if phase.is_terminal() {
                assert!(
                    next.is_none(),
                    "{phase:?} is terminal and must absorb {event:?}"
                );
                continue;
            }

            if is_documented_noop(&phase, &event) {
                assert!(
                    next.is_none(),
                    "{phase:?} + {event:?} is a documented no-op"
                );
                continue;
            }

            let next = next.unwrap_or_else(|| panic!("{phase:?} has no transition for {event:?}"));

            // The startup latch resolves once and is never revised.
            match (lifecycle.startup(), next.phase()) {
                (Some(resolved), _) => assert_eq!(
                    next.startup(),
                    Some(resolved),
                    "{phase:?} + {event:?} must not revise a resolved startup"
                ),
                (None, SubscriptionPhase::Starting | SubscriptionPhase::Reconnecting { .. }) => {
                    assert!(
                        next.startup().is_none(),
                        "{phase:?} + {event:?} must leave startup pending, got {:?}",
                        next.startup()
                    );
                }
                (None, SubscriptionPhase::Streaming) => {
                    assert_eq!(next.startup(), Some(&StartupResult::Started));
                }
                (None, SubscriptionPhase::Failed { message }) => {
                    assert_eq!(
                        next.startup(),
                        Some(&StartupResult::Failed {
                            message: message.clone()
                        })
                    );
                }
                (None, SubscriptionPhase::Ended | SubscriptionPhase::Unsubscribed) => {
                    assert!(
                        matches!(next.startup(), Some(StartupResult::Closed { .. })),
                        "{phase:?} + {event:?} must close startup, got {:?}",
                        next.startup()
                    );
                }
            }
        }
    }
}

/// The design-critical cells, named so the table cannot drift silently.
#[test]
fn the_lifecycle_table_names_each_design_critical_transition() {
    use LifecycleEvent as E;
    use SubscriptionPhase as P;

    let stalled = P::Reconnecting {
        message: "stalled".to_string(),
    };

    // A start ack moves a fresh handle into streaming; a duplicate ack is a no-op.
    assert_eq!(next_phase(&P::Starting, &E::Started), Some(P::Streaming));
    assert_eq!(next_phase(&P::Streaming, &E::Started), None);
    assert_eq!(next_phase(&P::Starting, &E::Resumed), None);

    // Reconnect is transient: stall in, resume out, identical re-stall is a no-op.
    assert_eq!(
        next_phase(
            &P::Streaming,
            &E::Stall {
                message: "stalled".to_string()
            }
        ),
        Some(stalled.clone())
    );
    assert_eq!(next_phase(&stalled, &E::Resumed), Some(P::Streaming));
    assert_eq!(
        next_phase(
            &stalled,
            &E::Stall {
                message: "stalled".to_string()
            }
        ),
        None
    );
    assert_eq!(
        next_phase(
            &stalled,
            &E::Stall {
                message: "again".to_string()
            }
        ),
        Some(P::Reconnecting {
            message: "again".to_string()
        })
    );

    // Terminal outcomes, from every live phase.
    for phase in [P::Starting, P::Streaming, stalled] {
        assert_eq!(next_phase(&phase, &E::End), Some(P::Ended));
        assert_eq!(
            next_phase(
                &phase,
                &E::Fail {
                    message: "boom".to_string()
                }
            ),
            Some(P::Failed {
                message: "boom".to_string()
            })
        );
        assert_eq!(next_phase(&phase, &E::Unsubscribe), Some(P::Unsubscribed));
    }

    // Startup failure is only admissible while starting.
    assert_eq!(
        next_phase(
            &P::Starting,
            &E::StartupFailed {
                message: "no endpoint".to_string()
            }
        ),
        Some(P::Failed {
            message: "no endpoint".to_string()
        })
    );
    assert_eq!(
        next_phase(
            &P::Streaming,
            &E::StartupFailed {
                message: "no endpoint".to_string()
            }
        ),
        None
    );
}

/// An illegal move is a typed decline carrying the phase and the event, not a
/// falsy return.
#[test]
fn an_illegal_move_is_a_typed_decline() {
    let handle = streaming_handle();

    // An event that means nothing in this phase.
    let decline = handle.apply(LifecycleEvent::Started).unwrap_err();
    assert_eq!(
        decline,
        LifecycleDecline::Inadmissible {
            phase: SubscriptionPhase::Streaming,
            event: LifecycleEvent::Started,
        }
    );
    assert_eq!(decline.phase(), &SubscriptionPhase::Streaming);
    assert_eq!(decline.event(), &LifecycleEvent::Started);

    // A terminal phase absorbs every later event, and the decline says which.
    handle.apply(LifecycleEvent::End).unwrap();
    let late = LifecycleEvent::Stall {
        message: "late".to_string(),
    };
    let decline = handle.apply(late.clone()).unwrap_err();
    assert_eq!(
        decline,
        LifecycleDecline::Terminal {
            phase: SubscriptionPhase::Ended,
            event: late,
        }
    );
    assert_eq!(decline.phase(), &SubscriptionPhase::Ended);
}

// ---------------------------------------------------------------------------
// Publication order
// ---------------------------------------------------------------------------

/// Publication must happen inside the critical section that writes the state.
///
/// The hook fires in the state-write/publication window. A second `apply` started
/// there must NOT complete: if it could, two concurrent applies could publish in
/// the opposite order from their authoritative state writes, and a retained
/// subscriber would read a phase older than the authority's last transition.
#[test]
fn a_publication_cannot_be_displaced_by_a_concurrent_transition() {
    let handle = streaming_handle();
    let mut watcher = handle.subscribe_lifecycle();

    let completed = Arc::new(AtomicUsize::new(0));
    let armed = Arc::new(AtomicBool::new(true));
    let (attempt_sender, attempt_receiver) = std::sync::mpsc::channel::<()>();

    let racer = {
        let handle = Arc::clone(&handle);
        let completed = Arc::clone(&completed);
        std::thread::spawn(move || {
            attempt_receiver.recv().unwrap();
            let _ = handle.apply(LifecycleEvent::End);
            completed.fetch_add(1, Ordering::SeqCst);
        })
    };

    let hook_completed = Arc::clone(&completed);
    let hook_armed = Arc::clone(&armed);
    handle.set_publish_hook(move || {
        if !hook_armed.load(Ordering::SeqCst) {
            return;
        }
        // One shot: the transition under test is the only one that interleaves.
        hook_armed.store(false, Ordering::SeqCst);
        let _ = attempt_sender.send(());
        // Bounded window in which a displaced publication would let the racer
        // finish. The fixed implementation blocks the racer on the authority
        // lock for the whole window, so no timing assumption decides the verdict.
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_millis(200) {
            assert_eq!(
                hook_completed.load(Ordering::SeqCst),
                0,
                "a concurrent transition completed inside the state-write/publication window: \
                 the retained publication is not ordered by the lifecycle authority"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    handle
        .apply(LifecycleEvent::Stall {
            message: "stalled".to_string(),
        })
        .unwrap();

    racer.join().unwrap();
    assert_eq!(
        completed.load(Ordering::SeqCst),
        1,
        "the racer must still land"
    );

    // The retained publication ends on the authority's last transition, never
    // behind it.
    let published = watcher.borrow_and_update().clone();
    assert_eq!(published, handle.lifecycle());
    assert_eq!(published.phase(), &SubscriptionPhase::Ended);
}

// ---------------------------------------------------------------------------
// Startup: retained, multi-waiter, resolved exactly once
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_startup_waiter_observes_the_same_success() {
    let handle = make_handle();

    let early = (0..3)
        .map(|_| {
            let handle = Arc::clone(&handle);
            tokio::spawn(async move { handle.await_startup().await })
        })
        .collect::<Vec<_>>();
    tokio::task::yield_now().await;

    handle.apply(LifecycleEvent::Started).unwrap();
    for waiter in early {
        assert_eq!(waiter.await.unwrap(), StartupResult::Started);
    }

    // Late and repeated waiters observe the same latched success.
    assert_eq!(handle.await_startup().await, StartupResult::Started);
    assert_eq!(handle.await_startup().await, StartupResult::Started);
}

#[tokio::test]
async fn every_startup_waiter_observes_the_same_failure() {
    let handle = make_handle();

    let early = (0..3)
        .map(|_| {
            let handle = Arc::clone(&handle);
            tokio::spawn(async move { handle.await_startup().await })
        })
        .collect::<Vec<_>>();
    tokio::task::yield_now().await;

    handle
        .apply(LifecycleEvent::StartupFailed {
            message: "no endpoint".to_string(),
        })
        .unwrap();
    let expected = StartupResult::Failed {
        message: "no endpoint".to_string(),
    };
    for waiter in early {
        assert_eq!(waiter.await.unwrap(), expected);
    }
    assert_eq!(handle.await_startup().await, expected);
}

#[tokio::test]
async fn a_startup_waiter_blocks_until_startup_resolves() {
    let handle = make_handle();

    let waiter = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { handle.await_startup().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!waiter.is_finished(), "startup must not resolve early");

    handle.apply(LifecycleEvent::Started).unwrap();
    assert_eq!(
        timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap(),
        StartupResult::Started
    );
}

#[tokio::test]
async fn an_ack_after_unsubscribe_cannot_report_active() {
    let handle = make_handle();

    handle.unsubscribe();
    assert_eq!(
        handle.apply(LifecycleEvent::Started),
        Err(LifecycleDecline::Terminal {
            phase: SubscriptionPhase::Unsubscribed,
            event: LifecycleEvent::Started,
        }),
        "a late start ack must not reopen an unsubscribed handle"
    );
    assert_eq!(handle.phase(), SubscriptionPhase::Unsubscribed);
    assert_eq!(
        handle.await_startup().await,
        StartupResult::Closed {
            phase: SubscriptionPhase::Unsubscribed
        }
    );
}

// ---------------------------------------------------------------------------
// Read paths observe one terminal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_terminal_reached_by_iteration_is_observed_by_the_bulk_drain() {
    let handle = streaming_handle();
    drive_items(&handle, stream::iter(vec![Ok(header())])).await;

    assert!(matches!(handle.read_next().await, ReadOutcome::Item(_)));
    assert!(matches!(
        handle.read_next().await,
        ReadOutcome::Terminal(TerminalNotice::Ended)
    ));

    let drain = handle.drain_raw();
    assert!(drain.items.is_empty(), "the item was already read");
    assert_eq!(
        drain.notice,
        Some(DrainNotice::Terminal(TerminalNotice::Ended))
    );
}

#[tokio::test]
async fn a_terminal_reached_by_the_bulk_drain_is_observed_by_iteration() {
    let handle = streaming_handle();
    drive_items(&handle, stream::iter(vec![Ok(header())])).await;

    let drain = handle.drain_raw();
    assert_eq!(
        drain.items.len(),
        1,
        "pre-terminal items belong to this drain"
    );
    assert_eq!(
        drain.notice,
        Some(DrainNotice::Terminal(TerminalNotice::Ended))
    );

    assert!(matches!(
        handle.read_next().await,
        ReadOutcome::Terminal(TerminalNotice::Ended)
    ));
}

#[tokio::test]
async fn a_terminal_failure_is_observed_by_every_read_path() {
    let handle = streaming_handle();
    drive_items(
        &handle,
        stream::iter(vec![Err("transport died".to_string())]),
    )
    .await;

    let expected = DrainNotice::Terminal(TerminalNotice::Failed {
        message: "transport died".to_string(),
    });
    assert_eq!(handle.drain_raw().notice, Some(expected.clone()));
    assert!(matches!(
        handle.read_next().await,
        ReadOutcome::Terminal(TerminalNotice::Failed { .. })
    ));
}

#[tokio::test]
async fn items_before_the_terminal_are_delivered_then_the_terminal_repeats() {
    let handle = streaming_handle();
    drive_items(&handle, stream::iter(vec![Ok(header()), Ok(log())])).await;

    assert!(matches!(
        handle.read_next().await,
        ReadOutcome::Item(RawSubItem::Header(_))
    ));
    assert!(matches!(
        handle.read_next().await,
        ReadOutcome::Item(RawSubItem::Log(_))
    ));
    for _ in 0..2 {
        assert!(matches!(
            handle.read_next().await,
            ReadOutcome::Terminal(TerminalNotice::Ended)
        ));
    }

    // A write that arrives after the terminal is refused, so no trailing data can
    // ever appear behind the terminal notice.
    buffer_item(&handle, header());
    let drain = handle.drain_raw();
    assert!(drain.items.is_empty(), "post-terminal data must be refused");
    assert_eq!(
        drain.notice,
        Some(DrainNotice::Terminal(TerminalNotice::Ended))
    );
}

// ---------------------------------------------------------------------------
// Reconnect is transient
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_watchdog_stall_is_a_transient_reconnect_not_a_terminal_end() {
    let handle = make_handle();

    // A header-shaped stream that NEVER yields: a silently half-open socket where
    // `stream.next()` never resolves.
    let stalled: stream::Pending<alloy::rpc::types::Header> = stream::pending();
    let start = tokio::time::Instant::now();

    pump_header_stream(
        Arc::clone(&handle),
        stalled,
        Duration::from_millis(20),
        || async move { None },
    )
    .await;

    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the watchdog must fire within a bounded window"
    );
    match handle.phase() {
        SubscriptionPhase::Reconnecting { message } => assert!(
            message.contains("newHeads subscription stall"),
            "the stall message must carry the greppable prefix: {message}"
        ),
        other => panic!("expected a transient reconnect, got {other:?}"),
    }
    assert!(!handle.is_terminal(), "a stall must not be terminal");

    // A successful resubscribe returns to streaming.
    handle.apply(LifecycleEvent::Resumed).unwrap();
    assert_eq!(handle.phase(), SubscriptionPhase::Streaming);
}

#[tokio::test]
async fn a_reconnecting_subscription_keeps_the_read_side_open() {
    let handle = make_handle();
    handle
        .apply(LifecycleEvent::Stall {
            message: "stalled".to_string(),
        })
        .unwrap();

    let reader = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { handle.read_next().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !reader.is_finished(),
        "a reconnect must not close the iteration"
    );

    let (sender, items) = test_stream();
    let pump = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { drive_items(&handle, items).await }
    });

    handle.apply(LifecycleEvent::Resumed).unwrap();
    sender.send(Ok(header())).unwrap();

    let outcome = timeout(Duration::from_secs(1), reader)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(outcome, ReadOutcome::Item(RawSubItem::Header(_))),
        "data delivered after the resubscribe must reach the reader"
    );

    drop(sender);
    pump.await.unwrap();
}

/// A read must re-arm the coalesced signal: otherwise the next write sees it
/// already set, sends no wake-up, and a reader parked after a completed read
/// never wakes.
#[tokio::test]
async fn a_write_after_a_completed_read_wakes_the_waiting_reader() {
    let handle = streaming_handle();
    let (sender, items) = test_stream();
    let pump = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { drive_items(&handle, items).await }
    });

    sender.send(Ok(header())).unwrap();
    assert!(matches!(
        handle.read_next().await,
        ReadOutcome::Item(RawSubItem::Header(_))
    ));

    // Park a second reader, then write the item it is waiting for.
    let reader = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { handle.read_next().await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    sender.send(Ok(log())).unwrap();

    let outcome = timeout(Duration::from_secs(1), reader)
        .await
        .unwrap_or_else(|_| panic!("the reader must be woken by the write after the read"))
        .unwrap();
    assert!(matches!(outcome, ReadOutcome::Item(RawSubItem::Log(_))));

    drop(sender);
    pump.await.unwrap();
}

// ---------------------------------------------------------------------------
// The read side is claimed, not checked out
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_concurrent_read_gets_the_typed_refusal_instead_of_a_fake_close() {
    let handle = streaming_handle();

    let claim = handle.claim_read().unwrap();
    assert_eq!(
        handle.claim_read().unwrap_err(),
        ReadInProgress,
        "a second reader must get the typed refusal"
    );

    // A checked-out reader must not look like a closed subscription.
    assert!(!handle.is_terminal());
    assert!(handle.drain_raw().notice.is_none());

    drop(claim);
    let reclaimed = handle.claim_read().unwrap();
    assert_eq!(reclaimed.handle().phase(), SubscriptionPhase::Streaming);
}

// ---------------------------------------------------------------------------
// Unsubscribe
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unsubscribe_aborts_the_pump_and_is_terminal_against_a_late_pump_event() {
    let handle = streaming_handle();

    let (sender, items) = test_stream();
    let (done_sender, done_receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move {
            drive_items(&handle, items).await;
            let _ = done_sender.send(());
        }
    });
    *handle.task.lock() = Some(task);

    handle.unsubscribe();
    handle.unsubscribe(); // idempotent
    assert!(handle.task.lock().is_none(), "unsubscribe takes the pump");
    assert!(
        timeout(Duration::from_secs(1), done_receiver)
            .await
            .unwrap()
            .is_err(),
        "the abort must cancel the pump future instead of letting it finish"
    );

    assert_eq!(handle.phase(), SubscriptionPhase::Unsubscribed);
    for _ in 0..2 {
        assert!(matches!(
            handle.read_next().await,
            ReadOutcome::Terminal(TerminalNotice::Unsubscribed)
        ));
    }

    // A late pump event cannot reopen the handle, and a late write cannot append
    // trailing data behind the terminal notice.
    for late in [
        LifecycleEvent::Started,
        LifecycleEvent::End,
        LifecycleEvent::Fail {
            message: "late".to_string(),
        },
    ] {
        assert_eq!(
            handle.apply(late.clone()),
            Err(LifecycleDecline::Terminal {
                phase: SubscriptionPhase::Unsubscribed,
                event: late,
            }),
            "an unsubscribed handle absorbs every later event"
        );
    }
    buffer_item(&handle, header());
    let _ = sender.send(Ok(log()));

    let drain = handle.drain_raw();
    assert!(drain.items.is_empty(), "no post-terminal data");
    assert_eq!(
        drain.notice,
        Some(DrainNotice::Terminal(TerminalNotice::Unsubscribed))
    );
    assert_eq!(handle.phase(), SubscriptionPhase::Unsubscribed);
    assert!(matches!(
        handle.read_next().await,
        ReadOutcome::Terminal(TerminalNotice::Unsubscribed)
    ));
}

// ---------------------------------------------------------------------------
// The shared pump body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_pump_body_latches_a_clean_end_and_a_transport_failure() {
    let ended = streaming_handle();
    let empty: Vec<Result<RawSubItem, String>> = Vec::new();
    drive_items(&ended, stream::iter(empty)).await;
    assert_eq!(ended.phase(), SubscriptionPhase::Ended);

    let failed = streaming_handle();
    drive_items(
        &failed,
        stream::iter(vec![Err("transport died".to_string())]),
    )
    .await;
    assert_eq!(
        failed.phase(),
        SubscriptionPhase::Failed {
            message: "transport died".to_string()
        }
    );
}

#[tokio::test]
async fn a_healthy_header_stream_does_not_trip_the_watchdog_and_ends_cleanly() {
    let handle = make_handle();
    let headers = vec![
        alloy::rpc::types::Header::default(),
        alloy::rpc::types::Header::default(),
    ];

    pump_header_stream(
        Arc::clone(&handle),
        stream::iter(headers),
        Duration::from_mins(1),
        || async move { None },
    )
    .await;

    let drain = handle.drain_raw();
    assert_eq!(
        drain.items.len(),
        2,
        "both headers buffered, then a clean end"
    );
    assert_eq!(
        drain.notice,
        Some(DrainNotice::Terminal(TerminalNotice::Ended))
    );
}

// ---------------------------------------------------------------------------
// Double-buffer data plane
// ---------------------------------------------------------------------------

#[test]
fn drain_raw_swaps_buffers_so_a_write_is_delivered_exactly_once() {
    let handle = streaming_handle();

    buffer_item(&handle, header());
    let first = handle.drain_raw();
    assert_eq!(first.items.len(), 1);
    assert!(
        !handle.signaled.load(Ordering::Relaxed),
        "drain clears the signal"
    );

    buffer_item(&handle, log());
    let second = handle.drain_raw();
    assert!(matches!(second.items.as_slice(), [RawSubItem::Log(_)]));

    let third = handle.drain_raw();
    assert!(third.items.is_empty());
}

#[test]
fn drain_raw_on_a_quiet_subscription_reports_no_notice() {
    let handle = streaming_handle();
    let drain = handle.drain_raw();
    assert!(drain.items.is_empty());
    assert!(drain.notice.is_none());
}
