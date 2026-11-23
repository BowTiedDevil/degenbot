//! Failure-event telemetry for the engine (ADR-043).
//!
//! High-frequency diagnostic events use [`DIAGNOSTIC_TARGET`]; the
//! console-vs-trace filter split those events ride lives in
//! [`degenbot_core::telemetry::resolve_filters`], which every sink
//! (the Python driver, the standalone console, the `OTel` layer) installs.

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
    crate::instruments::record(crate::instruments::CensusEvent::EngineError { kind });
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
/// `ROOT::` a span created while another is still current —
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

/// The loop-carried parent handle for an already-created pump span.
///
/// The pump keeps one span per epoch (and one per reorg window) and parents
/// every later child in that window to it, so the span outlives the subscriber
/// state it was created under. A span created while no subscriber was
/// installed is backed by no registry — `NoSubscriber::new_span` mints the
/// `0xDEAD` sentinel id, reached without an `enabled()` check once the callsite
/// interest is cached `always` — and handing that id to a registry that
/// arrives later aborts the pump inside `Registry::clone_span` ("tried to
/// clone Id(57005), but no span exists with that ID").
///
/// The validation is the span's OWN exporter context, never the ambient
/// dispatch: a valid `OTel` `SpanContext` means the exporter layer built the
/// span, so the registry that minted it resolves the parent id. Anything else
/// — created before any subscriber, no `OTel` layer, a disabled span — carries
/// no proof of backing, and the caller gets `None`: an intentional trace root
/// instead of a blind clone of an id no registry owns. The span itself is
/// still kept by the pump for recording and entry; both dispatch to the
/// subscriber that minted it and are inert on an unbacked handle.
///
/// Residual: a valid context proves membership in the registry that minted the
/// span, not in a REPLACEMENT registry. The pump's contract is one subscriber
/// installed once into the process-wide global slot; running the pump across a
/// subscriber swap is outside it.
#[cfg(feature = "otel")]
#[must_use]
pub(crate) fn loop_parent(span: &tracing::Span) -> Option<tracing::Span> {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    if !span.context().span().span_context().is_valid() {
        return None;
    }
    Some(span.clone())
}

/// No-`otel`-feature twin: there is no exporter context to validate ancestry
/// against, and a `tracing` id alone does not prove the ambient registry minted
/// the span. Nothing is carried into a `parent:` slot, so children of a
/// loop-carried span are intentional roots; the pump still keeps the span
/// itself for recording and entry.
#[cfg(not(feature = "otel"))]
#[must_use]
pub(crate) fn loop_parent(_span: &tracing::Span) -> Option<tracing::Span> {
    None
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

    /// `make_trace_root` detaches from the ambient context so the span
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
// Block lineage: the solve block and its exporter span context, carried as a
// value through every dispatch/request seam.
//
// The Rust→Python handoff broke the OTel context: the batch left the engine
// under the settle-entered block span, but the downstream work (the simulate
// fan-out, the verifier, the submit batch) has no ambient context and
// exported as FRESH root spans — disconnected Jaeger trace families
// correlated only by the block tag. [`BlockLineage`] is that context carried
// EXPLICITLY: the publish captures it ONCE, stamps it on the batch, and each
// consumer attaches exactly the lineage it was handed. Ancestry travels only
// as a value, so an interleaved N/N+1 pair (N publishing while N+1 is
// consumed) cannot merge families: a lineage the producer never captured is
// `unattached`, and an unattached lineage yields an intentionally unattached
// span.
// ---------------------------------------------------------------------------

/// The solve block plus the exporter span context of the block span that
/// work was published under — the exact ancestry every downstream span of
/// that block's work parents to.
///
/// A missing context is a real answer, never an error: no `otel` layer, a
/// disabled span, or a producer with nothing to hand over (a scripted batch,
/// a historical backfill solve). Consumers then build an intentionally
/// unattached span rather than guessing an ancestor.
#[derive(Clone, Debug)]
pub struct BlockLineage {
    block: u64,
    anchor: Option<degenbot_core::canonical_head::ChainAnchor>,
    #[cfg(feature = "otel")]
    context: Option<opentelemetry::trace::SpanContext>,
}

impl BlockLineage {
    /// The lineage of the CURRENT span at `block` — the producer's capture
    /// form (the settle gate enters the block span before delivery runs).
    ///
    /// A span with no valid exporter context answers
    /// [`Self::unattached`], so callers never gate their logic on telemetry.
    #[must_use]
    pub fn capture(block: u64) -> Self {
        Self::from_span(block, &tracing::Span::current())
    }

    /// The lineage of `span` at `block`, captured explicitly from the handle
    /// the driver already holds (the pump's per-epoch span).
    #[cfg(feature = "otel")]
    #[must_use]
    pub fn from_span(block: u64, span: &tracing::Span) -> Self {
        use opentelemetry::trace::TraceContextExt as _;
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        let sc = span.context().span().span_context().clone();
        Self {
            block,
            anchor: None,
            context: sc.is_valid().then_some(sc),
        }
    }

    /// No-`otel`-feature twin: there is no exporter context to capture, so
    /// every lineage is [`Self::unattached`].
    #[cfg(not(feature = "otel"))]
    #[must_use]
    pub fn from_span(block: u64, _span: &tracing::Span) -> Self {
        Self::unattached(block)
    }

    /// A `block` lineage with no exporter context: spans built from it are
    /// deliberate trace roots.
    #[must_use]
    pub const fn unattached(block: u64) -> Self {
        #[cfg(feature = "otel")]
        let lineage = Self {
            block,
            anchor: None,
            context: None,
        };
        #[cfg(not(feature = "otel"))]
        let lineage = Self {
            block,
            anchor: None,
        };
        lineage
    }

    /// The same lineage, now naming the exact solve anchor it belongs to.
    ///
    /// The anchor is the ONE identity the batch, the executor work, and every
    /// downstream span share (ADR-105 D10); [`Self::block`] becomes the
    /// anchor's height, so a lineage can never name one height and another
    /// block. Constructors stay block-only so an unanchored producer (a
    /// scripted batch, a historical backfill with no header) is
    /// representable — and answers `None` from [`Self::anchor`].
    #[must_use]
    pub fn with_anchor(mut self, anchor: degenbot_core::canonical_head::ChainAnchor) -> Self {
        self.anchor = Some(anchor);
        self
    }

    /// The exact solve anchor this lineage names, when the producer knew it.
    #[must_use]
    pub const fn anchor(&self) -> Option<degenbot_core::canonical_head::ChainAnchor> {
        self.anchor
    }

    /// The block this lineage belongs to — the SOLVE block, never the
    /// consumer's clock. When the lineage names an anchor, this IS the anchor's
    /// height.
    #[must_use]
    pub const fn block(&self) -> u64 {
        match self.anchor {
            Some(anchor) => anchor.number(),
            None => self.block,
        }
    }

    /// The exporter context to parent to, when one was captured.
    #[cfg(feature = "otel")]
    fn context(&self) -> Option<&opentelemetry::trace::SpanContext> {
        self.context.as_ref()
    }
}

/// A bare block number converts to an [`BlockLineage::unattached`] lineage:
/// the caller had a block and no exporter context, and the downstream span is
/// a deliberate root rather than a guess at an ancestor.
impl From<u64> for BlockLineage {
    fn from(block: u64) -> Self {
        Self::unattached(block)
    }
}

/// A borrowed lineage converts by clone, so a seam that takes
/// `impl Into<BlockLineage>` accepts the caller's own value by reference.
impl From<&BlockLineage> for BlockLineage {
    fn from(lineage: &BlockLineage) -> Self {
        lineage.clone()
    }
}

/// Re-attach `lineage`'s block span as a REMOTE parent on an EXISTING span —
/// the detached-task form (the ADR-021 verifier loop, the submit batch),
/// whose constructor has no ambient context to inherit. An
/// [`BlockLineage::unattached`] lineage attaches nothing, so the span stays an
/// intentional root. Best-effort / never blocks the caller's logic.
#[cfg(feature = "otel")]
pub fn attach_lineage(span: &tracing::Span, lineage: &BlockLineage) {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    let Some(sc) = lineage.context() else {
        return;
    };
    let cx = opentelemetry::Context::new().with_remote_span_context(sc.clone());
    // Best-effort: a rejected parent propagation degrades to a root
    // span, never blocks the caller.
    let _ = span.set_parent(cx);
}

#[cfg(not(feature = "otel"))]
pub fn attach_lineage(_span: &tracing::Span, _lineage: &BlockLineage) {}

/// Build the `degenbot.simulate.dispatch` span for one deployment lane's
/// simulate fan-out, parented to `lineage`'s block span — the SOLVE block the
/// candidates were produced for, never the consumer's clock (which can
/// already be one block ahead). Attribute parity with the historical inline
/// span is exact (`current_block`, `phase_candidate_count`).
#[must_use]
pub fn simulate_dispatch_span(
    lineage: &BlockLineage,
    current_block: u64,
    candidate_count: usize,
) -> tracing::Span {
    let span = tracing::info_span!(
        "degenbot.simulate.dispatch",
        current_block,
        phase_candidate_count = candidate_count,
        chain_id = tracing::field::Empty,
        block_hash = tracing::field::Empty,
        parent_block_hash = tracing::field::Empty,
    );
    record_anchor_fields(&span, lineage.anchor());
    attach_lineage(&span, lineage);
    span
}

/// Record the exact-anchor span fields on `span`, when the lineage names one.
///
/// Hashes travel as span ATTRIBUTES only (ADR-105 D10): never a Prometheus
/// label, never a metric attribute key. The values are the anchor's own, so an
/// interleaved N/N+1 pair carries two distinct block identities on two distinct
/// traces.
pub fn record_anchor_fields(
    span: &tracing::Span,
    anchor: Option<degenbot_core::canonical_head::ChainAnchor>,
) {
    let Some(anchor) = anchor else {
        return;
    };
    span.record("chain_id", tracing::field::display(anchor.chain_id()));
    span.record("block_hash", tracing::field::display(anchor.hash()));
    span.record(
        "parent_block_hash",
        tracing::field::display(anchor.parent_hash()),
    );
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

/// A span no registry knows — the handle a long-lived process holds when it
/// creates spans before any subscriber is installed. `new_span` mints the
/// no-op subscriber's `0xDEAD` sentinel id, reached without an `enabled()`
/// check once the callsite interest is cached `always`; the dispatching
/// subscriber here is a real one so the mint does not hinge on an
/// interest-cache state a test cannot force.
#[cfg(test)]
fn unbacked_span() -> tracing::Span {
    struct SentinelSubscriber;

    impl tracing::Subscriber for SentinelSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(0xDEAD)
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, _event: &tracing::Event<'_>) {}

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    tracing::subscriber::with_default(SentinelSubscriber, || tracing::info_span!("test.unbacked"))
}

#[cfg(test)]
mod span_backing_tests {
    //! Pins the loop-parent contract on [`super::loop_parent`]: ancestry rides
    //! only when the span's OWN exporter context validates it, never the
    //! ambient dispatch's identity.

    #[test]
    fn unbacked_handle_is_never_carried_once_a_subscriber_arrives() {
        // Minted while no subscriber was installed. Reusing the handle as a
        // `parent:` after this point aborts inside `Registry::clone_span` —
        // the sentinel exists in no registry — so the seam must refuse it.
        let unbacked = super::unbacked_span();
        let subscriber = tracing_subscriber::registry();
        tracing::subscriber::with_default(subscriber, || {
            assert!(
                super::loop_parent(&unbacked).is_none(),
                "a handle no registry backs must never be carried as a parent"
            );
            // The caller's fallback is an intentional trace root: no clone of
            // an id the ambient registry does not know.
            drop(tracing::info_span!(
                parent: tracing::Span::none(),
                "test.root_child"
            ));

            // The registry DOES know this one, but a tracing id alone is not a
            // validation the seam can carry across subscriber generations.
            let backed = tracing::info_span!("test.registry_backed");
            assert!(
                super::loop_parent(&backed).is_none(),
                "without an exporter context there is nothing to validate"
            );
        });
    }

    #[test]
    #[should_panic(expected = "no span exists with that ID")]
    fn the_unbacked_handle_would_panic_if_used_as_a_raw_parent() {
        // Negative probe: proves the rejection above is load-bearing (the
        // minted handle really is in no registry) and names the pump-abort
        // this seam exists to prevent.
        let unbacked = super::unbacked_span();
        let subscriber = tracing_subscriber::registry();
        tracing::subscriber::with_default(subscriber, || {
            drop(tracing::info_span!(parent: unbacked, "test.panicking_child"));
        });
    }

    #[cfg(feature = "otel")]
    #[test]
    #[expect(clippy::expect_used)] // the ancestry pins assert loudly
    fn valid_exporter_context_is_kept_and_parents_a_deliberate_child() {
        use crate::otel;
        use opentelemetry_sdk::trace::InMemorySpanExporter;
        use tracing_subscriber::layer::SubscriberExt;

        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));

        tracing::subscriber::with_default(subscriber, || {
            let epoch = tracing::info_span!("test.epoch.run");
            let kept = super::loop_parent(&epoch).expect("exporter-backed span is carried");
            drop(tracing::info_span!(parent: &kept, "test.stage.first_log"));
            drop(epoch);
        });
        provider.force_flush().expect("flush");

        let spans = exporter.get_finished_spans().expect("spans");
        let epoch = spans
            .iter()
            .find(|span| span.name.as_ref() == "test.epoch.run")
            .expect("epoch span exported");
        let stage = spans
            .iter()
            .find(|span| span.name.as_ref() == "test.stage.first_log")
            .expect("stage span exported");
        assert_eq!(
            stage.parent_span_id,
            epoch.span_context.span_id(),
            "validated ancestry: the stage span must hang off the epoch span"
        );
        assert_eq!(
            stage.span_context.trace_id(),
            epoch.span_context.trace_id(),
            "parent and child must share one trace"
        );
    }

    #[cfg(not(feature = "otel"))]
    #[test]
    fn no_carried_parent_without_an_exporter_to_validate_ancestry() {
        // No `otel` feature: there is no exporter context to validate a parent
        // against, so the loop carries none and its children are intentional
        // roots. The registry-backed case pins that this is not a dispatch
        // test: the span IS real here, and still nothing is carried.
        let subscriber = tracing_subscriber::registry();
        let kept = tracing::subscriber::with_default(subscriber, || {
            super::loop_parent(&tracing::info_span!("test.registry_backed"))
        });
        assert!(kept.is_none(), "no exporter context, no carried parent");

        assert!(
            super::loop_parent(&super::unbacked_span()).is_none(),
            "an unbacked handle must never be carried either"
        );
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the lineage tests build fixture anchors and assert on a capture map they own"
)]
mod lineage_anchor_tests {
    use super::*;
    use degenbot_core::canonical_head::ChainAnchor;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    fn anchor(number: u64, tag: u8) -> ChainAnchor {
        ChainAnchor::new(
            1,
            number,
            alloy::primitives::B256::with_last_byte(tag),
            alloy::primitives::B256::with_last_byte(tag.wrapping_sub(1)),
        )
        .unwrap()
    }

    /// Field values per span id — the default-lane observable for span
    /// ATTRIBUTES (no exporter needed).
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<HashMap<u64, HashMap<String, String>>>>);

    impl Capture {
        fn of(&self, span: &tracing::Span) -> HashMap<String, String> {
            let id = span
                .id()
                .expect("the capture subscriber mints an id")
                .into_u64();
            self.0.lock().unwrap().get(&id).cloned().unwrap_or_default()
        }
    }

    struct Sink<'a>(&'a mut HashMap<String, String>);

    impl Visit for Sink<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .insert(field.name().to_string(), format!("{value:?}"));
        }
        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
    }

    struct CaptureSubscriber(Capture);

    impl Subscriber for CaptureSubscriber {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, attributes: &Attributes<'_>) -> Id {
            let mut map = self.0 .0.lock().unwrap();
            let id = Id::from_u64(map.len() as u64 + 1);
            let fields = map.entry(id.clone().into_u64()).or_default();
            attributes.record(&mut Sink(fields));
            id
        }
        fn record(&self, id: &Id, values: &Record<'_>) {
            let mut map = self.0 .0.lock().unwrap();
            let fields = map.entry(id.clone().into_u64()).or_default();
            values.record(&mut Sink(fields));
        }
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, _: &Event<'_>) {}
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    #[test]
    fn a_lineage_names_its_anchor_height_as_its_block() {
        let head = anchor(101, 0xaa);
        let anchored = BlockLineage::unattached(999).with_anchor(head);
        assert_eq!(anchored.block(), 101, "the anchor owns the height");
        assert_eq!(anchored.anchor(), Some(head));
        let bare = BlockLineage::unattached(7);
        assert_eq!(bare.anchor(), None, "an unanchored producer says so");
        assert_eq!(bare.block(), 7, "and keeps the height it was handed");
    }

    #[test]
    fn interleaved_solve_spans_carry_their_own_exact_block_identity() {
        let capture = Capture::default();
        let subscriber = CaptureSubscriber(capture.clone());
        let n = BlockLineage::unattached(100).with_anchor(anchor(100, 0x10));
        let n_plus_one = BlockLineage::unattached(101).with_anchor(anchor(101, 0x20));
        tracing::subscriber::with_default(subscriber, || {
            let first = simulate_dispatch_span(&n, 101, 3);
            let second = simulate_dispatch_span(&n_plus_one, 101, 4);
            let first_fields = capture.of(&first);
            let second_fields = capture.of(&second);
            assert_eq!(
                first_fields.get("block_hash").map(String::as_str),
                Some(format!("{:?}", alloy::primitives::B256::with_last_byte(0x10)).as_str()),
                "the N lane span names N's block, not the consumer clock's"
            );
            assert_eq!(
                second_fields.get("block_hash").map(String::as_str),
                Some(format!("{:?}", alloy::primitives::B256::with_last_byte(0x20)).as_str())
            );
            assert_ne!(
                first_fields.get("block_hash"),
                second_fields.get("block_hash"),
                "an interleaved pair never shares one block identity"
            );
            assert_eq!(
                first_fields.get("current_block").map(String::as_str),
                Some("101"),
                "the policy clock stays the consumer's block"
            );
            assert_eq!(first_fields.get("chain_id").map(String::as_str), Some("1"));
        });
    }

    #[test]
    fn an_unanchored_lineage_records_no_block_hash() {
        let capture = Capture::default();
        let subscriber = CaptureSubscriber(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            let span = simulate_dispatch_span(&BlockLineage::unattached(42), 43, 1);
            let fields = capture.of(&span);
            assert!(
                !fields.contains_key("block_hash"),
                "no anchor, no hash: an unattached lineage is a deliberate root with no identity claim"
            );
        });
    }
}
