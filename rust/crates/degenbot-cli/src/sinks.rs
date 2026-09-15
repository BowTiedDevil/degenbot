//! The telemetry sinks + boot order (ADR-043, ADR-051 D2/D9).
//!
//! The boot order mirrors `degenbot-python`'s `#[pymodule]` init, because the
//! order is what makes the console's log surface identical to the Python
//! driver's:
//!
//! 1. **Typed config first.** `BotConfigLoader` (schema defaults + the
//!    standard file layer + `DEGENBOT_*` env) is installed into the typed
//!    holder BEFORE any subscriber exists, so `telemetry.log_level` /
//!    `telemetry.diag` are the values the filter resolution actually sees. An
//!    invalid config is a loud boot refusal (exit 2), exactly as Python's
//!    module init refuses.
//! 2. **The tracing registry.** A `fmt` layer on stderr filtered by
//!    [`resolve_filters`](degenbot_bot::telemetry::resolve_filters) with the
//!    Python-driver wiring default (`info`) - the same resolution the binding
//!    boots, so an explicit `RUST_LOG` wins verbatim on this sink too - plus
//!    the [`progress`](crate::progress) bar layer (ADR-051 D9).
//! 3. **The shared ADR-043 contracts**: `warn_retired_env_names`,
//!    `install_panic_hook`, then the one worker-census boot line.
//!
//! OTLP/metrics are deliberately NOT booted here: a console invocation is a
//! short-lived process, not a scrape target, and the Python driver's OTLP layer
//! belongs to the long-running bot process. `telemetry.otel` is still honored as
//! config (the typed loader refuses a bad value) - it just has no sink to gate.

use std::sync::Arc;

use degenbot_bot::telemetry as bot_telemetry;
use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};
use tracing_subscriber::EnvFilter;

use crate::progress::{self, Layer as ProgressLayer, Painter};

/// The process-lifetime telemetry handles. Dropping it clears the progress bar.
#[derive(Debug)]
#[must_use = "the boot guard must outlive the command run"]
pub struct TelemetryBoot {
    painter: Arc<Painter>,
}

impl Drop for TelemetryBoot {
    fn drop(&mut self) {
        self.painter.finish();
    }
}

/// Boot the console's telemetry sinks (see the module docs for the order).
///
/// # Errors
///
/// The refusal message when the typed configuration is invalid - the process
/// cannot honor a policy it does not have, so the boot is refused exactly as
/// the Python module init refuses it.
pub fn boot() -> Result<TelemetryBoot, String> {
    // Step 1: the typed config. The loader is the ONE env-reading site.
    match degenbot_config::BotConfigLoader::new()
        .with_standard_file_paths()
        .load()
    {
        Ok(loaded) => {
            // First-wins, mirroring the Python boot path.
            let _ = degenbot_bot::bot_core::stance::install(Arc::new(loaded.config));
        }
        Err(error) => return Err(format!("invalid configuration - boot refused: {error}")),
    }

    // Step 2: the console filter, resolved through the SAME ADR-043 section 4
    // plan the Python driver uses (explicit RUST_LOG wins verbatim).
    let plan = bot_telemetry::resolve_filters(bot_telemetry::CONSOLE_WIRING_DEFAULT_PYTHON);
    let painter = Arc::new(Painter::from_draw_target(progress::stderr_opt()));
    let console_filter = EnvFilter::new(&plan.console);
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_writer(std::io::stderr)
                .with_filter(console_filter.clone()),
        )
        .with(ProgressLayer::new(Arc::clone(&painter)).with_filter(console_filter));
    let installed = tracing::subscriber::set_global_default(subscriber).is_ok();

    // Step 3: the shared ADR-043 section 2/5 contracts + the census boot line.
    degenbot_core::telemetry::warn_retired_env_names();
    degenbot_core::telemetry::install_panic_hook();
    degenbot_core::worker_census::emit_boot_table();

    degenbot_core::op_info!(
        domain = pump,
        console = %plan.console,
        "telemetry boot complete"
    );
    if !installed {
        degenbot_core::op_warn!(
            domain = pump,
            "global tracing subscriber already installed; console wiring left untouched"
        );
    }

    Ok(TelemetryBoot { painter })
}
