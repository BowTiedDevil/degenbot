use ::degenbot_solvers::affected_keys::AffectedKey;
use ::degenbot_solvers::mixed::HopType;
/// Test-side affected-key plumbing (the trivial remainder of the deleted
/// (`EpochDelta` is sole authority), so the per-family sets below are
/// just a sorted `AffectedKey` builder the solve-shaped tests use to
/// call `run_epoch` / `solve_dirty`. No production
/// caller, no parity claim.
use hashbrown::HashSet;
/// Convert per-family key sets into delta-shaped affected keys
/// (sorted, retired `take_all` order).
#[must_use]
pub(crate) fn affected_keys(
    v2: &HashSet<u64>,
    v3: &HashSet<u64>,
    v4: &HashSet<u64>,
) -> Vec<AffectedKey> {
    let mut keys: Vec<AffectedKey> = v2
        .iter()
        .map(|&p| AffectedKey::new(HopType::V2, p))
        .chain(v3.iter().map(|&p| AffectedKey::new(HopType::V3, p)))
        .chain(v4.iter().map(|&p| AffectedKey::new(HopType::V4, p)))
        .collect();
    keys.sort_unstable();
    keys
}
/// The per-family dirty-set builder the solve tests use (the old
/// `DirtySets` insert + `to_affected_keys` take, single-threaded).
pub(crate) struct DirtyKeys {
    v2: HashSet<u64>,
    v3: HashSet<u64>,
    v4: HashSet<u64>,
}
impl DirtyKeys {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            v2: HashSet::new(),
            v3: HashSet::new(),
            v4: HashSet::new(),
        }
    }
    /// Insert `pool_id` into the set for `hop_type`.
    pub(crate) fn insert(&mut self, pool_id: u64, hop_type: HopType) {
        match hop_type {
            HopType::V2 => self.v2.insert(pool_id),
            HopType::V3 => self.v3.insert(pool_id),
            HopType::V4 => self.v4.insert(pool_id),
            _ => false, // non-pool hop types are never dirtied
        };
    }
    /// Delta-shaped sorted take view (no consume — tests re-use).
    #[must_use]
    pub(crate) fn to_affected_keys(&self) -> Vec<AffectedKey> {
        affected_keys(&self.v2, &self.v3, &self.v4)
    }
}
impl Default for DirtyKeys {
    fn default() -> Self {
        Self::new()
    }
}
// Cold-start trace (detached-cycle arm attribution): the cycle span must
// carry `cycle.arm`, derivable WITHOUT log archaeology. The helper below
// is the ONE wiring site (the solve cycle, at the machine's begin_cycle
// verdict). WFF6MM: one arm remains, so one stamp.
#[cfg(feature = "otel")]
#[test]
#[expect(clippy::expect_used)]
fn cycle_arm_span_field_stamps_the_arm() {
    use crate::arb_engine::engine_stages::record_cycle_arm_telemetry;
    use opentelemetry_sdk::trace::InMemorySpanExporter;
    use tracing_subscriber::layer::SubscriberExt;
    let exporter = InMemorySpanExporter::default();
    let (provider, tracer) = crate::otel::provider_with_exporter(exporter.clone());
    let subscriber = tracing_subscriber::registry().with(crate::otel::layer(tracer));
    tracing::subscriber::with_default(subscriber, || {
        let detached =
            tracing::info_span!("degenbot.arb.solve", cycle.arm = tracing::field::Empty,);
        let guard = detached.enter();
        assert_eq!(
            record_cycle_arm_telemetry(&detached, "detached"),
            "detached",
            "the helper returns the label the caller latches on the engine"
        );
        drop(guard);
    });
    provider.force_flush().expect("flush");
    let spans = exporter.get_finished_spans().expect("spans");
    let solve_spans: Vec<_> = spans
        .iter()
        .filter(|sp| sp.name.as_ref() == "degenbot.arb.solve")
        .collect();
    assert_eq!(
        solve_spans.len(),
        1,
        "expected exactly the one stamped cycle span; got {spans:?}"
    );
    let arm_values: Vec<String> = solve_spans
        .iter()
        .filter_map(|sp| {
            sp.attributes
                .iter()
                .find(|kv| kv.key == opentelemetry::Key::from_static_str("cycle.arm"))
                .and_then(|kv| match &kv.value {
                    opentelemetry::Value::String(v) => Some(v.to_string()),
                    _ => None,
                })
        })
        .collect();
    assert!(
        arm_values == vec!["detached".to_string()],
        "the one dispatch arm must stamp cycle.arm exactly once; got {arm_values:?}"
    );
}
// -------------------------------------------------------------------
