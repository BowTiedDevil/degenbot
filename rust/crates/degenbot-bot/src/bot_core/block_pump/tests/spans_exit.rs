use super::*;

/// MQUKB6 (epic KDUED5): one entered `degenbot.epoch` root span per
/// observed header, carrying a `block.number` field, parented under the
/// `run_with_stream` instrument span. In-memory exporter +
/// `set_global_default` (the repo convention: the thread-local `set_default`
/// is process-unsafe in a parallel test with cross-thread span handles —
/// `telemetry::publish_block_context` and the exact-match reparent store
/// carry otel span state across threads). No other lib test takes the
/// once-per-process global slot (the BGGTEG interest paint is
/// `not(otel)`-gated), so this test wins it; its `set_global_default`
/// repaints the callsite interest cache exactly like the paint would.
/// The `OTel` layer itself is covered by the `otel_plumbing` integration
/// tests.
#[cfg(feature = "otel")]
#[tokio::test]
async fn header_arms_per_block_span_with_number_and_parent() {
    const NEXT_BLOCK: u64 = MY_BLOCK + 1;
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;

    // Unique block number (0xDEADBEEF): with a global subscriber,
    // concurrent tests' pump spans land in this exporter too, so assert
    // on THIS test's header by number, not on total span counts.
    const MY_BLOCK: u64 = 0xDEAD_BEEF;
    const MY_BLOCK_I64: i64 = 0xDEAD_BEEF;

    let (mut pump, _sink) = pump_for_test(None);

    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    // Global subscriber (repo convention, `set_global_default` - the
    // thread-local `set_default` is process-unsafe in parallel tests). No
    // other lib test takes the once-per-process global slot.
    tracing::subscriber::set_global_default(subscriber)
        .expect("global default already set by another test");

    // JYCTXI: a second header exercises the consecutive-header case —
    // the new span must detach from the still-entered previous block
    // span (loop-context guard) instead of chaining into one mega-trace.
    let events: Vec<WsEvent> = vec![
        WsEvent::BlockHeader {
            number: MY_BLOCK,
            timestamp: 1,
            base_fee_per_gas: Some(1),
            gas_used: 1,
            gas_limit: 1,
        },
        WsEvent::BlockHeader {
            number: NEXT_BLOCK,
            timestamp: 2,
            base_fee_per_gas: Some(2),
            gas_used: 2,
            gas_limit: 2,
        },
    ];
    let combined = stream::iter(events).boxed();
    pump.run_test_loop(combined, MY_BLOCK - 1).await;

    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");

    // Select THIS test's header span by its unique number (tracing-
    // opentelemetry 0.33 maps u64 fields to strings; an OTel bump may
    // switch to I64 - accept both representations).
    let my_spans: Vec<_> = spans
            .iter()
            .filter(|sp| {
                sp.name.as_ref() == "degenbot.epoch.run"
                    && sp.attributes.iter().any(|kv| {
                        kv.key == opentelemetry::Key::from_static_str("epoch.block")
                            && (matches!(kv.value, opentelemetry::Value::I64(v) if v == MY_BLOCK_I64)
                                || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == MY_BLOCK.to_string().as_str()))
                    })
            })
            .collect();
    assert_eq!(
        my_spans.len(),
        1,
        "expected exactly one span for block {}; got names: {:?}",
        MY_BLOCK,
        spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
    );
    let block_span = &my_spans[0];

    // BF43PM: the epoch root carries the rewind generation (no reorg yet
    // in this fixture — seq 0).
    assert!(
        block_span.attributes.iter().any(|kv| {
            kv.key == opentelemetry::Key::from_static_str("epoch.seq")
                && matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == "0")
        }),
        "epoch root must carry epoch.seq; got {:?}",
        block_span.attributes
    );

    // MQUKB6-T0: the per-block span is now a trace ROOT — the former
    // `run_with_stream` instrument span was a never-closing root that OTel
    // never exported (orphaning every pump-task span under a missing
    // parent). Roots export cleanly; parent_span_id is the zero sentinel.
    assert_eq!(
        block_span.parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "per-block span for block {} must be a trace root; parent_span_id: {:?}",
        MY_BLOCK,
        block_span.parent_span_id
    );

    // JYCTXI: the NEXT header's span must ALSO be a trace root in its own
    // trace — created while block {}'s span was still entered (the loop
    // context guard), it must detach rather than chain into a mega-trace.
    let next_spans: Vec<_> = spans
            .iter()
            .filter(|sp| {
                sp.name.as_ref() == "degenbot.epoch.run"
                    && sp.attributes.iter().any(|kv| {
                        kv.key == opentelemetry::Key::from_static_str("epoch.block")
                            && (matches!(kv.value, opentelemetry::Value::I64(v) if v == i64::try_from(NEXT_BLOCK).unwrap_or(i64::MAX))
                                || matches!(kv.value, opentelemetry::Value::String(ref v) if v.as_str() == NEXT_BLOCK.to_string().as_str()))
                    })
            })
            .collect();
    assert_eq!(next_spans.len(), 1, "expected one span for the next header");
    let next_span = &next_spans[0];
    assert_eq!(
        next_span.parent_span_id,
        opentelemetry::trace::SpanId::INVALID,
        "consecutive-header span must also be a trace root; parent_span_id: {:?}",
        next_span.parent_span_id
    );
    assert_ne!(
        next_span.span_context.trace_id(),
        block_span.span_context.trace_id(),
        "consecutive headers must be separate traces (mega-trace regression)"
    );
}

/// TQ7PD6 regression: a header burst through the pump must CLOSE (export)
/// every per-epoch span, never leaking still-entered spans on worker
/// threads (the pre-fix loop-wide `Span::enter()` guard lived across the
/// select's await points; when the multi-threaded runtime migrated the task
/// between workers, it entered on one thread and dropped on another, so the
/// span stayed entered in the abandoned worker's TLS — never closed, never
/// exported, every child orphaned). The DETERMINISTIC defense is the
/// structural fix (no `enter` guard may outlive a poll); this test locks
/// the observable symptom — all N spans closed — and exercises cross-await
/// parking so CI load that DOES migrate the task surfaces the old leak.
///
/// SONJQA/G3 note (BF43PM): the pump-level `log_wait` force-close test
/// was retired with the `pump.log_wait` waterfall — quiet headers now open
/// NO stage span at all. The force-close law it pinned lives on as the
/// `stage_telemetry::otel_tests::stale_stage_span_exports_force_closed`
/// pinned export test against `StageTelemetry::force_close_aged`, driven
/// from the timed-exit tick with the same `stage_max_age` bound.
#[cfg(feature = "otel")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn header_burst_closes_every_block_span() {
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;

    const BASE: u64 = 0xBEEF_0000;
    const COUNT: u64 = 32;

    let (mut pump, _sink) = pump_for_test(None);
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
    // NB: set_global_default can only be installed once per process. This
    // test and the sibling header-span test both take it; cargo runs each
    // lib test in its own process by default, but to be robust against a
    // shared process use set_default (thread-local) where possible. The
    // header_arms test above uses the global slot; this one uses a local
    // guard so they can coexist under `--test-threads`.
    let _guard = tracing::subscriber::set_default(subscriber);

    let events: Vec<WsEvent> = (0..COUNT)
        .map(|i| WsEvent::BlockHeader {
            number: BASE + i,
            timestamp: 1,
            base_fee_per_gas: Some(1),
            gas_used: 1,
            gas_limit: 1,
        })
        .collect();
    // Force a park between headers: a ready stream never suspends, so the
    // task would stay on one worker and the pre-fix leaked-enter bug (which
    // only manifests when the task MIGRATES across an enter guard) would not
    // be exercised. A 1ms sleep makes every inter-header await pend, giving
    // the multi-threaded runtime a migration opportunity each iteration.
    let combined = stream::iter(events)
        .then(|e| async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            e
        })
        .boxed();
    pump.run_test_loop(combined, BASE - 1).await;
    // The channels may still be flushing; give the idle settle one beat.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");

    let mut seen = std::collections::HashSet::new();
    for sp in &spans {
        if sp.name.as_ref() == "degenbot.epoch.run" {
            for kv in &sp.attributes {
                if kv.key == opentelemetry::Key::from_static_str("epoch.block") {
                    if let opentelemetry::Value::String(ref v) = kv.value {
                        if let Ok(n) = v.as_str().parse::<u64>() {
                            seen.insert(n);
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        seen.len(),
        usize::try_from(COUNT).unwrap_or(usize::MAX),
        "every header must export a CLOSED epoch span; got {}/{}",
        seen.len(),
        COUNT
    );
}

/// S53STH: the cooperative timed-exit path must make a PARKED select wake
/// and return promptly (unwinding all span guards on this task) when the
/// hotpath timer raises the flag mid-park — not sit out the full settle
/// window, and never `process::exit`.
#[cfg(feature = "hotpath")]
#[tokio::test(flavor = "current_thread")]
async fn timed_exit_flag_exits_parked_select_promptly() {
    let (mut pump, _sink) = pump_for_test(None);
    // Raise the flag from outside after 100ms — mid-park on the select's
    // settle window. The 500ms timed-exit tick polls it and breaks the
    // loop; success is sub-second return (vs the 60s park regression).
    let flag = Arc::clone(&pump.shutdown);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let events: Vec<WsEvent> = vec![WsEvent::BlockHeader {
        number: 0xB000_0001,
        timestamp: 1,
        base_fee_per_gas: Some(1),
        gas_used: 1,
        gas_limit: 1,
    }];
    let started = std::time::Instant::now();
    pump.run_test_loop(stream::iter(events).boxed(), 0xB000_0000)
        .await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "watch-raised shutdown must exit promptly, took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn shutdown_flag_exits_loop_promptly() {
    let (mut pump, _sink) = pump_for_test(None);
    // Pre-raise: the very first select! arm sees the watch fire and breaks.
    pump.shutdown
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let events: Vec<WsEvent> = vec![WsEvent::BlockHeader {
        number: 0xB000_0001,
        timestamp: 1,
        base_fee_per_gas: Some(1),
        gas_used: 1,
        gas_limit: 1,
    }];
    let started = std::time::Instant::now();
    pump.run_test_loop(stream::iter(events).boxed(), 0xB000_0000)
        .await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "shutdown flag must exit the loop promptly, took {:?}",
        started.elapsed()
    );
}
