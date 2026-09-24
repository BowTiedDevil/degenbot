//! Console-vs-trace telemetry split (ADR-043 section 4).
//!
//! High-frequency diagnostic events use the [`DIAGNOSTIC_TARGET`] target;
//! [`resolve_filters`] resolves each record layer's `EnvFilter` from the typed
//! telemetry config: the console sinks (stderr fmt + the Python log forwarder)
//! run at `telemetry.log_level` (or the wiring default) plus per-domain
//! `telemetry.diag` escalations, while the `OTel` layer keeps
//! `warn,degenbot=debug` — a Jaeger trace answers "what did the engine do"
//! without the stdout firehose, and stdout stays operator-grade. Warn and
//! above pass every sink.
//!
//! Convention: any event that is per-header/per-event/per-solve noise on the
//! console but signal inside a trace gets `target: DIAGNOSTIC_TARGET`.
//!
//! An explicit `RUST_LOG` is a branch: it wins verbatim on every sink and the
//! config knobs are ignored (one WARN names the active source).

/// Target for high-frequency diagnostic events (see module docs).
pub const DIAGNOSTIC_TARGET: &str = "degenbot::diag";

/// Closed failure-kind taxonomy for `degenbot.errors{kind}` and the
/// `exception_type` attribute. Compile-time closed: callers pass these consts
/// (`&'static str`), never arbitrary strings, so Prometheus series cannot
/// blow up in cardinality. Pool/path detail belongs in the TRACE, not here.
pub mod error_kind {
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
    /// Post-tombstone delivery jitter: a forward log arriving after
    /// its block's D1 tombstone. Dropped un-applied via the benign late-admit
    /// path — counted delivery noise, never a structural fault (the deduped
    /// event exists so the raw rate stays visible; spikes = an out-of-order
    /// WS feed, not a state-machine problem).
    pub const LATE_LOG: &str = "late_log";

    /// Solver clamp skipped: a non-V2/V3/V4 hop has no byte-exact twin at the
    /// clamp seam, so its reported output stands unclamped. One kind per
    /// family so the first live occurrence is visible in the error census.
    pub const CLAMP_SKIP_SOLIDLY_STABLE: &str = "clamp_skip_solidly_stable";
    pub const CLAMP_SKIP_BALANCER_WEIGHTED: &str = "clamp_skip_balancer_weighted";
    pub const CLAMP_SKIP_BALANCER_STABLE: &str = "clamp_skip_balancer_stable";
    pub const CLAMP_SKIP_CURVE_STABLESWAP: &str = "clamp_skip_curve_stableswap";
}

/// Closed REASON taxonomy for kinds that discriminate a sub-cause. Values
/// are the ADR-040 bucket-table reason keys ("kind.reason"). Compile-time
/// closed like [`error_kind`]; the `failure_policy` matrix maps every pair.
pub mod error_reason {

    /// `sim_failure` reason split (ADR-040): the encode/revert distinction.
    pub const SIM_PRE_ENCODE: &str = "pre_encode";
    pub const SIM_REVERT_POOL_STATE: &str = "revert_pool_state";
    pub const SIM_REVERT_ECONOMICS: &str = "revert_economics";
    pub const SIM_RPC: &str = "rpc";
}

/// Third-party transport crates whose routine INFO is throttled to warn on
/// every sink (their WARN/ERROR still pass).
pub const ALLOY_NOISE_TARGETS: &[&str] = &[
    "alloy_pubsub",
    "alloy_transport",
    "alloy_transport_ws",
    "alloy_transport_ipc",
    "alloy_transport_http",
    "alloy_provider",
    "alloy_rpc",
    "alloy_network",
    "alloy_contract",
    "tungstenite",
];

/// Console wiring default for the Python driver (ADR-043 section 6).
pub const CONSOLE_WIRING_DEFAULT_PYTHON: &str = "info";
/// Console wiring default for the standalone Rust bot (ADR-043 section 6).
pub const CONSOLE_WIRING_DEFAULT_RUST: &str = "warn";
/// `OTel` record-layer default (ADR-043 section 4): all of degenbot at debug,
/// everything else at warn.
pub const OTEL_RECORD_DEFAULT: &str = "warn,degenbot=debug";

/// The resolved filter directives for the two record layers (ADR-043 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterPlan {
    /// Console (stderr fmt + Python forwarder) directives.
    pub console: String,
    /// `OTel` record-layer directives.
    pub otel: String,
    /// True when an explicit `RUST_LOG` supplied both; the config knobs are
    /// then ignored (with one WARN naming the active source).
    pub rust_log: bool,
}

/// Resolve the console + `OTel` record filters (ADR-043 §4).
///
/// Precedence is a BRANCH: an explicit `RUST_LOG` is used verbatim on both
/// layers and `telemetry.log_level` / `telemetry.diag` are ignored (one WARN
/// names the active source). Otherwise the console layer is `wiring_default`,
/// overridden by `telemetry.log_level` and escalated per-domain by the
/// validated `telemetry.diag` map; the `OTel` layer keeps its
/// `warn,degenbot=debug` default.
#[must_use]
pub fn resolve_filters(wiring_default: &str) -> FilterPlan {
    use std::fmt::Write as _;
    if let Ok(raw) = std::env::var("RUST_LOG") {
        if !raw.trim().is_empty() {
            warn_config_filters_ignored_once();
            return FilterPlan {
                console: raw.clone(),
                otel: raw,
                rust_log: true,
            };
        }
    }
    let cfg = ::degenbot_config::holder::config();
    let level = cfg
        .telemetry
        .log_level
        .map_or_else(|| wiring_default.to_string(), |l| l.to_string());
    let mut console = level;
    for target in ALLOY_NOISE_TARGETS {
        let _ = write!(console, ",{target}=warn");
    }
    for (domain, level) in &cfg.telemetry.diag {
        let _ = write!(console, ",degenbot::{domain}={level}");
    }
    let mut otel = OTEL_RECORD_DEFAULT.to_string();
    for target in ALLOY_NOISE_TARGETS {
        let _ = write!(otel, ",{target}=warn");
    }
    FilterPlan {
        console,
        otel,
        rust_log: false,
    }
}

/// Emit the "config knobs ignored under `RUST_LOG`" WARN exactly once.
fn warn_config_filters_ignored_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let cfg = ::degenbot_config::holder::config();
        if cfg.telemetry.log_level.is_some() || !cfg.telemetry.diag.is_empty() {
            degenbot_core::op_warn!(
                domain = pump,
                source = "RUST_LOG",
                "RUST_LOG is set; telemetry.log_level and telemetry.diag are ignored (RUST_LOG is the active filter source)"
            );
        }
    });
}

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

/// `Some(span)` when a real subscriber backs `span`, `None` when the ambient
/// dispatch is `tracing`'s no-op [`NoSubscriber`](tracing::subscriber::NoSubscriber).
///
/// `NoSubscriber::new_span` mints the sentinel id `0xDEAD` — neither
/// `Span::make_with` nor the span macro's `always`-interest short-circuit
/// consults the current dispatch's `enabled()` — so a span created while the
/// no-op subscriber is ambient is an UNBACKED handle. Reusing it as an
/// explicit `parent:` after a real subscriber arrives panics inside
/// `Registry::clone_span` ("tried to clone Id(57005), but no span exists with
/// that ID"): the sentinel exists in no real registry. Loop-carried spans (the
/// pump's per-epoch block root and the reorg-window root outlive many blocks)
/// must pass through here at CREATION time — the check is only meaningful
/// against the dispatch that minted the span — so a subscriber installed
/// mid-run degrades the parent to a root span instead of aborting the pump.
#[must_use]
pub(crate) fn subscriber_backed_span(span: &tracing::Span) -> Option<tracing::Span> {
    if tracing::dispatcher::get_default(|dispatch| {
        dispatch.is::<tracing::subscriber::NoSubscriber>()
    }) {
        return None;
    }
    Some(span.clone())
}

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
            "repeat suppressed by cooldown"
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

// ---------------------------------------------------------------------------
// Block-context propagation bridge (session f701ccd3).
//
// The Rust→Python handoff broke the OTel context: the batch leaves the
// engine under the settle-entered block span, but the Python-driven simulate
// future (`degenbot.simulate.dispatch`) created a FRESH root span — two
// disconnected Jaeger trace families correlated only by the `current_block`
// tag. The bridge: capture the exporter-visible span context at batch-send
// time (`publish_block_context`, keyed by the batch's solve block), and
// re-attach it as a REMOTE parent when the Python seam builds its dispatch
// span (`simulate_dispatch_span`). Same trace id + block span id as parent —
// Jaeger renders the full chain (epoch → arb.solve → simulate.dispatch
// → bundle.*) as ONE trace. Remote (not in-process) semantics is correct:
// the block span is usually already closed when the future starts.
// ---------------------------------------------------------------------------

/// Live block-span contexts, keyed by the batch's `solve_block`. Bounded at
/// the last 8 settle points (publish → Python simulate latency is < 1 block;
/// older entries are dead weight). Lock hold is nanoseconds — no I/O, no
/// await; safe for the hot path.
#[cfg(feature = "otel")]
static PUBLISHED_BLOCK_CONTEXTS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<u64, opentelemetry::trace::SpanContext>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[cfg(feature = "otel")]
const PUBLISHED_BLOCK_CONTEXTS_CAPACITY: usize = 8;

/// Capture the CURRENT span's exporter context as the propagation parent for
/// `block`. Call at result batch send time (with the block span entered by
/// the settle gate — see `DeliveryPolicy::diff_and_send`). No-op when the
/// active span has no `OTel` context (layer not installed / disabled span) so
/// callers never gate their logic on telemetry.
///
/// Runs with the block span entered by the settle gate in production.
#[cfg(feature = "otel")]
pub fn publish_block_context(block: u64) {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    let sc = tracing::Span::current()
        .context()
        .span()
        .span_context()
        .clone();
    if !sc.is_valid() {
        return;
    }
    let mut map = PUBLISHED_BLOCK_CONTEXTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if map.len() >= PUBLISHED_BLOCK_CONTEXTS_CAPACITY {
        if let Some(&oldest) = map.keys().min() {
            map.remove(&oldest);
        }
    }
    map.insert(block, sc);
}

#[cfg(not(feature = "otel"))]
pub fn publish_block_context(_block: u64) {}

/// Exporter-visible context for `block`: exact hit first, then the nearest
/// previous block — consumers' notion of "current block" can be one ahead
/// (a batch is dispatched / a block judged after the next header arrives),
/// and parenting to the closest earlier block span is the correct lineage
/// either way. Bounded-ring insert order makes the scan trivially cheap.
#[cfg(feature = "otel")]
fn published_parent_for(current_block: u64) -> Option<opentelemetry::trace::SpanContext> {
    let map = PUBLISHED_BLOCK_CONTEXTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.get(&current_block).cloned().or_else(|| {
        map.keys()
            .filter(|&&b| b <= current_block)
            .max()
            .and_then(|b| map.get(b).cloned())
    })
}

/// Re-attach the published block's span context as a remote parent on an
/// EXISTING span. For spans created on detached tasks (the ADR-021 verifier
/// loop) where the constructor form of [`simulate_dispatch_span`] does not
/// apply: the task has no ambient context, so the parent is pinned explicitly
/// after span creation. Best-effort / never blocks the caller's logic.
#[cfg(feature = "otel")]
pub fn attach_published_parent(span: &tracing::Span, current_block: u64) {
    if let Some(sc) = published_parent_for(current_block) {
        use opentelemetry::trace::TraceContextExt as _;
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        let cx = opentelemetry::Context::new().with_remote_span_context(sc);
        // Best-effort: a rejected parent propagation degrades to a root
        // span, never blocks the caller.
        let _ = span.set_parent(cx);
    }
}

#[cfg(not(feature = "otel"))]
pub fn attach_published_parent(_span: &tracing::Span, _current_block: u64) {}

/// Build the `degenbot.simulate.dispatch` span for the Python simulate fan-
/// out, re-attaching the published block's span context as a remote parent
/// when one is registered for `current_block` (f701ccd3 bridge). Attribute
/// parity with the historical inline span is exact (`current_block`,
/// `phase_candidate_count`).
#[cfg(feature = "otel")]
#[must_use]
pub fn simulate_dispatch_span(current_block: u64, candidate_count: usize) -> tracing::Span {
    let span = tracing::info_span!(
        "degenbot.simulate.dispatch",
        current_block,
        phase_candidate_count = candidate_count
    );
    attach_published_parent(&span, current_block);
    span
}

#[cfg(not(feature = "otel"))]
#[must_use]
pub fn simulate_dispatch_span(current_block: u64, candidate_count: usize) -> tracing::Span {
    tracing::info_span!(
        "degenbot.simulate.dispatch",
        current_block,
        phase_candidate_count = candidate_count
    )
}

/// Exact-match-only re-attach for SOLVE spans: a solve span must
/// parent to its OWN block's published span, or nowhere else. The nearest-
/// previous fallback of [`attach_published_parent`] is correct when the
/// consumer's notion of "current block" may run one AHEAD of the published
/// contexts (the Python simulate seam, the verifier); it is WRONG for the
/// solver arms — a fresh ad-hoc in-block solve (ambient = block N's loop
/// context) must not be re-parented onto block N-1's closed span just
/// because its publish has not landed yet. Exact hit = the late
/// finalize/drain crossing a block boundary (19/20 of the recent Jaeger
/// traces carried block N-1's arb.solve inside block N's trace); exact miss
/// = keep the ambient parent, never orphan, never mis-date.
#[cfg(feature = "otel")]
pub fn attach_published_parent_exact(span: &tracing::Span, block: u64) {
    if let Some(sc) = published_parent_exact(block) {
        use opentelemetry::trace::TraceContextExt as _;
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        let cx = opentelemetry::Context::new().with_remote_span_context(sc);
        let _ = span.set_parent(cx);
    }
}

#[cfg(not(feature = "otel"))]
pub fn attach_published_parent_exact(_span: &tracing::Span, _block: u64) {}

/// Exporter-visible context for EXACTLY `block` — no fallback.
#[cfg(feature = "otel")]
fn published_parent_exact(block: u64) -> Option<opentelemetry::trace::SpanContext> {
    let map = PUBLISHED_BLOCK_CONTEXTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.get(&block).cloned()
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

/// A long-lived process hits the same callsite before and after any subscriber
/// exists; the guard is keyed on the dispatch, not on the span's id.
#[cfg(test)]
fn unbacked_span() -> tracing::Span {
    tracing::info_span!("test.unbacked")
}

#[cfg(test)]
#[expect(clippy::expect_used)] // the pins assert loudly
mod span_backing_tests {
    //! Pins the sentinel-id hazard behind [`super::subscriber_backed_span`].

    #[test]
    fn no_subscriber_spans_are_unbacked_and_real_registry_spans_survive() {
        // With the no-op subscriber ambient, a span is backed by no real
        // registry: `NoSubscriber::new_span` returns the 0xDEAD sentinel (it is
        // reached without an `enabled()` check — see `Span::make_with`), and a
        // later `Registry::clone_span` of that id panics. The guard is the
        // dispatch, so it drops the handle whatever the span's id looks like.
        tracing::dispatcher::with_default(&tracing::Dispatch::none(), || {
            assert!(
                super::subscriber_backed_span(&super::unbacked_span()).is_none(),
                "a NoSubscriber handle must never be kept as a parent"
            );
        });

        // A real registry-backed handle survives, and parenting to it in its
        // own registry is safe — the exact call shape that panicked while a
        // sentinel handle survived to the publish stage.
        let subscriber = tracing_subscriber::registry();
        tracing::subscriber::with_default(subscriber, || {
            let backed = tracing::info_span!("test.backed");
            let kept = super::subscriber_backed_span(&backed).expect("registry-backed span");
            drop(tracing::info_span!(parent: kept, "test.child"));
        });
    }
}
