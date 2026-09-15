//! The `indicatif` progress bar (ADR-051 D9).
//!
//! Progress rendering exists ONLY here: `degenbot-cli-core` is indicatif-free
//! (`just check-cli-core-purity`), and the updater cores report through their
//! `ProgressSink` seam. cli-core's arms run the cores with `NoProgress` (the
//! sealed semantic home hard-codes it), so the facade paints the bar from the
//! cores' own operator-facing progress events: the throttled `op_info!` lines
//! (`Q5IKHX`) carry `progress_pct` + the chunk fields on the closed domain
//! targets, and [`Layer`] turns exactly those events into bar updates.
//!
//! # The two surfaces
//!
//! - **TTY**: [`stderr_opt`] yields a draw target and the bar paints on stderr.
//! - **non-TTY** (a pipe, a CI log): [`stderr_opt`] yields `None`, so no bar is
//!   ever constructed and painting is a strict no-write; the throttled
//!   `op_info!` lines from the `fmt` sink remain the non-TTY progress surface.
//!
//! (indicatif 0.18 dropped `ProgressDrawTarget::stderr_opt`; [`stderr_opt`]
//! restores exactly that contract with `std::io::IsTerminal`.)

use std::io::IsTerminal as _;
use std::sync::Arc;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;

/// The pool updater's progress-event target (`pool update: chunk committed`).
pub const POOL_TARGET: &str = "degenbot::ingest";

/// The Aave updater's progress-event target (`aave update: chunk committed`).
pub const AAVE_TARGET: &str = "degenbot::aave";

/// A stderr draw target only when stderr is a terminal.
///
/// The non-TTY surface is the cores' throttled `op_info!` lines; returning
/// `None` here is what makes a redirected run byte-stable.
#[must_use]
pub fn stderr_opt() -> Option<ProgressDrawTarget> {
    std::io::stderr()
        .is_terminal()
        .then(ProgressDrawTarget::stderr)
}

/// The bar painter.
#[derive(Debug)]
pub struct Painter {
    bar: Option<ProgressBar>,
}

impl Painter {
    /// Build a painter over `target`. `None` builds the strict no-op painter:
    /// there is no bar to write through, so [`Painter::paint`] cannot reach a
    /// console.
    #[must_use]
    pub fn from_draw_target(target: Option<ProgressDrawTarget>) -> Self {
        let bar = target.map(|target| {
            let bar = ProgressBar::with_draw_target(Some(100), target);
            let style = ProgressStyle::with_template("{msg} [{bar:40}] {pos:>3}%")
                .unwrap_or_else(|_| ProgressStyle::default_bar());
            bar.set_style(style);
            bar
        });
        Self { bar }
    }

    /// Whether a draw target is attached (a TTY console).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.bar.is_some()
    }

    /// Paint the bar at `percent` with `message`. A no-op when no draw target
    /// is attached.
    pub fn paint(&self, percent: u64, message: &str) {
        if let Some(bar) = &self.bar {
            bar.set_length(100);
            bar.set_position(percent.min(100));
            bar.set_message(message.to_string());
        }
    }

    /// The bar's current position (`None` without a draw target).
    #[must_use]
    pub fn position(&self) -> Option<u64> {
        self.bar.as_ref().map(ProgressBar::position)
    }

    /// The bar's current message (`None` without a draw target).
    #[must_use]
    pub fn message(&self) -> Option<String> {
        self.bar.as_ref().map(ProgressBar::message)
    }

    /// Clear the bar (the run is over).
    pub fn finish(&self) {
        if let Some(bar) = &self.bar {
            bar.finish_and_clear();
        }
    }
}

/// The tracing layer that paints [`Painter`] from the cores' chunk progress
/// events.
#[derive(Debug)]
pub struct Layer {
    painter: Arc<Painter>,
}

impl Layer {
    /// A layer painting into `painter`.
    #[must_use]
    pub fn new(painter: Arc<Painter>) -> Self {
        Self { painter }
    }
}

impl<S: Subscriber> tracing_subscriber::Layer<S> for Layer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let metadata = event.metadata();
        if metadata.target() != POOL_TARGET && metadata.target() != AAVE_TARGET {
            return;
        }
        if *metadata.level() != Level::INFO {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        if let Some(percent) = fields.progress_pct {
            self.painter
                .paint(percent, fields.message.as_deref().unwrap_or("update"));
        }
    }
}

/// The two fields the progress events carry that the bar needs.
#[derive(Debug, Default)]
struct Fields {
    progress_pct: Option<u64>,
    message: Option<String>,
}

impl tracing::field::Visit for Fields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "progress_pct" {
            self.progress_pct = Some(value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(format!("{value:?}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use indicatif::ProgressDrawTarget;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::{Layer, Painter, POOL_TARGET};

    #[test]
    fn no_draw_target_means_no_painter_and_no_write() {
        let painter = Painter::from_draw_target(None);
        assert!(!painter.is_active());
        painter.paint(100, "pool update: chunk committed");
        assert_eq!(painter.position(), None);
        assert_eq!(painter.message(), None);
        painter.finish();
    }

    #[test]
    fn progress_layer_paints_from_the_core_progress_event() {
        let painter = Arc::new(Painter::from_draw_target(
            Some(ProgressDrawTarget::hidden()),
        ));
        assert!(painter.is_active());
        let subscriber = tracing_subscriber::registry().with(Layer::new(Arc::clone(&painter)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: POOL_TARGET,
                chain_id = 8453i64,
                chunk_start = 1u64,
                chunk_end = 500u64,
                progress_pct = 42u64,
                "pool update: chunk committed"
            );
        });
        assert_eq!(painter.position(), Some(42));
        assert_eq!(
            painter.message().as_deref(),
            Some("pool update: chunk committed")
        );
        painter.finish();
    }

    #[test]
    fn unrelated_events_do_not_paint() {
        let painter = Arc::new(Painter::from_draw_target(
            Some(ProgressDrawTarget::hidden()),
        ));
        let subscriber = tracing_subscriber::registry().with(Layer::new(Arc::clone(&painter)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "degenbot::rpc", progress_pct = 99u64, "not a chunk");
        });
        assert_eq!(painter.position(), Some(0));
        painter.finish();
    }
}
