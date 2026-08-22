//! Console-vs-trace telemetry split (2026-08-22 logging audit).
//!
//! High-frequency diagnostic events use the [`DIAGNOSTIC_TARGET`] target so
//! the console sinks (stderr fmt + the Python log forwarder) can cap them via
//! [`DIAGNOSTIC_CONSOLE_CAP_DIRECTIVE`] while the `OTel` layer keeps full
//! detail: a Jaeger trace answers "what did the engine do" without the stdout
//! firehose, and stdout stays operator-grade. Warn and above always pass
//! every sink (the cap directive is at warn).
//!
//! Convention: any event that is per-header/per-event/per-solve noise on the
//! console but signal inside a trace gets `target: DIAGNOSTIC_TARGET`. An
//! explicit `RUST_LOG` override bypasses the cap entirely (power-user
//! contract: you asked for exactly what `RUST_LOG` says, on every sink).

/// Target for high-frequency diagnostic events (see module docs).
pub const DIAGNOSTIC_TARGET: &str = "degenbot::diag";

/// `EnvFilter` directive capping [`DIAGNOSTIC_TARGET`] at warn on console sinks.
pub const DIAGNOSTIC_CONSOLE_CAP_DIRECTIVE: &str = "degenbot::diag=warn";

/// Surface a failure through every telemetry sink, idiomatically:
///
/// 1. The ACTIVE SPAN is marked failed (`otel.status_code = "ERROR"`, mapped
///    by tracing-opentelemetry to span `STATUS_ERROR`) — Jaeger renders the
///    trace red and it becomes queryable via `tags={"error":"true"}`.
/// 2. An `exception` event per `OTel` semantic conventions (`exception.type` /
///    `exception.message`) is recorded onto that span, so one click shows
///    WHAT failed in full block/path/pool context.
/// 3. (Task D63GSE-2) the `degenbot.errors{kind}` counter — detection is
///    push-based via Prometheus; traces are for investigation.
///
/// # Convention
///
/// Every failure seam calls this EXACTLY ONCE per distinct failure, BEFORE
/// any exit/continue decision. Callers own dedup (see the failure-policy
/// task): an error storm must not become a counter/event storm. Errors use
/// [`DIAGNOSTIC_TARGET`]'s uncapped sibling treatment — they are emitted at
/// ERROR level, which passes EVERY sink including the console cap.
pub fn record_exception(kind: &'static str, err: impl std::fmt::Display) {
    let span = tracing::Span::current();
    span.record("otel.status_code", "ERROR");
    tracing::error!(
        target: DIAGNOSTIC_TARGET,
        // NOTE: the OTel semantic names are `exception.type` / `exception.message`,
        // but tracing macros cannot express a dotted field whose segment is a
        // Rust keyword (`type`), so the underscore form is used. Jaeger renders
        // the attributes verbatim; only strict log-backend convention tooling
        // would care about the dot.
        exception_type = kind,
        exception_message = %err,
        "exception"
    );
}

#[cfg(all(test, feature = "otel"))]
#[expect(clippy::expect_used)] // telemetry contract asserts loudly
mod otel_tests {
    //! Pins the `OTel` contract of [`record_exception`] against the in-memory
    //! exporter seam: the ACTIVE span must export with status ERROR and carry
    //! an `exception` event with semantic-convention fields. Runs on a
    //! thread-local subscriber (`with_default`) — the global subscriber slot
    //! is owned by other suites.
    use crate::otel;
    use opentelemetry::trace::Status;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn record_exception_marks_span_error_and_records_event() {
        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                "test.failed.solve",
                path.id = 42u64,
                otel.status_code = tracing::field::Empty,
            );
            let _guard = span.enter();
            crate::telemetry::record_exception("sim_failure", "hop 1 diverged by +13 wei");
        });
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        let span = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "test.failed.solve")
            .expect("failed-op span exported");

        assert_eq!(
            span.status,
            Status::Error {
                description: "".into()
            },
            "span status must be ERROR"
        );

        let exception = span
            .events
            .iter()
            .find(|e| e.name == "exception")
            .expect("exception event recorded on the span");
        let attr_value = |key: &str| {
            exception.attributes.iter().find_map(|kv| {
                if kv.key.as_str() == key {
                    Some(format!("{}", kv.value))
                } else {
                    None
                }
            })
        };
        assert_eq!(
            attr_value("exception_type").as_deref(),
            Some("sim_failure"),
            "exception.type must be the failure kind"
        );
        assert!(
            attr_value("exception_message").is_some_and(|m| m.contains("+13 wei")),
            "exception.message must carry the error detail"
        );
    }
}
