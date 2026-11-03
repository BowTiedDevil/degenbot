//! The crawl's measurement surface: unit-stage wall time, split by outcome
//! class, and the summary fold that consumes receipts.
//!
//! The stage profile is the crawl port's before/after instrument: per class and
//! per stage it reports total milliseconds and share of unit time, which is
//! what says where a unit's wall time goes and which stage owns a refused
//! unit's cost. [`StageProfile::summary_lines`] renders the block byte for byte
//! as `build_paths.UnitStageProfile.summary_lines` does, so a Rust crawl's
//! profile compares against a recorded Python one without translation.
//!
//! [`CrawlSummary`] is the crawl's one fold point: it feeds each receipt
//! through the core's outcome counter fold
//! (`bot_core::registration_ledger::PipelineReport::absorb`) and then through
//! the profile, and renders the counters and the block.

use crate::arb_engine::crawl::receipt::{CrawlReceipt, StageSplit, UnitStage};
use crate::bot_core::registration_ledger::{
    PipelineReport, RegistrationOutcome, RegistrationUnitOutcome,
};

/// The skip classes, indexed by the answering stage — the memo, build, and
/// eligibility stages are the only ones that can answer a skip.
const SKIP_CLASSES: [UnitStageClass; 3] = [
    UnitStageClass::MemoSkip,
    UnitStageClass::BuildSkip,
    UnitStageClass::EligibilitySkip,
];

/// The line tag the profile and counters blocks carry.
///
/// It is the Python pipeline's own `[build_paths]` prefix, kept because the
/// blocks are a measurement instrument: a Rust crawl's render is compared byte
/// for byte against a recorded Python one, and the captured lines carry the tag.
/// The core logs nothing here — a driver decides where the text goes — so the
/// tag is measured output rather than a console-area duplicate (ADR-043 §7).
const LINE_TAG: &str = "[build_paths]";

/// Nanoseconds per millisecond, as the profile formats milliseconds.
const NANOS_PER_MILLI: f64 = 1_000_000.0;

/// Nanoseconds per microsecond, as the profile formats microseconds per unit.
const NANOS_PER_MICRO: f64 = 1_000.0;

/// One stage-profile class: the outcome classes the crawl is measured by, the
/// terminal kinds that would otherwise go uncounted, and the skips whose stage
/// split never reached the fold.
///
/// Declaration order is render order (`build_paths.UNIT_OUTCOME_CLASSES`).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnitStageClass {
    /// A completed registration, created or deduplicated.
    Registered,
    /// The ledger's memos refused the unit.
    MemoSkip,
    /// Pool construction refused the unit.
    BuildSkip,
    /// The eligibility gate refused the unit.
    EligibilitySkip,
    /// The engine counted a rejection.
    EngineReject,
    /// A transient register failure.
    RegisterFail,
    /// The benign registered-path cap stop.
    CapSkip,
    /// A skip folded without a usable stage split — counted so the classes
    /// still add up to the unit count.
    UnattributedSkip,
}

impl UnitStageClass {
    /// The class count.
    pub const COUNT: usize = 8;

    /// Every class, in render order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Registered,
        Self::MemoSkip,
        Self::BuildSkip,
        Self::EligibilitySkip,
        Self::EngineReject,
        Self::RegisterFail,
        Self::CapSkip,
        Self::UnattributedSkip,
    ];

    /// The class's position in [`UnitStageClass::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Registered => 0,
            Self::MemoSkip => 1,
            Self::BuildSkip => 2,
            Self::EligibilitySkip => 3,
            Self::EngineReject => 4,
            Self::RegisterFail => 5,
            Self::CapSkip => 6,
            Self::UnattributedSkip => 7,
        }
    }

    /// The class's rendered name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::MemoSkip => "memo-skip",
            Self::BuildSkip => "build-skip",
            Self::EligibilitySkip => "eligibility-skip",
            Self::EngineReject => "engine-reject",
            Self::RegisterFail => "register-fail",
            Self::CapSkip => "cap-skip",
            Self::UnattributedSkip => "unattributed-skip",
        }
    }

    /// Name the class an outcome folds under (`build_paths.unit_outcome_class`).
    ///
    /// A skip is attributed to the stage that answered it, which is what lets
    /// the profile answer where a refused unit's wall time goes per refusal
    /// site. A skip whose split did not reach the fold (a caller-built outcome,
    /// or a split that ran past the eligibility stage) is counted as
    /// [`UnitStageClass::UnattributedSkip`] — never guessed into a stage class.
    #[must_use]
    pub fn classify(outcome: &RegistrationUnitOutcome, split: &StageSplit) -> Self {
        match outcome {
            RegistrationUnitOutcome::Skip { .. } => {
                let ran = split.len();
                if (1..=SKIP_CLASSES.len()).contains(&ran) {
                    SKIP_CLASSES[ran - 1]
                } else {
                    Self::UnattributedSkip
                }
            }
            RegistrationUnitOutcome::Registered { .. } => Self::Registered,
            RegistrationUnitOutcome::Reject { .. } => Self::EngineReject,
            RegistrationUnitOutcome::RegisterFailed { .. } => Self::RegisterFail,
            RegistrationUnitOutcome::Cap => Self::CapSkip,
        }
    }
}

/// Aggregate unit-stage wall time, split by outcome class
/// (`build_paths.UnitStageProfile`).
///
/// The per-unit half of the measurement lives on the receipt (a unit times only
/// itself); this is the fold the crawl's single receipt loop owns — one counter
/// bump and one add per stage, negligible beside the work it measures.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StageProfile {
    /// Units folded, per class.
    units: [usize; UnitStageClass::COUNT],
    /// Per-class per-stage wall time in ns, in [`UnitStage::ALL`] order.
    stage_ns: [[u64; UnitStage::COUNT]; UnitStageClass::COUNT],
}

impl StageProfile {
    /// Fold one receipt's outcome under its class.
    pub fn absorb(&mut self, receipt: &CrawlReceipt) {
        self.absorb_outcome(&receipt.outcome, &receipt.split);
    }

    /// Fold one outcome and its stage split under the outcome's class.
    ///
    /// ## Parameters
    ///
    /// - `outcome`: the unit's terminal outcome.
    /// - `split`: the stages the unit ran, front-truncated at the answering
    ///   one; the class for a skip is read off its length.
    pub fn absorb_outcome(&mut self, outcome: &RegistrationUnitOutcome, split: &StageSplit) {
        let class = UnitStageClass::classify(outcome, split).index();
        self.units[class] += 1;
        let buckets = &mut self.stage_ns[class];
        for (index, delta) in split.as_slice().iter().enumerate() {
            buckets[index] += *delta;
        }
    }

    /// Units folded under one class.
    #[must_use]
    pub fn units(&self, class: UnitStageClass) -> usize {
        self.units[class.index()]
    }

    /// One class's per-stage wall time in ns, in [`UnitStage::ALL`] order.
    #[must_use]
    pub fn class_stage_ns(&self, class: UnitStageClass) -> [u64; UnitStage::COUNT] {
        self.stage_ns[class.index()]
    }

    /// Units folded, over every class.
    #[must_use]
    pub fn total_units(&self) -> usize {
        self.units.iter().sum()
    }

    /// Per-stage wall time across every class, in [`UnitStage::ALL`] order.
    #[must_use]
    pub fn overall_stage_ns(&self) -> [u64; UnitStage::COUNT] {
        let mut overall = [0; UnitStage::COUNT];
        for (index, delta) in overall.iter_mut().enumerate() {
            for buckets in &self.stage_ns {
                *delta += buckets[index];
            }
        }
        overall
    }

    /// Total unit time in ns.
    #[must_use]
    pub fn total_ns(&self) -> u64 {
        self.overall_stage_ns().iter().sum()
    }

    /// Render the profile as the crawl's trim block: the totals, the all-units
    /// stage split, then one line per class that saw a unit.
    ///
    /// The block is byte-identical to the Python pipeline's
    /// (`build_paths.UnitStageProfile.summary_lines`): the port's before/after
    /// measurement compares the two renders directly.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "the rows are decimal measurements rendered from ns counters; f64 is the render space, as the Python f-string math"
    )]
    pub fn summary_lines(&self) -> Vec<String> {
        let total_units = self.total_units();
        let overall = self.overall_stage_ns();
        let total_ns: u64 = overall.iter().sum();
        let mut lines = Vec::with_capacity(2 + UnitStageClass::COUNT);
        lines.push(format!(
            "{LINE_TAG} Unit-stage profile (cumulative): {total_units} units, {}ms of unit time",
            format_thousands(total_ns as f64 / NANOS_PER_MILLI, 1)
        ));
        lines.push(format!(
            "{LINE_TAG}   all units: {}",
            stage_split(&overall, total_ns)
        ));
        for class in UnitStageClass::ALL {
            let units = self.units[class.index()];
            if units == 0 {
                continue;
            }
            let buckets = self.stage_ns[class.index()];
            let class_ns: u64 = buckets.iter().sum();
            let share = if total_ns == 0 {
                0.0
            } else {
                100.0 * class_ns as f64 / total_ns as f64
            };
            lines.push(format!(
                "{LINE_TAG}   {}: {units} units, {}ms ({share:.1}% of unit time, {}us/unit) — {}",
                class.name(),
                format_thousands(class_ns as f64 / NANOS_PER_MILLI, 1),
                format_thousands(class_ns as f64 / units as f64 / NANOS_PER_MICRO, 0),
                stage_split(&buckets, class_ns),
            ));
        }
        lines
    }
}

/// The crawl's cumulative measurement: the pipeline counters and the stage
/// profile, folded from receipts at one point.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CrawlSummary {
    /// The outcome counters, folded with the core's arithmetic
    /// (`PipelineReport::absorb`) — the one definition of how unit outcomes
    /// accumulate.
    pub report: PipelineReport,
    /// The unit-stage wall-time aggregate.
    pub profile: StageProfile,
    /// Breakdown labels in first-seen order: `Counter.most_common`'s tie-break,
    /// which a sorted map alone cannot reproduce.
    first_seen: Vec<String>,
}

impl CrawlSummary {
    /// Fold one receipt into both aggregates — the crawl's one fold point.
    ///
    /// Folding the same receipt twice double-counts the unit and folding it not
    /// at all loses it; both surface in
    /// [`PipelineReport::fold_identities_hold`] and the folded-unit count.
    pub fn absorb(&mut self, receipt: &CrawlReceipt) {
        self.report.absorb(&receipt.outcome);
        self.profile.absorb(receipt);
        if let Some(label) = folded_label(&receipt.outcome) {
            if !self.first_seen.iter().any(|seen| seen == label) {
                self.first_seen.push(label.to_owned());
            }
        }
    }

    /// The `n` most frequent skip reasons, count first and first-seen order for
    /// ties — `Counter.most_common(n)`'s order.
    ///
    /// A label the counters hold but this fold never recorded (foreign counter
    /// mutation) is ranked after the recorded ones rather than dropped from the
    /// line.
    #[must_use]
    pub fn top_reasons(&self, n: usize) -> Vec<(&str, usize)> {
        let mut ranked: Vec<(&str, usize)> = self
            .first_seen
            .iter()
            .filter_map(|label| {
                self.report
                    .skip_reasons
                    .get(label)
                    .map(|count| (label.as_str(), *count))
            })
            .collect();
        for (label, count) in &self.report.skip_reasons {
            if !self.first_seen.iter().any(|seen| seen == label) {
                ranked.push((label.as_str(), *count));
            }
        }
        // The sort is stable: equal counts keep their first-seen order.
        ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        ranked.truncate(n);
        ranked
    }

    /// The counters line (`build_paths.emit_registration_progress`), including
    /// the top-eight skip-reason breakdown.
    #[must_use]
    pub fn progress_line(&self) -> String {
        let breakdown = self
            .top_reasons(8)
            .iter()
            .map(|(label, count)| format!("{label}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let report = &self.report;
        format!(
            "{LINE_TAG} Progress: {} paths registered, {} skipped, {} token-filtered, {} engine-rejected, {} register-fail, {} cap-skipped, {} duplicates {{skip_reasons: {breakdown}}}",
            report.path_count,
            report.skip_count,
            report.token_filter_count,
            report.engine_reject_count,
            report.register_fail_count,
            report.cap_skip_count,
            report.dup_count,
        )
    }

    /// The stage-profile block (`build_paths.emit_stage_profile`).
    #[must_use]
    pub fn stage_profile_lines(&self) -> Vec<String> {
        self.profile.summary_lines()
    }
}

/// The breakdown label one fold records, if it records one.
///
/// The core fold inserts at most one label per receipt: a skip's own label, the
/// cap stop's tag, a register failure's tag, or the duplicate tag for a
/// deduplicated registration. A rejection and a created registration insert
/// nothing (their counts carry them).
fn folded_label(outcome: &RegistrationUnitOutcome) -> Option<&str> {
    match outcome {
        RegistrationUnitOutcome::Skip { label, .. } => Some(label.as_label()),
        RegistrationUnitOutcome::Cap => Some(RegistrationOutcome::PathCap.as_str()),
        RegistrationUnitOutcome::RegisterFailed { .. } => {
            Some(RegistrationOutcome::RegisterFailed.as_str())
        }
        RegistrationUnitOutcome::Registered { created: false, .. } => {
            Some(RegistrationOutcome::Dup.as_str())
        }
        RegistrationUnitOutcome::Reject { .. } | RegistrationUnitOutcome::Registered { .. } => None,
    }
}

/// Spell one profile row: per stage, milliseconds and share of the row.
#[expect(
    clippy::cast_precision_loss,
    reason = "the row is a decimal render of ns counters; f64 is the render space, as the Python f-string math"
)]
fn stage_split(buckets: &[u64; UnitStage::COUNT], total_ns: u64) -> String {
    let columns: Vec<String> = UnitStage::ALL
        .iter()
        .map(|stage| {
            let delta = buckets[stage.index()];
            let share = if total_ns == 0 {
                0.0
            } else {
                100.0 * delta as f64 / total_ns as f64
            };
            format!(
                "{}={}ms({share:.1}%)",
                stage.name(),
                format_thousands(delta as f64 / NANOS_PER_MILLI, 1)
            )
        })
        .collect();
    columns.join(" ")
}

/// Format `value` with `decimals` decimals and a comma-grouped integer part —
/// the Python `:,.{decimals}f` the profile's rows use.
///
/// Rust's fixed-precision float formatting rounds the exact binary value to the
/// requested digits (ties to even), which is what Python's format does; the only
/// difference is grouping, added here.
fn format_thousands(value: f64, decimals: usize) -> String {
    let raw = format!("{value:.decimals$}");
    match raw.split_once('.') {
        Some((integer, fraction)) => format!("{}.{fraction}", group_digits(integer)),
        None => group_digits(&raw),
    }
}

/// Insert the thousands separators into a formatted integer part.
fn group_digits(integer: &str) -> String {
    let digits = integer.strip_prefix('-').unwrap_or(integer);
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if integer.starts_with('-') {
        grouped.push('-');
    }
    for (index, digit) in digits.char_indices() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use crate::bot_core::registration_ledger::OutcomeLabel;

    use super::*;

    fn skip(label: RegistrationOutcome, counts_as_skip: bool) -> RegistrationUnitOutcome {
        RegistrationUnitOutcome::Skip {
            label: OutcomeLabel::Vocabulary(label),
            counts_as_skip,
            detail: None,
        }
    }

    fn free_form_skip(label: &str) -> RegistrationUnitOutcome {
        RegistrationUnitOutcome::Skip {
            label: OutcomeLabel::FreeForm(label.to_owned()),
            counts_as_skip: true,
            detail: None,
        }
    }

    fn registered(created: bool, v4_hops: usize) -> RegistrationUnitOutcome {
        RegistrationUnitOutcome::Registered { created, v4_hops }
    }

    fn receipt(seq: u64, outcome: RegistrationUnitOutcome, split: StageSplit) -> CrawlReceipt {
        CrawlReceipt {
            seq,
            hop_sig: None,
            outcome,
            split,
            path_id: None,
        }
    }

    fn profile_of(folds: Vec<(RegistrationUnitOutcome, StageSplit, usize)>) -> StageProfile {
        let mut profile = StageProfile::default();
        for (outcome, split, times) in folds {
            for _ in 0..times {
                profile.absorb_outcome(&outcome, &split);
            }
        }
        profile
    }

    // A memo refusal carries one split entry, a build refusal two, an
    // eligibility refusal three, and a registration all five.
    #[test]
    fn skip_class_is_the_stage_that_answered_it() {
        let cases = [
            (0, UnitStageClass::UnattributedSkip),
            (1, UnitStageClass::MemoSkip),
            (2, UnitStageClass::BuildSkip),
            (3, UnitStageClass::EligibilitySkip),
            (4, UnitStageClass::UnattributedSkip),
            (5, UnitStageClass::UnattributedSkip),
        ];
        for (ran, expected) in cases {
            let mut split = StageSplit::new();
            for _ in 0..ran {
                assert!(split.lap(1));
            }
            assert_eq!(
                UnitStageClass::classify(&skip(RegistrationOutcome::BuildV3Refused, true), &split),
                expected,
                "a skip that ran {ran} stages"
            );
        }
        // A deduplicated registration answers at the eligibility gate's
        // registered-path fast path, but its class is the registration's own.
        let dup = StageSplit::truncated(&[1, 2, 3]);
        assert_eq!(
            UnitStageClass::classify(&registered(false, 0), &dup),
            UnitStageClass::Registered
        );
        for (outcome, expected) in [
            (
                RegistrationUnitOutcome::Reject { detail: None },
                UnitStageClass::EngineReject,
            ),
            (
                RegistrationUnitOutcome::RegisterFailed { detail: None },
                UnitStageClass::RegisterFail,
            ),
            (RegistrationUnitOutcome::Cap, UnitStageClass::CapSkip),
        ] {
            assert_eq!(
                UnitStageClass::classify(&outcome, &StageSplit::new()),
                expected
            );
        }
    }

    #[test]
    fn a_refusal_and_its_memo_replay_are_distinct_classes() {
        // The build stage answered the refusal (two stages ran) ...
        let mut profile = StageProfile::default();
        profile.absorb_outcome(
            &skip(RegistrationOutcome::BuildV4Refused, true),
            &StageSplit::truncated(&[7, 9]),
        );
        assert_eq!(profile.units(UnitStageClass::BuildSkip), 1);
        // ... and the unregistrable-pool memo answered the replay with one
        // stage, so the replay never reached a build.
        let replay = StageSplit::truncated(&[4]);
        assert_eq!(replay.answering_stage(), Some(UnitStage::Memo));
        profile.absorb_outcome(&skip(RegistrationOutcome::BuildV4Refused, true), &replay);
        assert_eq!(profile.units(UnitStageClass::MemoSkip), 1);
        assert_eq!(profile.units(UnitStageClass::BuildSkip), 1);
        assert_eq!(
            profile.class_stage_ns(UnitStageClass::BuildSkip),
            [7, 9, 0, 0, 0]
        );
        assert_eq!(
            profile.class_stage_ns(UnitStageClass::MemoSkip),
            [4, 0, 0, 0, 0]
        );
    }

    #[test]
    fn the_profile_aggregates_per_class_and_per_stage() {
        let profile = profile_of(vec![
            (
                registered(true, 0),
                StageSplit::truncated(&[100, 200, 300, 400, 500]),
                1,
            ),
            (
                skip(RegistrationOutcome::BuildV3Refused, true),
                StageSplit::truncated(&[10, 20]),
                1,
            ),
            (
                RegistrationUnitOutcome::Reject { detail: None },
                StageSplit::truncated(&[1, 2, 3, 4, 5]),
                1,
            ),
            // A caller-built outcome carries no split: counted, unmeasured.
            (RegistrationUnitOutcome::Cap, StageSplit::new(), 1),
        ]);
        assert_eq!(profile.units(UnitStageClass::Registered), 1);
        assert_eq!(profile.units(UnitStageClass::BuildSkip), 1);
        assert_eq!(profile.units(UnitStageClass::EngineReject), 1);
        assert_eq!(profile.units(UnitStageClass::CapSkip), 1);
        assert_eq!(profile.total_units(), 4);
        assert_eq!(
            profile.class_stage_ns(UnitStageClass::Registered),
            [100, 200, 300, 400, 500]
        );
        assert_eq!(
            profile.class_stage_ns(UnitStageClass::BuildSkip),
            [10, 20, 0, 0, 0]
        );
        assert_eq!(profile.class_stage_ns(UnitStageClass::CapSkip), [0; 5]);
        assert_eq!(profile.overall_stage_ns(), [111, 222, 303, 404, 505]);
        assert_eq!(profile.total_ns(), 1545);
    }

    /// The render, byte for byte, of the Python pipeline's `summary_lines` for
    /// the same aggregates — including a class with no unit rendering nothing.
    #[test]
    fn renders_byte_identically_to_the_python_block() {
        let profile = profile_of(vec![
            (
                registered(true, 0),
                StageSplit::truncated(&[100, 200, 300, 400, 500]),
                1,
            ),
            (
                skip(RegistrationOutcome::BuildV3Refused, true),
                StageSplit::truncated(&[10, 20]),
                1,
            ),
            (
                RegistrationUnitOutcome::Reject { detail: None },
                StageSplit::truncated(&[1, 2, 3, 4, 5]),
                1,
            ),
            (RegistrationUnitOutcome::Cap, StageSplit::new(), 1),
        ]);
        let expected = [
            "[build_paths] Unit-stage profile (cumulative): 4 units, 0.0ms of unit time",
            "[build_paths]   all units: memo=0.0ms(7.2%) build=0.0ms(14.4%) eligibility=0.0ms(19.6%) verify=0.0ms(26.1%) register=0.0ms(32.7%)",
            "[build_paths]   registered: 1 units, 0.0ms (97.1% of unit time, 2us/unit) — memo=0.0ms(6.7%) build=0.0ms(13.3%) eligibility=0.0ms(20.0%) verify=0.0ms(26.7%) register=0.0ms(33.3%)",
            "[build_paths]   build-skip: 1 units, 0.0ms (1.9% of unit time, 0us/unit) — memo=0.0ms(33.3%) build=0.0ms(66.7%) eligibility=0.0ms(0.0%) verify=0.0ms(0.0%) register=0.0ms(0.0%)",
            "[build_paths]   engine-reject: 1 units, 0.0ms (1.0% of unit time, 0us/unit) — memo=0.0ms(6.7%) build=0.0ms(13.3%) eligibility=0.0ms(20.0%) verify=0.0ms(26.7%) register=0.0ms(33.3%)",
            "[build_paths]   cap-skip: 1 units, 0.0ms (0.0% of unit time, 0us/unit) — memo=0.0ms(0.0%) build=0.0ms(0.0%) eligibility=0.0ms(0.0%) verify=0.0ms(0.0%) register=0.0ms(0.0%)",
        ];
        assert_eq!(profile.summary_lines(), expected);
        assert!(
            !profile
                .summary_lines()
                .iter()
                .any(|line| line.contains("memo-skip")),
            "a class with no unit renders nothing"
        );
    }

    /// The render at production scale: comma grouping in the millisecond and
    /// microseconds-per-unit columns, every class row, and the
    /// unattributed-skip fallback. The expected block is the Python pipeline's
    /// output for the same folds.
    #[test]
    fn renders_byte_identically_to_the_python_block_at_scale() {
        let profile = profile_of(vec![
            (
                registered(true, 0),
                StageSplit::from_array([
                    86_745_100_000,
                    66_391_200_000,
                    64_173_500_000,
                    109_691_100_000,
                    257_329_000_000,
                ]),
                1_234,
            ),
            (
                skip(RegistrationOutcome::PathRejected, true),
                StageSplit::truncated(&[50_727_300_000]),
                779_730,
            ),
            (
                skip(RegistrationOutcome::BuildV2Refused, true),
                StageSplit::truncated(&[15_800_000, 967_800_000]),
                315,
            ),
            (
                skip(RegistrationOutcome::DirectionMismatch, true),
                StageSplit::truncated(&[3, 7, 11]),
                12,
            ),
            (
                RegistrationUnitOutcome::Reject { detail: None },
                StageSplit::truncated(&[13, 17, 19, 23, 29]),
                7,
            ),
            (
                RegistrationUnitOutcome::RegisterFailed { detail: None },
                StageSplit::truncated(&[5, 5, 5, 5, 5]),
                3,
            ),
            (
                RegistrationUnitOutcome::Cap,
                StageSplit::from_array([100_000, 100_000, 100_000, 100_000, 300_000]),
                3,
            ),
            // No split at all, and a skip whose split ran past the eligibility
            // stage: both counted, neither attributed to a stage.
            (
                skip(RegistrationOutcome::V4NoHash, true),
                StageSplit::new(),
                1,
            ),
            (
                skip(RegistrationOutcome::V4NoHash, true),
                StageSplit::from_array([1, 2, 3, 4, 5]),
                1,
            ),
        ]);
        let expected = [
            "[build_paths] Unit-stage profile (cumulative): 781306 units, 40,274,970,561.7ms of unit time",
            "[build_paths]   all units: memo=39,660,646,059.7ms(98.5%) build=82,231,598.1ms(0.2%) eligibility=79,190,099.3ms(0.2%) verify=135,358,817.7ms(0.3%) register=317,543,986.9ms(0.8%)",
            "[build_paths]   registered: 1234 units, 721,063,096.6ms (1.8% of unit time, 584,329,900us/unit) — memo=107,043,453.4ms(14.8%) build=81,926,740.8ms(11.4%) eligibility=79,190,099.0ms(11.0%) verify=135,358,817.4ms(18.8%) register=317,543,986.0ms(44.0%)",
            "[build_paths]   memo-skip: 779730 units, 39,553,597,629.0ms (98.2% of unit time, 50,727,300us/unit) — memo=39,553,597,629.0ms(100.0%) build=0.0ms(0.0%) eligibility=0.0ms(0.0%) verify=0.0ms(0.0%) register=0.0ms(0.0%)",
            "[build_paths]   build-skip: 315 units, 309,834.0ms (0.0% of unit time, 983,600us/unit) — memo=4,977.0ms(1.6%) build=304,857.0ms(98.4%) eligibility=0.0ms(0.0%) verify=0.0ms(0.0%) register=0.0ms(0.0%)",
            "[build_paths]   eligibility-skip: 12 units, 0.0ms (0.0% of unit time, 0us/unit) — memo=0.0ms(14.3%) build=0.0ms(33.3%) eligibility=0.0ms(52.4%) verify=0.0ms(0.0%) register=0.0ms(0.0%)",
            "[build_paths]   engine-reject: 7 units, 0.0ms (0.0% of unit time, 0us/unit) — memo=0.0ms(12.9%) build=0.0ms(16.8%) eligibility=0.0ms(18.8%) verify=0.0ms(22.8%) register=0.0ms(28.7%)",
            "[build_paths]   register-fail: 3 units, 0.0ms (0.0% of unit time, 0us/unit) — memo=0.0ms(20.0%) build=0.0ms(20.0%) eligibility=0.0ms(20.0%) verify=0.0ms(20.0%) register=0.0ms(20.0%)",
            "[build_paths]   cap-skip: 3 units, 2.1ms (0.0% of unit time, 700us/unit) — memo=0.3ms(14.3%) build=0.3ms(14.3%) eligibility=0.3ms(14.3%) verify=0.3ms(14.3%) register=0.9ms(42.9%)",
            "[build_paths]   unattributed-skip: 2 units, 0.0ms (0.0% of unit time, 0us/unit) — memo=0.0ms(6.7%) build=0.0ms(13.3%) eligibility=0.0ms(20.0%) verify=0.0ms(26.7%) register=0.0ms(33.3%)",
        ];
        assert_eq!(profile.summary_lines(), expected);
    }

    #[test]
    fn an_empty_profile_renders_the_totals_only() {
        assert_eq!(
            StageProfile::default().summary_lines(),
            [
                "[build_paths] Unit-stage profile (cumulative): 0 units, 0.0ms of unit time",
                "[build_paths]   all units: memo=0.0ms(0.0%) build=0.0ms(0.0%) eligibility=0.0ms(0.0%) verify=0.0ms(0.0%) register=0.0ms(0.0%)",
            ]
        );
    }

    /// The formatting table `format(value, ',.{d}f')` answers, ties included:
    /// the profile's columns are that format, so a rounding divergence would
    /// break the port's before/after comparison.
    #[test]
    fn fixed_point_formatting_matches_python() {
        let cases: [(f64, usize, &str); 18] = [
            (0.5, 0, "0"),
            (1.5, 0, "2"),
            (2.5, 0, "2"),
            (3.5, 0, "4"),
            (0.25, 1, "0.2"),
            (0.35, 1, "0.3"),
            (0.15, 1, "0.1"),
            (0.05, 1, "0.1"),
            (1_234_567.5, 0, "1,234,568"),
            (1_234_567.25, 1, "1,234,567.2"),
            (0.0, 1, "0.0"),
            (1_000_000_000_000.0, 1, "1,000,000,000,000.0"),
            (999.95, 1, "1,000.0"),
            (999.85, 1, "999.9"),
            (584_329.9, 0, "584,330"),
            (50_727.3, 0, "50,727"),
            (1.05, 1, "1.1"),
            (2.675, 2, "2.67"),
        ];
        for (value, decimals, expected) in cases {
            assert_eq!(
                format_thousands(value, decimals),
                expected,
                "format({value}, ',.{decimals}f')"
            );
        }
    }

    /// Folding the ported outcome sequence: the core counters, the fold
    /// identities, and the counters line the Python pipeline emits for it.
    #[test]
    fn the_summary_folds_receipts_with_the_core_arithmetic() {
        let mut summary = CrawlSummary::default();
        let folds = [
            (skip(RegistrationOutcome::BuildV3Refused, true), 3),
            (skip(RegistrationOutcome::PathRejected, true), 3),
            (RegistrationUnitOutcome::Cap, 1),
            (RegistrationUnitOutcome::RegisterFailed { detail: None }, 1),
            (registered(true, 1), 1),
            (registered(false, 2), 1),
            (RegistrationUnitOutcome::Reject { detail: None }, 1),
            (skip(RegistrationOutcome::V4HookRejected, false), 1),
            (free_form_skip("build-v3:ConnectionError"), 1),
        ];
        let mut seq = 0;
        for (outcome, times) in folds {
            for _ in 0..times {
                summary.absorb(&receipt(seq, outcome.clone(), StageSplit::new()));
                seq += 1;
            }
        }
        let report = &summary.report;
        assert_eq!(report.units_folded, 13);
        assert_eq!(report.path_count, 1);
        assert_eq!(report.skip_count, 8);
        assert_eq!(report.cap_skip_count, 1);
        assert_eq!(report.engine_reject_count, 1);
        assert_eq!(report.other_exc_count, 1);
        assert_eq!(report.register_fail_count, 1);
        assert_eq!(report.dup_count, 1);
        assert_eq!(report.v4_pool_count, 3, "counted on created AND duplicate");
        assert_eq!(report.v4_hook_rejected, 1);
        assert_eq!(report.uncounted_skip_count, 1);
        assert_eq!(report.token_filter_count, 0, "the fold never touches it");
        assert!(report.capped);
        assert!(report.fold_identities_hold());
        assert_eq!(
            summary
                .report
                .skip_reasons
                .iter()
                .map(|(label, count)| (label.as_str(), *count))
                .collect::<Vec<_>>(),
            [
                ("build-v3-refused", 3),
                ("build-v3:ConnectionError", 1),
                ("dup", 1),
                ("path-cap", 1),
                ("path-rejected-memo", 3),
                ("register-fail", 1),
                ("v4-hook-rejected", 1),
            ],
            "the counters are the core fold's; only the render orders them"
        );
        assert_eq!(
            summary.progress_line(),
            "[build_paths] Progress: 1 paths registered, 8 skipped, 0 token-filtered, 1 engine-rejected, 1 register-fail, 1 cap-skipped, 1 duplicates {skip_reasons: build-v3-refused=3, path-rejected-memo=3, path-cap=1, register-fail=1, dup=1, v4-hook-rejected=1, build-v3:ConnectionError=1}"
        );
    }

    #[test]
    fn the_breakdown_ranks_ties_by_first_seen() {
        let mut summary = CrawlSummary::default();
        for (seq, outcome) in [
            skip(RegistrationOutcome::BuildV2Refused, true),
            skip(RegistrationOutcome::BuildV3Refused, true),
            skip(RegistrationOutcome::BuildV2Refused, true),
            skip(RegistrationOutcome::BuildV3Refused, true),
            skip(RegistrationOutcome::V4NoHash, true),
        ]
        .into_iter()
        .enumerate()
        {
            summary.absorb(&receipt(seq as u64, outcome, StageSplit::truncated(&[1])));
        }
        assert_eq!(
            summary.top_reasons(8),
            [
                ("build-v2-refused", 2),
                ("build-v3-refused", 2),
                ("v4-no-hash", 1),
            ]
        );
        assert_eq!(summary.top_reasons(2).len(), 2, "the bound is the top n");
    }

    #[test]
    fn the_profile_folds_each_receipt_once() {
        let mut summary = CrawlSummary::default();
        summary.absorb(&receipt(
            0,
            registered(true, 0),
            StageSplit::from_array([1, 2, 3, 4, 5]),
        ));
        summary.absorb(&receipt(
            1,
            skip(RegistrationOutcome::BuildV3Refused, true),
            StageSplit::truncated(&[6, 7]),
        ));
        assert_eq!(summary.profile.total_units(), summary.report.units_folded);
        assert_eq!(summary.profile.total_ns(), 28);
        assert_eq!(
            summary.stage_profile_lines(),
            summary.profile.summary_lines()
        );
    }
}
