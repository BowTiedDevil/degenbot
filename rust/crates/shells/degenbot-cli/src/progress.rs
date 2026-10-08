//! The `indicatif` progress bar (ADR-051 D9).
//!
//! Progress rendering exists ONLY here: `degenbot-cli-core` is indicatif-free
//! (`just check-cli-core-purity`), and the updater cores report through their
//! `ProgressSink` seam. cli-core's arms run the cores with `NoProgress` (the
//! sealed semantic home hard-codes it), so the facade paints the bar from the
//! cores' own operator-facing progress events: the throttled `op_info!` lines
//! carry `progress_pct` + the chunk fields on the closed domain
//! targets, and [`Layer`] turns exactly those events into bar updates.
//!
//! # The two surfaces
//!
//! - **TTY**: [`stderr_opt`] yields a draw target and the bar paints on stderr.
//! - **non-TTY** (a pipe, a CI log, a nohup redirect): [`stderr_opt`] yields
//!   `None`, and the painter falls back to the one-line-per-chunk record on
//!   stdout — cursor, chunks committed, elapsed — so a redirected run is
//!   observable. The cores' throttled INFO `op_info!` lines are NOT that
//!   surface: the console filter defaults to `warn`, so they never reach the
//!   fmt sink, which is why a redirected run used to print nothing at all. The
//!   progress layer therefore runs without the console filter and self-filters
//!   by target + level — the two chunk-committed targets at INFO are its
//!   entire input domain.
//!
//! (indicatif 0.18 dropped `ProgressDrawTarget::stderr_opt`; [`stderr_opt`]
//! restores exactly that contract with `std::io::IsTerminal`.)

use std::io::{IsTerminal as _, Write};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;

/// The pool updater's progress-event target (`pool update: chunk committed`).
pub const POOL_TARGET: &str = "degenbot::ingest";

/// The Aave updater's progress-event target (`aave update: chunk committed`).
pub const AAVE_TARGET: &str = "degenbot::aave";

/// A stderr draw target only when stderr is a terminal.
///
/// Off a terminal the console painter falls back to the redirected-stdout
/// record surface (see [`Painter::console`]).
#[must_use]
pub fn stderr_opt() -> Option<ProgressDrawTarget> {
    std::io::stderr()
        .is_terminal()
        .then(ProgressDrawTarget::stderr)
}

/// The non-TTY progress surface: one line per committed chunk on the
/// redirected stdout, carrying the chunk cursor, the running chunk count, and
/// the elapsed time. Progress must never crash or wedge the run, so a poisoned
/// lock or a write failure is swallowed.
struct Record {
    out: Mutex<Box<dyn Write + Send>>,
    start: Instant,
    chunks: AtomicU64,
}

impl Record {
    fn line(&self, percent: u64, cursor: Option<u64>, message: &str) {
        let chunks = self
            .chunks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        let elapsed = self.start.elapsed().as_secs();
        let line = match cursor {
            Some(cursor) => format!(
                "{message} cursor={cursor} chunks={chunks} elapsed={elapsed}s pct={percent}%"
            ),
            None => format!("{message} chunks={chunks} elapsed={elapsed}s pct={percent}%"),
        };
        if let Ok(mut out) = self.out.lock() {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

/// The bar painter.
pub struct Painter {
    bar: Option<ProgressBar>,
    record: Option<Record>,
}

impl std::fmt::Debug for Painter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Painter")
            .field("bar", &self.bar)
            .field("record", &self.record.is_some())
            .finish()
    }
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
        Self { bar, record: None }
    }

    /// The console painter: the bar when stderr is a terminal, otherwise the
    /// one-line-per-chunk record on stdout when stdout itself is redirected.
    #[must_use]
    pub fn console() -> Self {
        if std::io::stderr().is_terminal() {
            return Self::from_draw_target(Some(ProgressDrawTarget::stderr()));
        }
        Self {
            bar: None,
            record: (!std::io::stdout().is_terminal()).then(|| Record {
                out: Mutex::new(Box::new(std::io::stdout())),
                start: Instant::now(),
                chunks: AtomicU64::new(0),
            }),
        }
    }

    /// A painter whose record surface writes into `out`.
    #[cfg(test)]
    fn with_record(out: Box<dyn Write + Send>) -> Self {
        Self {
            bar: None,
            record: Some(Record {
                out: Mutex::new(out),
                start: Instant::now(),
                chunks: AtomicU64::new(0),
            }),
        }
    }

    /// Whether any progress surface is attached (a terminal bar, or a
    /// redirected-stdout record).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.bar.is_some() || self.record.is_some()
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

    /// Report one committed chunk: paint the bar when a terminal is attached,
    /// otherwise append one progress record line to the redirected stdout.
    pub fn chunk(&self, percent: u64, cursor: Option<u64>, message: Option<&str>) {
        if self.bar.is_some() {
            self.paint(percent, message.unwrap_or("update"));
        } else if let Some(record) = &self.record {
            record.line(percent, cursor, message.unwrap_or("update"));
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
                .chunk(percent, fields.chunk_end, fields.message.as_deref());
        }
    }
}

/// The fields the progress events carry that the surfaces need.
#[derive(Debug, Default)]
struct Fields {
    progress_pct: Option<u64>,
    chunk_end: Option<u64>,
    message: Option<String>,
}

impl tracing::field::Visit for Fields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "progress_pct" => self.progress_pct = Some(value),
            "chunk_end" => self.chunk_end = Some(value),
            _ => {}
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
    #![expect(clippy::unwrap_used)]

    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use indicatif::ProgressDrawTarget;
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::{Layer, Painter, AAVE_TARGET, POOL_TARGET};

    /// A writer capturing the record surface's output for assertions.
    struct CaptureSink(Arc<Mutex<Vec<u8>>>);

    impl Write for CaptureSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

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

    #[test]
    fn non_tty_run_records_one_line_per_chunk() {
        let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
        let painter = Arc::new(Painter::with_record(Box::new(CaptureSink(Arc::clone(
            &captured,
        )))));
        let subscriber = tracing_subscriber::registry().with(Layer::new(Arc::clone(&painter)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: AAVE_TARGET,
                chain_id = 8453i64,
                chunk_start = 1u64,
                chunk_end = 500u64,
                chunks_committed = 1u64,
                progress_pct = 42u64,
                "aave update: chunk committed"
            );
            tracing::info!(
                target: AAVE_TARGET,
                chunk_end = 1000u64,
                chunks_committed = 2u64,
                progress_pct = 84u64,
                "aave update: chunk committed"
            );
        });
        let text = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        let mut lines = text.lines();
        let first = lines.next().unwrap();
        assert!(
            first.starts_with("aave update: chunk committed cursor=500 chunks=1 elapsed="),
            "unexpected first record: {first}"
        );
        assert!(
            first.ends_with("pct=42%"),
            "unexpected first record: {first}"
        );
        let second = lines.next().unwrap();
        assert!(
            second.contains("cursor=1000 chunks=2 ") && second.ends_with("pct=84%"),
            "unexpected second record: {second}"
        );
        assert!(
            lines.next().is_none(),
            "one line per committed chunk: {text}"
        );
    }
}
