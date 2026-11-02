//! The crawl's per-unit receipt: what a candidate was, where it stopped, and
//! what it became.
//!
//! The crawl submits candidate paths as units and resolves them in FIFO order,
//! one receipt each. A receipt carries the core's terminal outcome vocabulary
//! (`bot_core::registration_ledger::RegistrationUnitOutcome`) beside the unit's
//! front-truncated stage split: the split's length names the stage that
//! answered the unit, which is what lets the stage profile answer where a
//! refused unit's wall time went (`build_paths.py`'s `UNIT_STAGES` and
//! `UnitStageClock`).
//!
//! [`StageSplit`] holds the truncation rule in the type: a receipt cannot
//! report a stage the unit never ran, and a class name is never guessed from a
//! stage that did not answer.

use crate::bot_core::registration_ledger::{
    HopSignature, RegistrationOutcome, RegistrationUnitOutcome,
};

/// The registration unit's stages, in execution order — the stage profile's
/// columns.
///
/// Mirrors `build_paths.UNIT_STAGES`: a unit laps these in order and its
/// receipt's split is truncated at the stage that answered it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnitStage {
    /// The ledger's memos answered before any construction work started.
    Memo,
    /// Pool construction (the build flight).
    Build,
    /// The eligibility gate: direction agreement, the memos, the policy.
    Eligibility,
    /// The registration lifecycle's state verification.
    Verify,
    /// Path registration + the eager solve.
    Register,
}

impl UnitStage {
    /// The stage count — the split's width and the profile's column count.
    pub const COUNT: usize = 5;

    /// Every stage, in execution order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Memo,
        Self::Build,
        Self::Eligibility,
        Self::Verify,
        Self::Register,
    ];

    /// The stage's position in [`UnitStage::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Memo => 0,
            Self::Build => 1,
            Self::Eligibility => 2,
            Self::Verify => 3,
            Self::Register => 4,
        }
    }

    /// The stage's column name, as the profile renders it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Memo => "memo",
            Self::Build => "build",
            Self::Eligibility => "eligibility",
            Self::Verify => "verify",
            Self::Register => "register",
        }
    }
}

/// One unit's per-stage wall time in nanoseconds, front-truncated at the stage
/// that answered it.
///
/// The length is the answering stage's index + 1: a memo refusal holds one
/// delta, a build refusal two, an eligibility refusal three, and a completed
/// registration all five. Aggregation only — nothing logs a unit's split.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StageSplit {
    /// Per-stage deltas in [`UnitStage::ALL`] order; entries past `len` are
    /// never read (and never written by [`StageSplit::lap`]).
    ns: [u64; UnitStage::COUNT],
    /// Stages the unit ran.
    len: usize,
}

impl StageSplit {
    /// An empty split — a unit that answered before it ran a stage.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            ns: [0; UnitStage::COUNT],
            len: 0,
        }
    }

    /// A split holding a caller-held fixed-width array.
    ///
    /// ## Parameters
    ///
    /// - `ns`: one delta per stage the unit ran, in [`UnitStage::ALL`] order.
    ///
    /// Entries past [`UnitStage::COUNT`] are dropped, matching the profile's
    /// own absorb clamp: the columns are the unit's five stages.
    #[must_use]
    pub fn truncated(ns: &[u64]) -> Self {
        let len = ns.len().min(UnitStage::COUNT);
        let mut split = Self::new();
        split.ns[..len].copy_from_slice(&ns[..len]);
        split.len = len;
        split
    }

    /// A split holding every stage's delta.
    #[must_use]
    pub const fn from_array(ns: [u64; UnitStage::COUNT]) -> Self {
        Self {
            ns,
            len: UnitStage::COUNT,
        }
    }

    /// Close the stage now running into the split.
    ///
    /// ## Parameters
    ///
    /// - `ns`: the stage's wall time.
    ///
    /// The split's own length names the stage (`UnitStage::ALL[self.len()]`),
    /// so a unit's recorded order cannot drift from [`UnitStage::ALL`]. A split
    /// already holding every stage ignores the delta and returns `false`: there
    /// is no sixth column, and the profile records nothing for it.
    #[must_use]
    pub fn lap(&mut self, ns: u64) -> bool {
        if self.len == UnitStage::COUNT {
            return false;
        }
        self.ns[self.len] = ns;
        self.len += 1;
        true
    }

    /// The stages the unit ran, one delta each, in [`UnitStage::ALL`] order.
    #[must_use]
    pub fn as_slice(&self) -> &[u64] {
        &self.ns[..self.len]
    }

    /// How many stages the unit ran.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the unit answered before running a stage.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The stage that answered the unit — the last one it ran.
    #[must_use]
    pub fn answering_stage(&self) -> Option<UnitStage> {
        let last = self.len.checked_sub(1)?;
        UnitStage::ALL.get(last).copied()
    }

    /// One stage's delta; `0` when the unit never reached it.
    #[must_use]
    pub const fn stage_ns(&self, stage: UnitStage) -> u64 {
        self.ns[stage.index()]
    }

    /// Total wall time across the stages the unit ran.
    #[must_use]
    pub fn total_ns(&self) -> u64 {
        self.as_slice().iter().sum()
    }
}

/// One candidate's terminal record.
///
/// The receipt is the unit's whole result: identity, the one terminal outcome,
/// and the answering stage's timing. The driver folds each receipt once
/// (`PipelineReport::absorb` + `StageProfile::absorb`) and renders the summary;
/// nothing else observes a unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrawlReceipt {
    /// FIFO submission order, which is discovery order — the progress anchors'
    /// spine.
    pub seq: u64,
    /// The hop signature (engine pool id + direction per hop) the ledger's
    /// memos key on. `None` when the unit refused before a signature existed
    /// (an unknown pool type, a path whose directions the resolver could not
    /// agree).
    pub hop_sig: Option<HopSignature>,
    /// The one terminal outcome the unit answered.
    pub outcome: RegistrationUnitOutcome,
    /// The per-stage wall time, front-truncated at the answering stage.
    pub split: StageSplit,
    /// The engine path id, `Some` only for a created registration.
    pub path_id: Option<u64>,
}

impl CrawlReceipt {
    /// The stage that answered the unit, if it ran one.
    #[must_use]
    pub fn answering_stage(&self) -> Option<UnitStage> {
        self.split.answering_stage()
    }

    /// The unit kind, spelled as the core's exported vocabulary
    /// ([`RegistrationUnitOutcome::KINDS`]) — the labels the Python driver
    /// mints its own kind enum from.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        RegistrationUnitOutcome::KINDS[self.kind_index()]
    }

    /// The position of the unit's kind in [`RegistrationUnitOutcome::KINDS`].
    const fn kind_index(&self) -> usize {
        match self.outcome {
            RegistrationUnitOutcome::Skip { .. } => 0,
            RegistrationUnitOutcome::Reject { .. } => 1,
            RegistrationUnitOutcome::Cap => 2,
            RegistrationUnitOutcome::RegisterFailed { .. } => 3,
            RegistrationUnitOutcome::Registered { .. } => 4,
        }
    }

    /// The stable refusal tag the summary's breakdown records, if the outcome
    /// records one: the skip's label, the cap stop's `path-cap`, or the
    /// register failure's `register-fail`.
    ///
    /// A rejection and a completed registration record no breakdown entry, so
    /// they answer `None` (the duplicate case enters the breakdown through the
    /// core's fold, not through the outcome's own tag).
    #[must_use]
    pub fn refusal_tag(&self) -> Option<&str> {
        match &self.outcome {
            RegistrationUnitOutcome::Skip { label, .. } => Some(label.as_label()),
            RegistrationUnitOutcome::Cap => Some(RegistrationOutcome::PathCap.as_str()),
            RegistrationUnitOutcome::RegisterFailed { .. } => {
                Some(RegistrationOutcome::RegisterFailed.as_str())
            }
            RegistrationUnitOutcome::Reject { .. } | RegistrationUnitOutcome::Registered { .. } => {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bot_core::registration_ledger::OutcomeLabel;

    use super::*;

    fn receipt(outcome: RegistrationUnitOutcome) -> CrawlReceipt {
        CrawlReceipt {
            seq: 0,
            hop_sig: None,
            outcome,
            split: StageSplit::new(),
            path_id: None,
        }
    }

    fn refusal_tag(outcome: RegistrationUnitOutcome) -> Option<String> {
        receipt(outcome).refusal_tag().map(str::to_owned)
    }

    #[test]
    fn stage_split_front_truncates_at_the_answering_stage() {
        let mut split = StageSplit::new();
        assert_eq!(split.answering_stage(), None);
        assert!(split.lap(7));
        assert_eq!(split.answering_stage(), Some(UnitStage::Memo));
        assert!(split.lap(11));
        assert_eq!(split.answering_stage(), Some(UnitStage::Build));
        assert_eq!(split.as_slice(), &[7, 11]);
        assert_eq!(split.total_ns(), 18);
        assert_eq!(split.stage_ns(UnitStage::Memo), 7);
        assert_eq!(
            split.stage_ns(UnitStage::Verify),
            0,
            "a stage never reached"
        );
    }

    #[test]
    fn a_split_never_grows_past_the_stage_columns() {
        let mut split = StageSplit::new();
        for _ in 0..UnitStage::COUNT {
            assert!(split.lap(1));
        }
        assert!(!split.lap(1), "an extra lap has no column to land in");
        assert_eq!(split.len(), UnitStage::COUNT);
        assert_eq!(split.answering_stage(), Some(UnitStage::Register));
    }

    #[test]
    fn a_caller_held_array_clamps_to_the_stage_columns() {
        let split = StageSplit::truncated(&[1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(split.as_slice(), &[1, 2, 3, 4, 5]);
        assert_eq!(StageSplit::from_array([1, 2, 3, 4, 5]), split);
    }

    #[test]
    fn kinds_are_the_core_vocabulary() {
        let cases: Vec<(RegistrationUnitOutcome, &str)> = vec![
            (
                RegistrationUnitOutcome::Skip {
                    label: OutcomeLabel::FreeForm("build-v3:ConnectionError".to_owned()),
                    counts_as_skip: true,
                    detail: None,
                },
                "skip",
            ),
            (RegistrationUnitOutcome::Reject { detail: None }, "reject"),
            (RegistrationUnitOutcome::Cap, "cap"),
            (
                RegistrationUnitOutcome::RegisterFailed { detail: None },
                "register-fail",
            ),
            (
                RegistrationUnitOutcome::Registered {
                    created: true,
                    v4_hops: 0,
                },
                "registered",
            ),
        ];
        for (outcome, kind) in cases {
            let receipt = receipt(outcome);
            assert_eq!(receipt.kind(), kind);
            assert!(
                RegistrationUnitOutcome::KINDS.contains(&receipt.kind()),
                "{kind} is not in the core's exported kind list"
            );
        }
        assert_eq!(
            RegistrationUnitOutcome::KINDS,
            ["skip", "reject", "cap", "register-fail", "registered"],
            "the kind order the Python enum is minted from"
        );
    }

    #[test]
    fn refusal_tags_are_the_labels_the_breakdown_records() {
        let outcome = refusal_tag;
        assert_eq!(
            outcome(RegistrationUnitOutcome::Skip {
                label: OutcomeLabel::Vocabulary(RegistrationOutcome::BuildV3Refused),
                counts_as_skip: true,
                detail: None,
            }),
            Some("build-v3-refused".to_owned())
        );
        assert_eq!(
            outcome(RegistrationUnitOutcome::Skip {
                label: OutcomeLabel::FreeForm("build-v3:ConnectionError".to_owned()),
                counts_as_skip: false,
                detail: None,
            }),
            Some("build-v3:ConnectionError".to_owned()),
            "a free-form driver tag rides the receipt verbatim"
        );
        assert_eq!(
            outcome(RegistrationUnitOutcome::Cap),
            Some("path-cap".to_owned())
        );
        assert_eq!(
            outcome(RegistrationUnitOutcome::RegisterFailed {
                detail: Some("ValueError: x".to_owned()),
            }),
            Some("register-fail".to_owned())
        );
        assert_eq!(
            outcome(RegistrationUnitOutcome::Reject { detail: None }),
            None
        );
        assert_eq!(
            outcome(RegistrationUnitOutcome::Registered {
                created: true,
                v4_hops: 1,
            }),
            None
        );
    }
}
