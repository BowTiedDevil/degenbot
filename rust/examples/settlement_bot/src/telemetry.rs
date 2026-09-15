//! Telemetry boot prelude for the standalone settlement-bot driver.
//!
//! Gap G6 : the pure-Rust parity twin of the Python
//! driver must boot the SAME observability stack the Python driver boots, in
//! the same order, so the boot census / telemetry announcements are observable
//! from the standalone binary. Before this module the example exposed no
//! telemetry at all.
//!
//! # Boot order (mirrors `degenbot-python`'s pymodule init)
//!
//! 1. Typed config install — `BotConfigLoader` (schema defaults +
//!    `DEGENBOT_*` env) is the ONE env-reading site for telemetry keys, so
//!    `DEGENBOT_LOG_LEVEL`, `DEGENBOT_TELEMETRY_DIAG`, `DEGENBOT_OTEL` and
//!    `DEGENBOT_METRICS_ADDR` take effect. The driver's manual pre-0.6
//!    `config.toml` cascade (rpc/ws/database) is untouched: the typed loader
//!    runs env-only here because those file sections are retired layout items
//!    the validated modern file layer refuses.
//! 2. `tracing_subscriber` console layer — compact fmt on stderr, filtered by
//!    the shared ADR-043 section 4 plan
//!    (`bot_telemetry::resolve_filters`). The example is the Python driver's
//!    parity twin, so it adopts the Python-driver wiring default (`info`) so
//!    the boot census the Python boot emits is actually visible.
//! 3. OTLP span layer — env-gated (`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` >
//!    `OTEL_EXPORTER_OTLP_ENDPOINT`) and built through the core
//!    `degenbot::bot::otel` surface. An absent endpoint is a quiet no-op
//!    (never fatal): the exporter's own `http://localhost:4318` default is
//!    deliberately NOT taken — a standalone parity run has no collector, and
//!    forcing the default would spew export failures into every boot.
//! 4. Prometheus scrape endpoint — `degenbot::bot::metrics` via
//!    `metrics_addr_from_env()` (`DEGENBOT_METRICS_ADDR`, default
//!    `127.0.0.1:9464`). A bind/exporter failure degrades loudly but
//!    non-fatally.
//! 5. `install_panic_hook()` + `warn_retired_env_names()` — the shared
//!    ADR-043 section 2/5 contracts — then the ONE worker-census boot line
//!    (`worker_census::emit_boot_table`), which includes the scrape-server
//!    row registered in step 4.
//!
//! # Shutdown
//!
//! [`TelemetryBoot`] owns the flush-before-teardown contract (ADR-043
//! section 6): `Drop` flushes + shuts the OTLP provider and stops the scrape
//! server while the exporter's runtime is still alive, mirroring the Python
//! `flush_telemetry` note in `degenbot/telemetry/__init__.py`.
//!
//! # Degradation
//!
//! Every step is best-effort with a loud warning — the Python driver's
//! contract is "logging stays up, telemetry is optional". No failure in this
//! module aborts the bot.

use std::net::SocketAddr;

use degenbot::bot::{metrics, otel, telemetry as bot_telemetry};
use degenbot::{op_info, op_warn};
use tracing_subscriber::layer::{Layer, SubscriberExt as _};
use tracing_subscriber::EnvFilter;

/// Compact stderr console layer, generic over the subscriber so each
/// composition branch (bare registry vs OTel-layered registry) infers its own
/// subscriber type.
fn console_layer<S>(directives: &str) -> impl Layer<S> + Send + Sync
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .compact()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::new(directives))
}

/// OTLP endpoint env names, highest precedence first — the `OTel` spec order
/// `provider_from_env_endpoint` resolves (signal-specific, then generic).
const OTLP_ENDPOINT_ENVS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_ENDPOINT",
];

/// Process-lifetime telemetry handles. The driver holds exactly one for its
/// whole `run()`; dropping it performs the ADR-043 section 6
/// flush-before-teardown.
#[must_use]
pub(crate) struct TelemetryBoot {
    otel: Option<otel::OtelHandle>,
}

impl Drop for TelemetryBoot {
    fn drop(&mut self) {
        op_info!(
            domain = pump,
            "telemetry shutdown: flushing OTLP spans + stopping the metrics scrape endpoint"
        );
        // ADR-043 section 6: flush + shut the span provider BEFORE the tokio
        // runtime behind the exporter is torn down. Best-effort: a failure
        // degrades to the log, never a panic on the way out.
        if let Some(handle) = self.otel.take() {
            if let Err(error) = handle.flush() {
                op_warn!(domain = pump, error = %error, "OTel flush before exit failed (continuing)");
            }
            if let Err(error) = handle.shutdown() {
                op_warn!(domain = pump, error = %error, "OTel provider shutdown failed (continuing)");
            }
        }
        metrics::shutdown_global_metrics();
    }
}

/// Boot the driver telemetry stack (see the module docs for the order and the
/// degradation rules). Never fails.
pub(crate) fn init() -> TelemetryBoot {
    // Step 1: install the typed config (one env-reading site for DEGENBOT_*).
    let config_error = match degenbot::config::BotConfigLoader::new().load() {
        Ok(loaded) => {
            // First-wins, mirroring degenbot-python's production boot path.
            let _ = degenbot::bot_core::stance::install(std::sync::Arc::new(loaded.config));
            None
        }
        Err(error) => Some(error.to_string()),
    };

    // Step 2: resolve the console/OTel record filters (ADR-043 section 4).
    let plan = bot_telemetry::resolve_filters(bot_telemetry::CONSOLE_WIRING_DEFAULT_PYTHON);
    let otel_enabled = degenbot::config::holder::config().telemetry.otel;
    let endpoint_env = OTLP_ENDPOINT_ENVS
        .into_iter()
        .find(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()));

    // Step 3: build the OTLP layer only when an endpoint is actually
    // configured (absent endpoint = quiet no-op, never fatal).
    let mut otel_handle = None;
    let mut otel_layer = None;
    let mut otel_error = None;
    if otel_enabled && endpoint_env.is_some() {
        match otel::provider_from_env_endpoint() {
            Ok((provider, tracer)) => {
                otel_handle = Some(otel::OtelHandle::new(provider));
                otel_layer = Some(otel::layer(tracer));
            }
            Err(error) => otel_error = Some(error.to_string()),
        }
    }

    // The OTel layer binds S=Registry, so it must sit directly on the bare
    // registry with the console/fmt layer composed on top (the core
    // `init_otel_tracing` ordering).
    let installed = match otel_layer {
        Some(layer) => tracing::subscriber::set_global_default(
            tracing_subscriber::registry()
                .with(layer.with_filter(EnvFilter::new(&plan.otel)))
                .with(console_layer(&plan.console)),
        )
        .is_ok(),
        None => tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(console_layer(&plan.console)),
        )
        .is_ok(),
    };

    // Step 4: Prometheus scrape endpoint. Independent of the OTLP endpoint (a
    // collector is optional; a scrape endpoint is not), gated on the same
    // `telemetry.otel` switch the Python driver uses.
    let scrape_addr: Option<SocketAddr> = if otel_enabled {
        match metrics::init_global_metrics() {
            Ok(()) => metrics::metrics_addr_from_env().ok(),
            Err(error) => {
                op_warn!(domain = pump, error = %error, "metrics endpoint disabled (continuing without a scrape endpoint)");
                None
            }
        }
    } else {
        None
    };

    // Step 5: the shared ADR-043 panic + retired-env contracts, then the ONE
    // census boot line (the scrape-server row registered above is in it).
    degenbot::telemetry::install_panic_hook();
    degenbot::telemetry::warn_retired_env_names();
    degenbot::core::worker_census::emit_boot_table();

    // Step 6: boot census announcements on the closed domain targets.
    if let Some(error) = config_error {
        op_warn!(domain = pump, error = %error, "typed BotConfig load failed; telemetry keys fall back to schema defaults");
    }
    if !installed {
        op_warn!(
            domain = pump,
            "global tracing subscriber already installed; console wiring left untouched"
        );
    }
    let otel_state = if otel_handle.is_some() {
        "active"
    } else if otel_enabled {
        "inactive (no OTLP endpoint configured)"
    } else {
        "inactive (DEGENBOT_OTEL=0)"
    };
    op_info!(
        domain = pump,
        console = %plan.console,
        otel = %otel_state,
        metrics_addr = %scrape_addr.map_or_else(|| "inactive".to_string(), |addr| addr.to_string()),
        "telemetry boot complete"
    );
    if let Some(error) = otel_error {
        op_warn!(domain = pump, error = %error, "OTel span layer disabled: OTLP exporter build failed (continuing)");
    } else if otel_handle.is_some() {
        op_info!(
            domain = pump,
            endpoint_env = endpoint_env.unwrap_or("OTEL_EXPORTER_OTLP_ENDPOINT"),
            "OTel OTLP span layer active"
        );
    }

    TelemetryBoot { otel: otel_handle }
}
