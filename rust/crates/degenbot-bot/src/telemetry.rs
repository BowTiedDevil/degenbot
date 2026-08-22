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
