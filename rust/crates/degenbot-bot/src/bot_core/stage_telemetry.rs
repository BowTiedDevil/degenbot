//! The per-stage stage-table telemetry seam (`StageTelemetry`) — ONE span per
//! ADR-041 stage transition, parented per block epoch (ergo `BF43PM`, epic
//! `MROOY7`).
//!
//! Replaces the legacy `pump.block` / `pump.log_wait`
//! waterfall shape (which attributed whole stretches of the per-block trace to
//! opaque children) with the machine's stage cycle rendered directly:
//!
//! - `degenbot.epoch` — one trace ROOT per block epoch. This is the renamed
//!   per-header beat span (was the `pump.block` beat), now carrying the epoch
//!   context (`epoch.block` + `epoch.seq`) every span below answers to.
//!   `seq` is the rewind generation: a reorg epoch reads as a fresh root with
//!   a bumped `epoch.seq`, so Jaeger/Grafana tell the migration's regression
//!   story per epoch, not just per block.
//! - `degenbot.stage.<stage>` — one span per stage transition, a child of the
//!   current epoch root, with attrs `epoch.block`, `epoch.seq`,
//!   `stage.from` → `stage.to`, and `queue.age_us` (how long the epoch sat in
//!   `stage.from` waiting — the queue age).
//!   Open-ended rows (`streaming`, `rewind`) hold a span until the next
//!   transition; point rows (`quiesced`, `publish`, `finalize`) emit and drop
//!   immediately (they carry no waiting time of their own).
//!
//! ## G3 stall lesson (preserved — SONJQA, trace a1ad51bd)
//!
//! The old `pump.log_wait` child was force-closed past
//! `LOG_WAIT_MAX_AGE_SECS` because a quiet gap left it dangling 12.7s against
//! a 319µs parent, corrupting the waterfall. The lesson generalizes to every
//! OPEN stage span in the new shape: an interval span left open by a stalled
//! feed or a stuck reorg window must be force-closed at the pump's timed-exit
//! tick (500ms granularity), never left to the next epoch.
//! [`force_close_aged`](Self::force_close_aged) owns that law; the new-shape
//! export test lives in this module (`stale_stage_span_exports_force_closed`),
//! replacing the pump-level `log_wait_never_outlives_its_block_span` fixture.
//!
//! Metric series added for the Final-integration A/B (declared in
//! `instruments.rs`):
//! - `degenbot.stage.publish_cycle` — first relevant log → publish (cycle duration).
//! - `degenbot.stage.rewind` — Rewind frequency (one per `EnterReorg`).
//! - `degenbot.stage.rewind_duration` — Rewind open → close.
//!
//! Cardinality note (see `instruments.rs`): the metric series stay label-free
//! — per-epoch context rides the SPANS, not the histograms; the instruments
//! file's closed-set label law forbids unbounded epoch labels on metrics.

use degenbot_core::op_warn;
use std::time::{Duration, Instant};

use tracing::Span;

use super::epoch::Epoch;
use super::stage_handlers::Stage;

/// SONJQA: max age of a held (open-ended) stage-span interval before the pump
/// force-closes it (with a stall warning). Carried over from
/// `LOG_WAIT_MAX_AGE_SECS` (the `pump.log_wait` waterfall child bound): the
/// observed failure shape (trace a1ad51bd, block 25913381) was a 12.7s
/// all-quiet header gap leaving the wait child open until the NEXT header
/// while its parent exported at arm-exit (319us), corrupting the waterfall.
/// Degenerate relative to the 30s staleness watchdog, a fresh 5s bound keeps
/// children within a healthy block cadence.
pub const STAGE_MAX_AGE_SECS: u64 = 5;

/// The open-ended stage rows (the only ones holding a span open). `Streaming`
/// holds from the epoch's first relevant log until quiesce/tombstone/rewind;
/// `Rewind` holds from `EnterReorg` until `CloseReorg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenKind {
    Streaming,
    Rewind,
}

impl OpenKind {
    fn slug(self) -> &'static str {
        match self {
            OpenKind::Streaming => "streaming",
            OpenKind::Rewind => "rewind",
        }
    }
}

/// The attribute slug for a stage-table row (`None` = Streaming — the
/// machine's open stage row).
fn slug(stage: Option<Stage>) -> &'static str {
    match stage {
        None => "streaming",
        Some(s) => match s {
            Stage::StreamingComplete => "quiesced",
            Stage::Resolve => "resolve",
            Stage::Solve => "solve",
            Stage::Simulate => "simulate",
            Stage::Gate => "gate",
            Stage::Publish => "publish",
            Stage::Finalize => "finalize",
            Stage::Rewind => "rewind",
        },
    }
}

/// Point stage span: created and dropped immediately — it carries the
/// transition’s epoch context, from/to, and queue age, but no future. The
/// span NAME must be a `tracing` callsite literal, hence the macro (the
/// queue age is recorded post-creation — an Empty-declared field records).
macro_rules! point_span {
    ($parent:expr, $name:literal, $from:literal, $to:literal, $epoch:expr, $logs:expr, $age:expr) => {{
        let span = tracing::info_span!(
            parent: $parent.clone(),
            $name,
            epoch.block = $epoch.block(),
            epoch.seq = $epoch.seq(),
            stage.from = $from,
            stage.to = $to,
            queue.age_us = tracing::field::Empty,
            logs.n = $logs,
        );
        if let Some(a) = $age {
            span.record("queue.age_us", u64::try_from(a.as_micros()).unwrap_or(u64::MAX));
        }
        span
    }};
}

struct OpenStage {
    kind: OpenKind,
    span: tracing::Span,
    opened_at: Instant,
    epoch: Epoch,
}

/// Owns the stage waterfall state for one pump run: the open interval span
/// (at most one), the publish-cycle anchor, and the rewind-duration anchor.
/// NO provider, NO timers — the pump drives every call; the `Instant` anchors
/// are the only time kept (span lifetimes are tracing's business).
pub struct StageTelemetry {
    open: Option<OpenStage>,
    cycle_started_at: Option<Instant>,
    rewind_opened_at: Option<Instant>,
}

impl StageTelemetry {
    /// A fresh seam (start of the pump run).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            open: None,
            cycle_started_at: None,
            rewind_opened_at: None,
        }
    }

    /// A new epoch root opened (every accepted header). Closes any open
    /// interval from the prior epoch so it never dangles into the new one.
    ///
    /// NOTE: `rewind_opened_at` is intentionally NOT reset here — a Rewind
    /// interval spanning a quiet header gap must still report its duration
    /// when the window closes.
    pub fn new_epoch(&mut self) {
        self.close_open();
        self.cycle_started_at = None;
    }

    /// The epoch's first relevant log arrived (the Streaming row opens).
    /// Idempotent within an epoch: only opens when no interval is held.
    pub fn on_first_log(&mut self, parent: &Span, epoch: Epoch) {
        if self.open.is_none() {
            self.open_interval(parent, OpenKind::Streaming, epoch, None);
        }
        if self.cycle_started_at.is_none() {
            self.cycle_started_at = Some(Instant::now());
        }
    }

    /// Quiesced (the `StreamingComplete` row): all dispatched logs of the epoch
    /// applied. Closes the Streaming interval (recording its age) and emits
    /// the point span.
    pub fn on_quiesced(&mut self, parent: &Span, epoch: Epoch, logs: u64) {
        let age = self.close_open();
        drop(point_span!(
            parent,
            "degenbot.stage.quiesced",
            "streaming",
            "quiesced",
            epoch,
            logs,
            age
        ));
    }

    /// Tombstone (the Finalize row): the epoch is proven complete.
    pub fn on_tombstone(&mut self, parent: &Span, epoch: Epoch) {
        let age = self.close_open();
        drop(point_span!(
            parent,
            "degenbot.stage.finalize",
            "streaming",
            "finalize",
            epoch,
            0,
            age
        ));
    }

    /// Publish (the Published row): the quiesce-gated publish dispatched.
    /// Also records the publish-cycle histogram (first relevant log →
    /// publish) for the A/B comparison, once per quiesce cycle.
    pub fn on_publish(&mut self, parent: &Span, epoch: Epoch) {
        let age = self.close_open();
        if let Some(started) = self.cycle_started_at.take() {
            if let Some(p) = crate::instruments::pipeline() {
                p.observe_publish_cycle(started.elapsed().as_secs_f64());
            }
        }
        drop(point_span!(
            parent,
            "degenbot.stage.publish",
            "quiesced",
            "publish",
            epoch,
            0,
            age
        ));
    }

    /// `EnterReorg` (the `Rewind` stage from ANY row — invariant I6). Closes
    /// any open interval in the PRE-rewind generation and opens the Rewind
    /// interval in the fresh epoch, counting Rewind frequency.
    pub fn on_enter_reorg(&mut self, parent: &Span, epoch: Epoch, from: Option<Stage>) {
        self.close_open();
        self.open_interval(parent, OpenKind::Rewind, epoch, from);
        self.rewind_opened_at = Some(Instant::now());
        if let Some(p) = crate::instruments::pipeline() {
            p.count_rewind();
        }
    }

    /// `CloseReorg`: the unwind window closed at `new_head`. Closes the Rewind
    /// interval (recording its duration) and reopens Streaming for the fresh
    /// epoch (the cycle restarts — the machine resets the row).
    pub fn on_close_reorg(&mut self, parent: &Span, epoch: Epoch) {
        self.close_rewind_and_record();
        self.open_interval(parent, OpenKind::Streaming, epoch, Some(Stage::Rewind));
    }

    /// SONJQA (G3, preserved): force-close a held stage interval past
    /// `max_age`. The pump's timed-exit tick (500ms) drives this; a stalled
    /// feed or a stuck reorg window never leaves a waterfall child dangling
    /// against a long-closed epoch root.
    pub fn force_close_aged(&mut self, max_age: Duration) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let age = open.opened_at.elapsed();
        if age <= max_age {
            return;
        }
        let (kind, epoch) = (open.kind, open.epoch);
        if let Some(open) = self.open.take() {
            open.span.record(
                "queue.age_us",
                u64::try_from(age.as_micros()).unwrap_or(u64::MAX),
            );
            open.span.record("queue.force_closed", true);
        }
        op_warn!(
            domain = state,
            stage = kind.slug(),
            epoch.block = epoch.block(),
            epoch.seq = epoch.seq(),
            stall_secs = age.as_secs(),
            "[pump] stage interval expired without a transition; stage span force-closed"
        );
        if kind == OpenKind::Rewind {
            self.rewind_opened_at = None;
        }
    }

    /// Open an interval span (Streaming or Rewind) under `parent` (the span
    /// NAME must be a `tracing` callsite literal, hence the two branches).
    fn open_interval(&mut self, parent: &Span, kind: OpenKind, epoch: Epoch, from: Option<Stage>) {
        let from_slug = from.map_or("streaming", |s| slug(Some(s)));
        let span = match kind {
            OpenKind::Streaming => tracing::info_span!(
                parent: parent.clone(),
                "degenbot.stage.streaming",
                epoch.block = epoch.block(),
                epoch.seq = epoch.seq(),
                stage.from = from_slug,
                stage.to = "streaming",
                queue.age_us = tracing::field::Empty,
                queue.force_closed = tracing::field::Empty,
            ),
            OpenKind::Rewind => tracing::info_span!(
                parent: parent.clone(),
                "degenbot.stage.rewind",
                epoch.block = epoch.block(),
                epoch.seq = epoch.seq(),
                stage.from = from_slug,
                stage.to = "rewind",
                queue.age_us = tracing::field::Empty,
                queue.force_closed = tracing::field::Empty,
            ),
        };
        self.open = Some(OpenStage {
            kind,
            span,
            opened_at: Instant::now(),
            epoch,
        });
    }

    /// Close the held interval with its age recorded (the outgoing stage's
    /// queue age). Returns the age so point spans can carry it too.
    /// Streaming closes also project the age to
    /// `degenbot.stage.streaming_age` (first relevant log → quiesce/tombstone
    /// wait) so the stage leg is scrapeable without Jaeger; Rewind already
    /// owns `degenbot.stage.rewind_duration` at `close_rewind_and_record`.
    fn close_open(&mut self) -> Option<Duration> {
        let open = self.open.take()?;
        let age = open.opened_at.elapsed();
        open.span.record(
            "queue.age_us",
            u64::try_from(age.as_micros()).unwrap_or(u64::MAX),
        );
        if open.kind == OpenKind::Streaming {
            if let Some(p) = crate::instruments::pipeline() {
                p.observe_streaming_age(age.as_secs_f64());
            }
        }
        Some(age)
    }

    /// Close the Rewind interval and record its duration histogram.
    fn close_rewind_and_record(&mut self) {
        self.close_open();
        if let Some(opened) = self.rewind_opened_at.take() {
            if let Some(p) = crate::instruments::pipeline() {
                p.observe_rewind_duration(opened.elapsed().as_secs_f64());
            }
        }
    }
}

/// Point stage span: created and dropped immediately — it carries the
/// transition's epoch context, from/to, and queue age, but no future. The
/// span NAME must be a `tracing` callsite literal, hence the macro (the
/// queue age is recorded post-creation — an Empty-declared field records).
impl Default for StageTelemetry {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
impl StageTelemetry {
    /// Test-only view of the held interval kind + its anchors.
    fn open_kind(&self) -> Option<OpenKind> {
        self.open.as_ref().map(|o| o.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Non-otel logic tests: spans are no-ops without a subscriber; only the
    // seam's OWN state machine is under test here.

    #[test]
    fn first_log_opens_streaming_and_quiesce_closes_it() {
        let mut tel = StageTelemetry::new();
        let parent = Span::none();
        let epoch = Epoch::at(100);
        tel.on_first_log(&parent, epoch);
        assert_eq!(tel.open_kind(), Some(OpenKind::Streaming));
        // Second log of the same burst: idempotent (no second interval).
        tel.on_first_log(&parent, epoch);
        assert_eq!(tel.open_kind(), Some(OpenKind::Streaming));
        tel.on_quiesced(&parent, epoch, 2);
        assert_eq!(tel.open_kind(), None);
    }

    #[test]
    fn reorg_opens_rewind_and_close_reopens_streaming() {
        let mut tel = StageTelemetry::new();
        let parent = Span::none();
        tel.on_first_log(&parent, Epoch::at(100));
        // Rewind from the Streaming row into a FRESH generation.
        let rewound = Epoch::with_generation(100, 1);
        tel.on_enter_reorg(&parent, rewound, None);
        assert_eq!(tel.open_kind(), Some(OpenKind::Rewind));
        tel.on_close_reorg(&parent, Epoch::with_generation(102, 1));
        assert_eq!(tel.open_kind(), Some(OpenKind::Streaming));
    }

    #[test]
    fn new_epoch_closes_leaked_interval_and_resets_cycle_anchor() {
        let mut tel = StageTelemetry::new();
        let parent = Span::none();
        tel.on_first_log(&parent, Epoch::at(100));
        assert!(tel.cycle_started_at.is_some());
        tel.new_epoch();
        assert_eq!(tel.open_kind(), None);
        assert!(tel.cycle_started_at.is_none());
    }

    #[test]
    fn force_close_aged_leaves_fresh_intervals_alone() {
        let mut tel = StageTelemetry::new();
        let parent = Span::none();
        tel.on_first_log(&parent, Epoch::at(100));
        tel.force_close_aged(Duration::from_secs(5));
        assert_eq!(
            tel.open_kind(),
            Some(OpenKind::Streaming),
            "a fresh interval must not be force-closed"
        );
    }

    /// Test-time fake aging: 6s before now, padded to the Windows-clock
    /// guarantee, never underflows here.
    #[expect(
        clippy::unwrap_used,
        reason = "test-time fake aging: now-6s cannot underflow"
    )]
    fn aged_instant() -> Instant {
        Instant::now().checked_sub(Duration::from_secs(6)).unwrap()
    }

    /// G3 force-close contract (SONJQA, generalized): a stale interval closes
    /// AT the expiry tick, with its age recorded — never at the next epoch.
    /// Export behavior (the span actually lands in the exporter, closed and
    /// stamped) is pinned by `stale_stage_span_exports_force_closed` below.
    #[test]
    fn force_close_aged_closes_and_clears_stale_interval() {
        let mut tel = StageTelemetry::new();
        let parent = Span::none();
        tel.on_first_log(&parent, Epoch::at(100));
        // Directly age the interval by rewinding its anchor.
        if let Some(open) = tel.open.as_mut() {
            open.opened_at = aged_instant();
        }
        tel.force_close_aged(Duration::from_secs(5));
        assert_eq!(tel.open_kind(), None, "stale interval must be force-closed");
    }

    #[test]
    fn point_stages_leave_no_interval_open() {
        let mut tel = StageTelemetry::new();
        let parent = Span::none();
        let epoch = Epoch::at(100);
        tel.on_publish(&parent, epoch);
        tel.on_tombstone(&parent, epoch);
        assert_eq!(tel.open_kind(), None, "point stages are emit-and-drop");
    }
}

/// With the otel feature, pin the export contract of the G3 force-close: the
/// stale streaming span must EXPORT (closed, age recorded, force-closed
/// stamp) — the new-shape successor of
/// `log_wait_never_outlives_its_block_span`.
#[cfg(all(test, feature = "otel"))]
mod otel_tests {
    use super::*;
    use crate::otel;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;

    /// Test-time fake aging: 6s before now, never underflows in tests.
    #[expect(
        clippy::unwrap_used,
        reason = "test-time fake aging: now-6s cannot underflow"
    )]
    fn aged_instant() -> Instant {
        Instant::now().checked_sub(Duration::from_secs(6)).unwrap()
    }

    #[test]
    #[expect(
        clippy::expect_used,
        reason = "otel test: flush and span collection must succeed, else the test fails loudly"
    )]
    #[expect(
        clippy::panic,
        reason = "assertion helper: diagnostic payload if the streaming span did not export"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "queue.age_us attribute is integral microseconds; an f64 encoding of it is integral too"
    )]
    fn stale_stage_span_exports_force_closed() {
        let exporter = InMemorySpanExporter::default();
        let (provider, tracer) = otel::provider_with_exporter(exporter.clone());
        let subscriber = tracing_subscriber::registry().with(otel::layer(tracer));
        let _guard = tracing::subscriber::set_default(subscriber);

        let mut tel = StageTelemetry::new();
        let root = tracing::info_span!("degenbot.epoch.run", epoch.block = 100u64);
        tel.on_first_log(&root, Epoch::at(100));
        // Age the interval past the max age, then run the force-close.
        if let Some(open) = tel.open.as_mut() {
            open.opened_at = aged_instant();
        }
        tel.force_close_aged(Duration::from_secs(5));
        drop(tel);
        drop(root);

        provider.force_flush().expect("flush");
        let spans = exporter.get_finished_spans().expect("spans");
        let streaming = spans
            .iter()
            .find(|sp| sp.name.as_ref() == "degenbot.stage.streaming")
            .unwrap_or_else(|| {
                panic!(
                    "stale streaming span must export; got {:?}",
                    spans.iter().map(|sp| sp.name.as_ref()).collect::<Vec<_>>()
                )
            });
        // The held interval was aged 6s by rewinding its anchor; the seam
        // records that age on the span's `queue.age_us` attribute — an
        // un-entered span's wall duration (start→end) is only this test's
        // create→drop window and can never reflect the aged interval (the
        // SAME shape as the force-closed production child: closed by handle,
        // not by an entered scope).
        let age_us: i64 = streaming
            .attributes
            .iter()
            .filter(|kv| kv.key == opentelemetry::Key::from_static_str("queue.age_us"))
            .map(|kv| match &kv.value {
                opentelemetry::Value::I64(v) => *v,
                opentelemetry::Value::F64(v) => *v as i64,
                opentelemetry::Value::String(v) => v.as_str().parse::<i64>().unwrap_or(0),
                _ => 0,
            })
            .max()
            .unwrap_or(0);
        assert!(
            age_us >= 5_000_000,
            "aged streaming span must carry its real age in queue.age_us; got {age_us}us"
        );
        let forced = streaming
            .attributes
            .iter()
            .any(|kv| kv.key == opentelemetry::Key::from_static_str("queue.force_closed"));
        assert!(
            forced,
            "force-close must stamp queue.force_closed on the span"
        );
    }
}
