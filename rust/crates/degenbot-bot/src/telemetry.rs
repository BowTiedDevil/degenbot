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

/// Closed failure-kind taxonomy for `degenbot.errors{kind}` and the
/// `exception_type` attribute. Compile-time closed: callers pass these consts
/// (`&'static str`), never arbitrary strings, so Prometheus series cannot
/// blow up in cardinality. Pool/path detail belongs in the TRACE, not here.
pub mod error_kind {
    /// ADR-021 solver-state tripwire (engine view diverged from chain).
    pub const SOLVER_STATE_DESYNC: &str = "solver_state_desync";
    /// Pump completeness tripwire (WS log drop / dead stream).
    pub const WS_COMPLETENESS: &str = "ws_completeness";
    /// Simulated-arb failure classified at the dispatch seam.
    pub const SIM_FAILURE: &str = "sim_failure";
    /// Broadcast / node rejection on submission.
    pub const SUBMIT_FAILURE: &str = "submit_failure";
    /// Post-submit monitor verdict failure.
    pub const MONITOR_FAILURE: &str = "monitor_failure";
    /// Liquidity verification mismatch (registration verify-lifecycle).
    pub const VERIFY_MISMATCH: &str = "verify_mismatch";
    /// Drain watchdog fired: backlog with no completion inside the window.
    pub const DRAIN_STALL: &str = "drain_stall";
    /// Drain channel closed: the background drainer task is dead.
    pub const DRAIN_DEAD: &str = "drain_dead";
}

/// Closed REASON taxonomy for kinds that discriminate a sub-cause. Values
/// are the ADR-040 bucket-table reason keys ("kind.reason"). Compile-time
/// closed like [`error_kind`]; the `failure_policy` matrix maps every pair.
pub mod error_reason {
    /// Solver-state tripwire classes (ADR-021 D2) — the reason sub-keys of
    /// `solver_state_desync`.
    pub const MISSED_LOG: &str = "missed_log";
    pub const UNHANDLED_REORG: &str = "unhandled_reorg";
    pub const STORAGE_MUTATED: &str = "storage_mutated";
    pub const DELIVERY_LAG: &str = "delivery_lag";
    pub const UNCLASSIFIED: &str = "unclassified";

    /// `sim_failure` reason split (ADR-040): the encode/revert distinction.
    pub const SIM_PRE_ENCODE: &str = "pre_encode";
    pub const SIM_REVERT_POOL_STATE: &str = "revert_pool_state";
    pub const SIM_REVERT_ECONOMICS: &str = "revert_economics";
    pub const SIM_RPC: &str = "rpc";
}

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
/// 3. The `degenbot.errors{kind}` counter — detection is
///    push-based via Prometheus; traces are for investigation.
///
/// # Convention
///
/// Every failure seam calls this EXACTLY ONCE per distinct failure, BEFORE
/// any exit/continue decision. Callers own dedup (see the failure-policy
/// task): an error storm must not become a counter/event storm. Errors use
/// [`DIAGNOSTIC_TARGET`]'s uncapped sibling treatment — they are emitted at
/// ERROR level, which passes EVERY sink including the console cap.
#[cfg(feature = "otel")]
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
    if let Some(p) = crate::instruments::pipeline() {
        p.count_error(kind);
    }
}

/// No-`otel`-feature twin: the console error line still fires (failures are
/// ALWAYS visible on stdout), only the span status/exception/counter are
/// compiled out. Keeps every failure seam ungated at the call site.
#[cfg(not(feature = "otel"))]
pub fn record_exception(kind: &'static str, err: impl std::fmt::Display) {
    tracing::error!(
        target: DIAGNOSTIC_TARGET,
        exception_type = kind,
        exception_message = %err,
        "exception"
    );
}

/// Detach a span from the ambient `OTel` context so it becomes its own trace
/// ROOT (JYCTXI / MQUKB6): a span created while another is still current —
/// e.g. the pump's per-block beat when the previous block's loop-context
/// span is still entered under a backfill `.instrument()` future — would
/// otherwise chain every block of a session into one ever-growing
/// mega-trace. Children still nest under the span afterwards (callers keep
/// it as their loop context); only the parent linkage at creation changes.
#[cfg(feature = "otel")]
pub(crate) fn make_trace_root(span: &tracing::Span) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    // Detaching cannot fail; the Result is informational.
    drop(span.set_parent(opentelemetry::context::Context::new()));
}

/// No-`otel`-feature twin: nothing to detach (compiles zero `OTel` code).
#[cfg(not(feature = "otel"))]
pub(crate) fn make_trace_root(_span: &tracing::Span) {}

/// No-op without the `otel` feature (nothing to flush).
#[cfg(not(feature = "otel"))]
pub fn flush_before_exit() {}

/// Mode-aware, storm-deduped variant of [`record_exception`].
///
/// `primary_id` is the stable per-bug identity (pool address, `path_id`, bucket
/// string): inside [`COOLDOWN_BLOCKS`](crate::failure_policy::COOLDOWN_BLOCKS)
/// of the same fingerprint neither the exception event nor the counter fires
/// again — trace spans still carry every occurrence. Returns `true` when the
/// failure WAS surfaced (first sighting / window elapsed), `false` when
/// suppressed; abort-seam callers combine this with
/// [`crate::failure_policy::failure_mode`] to decide what happens next.
#[must_use]
pub fn record_exception_keyed(
    kind: &'static str,
    primary_id: &str,
    block: u64,
    err: impl std::fmt::Display,
) -> bool {
    use crate::failure_policy::cooldowns;

    let admitted = cooldowns().admit(kind, primary_id, block);
    if admitted {
        record_exception(kind, err);
    } else {
        // Suppressed for alerting surfaces, but still visible at DEBUG so a
        // developer who opts into the firehose sees the repeats.
        tracing::debug!(
            target: DIAGNOSTIC_TARGET,
            exception_type = kind,
            fingerprint = %primary_id,
            block_number = block,
            "[error] repeat suppressed by cooldown"
        );
    }
    admitted
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

    /// JYCTXI: `make_trace_root` detaches from the ambient context so the span
    /// exports as its own trace ROOT (zero sentinel parent), even though it was
    /// created while another span is entered (the default parent lookup would
    /// otherwise chain).
    #[test]
    fn make_trace_root_detaches_to_own_trace() {
        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
        tracing::subscriber::with_default(subscriber, || {
            // Ambient parent: an entered span, so default parent lookup chains.
            let parent = tracing::info_span!("test.ambient.parent").entered();
            let root = tracing::info_span!("test.trace.root");
            crate::telemetry::make_trace_root(&root);
            // End + close while the parent is still current: exports the span,
            // exercising the detach-at-creation contract.
            drop(root);
            drop(parent);
        });
        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let root = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "test.trace.root")
            .expect("root span exported");
        assert_eq!(
            root.parent_span_id,
            opentelemetry::trace::SpanId::INVALID,
            "make_trace_root must detach: span must be its own trace root"
        );
    }
}

/// Best-effort flush of the `OTel` span exporter.
///
/// `std::process::abort()` skips destructors, so the batched span processor
/// would drop up to its whole export window — precisely at the failure sites
/// where the evidence matters most. Failure seams call this BEFORE any abort
/// decision. Best-effort by design: a flush failure is logged, never fatal
/// (the abort that follows is the loud part).
#[cfg(feature = "otel")]
pub fn flush_before_exit() {
    #[cfg(feature = "otel")]
    if let Some(handle) = crate::otel::global_handle() {
        if let Err(e) = handle.flush() {
            tracing::warn!(error = %e, "otel flush before exit failed (continuing)");
        }
    }
}
