//! The observability facade (ADR-043).
//!
//! The standard puts every diagnostic on a **closed** `degenbot::<domain>`
//! target, keeps engine diagnostics inside an engine span, and provides a
//! single entry point so call sites cannot hand-write a target, derive a
//! `[tag]` prefix, or emit a bare `tracing::info!` that bypasses the level
//! rubric.
//!
//! This module owns:
//!
//! - [`domain`] — the closed target set (single source of truth for the
//!   Phase-4 `telemetry.diag` control surface and the enforcement gate).
//! - the domain-target and event macros: `diag!`, `op_info!`, `op_warn!`,
//!   `op_error!`, `op_span!`.
//! - the engine-span guard ([`guard_diag`]) that fires when a diagnostic is
//!   emitted with no active engine span (ADR-043 §3).
//! - the console-vs-trace filter split ([`resolve_filters`]): the ONE
//!   resolution of both record layers every sink installs (the Python driver,
//!   the standalone console, the `OTel` layer), so no sink can invent its own
//!   precedence.
//!
//! The core crates emit through these macros; the pure-Rust binary and the
//! `PyO3` binding install sinks (the facade never installs a subscriber).

/// The closed target domain set (ADR-043 §3).
///
/// A diagnostic is a `debug` event under its own domain; the `OTel` spans layer
/// raises `degenbot=debug` by default so every domain survives in traces.
pub mod domain {
    /// Pool/token state mutations and applies.
    pub const STATE: &str = "degenbot::state";
    /// Path registration and lifecycle.
    pub const PATH: &str = "degenbot::path";
    /// Solve dispatch and path walking.
    pub const SOLVER: &str = "degenbot::solver";
    /// Simulation and revert diagnostics.
    pub const SIM: &str = "degenbot::sim";
    /// Block pump, drain, dispatch, and quiesce.
    pub const PUMP: &str = "degenbot::pump";
    /// Execution seam and submit decisions.
    pub const EXEC: &str = "degenbot::exec";
    /// Liquidity verification and divergence probes.
    pub const VERIFY: &str = "degenbot::verify";
    /// WS/HTTP log and header ingestion.
    pub const INGEST: &str = "degenbot::ingest";
    /// RPC provider and transport.
    pub const RPC: &str = "degenbot::rpc";
    /// Aave updater and event processing.
    pub const AAVE: &str = "degenbot::aave";

    /// Every domain, for the config validator and the enforcement gate.
    pub const ALL: [&str; 10] = [
        STATE, PATH, SOLVER, SIM, PUMP, EXEC, VERIFY, INGEST, RPC, AAVE,
    ];

    /// `true` iff `target` is one of the closed domain targets.
    #[must_use]
    pub fn is_valid(target: &str) -> bool {
        ALL.contains(&target)
    }
}

/// Every engine span name starts with this prefix (ADR-043 §3).
pub const ENGINE_SPAN_PREFIX: &str = "degenbot.";

/// `true` when the current (thread-local) span is an engine span.
///
/// Engine diagnostics must be emitted inside an engine span so they export to
/// Jaeger as span events; the `OTel` layer is spans-only, so an event outside a
/// span is silently dropped by the backend.
#[must_use]
pub fn in_engine_span() -> bool {
    tracing::Span::current()
        .metadata()
        .is_some_and(|meta| meta.name().starts_with(ENGINE_SPAN_PREFIX))
}

/// Once-only latch for the release-mode engine-span guard warning.
static DIAG_GUARD_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Guard a diagnostic event: assert (debug) / warn once (release) if no engine
/// span is active. Called by the `diag!` facade macro.
pub fn guard_diag(context: &str) {
    if in_engine_span() {
        return;
    }
    // The abort-on-regression form is opt-in (`strict-engine-spans`): a
    // diagnostic regression must not take down a live worker thread. The
    // one-shot WARN below is the always-on signal (ADR-043 §3).
    #[cfg(feature = "strict-engine-spans")]
    debug_assert!(
        false,
        "diagnostic emitted outside an engine span from {context} (ADR-043 §3)"
    );
    if !DIAG_GUARD_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(
            target: DIAGNOSTIC_TARGET,
            context,
            "diagnostic emitted outside an engine span (this warning fires once; ADR-043 §3)"
        );
    }
}

/// The diagnostic target used by failure events (matches `degenbot-bot`'s
/// `DIAGNOSTIC_TARGET`); panic failures ride it.
pub const DIAGNOSTIC_TARGET: &str = "degenbot::diag";

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
            crate::op_warn!(
                domain = pump,
                source = "RUST_LOG",
                "RUST_LOG is set; telemetry.log_level and telemetry.diag are ignored (RUST_LOG is the active filter source)"
            );
        }
    });
}

/// Install the process panic hook (ADR-043 §2).
///
/// Fires exactly one ERROR carrying the panic payload and the thread/task name,
/// with the active span marked ERROR so the `OTel` layer exports it as a span
/// exception. Chains the previously installed hook, so the default backtrace
/// still prints. Both consumers (the pure-Rust binary and the `PyO3` binding)
/// call this once at startup; the last installer wins, which is fine because
/// the behavior is identical.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("<unnamed>").to_owned();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| info.to_string());
        tracing::Span::current().record("otel.status_code", "ERROR");
        tracing::error!(
            target: DIAGNOSTIC_TARGET,
            exception_type = "panic",
            thread,
            payload,
            "panic captured"
        );
        previous(info);
    }));
}

/// Map a domain token to its closed target, or fail the build.
///
/// This is the compile-time half of the closed-set guarantee: a misspelled
/// domain is a `compile_error!`, not a runtime no-op.
///
/// ```
/// assert_eq!(degenbot_core::telemetry_target!(sim), "degenbot::sim");
/// assert_eq!(degenbot_core::telemetry_target!(aave), "degenbot::aave");
/// ```
///
/// ```compile_fail
/// // A misspelled domain fails the build instead of silently no-op'ing.
/// let _ = degenbot_core::telemetry_target!(sim2);
/// ```
#[macro_export]
macro_rules! telemetry_target {
    (state) => {
        $crate::telemetry::domain::STATE
    };
    (path) => {
        $crate::telemetry::domain::PATH
    };
    (solver) => {
        $crate::telemetry::domain::SOLVER
    };
    (sim) => {
        $crate::telemetry::domain::SIM
    };
    (pump) => {
        $crate::telemetry::domain::PUMP
    };
    (exec) => {
        $crate::telemetry::domain::EXEC
    };
    (verify) => {
        $crate::telemetry::domain::VERIFY
    };
    (ingest) => {
        $crate::telemetry::domain::INGEST
    };
    (rpc) => {
        $crate::telemetry::domain::RPC
    };
    (aave) => {
        $crate::telemetry::domain::AAVE
    };
    ($other:ident) => {
        ::core::compile_error!(::core::concat!(
            "unknown telemetry domain `",
            ::core::stringify!($other),
            "`; valid domains: state,path,solver,sim,pump,exec,verify,ingest,rpc,aave"
        ))
    };
}

/// A DEBUG diagnostic under a closed domain target, with the engine-span guard.
#[macro_export]
macro_rules! diag {
    (domain = $domain:ident, $($rest:tt)*) => {{
        $crate::telemetry::guard_diag(::core::module_path!());
        ::tracing::debug!(target: $crate::telemetry_target!($domain), $($rest)*)
    }};
}

/// A TRACE forensic diagnostic under a closed domain target (full-field
/// dumps: call traces, tick maps). Never on the console by default — the sink
/// must enable `degenbot=trace`; the phase-4 forensic file sink captures it.
#[macro_export]
macro_rules! diag_trace {
    (domain = $domain:ident, $($rest:tt)*) => {{
        $crate::telemetry::guard_diag(::core::module_path!());
        ::tracing::trace!(target: $crate::telemetry_target!($domain), $($rest)*)
    }};
}

/// A DEBUG lifecycle event under a closed domain target (the boot/census
/// verdicts: process-wide sizing, runtime construction). Deliberately
/// unguarded — unlike [`diag!`] these fire BEFORE any engine span exists, so
/// the engine-span guard would only raise false alarms.
#[macro_export]
macro_rules! op_debug {
    (domain = $domain:ident, $($rest:tt)*) => {
        ::tracing::debug!(target: $crate::telemetry_target!($domain), $($rest)*)
    };
}

/// An INFO lifecycle event under a closed domain target.
#[macro_export]
macro_rules! op_info {
    (domain = $domain:ident, $($rest:tt)*) => {
        ::tracing::info!(target: $crate::telemetry_target!($domain), $($rest)*)
    };
}

/// A WARN event under a closed domain target.
#[macro_export]
macro_rules! op_warn {
    (domain = $domain:ident, $($rest:tt)*) => {
        ::tracing::warn!(target: $crate::telemetry_target!($domain), $($rest)*)
    };
}

/// An ERROR event under a closed domain target.
#[macro_export]
macro_rules! op_error {
    (domain = $domain:ident, $($rest:tt)*) => {
        ::tracing::error!(target: $crate::telemetry_target!($domain), $($rest)*)
    };
}

/// An INFO span named on the domain target (the engine-span namespace).
#[macro_export]
macro_rules! op_span {
    (domain = $domain:ident, $name:literal $(, $($field:tt)*)?) => {
        ::tracing::info_span!(
            target: $crate::telemetry_target!($domain),
            $name $(, $($field)*)?
        )
    };
}

/// Closed list of retired verbosity env names (ADR-043 §5) mapped to the
/// equivalent domain directive. Detection only — no alias is honored. The
/// shim is deleted at 0.7 alongside the other gated cleanups.
pub const RETIRED_ENV_NAMES: &[(&str, &str)] = &[
    ("DEGENBOT_VERIFY_DBG", "degenbot::verify=debug"),
    ("DEGENBOT_V2_CALC_TRACE", "degenbot::sim=debug"),
    ("DEGENBOT_SIM_LOG_REVERTED_SWAPS", "degenbot::sim=debug"),
    ("DEGENBOT_SIM_DIVERGENCE_LOG", "degenbot::sim=debug"),
    ("DEGENBOT_DUMP_CALL_TRACE", "degenbot::sim=trace"),
    ("DEGENBOT_DUMP_TICK_MAPS", "degenbot::state=trace"),
    ("DEGENBOT_WS_TRACE", "degenbot::ingest=debug"),
    ("DEGENBOT_DRAIN_DBG", "degenbot::pump=debug"),
    ("DEGENBOT_TRACE_DISPATCH", "degenbot::pump=debug"),
    ("DEGENBOT_TRACE_REGISTER_SEED", "degenbot::path=debug"),
    ("DEGENBOT_TRACE_LIQUIDITY", "degenbot::state=debug"),
    ("DEGENBOT_TRACE_TICK", "degenbot::state=debug"),
    ("DEGENBOT_GATE_TRACE", "degenbot::solver=debug"),
    ("DEGENBOT_AAVE_EVTRACE", "degenbot::aave=debug"),
    ("DEGENBOT_AAVE_TX_TRACE", "degenbot::aave=debug"),
    // `DEGENBOT_LOG_FMT` named a second console tunnel; the console writer is
    // now derived from binding presence — exactly one per process (ADR-043 §6).
    (
        "DEGENBOT_LOG_FMT",
        "the console writer is derived (one writer per process)",
    ),
];

/// Boot-time detection (ADR-043 §5): WARN once per retired env name present in
/// the process environment, naming the equivalent domain directive. The
/// retired names no longer control anything, so this is a loud migration
/// signal rather than a compatibility shim. Idempotent and cheap; call once at
/// process boot.
pub fn warn_retired_env_names() {
    for (name, directive) in RETIRED_ENV_NAMES {
        if let Ok(value) = std::env::var(name) {
            crate::op_warn!(
                domain = pump,
                retired_env = %name,
                value = %value,
                equivalent = %directive,
                "retired telemetry flag set; use the domain directive instead (ADR-043 section 5)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::domain;
    use std::fmt::Write as _;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    static PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Clone, Default)]
    struct Capture {
        events: Arc<Mutex<Vec<String>>>,
        records: Arc<Mutex<Vec<String>>>,
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() != tracing::Level::ERROR {
                return;
            }
            let mut text = String::new();
            event.record(&mut Fmt(&mut text));
            self.events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(text);
        }

        fn on_record(
            &self,
            _span: &tracing::span::Id,
            values: &tracing::span::Record<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut text = String::new();
            values.record(&mut Fmt(&mut text));
            self.records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(text);
        }
    }

    struct Fmt<'a>(&'a mut String);

    impl tracing::field::Visit for Fmt<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            let _ = write!(self.0, "{}={value:?} ", field.name());
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            let _ = write!(self.0, "{}={value} ", field.name());
        }
    }

    #[test]
    fn domain_set_is_closed_and_complete() {
        assert!(domain::is_valid(domain::SIM));
        assert!(domain::is_valid(domain::AAVE));
        assert!(!domain::is_valid("degenbot::diag"));
        assert!(!domain::is_valid("sim"));
        assert_eq!(domain::ALL.len(), 10);
    }

    #[test]
    fn engine_span_detection_uses_the_degenbot_prefix() {
        let subscriber = tracing_subscriber::registry();
        tracing::subscriber::with_default(subscriber, || {
            assert!(!super::in_engine_span());
            let span = tracing::info_span!("degenbot.test.engine");
            let _guard = span.enter();
            assert!(super::in_engine_span());
        });
    }

    #[test]
    #[expect(clippy::panic)] // the test panics on purpose
    fn panic_hook_emits_one_error_and_marks_the_active_span_error() {
        let _guard = PANIC_HOOK_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            // The hook records otel.status_code on the ACTIVE span; declare the
            // field so the subscriber observes the write (ADR-043 §2).
            let span = tracing::info_span!(
                "degenbot.test.engine",
                otel.status_code = tracing::field::Empty,
            );
            let _entered = span.enter();
            super::install_panic_hook();
            let _ = std::panic::catch_unwind(|| panic!("boom-facade-123"));
            // Drop the facade hook; take_hook restores the default hook.
            let _ = std::panic::take_hook();
        });

        let events = capture
            .events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(events.len(), 1, "exactly one ERROR per panic: {events:?}");
        assert!(
            events[0].contains("boom-facade-123"),
            "payload carried: {events:?}"
        );
        assert!(
            events[0].contains("thread="),
            "thread name carried: {events:?}"
        );
        drop(events);

        let records = capture
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            records
                .iter()
                .any(|r| r.contains("otel.status_code") && r.contains("ERROR")),
            "active span marked Status::Error: {records:?}"
        );
    }

    #[test]
    fn facade_macros_expand_on_the_domain_target() {
        let subscriber = tracing_subscriber::registry();
        tracing::subscriber::with_default(subscriber, || {
            crate::op_info!(domain = sim, block = 1u64, "sim probe");
            let span = crate::op_span!(domain = sim, "degenbot.sim.probe", block = 1u64);
            let _guard = span.enter();
        });
    }

    /// The filter plan is ONE resolution both record layers take: an explicit
    /// `RUST_LOG` is used verbatim on both (`rust_log` reports it), and the
    /// config branch keeps the `OTel` record default while both layers
    /// throttle the third-party transport targets. Written against whatever
    /// environment runs the suite: the branch flag must name the source the
    /// process actually has, so either branch proves its own half.
    #[test]
    fn filter_plan_names_its_source_and_throttles_transport_noise() {
        let plan = super::resolve_filters("info");
        let rust_log = std::env::var("RUST_LOG").is_ok_and(|raw| !raw.trim().is_empty());
        assert_eq!(
            plan.rust_log, rust_log,
            "the plan must name the filter source the process used"
        );
        if plan.rust_log {
            assert_eq!(
                plan.console, plan.otel,
                "RUST_LOG is used verbatim on both record layers"
            );
            return;
        }
        assert!(
            plan.console.starts_with("info,"),
            "the wiring default leads the console layer: {plan:?}"
        );
        let mut expected_otel = super::OTEL_RECORD_DEFAULT.to_string();
        for target in super::ALLOY_NOISE_TARGETS {
            let _ = write!(expected_otel, ",{target}=warn");
            assert!(
                plan.console.contains(&format!(",{target}=warn")),
                "the console layer throttles {target}"
            );
        }
        assert_eq!(
            plan.otel, expected_otel,
            "the OTel layer keeps its record default plus the transport throttle"
        );
    }
}
