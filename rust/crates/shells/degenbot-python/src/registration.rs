//! `PyO3` projection of the core registration outcome ledger (S12).
//!
//! Thin translation only. The vocabulary (`RegistrationOutcome` and its
//! bounded tags), the typed build-refusal classification, and the four memos
//! live in `degenbot_bot::bot_core::registration_ledger`; this module converts
//! Python strings, tuples, and exception-derived kinds into them and converts
//! the answers back. Nothing here decides an outcome — a Python caller that
//! cannot name a failure kind gets a `ValueError` rather than a guessed tag.
//!
//! The tag list is exported rather than re-declared, so the Python side builds
//! its label enum FROM the core vocabulary and the two cannot drift.

use crate::prelude::*;

use degenbot_bot::bot_core::registration_ledger::{
    BuildFailure, BuildRefusal, HopSignature, MemoIdentity, OutcomeLabel, PathAdmission,
    PipelineReport, PolicyVerdict, RegistrationLedger, RegistrationOutcome,
    RegistrationUnitOutcome,
};
use degenbot_pathfinding::PoolKind;

/// A memoized stable refusal of one pool, as the Python pipeline reads it.
#[pyclass(name = "UnregistrablePoolRecord", module = "degenbot._ffi")]
pub struct PyUnregistrablePoolRecord {
    outcome: String,
    counts_as_skip: bool,
}

/// The typed memo key behind a Python-visible key string.
///
/// The core keys its memos on `MemoIdentity`; Python carries the `Display`
/// form (`p:0x…` / `v4id:0x…`). Anything else is a caller bug, so it fails
/// loudly instead of silently missing the memo.
fn memo_identity(key: &str) -> PyResult<MemoIdentity> {
    MemoIdentity::parse(key).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "not a pool memo key: {key:?} (use the value from registration_pool_memo_key)"
        ))
    })
}

#[pymethods]
impl PyUnregistrablePoolRecord {
    /// The bounded outcome tag.
    #[getter]
    fn outcome(&self) -> &str {
        &self.outcome
    }

    /// Whether the refusal adds to the generic skip counter.
    #[getter]
    fn counts_as_skip(&self) -> bool {
        self.counts_as_skip
    }
}

/// One typed build-refusal classification, as the Python pipeline reads it.
#[pyclass(name = "BuildRefusalView", module = "degenbot._ffi")]
pub struct PyBuildRefusal {
    outcome: String,
    stable: bool,
    counts_as_skip: bool,
    detail: Option<String>,
}

#[pymethods]
impl PyBuildRefusal {
    /// The bounded outcome tag.
    #[getter]
    fn outcome(&self) -> &str {
        &self.outcome
    }

    /// Whether the refusal is a pool fact (memoizable) rather than transient.
    #[getter]
    fn stable(&self) -> bool {
        self.stable
    }

    /// Whether the refusal adds to the generic skip counter.
    #[getter]
    fn counts_as_skip(&self) -> bool {
        self.counts_as_skip
    }

    /// The failure text (log-only; never a label).
    #[getter]
    fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// One eligibility-gate answer, as the Python registration pipeline reads it.
///
/// The gate's mechanical decisions — direction agreement, the rejected-path
/// memo, and the registered-path dup fast-path — are the core's; the Python
/// pipeline renders the verdict it is handed.
#[pyclass(name = "PathAdmission", module = "degenbot._ffi")]
pub struct PyPathAdmission {
    kind: String,
    hop_sig: HopSignature,
}

#[pymethods]
impl PyPathAdmission {
    /// The bounded verdict tag (one of `registration_admission_kinds()`).
    #[getter]
    fn kind(&self) -> &str {
        &self.kind
    }

    /// The hop signature `(pool_id, zero_for_one)` per hop — the signature
    /// the memos key on. Empty unless the verdict is `admitted`.
    #[getter]
    fn hop_sig(&self) -> HopSignature {
        self.hop_sig.clone()
    }
}

/// The core registration ledger: the four memos, owned by the core.
#[pyclass(name = "RegistrationLedger", module = "degenbot._ffi")]
pub struct PyRegistrationLedger {
    ledger: std::sync::Mutex<RegistrationLedger>,
}

#[pymethods]
impl PyRegistrationLedger {
    /// A ledger with empty memos.
    #[new]
    fn new() -> Self {
        Self {
            ledger: std::sync::Mutex::new(RegistrationLedger::new()),
        }
    }

    /// Whether this exact hop signature already completed registration.
    #[pyo3(signature = (hop_signature))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "pyo3 extracts the signature by value; there is no borrowed form"
    )]
    fn path_registered(&self, py: Python<'_>, hop_signature: HopSignature) -> bool {
        py.detach(|| self.lock().path_registered(hop_signature.as_slice()))
    }

    /// Record a completed registration (engine-created or engine-dedup'd).
    #[pyo3(signature = (hop_signature))]
    fn memoize_registered_path(&self, py: Python<'_>, hop_signature: HopSignature) {
        py.detach(|| self.lock().memoize_registered_path(hop_signature));
    }

    /// Whether this pool's verify lifecycle already completed.
    #[pyo3(signature = (key))]
    fn pool_verified(&self, py: Python<'_>, key: &str) -> PyResult<bool> {
        let key = memo_identity(key)?;
        Ok(py.detach(|| self.lock().pool_verified(&key)))
    }

    /// Record a COMPLETED verify lifecycle (a pool fact).
    #[pyo3(signature = (key))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "pyo3 extracts the key by value; there is no borrowed form"
    )]
    fn memoize_verified_pool(&self, py: Python<'_>, key: String) -> PyResult<()> {
        let key = memo_identity(&key)?;
        py.detach(|| self.lock().memoize_verified_pool(key));
        Ok(())
    }

    /// The memoized stable refusal for a pool key, or `None`.
    #[pyo3(signature = (key))]
    fn unregistrable_record(
        &self,
        py: Python<'_>,
        key: Option<&str>,
    ) -> PyResult<Option<PyUnregistrablePoolRecord>> {
        let key = key.map(memo_identity).transpose()?;
        Ok(py.detach(|| {
            self.lock()
                .unregistrable_record(key.as_ref())
                .map(|record| PyUnregistrablePoolRecord {
                    outcome: record.outcome.as_str().to_owned(),
                    counts_as_skip: record.counts_as_skip,
                })
        }))
    }

    /// Record a STABLE build refusal under its bounded tag.
    #[pyo3(signature = (key, outcome, counts_as_skip))]
    fn memoize_unregistrable(
        &self,
        py: Python<'_>,
        key: Option<&str>,
        outcome: &str,
        counts_as_skip: bool,
    ) -> PyResult<()> {
        let outcome = to_outcome(outcome)?;
        let key = key.map(memo_identity).transpose()?;
        py.detach(|| {
            self.lock()
                .memoize_unregistrable(key, outcome, counts_as_skip);
        });
        Ok(())
    }

    /// Whether this hop signature already hit a deterministic deny.
    #[pyo3(signature = (hop_signature))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "pyo3 extracts the signature by value; there is no borrowed form"
    )]
    fn path_rejected(&self, py: Python<'_>, hop_signature: HopSignature) -> bool {
        py.detach(|| self.lock().path_rejected(hop_signature.as_slice()))
    }

    /// Record a deterministic policy/predicate deny for a hop signature.
    #[pyo3(signature = (hop_signature))]
    fn memoize_rejected_path(&self, py: Python<'_>, hop_signature: HopSignature) {
        py.detach(|| self.lock().memoize_rejected_path(hop_signature));
    }

    /// The eligibility gate: direction agreement, the rejected-path memo,
    /// the submitted policy verdict, and the registered-path dup fast-path,
    /// in one ordered pass. `policy` is the strategy pre-filter's verdict,
    /// evaluated by the caller before this ask and submitted as data.
    #[pyo3(signature = (hop_pool_ids, pinned_directions, resolved_directions, policy))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "pyo3 extracts these by value and the core gate takes slices, so there is no consumed form"
    )]
    fn admit_path_with_policy(
        &self,
        py: Python<'_>,
        hop_pool_ids: Vec<u64>,
        pinned_directions: Option<Vec<bool>>,
        resolved_directions: Option<Vec<bool>>,
        policy: &str,
    ) -> PyResult<PyPathAdmission> {
        let policy = to_policy_verdict(policy)?;
        Ok(py.detach(|| {
            to_admission(self.lock().admit_path_with_policy(
                &hop_pool_ids,
                pinned_directions.as_deref(),
                resolved_directions.as_deref(),
                policy,
            ))
        }))
    }
}

/// The Python view of one [`PathAdmission`] verdict.
fn to_admission(admission: PathAdmission) -> PyPathAdmission {
    let kind = admission.as_str().to_owned();
    let hop_sig = match admission {
        PathAdmission::Admitted { hop_sig } => hop_sig,
        PathAdmission::DirectionMismatch
        | PathAdmission::RejectedPathMemo
        | PathAdmission::PolicyDenied
        | PathAdmission::AlreadyRegistered => Vec::new(),
    };
    PyPathAdmission { kind, hop_sig }
}

/// The strategy pre-filter verdict a driver submits with its gate ask.
///
/// # Errors
///
/// `ValueError` for a verdict outside the closed set: wire drift, never a
/// guessed branch.
fn to_policy_verdict(policy: &str) -> PyResult<PolicyVerdict> {
    match policy {
        "admitted" => Ok(PolicyVerdict::Admitted),
        "denied" => Ok(PolicyVerdict::Denied),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown policy verdict {other:?}: expected admitted or denied"
        ))),
    }
}

impl PyRegistrationLedger {
    fn lock(&self) -> std::sync::MutexGuard<'_, RegistrationLedger> {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The bounded outcome tag list — the vocabulary a Python consumer builds its
/// label enum from, so the tag set has exactly one owner.
#[must_use]
#[pyfunction]
#[pyo3(name = "registration_outcome_tags")]
pub fn registration_outcome_tags() -> Vec<&'static str> {
    RegistrationOutcome::tags()
}

/// The hop identity the negative memos key on, or `None` when the hop carries
/// no identity.
///
/// # Errors
///
/// `ValueError` for an unrecognized `pool_type`: wire drift, never a guessed
/// family.
#[pyfunction]
#[pyo3(name = "registration_pool_memo_key", signature = (pool_type, address, pool_hash))]
pub fn registration_pool_memo_key(
    pool_type: &str,
    address: Option<&str>,
    pool_hash: Option<&str>,
) -> PyResult<Option<String>> {
    let pool_kind = to_pool_kind(pool_type)?;
    Ok(RegistrationLedger::pool_memo_key(pool_kind, address, pool_hash).map(|key| key.to_string()))
}

/// Classify a hop-build failure. `failure_kind` is the TYPED refusal a Python
/// caller matched (`"hooked-pool"`, `"dynamic-fee"`, `"high-fee"`) or
/// `"transient"` for everything else; `detail` rides the record for logging and
/// never becomes a label.
///
/// # Errors
///
/// `ValueError` for an unknown `failure_kind` or an unrecognized `pool_type`:
/// a caller that cannot name a failure or a family gets no tag rather than a
/// guessed one.
#[pyfunction]
#[pyo3(name = "classify_build_refusal", signature = (failure_kind, pool_type, detail))]
pub fn classify_build_refusal(
    failure_kind: &str,
    pool_type: &str,
    detail: Option<String>,
) -> PyResult<PyBuildRefusal> {
    let failure = match failure_kind {
        "hooked-pool" => BuildFailure::HookedPool,
        "dynamic-fee" => BuildFailure::DynamicFee,
        "high-fee" => BuildFailure::HighFee,
        "transient" => BuildFailure::Transient(detail.clone().unwrap_or_default()),
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown build-failure kind {other:?}: expected hooked-pool, dynamic-fee, high-fee, or transient"
            )));
        }
    };
    let pool_kind = to_pool_kind(pool_type)?;
    Ok(to_view(RegistrationLedger::classify_build_refusal(
        &failure, pool_kind, detail,
    )))
}

fn to_view(refusal: BuildRefusal) -> PyBuildRefusal {
    PyBuildRefusal {
        outcome: refusal.outcome.as_str().to_owned(),
        stable: refusal.stable,
        counts_as_skip: refusal.counts_as_skip,
        detail: refusal.detail,
    }
}

/// The delta one unit fold applies to a driver's summary counters — the
/// core's [`PipelineReport`] after exactly one `absorb`. The counter
/// ARITHMETIC is the core's (one definition in
/// `degenbot_bot::bot_core::registration_ledger`); a driver that keeps its
/// own counter storage applies this answer mechanically, field for field.
#[pyclass(name = "RegistrationFoldDelta", module = "degenbot._ffi")]
pub struct PyRegistrationFoldDelta {
    report: PipelineReport,
}

#[pymethods]
impl PyRegistrationFoldDelta {
    /// New paths registered by this fold.
    #[getter]
    fn path_count(&self) -> usize {
        self.report.path_count
    }

    /// Skips added by this fold.
    #[getter]
    fn skip_count(&self) -> usize {
        self.report.skip_count
    }

    /// Benign post-cap skips added by this fold.
    #[getter]
    fn cap_skip_count(&self) -> usize {
        self.report.cap_skip_count
    }

    /// Engine rejections added by this fold.
    #[getter]
    fn engine_reject_count(&self) -> usize {
        self.report.engine_reject_count
    }

    /// Duplicates added by this fold.
    #[getter]
    fn dup_count(&self) -> usize {
        self.report.dup_count
    }

    /// Register failures added by this fold.
    #[getter]
    fn register_fail_count(&self) -> usize {
        self.report.register_fail_count
    }

    /// V4 hops witnessed by this fold (`Registered` outcomes only).
    #[getter]
    fn v4_pool_count(&self) -> usize {
        self.report.v4_pool_count
    }

    /// V4 hook rejections added by this fold.
    #[getter]
    fn v4_hook_rejected(&self) -> usize {
        self.report.v4_hook_rejected
    }

    /// V4 dynamic-fee rejections added by this fold.
    #[getter]
    fn v4_dynamic_fee_rejected(&self) -> usize {
        self.report.v4_dynamic_fee_rejected
    }

    /// Other counted exceptions added by this fold (folds with
    /// `engine_reject_count`).
    #[getter]
    fn other_exc_count(&self) -> usize {
        self.report.other_exc_count
    }

    /// Whether this fold latched the benign cap stop.
    #[getter]
    fn capped(&self) -> bool {
        self.report.capped
    }

    /// Units folded (always 1 — the one-fold-per-unit witness a driver
    /// accumulates against its own unit count).
    #[getter]
    fn units_folded(&self) -> usize {
        self.report.units_folded
    }

    /// Uncounted skips added by this fold (`counts_as_skip == false`).
    #[getter]
    fn uncounted_skip_count(&self) -> usize {
        self.report.uncounted_skip_count
    }

    /// The reason-label deltas as `(label, count)` pairs (a fold records at
    /// most one label).
    #[getter]
    fn skip_reasons(&self) -> Vec<(String, usize)> {
        self.report
            .skip_reasons
            .iter()
            .map(|(label, count)| (label.clone(), *count))
            .collect()
    }
}

/// The closed eligibility-verdict set — the vocabulary a Python consumer
/// builds its verdict labels from, so a verdict cannot drift between the core
/// and a driver.
#[must_use]
#[pyfunction]
#[pyo3(name = "registration_admission_kinds")]
pub fn registration_admission_kinds() -> Vec<&'static str> {
    PathAdmission::KINDS.to_vec()
}

/// The closed unit-kind set — the vocabulary a Python consumer builds its
/// kind labels from, so a kind cannot drift between the core and a driver.
#[must_use]
#[pyfunction]
#[pyo3(name = "registration_unit_kinds")]
pub fn registration_unit_kinds() -> Vec<&'static str> {
    RegistrationUnitOutcome::KINDS.to_vec()
}

/// Fold one unit outcome with the core's arithmetic and return the delta.
///
/// `kind` is one of `registration_unit_kinds()`; `tag` is the skip's reason
/// tag (a bounded `RegistrationOutcome` tag, or a free-form driver label
/// recorded verbatim — a skip without one is a caller bug); `detail` is the
/// log-only failure text.
///
/// # Errors
///
/// `ValueError` for a kind outside the closed set, or a skip with no tag:
/// wire drift is a loud construction failure, never a guessed outcome.
#[pyfunction]
#[pyo3(
    name = "fold_registration_unit",
    signature = (kind, tag, counts_as_skip, created, v4_hops, detail)
)]
pub fn fold_registration_unit(
    kind: &str,
    tag: Option<&str>,
    counts_as_skip: bool,
    created: bool,
    v4_hops: usize,
    detail: Option<String>,
) -> PyResult<PyRegistrationFoldDelta> {
    let outcome = to_unit_outcome(kind, tag, counts_as_skip, created, v4_hops, detail)?;
    let mut report = PipelineReport::default();
    report.absorb(&outcome);
    Ok(PyRegistrationFoldDelta { report })
}

/// The core unit outcome the driver's (kind, tag, ...) unit names.
///
/// # Errors
///
/// `ValueError` for an unknown kind or an untagged skip.
fn to_unit_outcome(
    kind: &str,
    tag: Option<&str>,
    counts_as_skip: bool,
    created: bool,
    v4_hops: usize,
    detail: Option<String>,
) -> PyResult<RegistrationUnitOutcome> {
    match kind {
        "skip" => {
            let tag = tag.ok_or_else(|| {
                pyo3::exceptions::PyValueError::new_err(
                    "a skip outcome names its refusal tag: expected a bounded                      registration-outcome tag or the driver's own label",
                )
            })?;
            Ok(RegistrationUnitOutcome::Skip {
                label: OutcomeLabel::from_driver_tag(tag),
                counts_as_skip,
                detail,
            })
        }
        "reject" => Ok(RegistrationUnitOutcome::Reject { detail }),
        "cap" => Ok(RegistrationUnitOutcome::Cap),
        "register-fail" => Ok(RegistrationUnitOutcome::RegisterFailed { detail }),
        "registered" => Ok(RegistrationUnitOutcome::Registered { created, v4_hops }),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown registration unit kind {other:?}: expected one of {:?}",
            RegistrationUnitOutcome::KINDS
        ))),
    }
}

/// The core outcome a bounded tag names, or a `ValueError` naming the closed
/// set — an unknown tag is a caller bug, never a silent default.
fn to_outcome(tag: &str) -> PyResult<RegistrationOutcome> {
    RegistrationOutcome::from_tag(tag).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "unknown registration outcome {tag:?}: expected one of {:?}",
            RegistrationOutcome::tags()
        ))
    })
}

/// The pool-family labels the registration seam spells — the wire vocabulary
/// the Python adapter sends, and the set an unknown label is judged against.
const POOL_FAMILY_LABELS: [&str; 3] = ["V2", "V3", "V4"];

/// The pool family a Python pool-type label names.
///
/// # Errors
///
/// `ValueError` for a label outside the closed set, naming the raw value and
/// the known set: an unrecognized family is Rust/Python wire drift and is
/// never classified (or memoized) under a family it does not have.
fn to_pool_kind(pool_type: &str) -> PyResult<PoolKind> {
    match pool_type {
        "V2" => Ok(PoolKind::V2),
        "V3" => Ok(PoolKind::V3),
        "V4" => Ok(PoolKind::V4),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown pool family {other:?}: expected one of {POOL_FAMILY_LABELS:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "tests assert on known-valid inputs")]
    #![expect(
        clippy::panic,
        reason = "negative-probe teeth: these panic! arms fail the test when a refusal wrongly folds"
    )]

    use super::*;

    #[test]
    fn known_family_labels_map_to_their_kind() {
        pyo3::Python::attach(|_py| {
            assert!(matches!(to_pool_kind("V2"), Ok(PoolKind::V2)));
            assert!(matches!(to_pool_kind("V3"), Ok(PoolKind::V3)));
            assert!(matches!(to_pool_kind("V4"), Ok(PoolKind::V4)));
        });
    }

    #[test]
    fn unknown_family_label_never_classifies_as_v3() {
        pyo3::Python::attach(|_py| {
            let unknown = format!("{:?}", to_pool_kind("sushiswap_v9"));
            let genuine_v3 = format!("{:?}", to_pool_kind("V3"));
            assert_ne!(
                unknown, genuine_v3,
                "an unrecognized family label classified identically to a genuine V3 hop"
            );
        });
    }

    #[test]
    fn unknown_family_label_errors_naming_the_raw_value_and_known_set() {
        pyo3::Python::attach(|_py| {
            for label in ["uniswap-v4", "v2", "V5", "sushiswap_v2", ""] {
                let message = to_pool_kind(label).unwrap_err().to_string();
                assert!(
                    message.contains(&format!("{label:?}")),
                    "raw value {label:?} missing from: {message}"
                );
                assert!(
                    message.contains("V2") && message.contains("V3") && message.contains("V4"),
                    "known set missing from: {message}"
                );
            }
        });
    }

    /// The fold's counter arithmetic over the FFI — the Python driver's
    /// delta path answers exactly what the core's own fold test pins
    /// (`registration_ledger::tests::fold_lands_every_outcome_in_its_counter_bucket`).
    #[test]
    fn fold_registration_unit_answers_the_core_delta_per_kind() {
        pyo3::Python::attach(|_py| {
            let skip =
                fold_registration_unit("skip", Some("v4-hook-rejected"), false, false, 0, None)
                    .unwrap();
            assert_eq!(skip.v4_hook_rejected(), 1);
            assert_eq!(skip.skip_count(), 0, "V4 admission refusals are not skips");
            assert_eq!(skip.uncounted_skip_count(), 1);
            assert_eq!(skip.skip_reasons(), vec![("v4-hook-rejected".into(), 1)]);

            let reject = fold_registration_unit("reject", None, true, false, 0, None).unwrap();
            assert_eq!(reject.engine_reject_count(), 1);
            assert_eq!(reject.other_exc_count(), 1);
            assert!(reject.skip_reasons().is_empty());

            let registered =
                fold_registration_unit("registered", None, true, true, 2, None).unwrap();
            assert_eq!(registered.path_count(), 1);
            assert_eq!(registered.v4_pool_count(), 2);
            assert_eq!(registered.units_folded(), 1);
            assert!(!registered.capped());

            let cap =
                fold_registration_unit("cap", Some("path-cap"), true, false, 0, None).unwrap();
            assert!(cap.capped());
            assert_eq!(cap.cap_skip_count(), 1);
            assert_eq!(cap.skip_count(), 1);

            let fail = fold_registration_unit(
                "register-fail",
                Some("register-fail"),
                true,
                false,
                0,
                Some("boom".into()),
            )
            .unwrap();
            assert_eq!(fail.register_fail_count(), 1);
            assert_eq!(fail.skip_reasons(), vec![("register-fail".into(), 1)]);
        });
    }

    /// An unknown kind or an untagged skip is a loud construction failure —
    /// the fold never guesses an outcome.
    #[test]
    fn fold_registration_unit_refuses_unknown_kinds_and_untagged_skips() {
        pyo3::Python::attach(|_py| {
            let unknown = match fold_registration_unit("dedup", None, true, false, 0, None) {
                Ok(_) => panic!("an unknown kind must not fold"),
                Err(e) => e.to_string(),
            };
            assert!(unknown.contains("dedup"), "{unknown}");
            assert!(unknown.contains("skip"), "{unknown}");

            let untagged = match fold_registration_unit("skip", None, true, false, 0, None) {
                Ok(_) => panic!("an untagged skip must not fold"),
                Err(e) => e.to_string(),
            };
            assert!(untagged.contains("refusal tag"), "{untagged}");
        });
    }

    /// The kind list is the closed set the Python adapter builds its kind
    /// labels from — pinned here and in the core.
    #[test]
    fn registration_unit_kinds_is_the_closed_set() {
        assert_eq!(
            registration_unit_kinds(),
            vec!["skip", "reject", "cap", "register-fail", "registered"]
        );
    }

    /// The eligibility verdicts are the closed set the Python adapter builds
    /// its verdict labels from — pinned here and in the core.
    #[test]
    fn registration_admission_kinds_is_the_closed_set() {
        assert_eq!(
            registration_admission_kinds(),
            vec![
                "direction-mismatch",
                "rejected-path-memo",
                "policy-denied",
                "admitted",
                "already-registered"
            ]
        );
    }

    /// The Python view carries the verdict tag and the plan, and nothing
    /// else: a refusal has no hop signature to hand back.
    #[test]
    fn admission_view_carries_the_verdict_and_the_plan() {
        pyo3::Python::attach(|_py| {
            let admitted = to_admission(PathAdmission::Admitted {
                hop_sig: vec![(7, true), (9, false)],
            });
            assert_eq!(admitted.kind(), "admitted");
            assert_eq!(admitted.hop_sig(), vec![(7, true), (9, false)]);

            for refusal in [
                PathAdmission::DirectionMismatch,
                PathAdmission::RejectedPathMemo,
                PathAdmission::PolicyDenied,
                PathAdmission::AlreadyRegistered,
            ] {
                let view = to_admission(refusal.clone());
                assert_eq!(view.kind(), refusal.as_str());
                assert!(view.hop_sig().is_empty(), "a refusal carries no plan");
            }
        });
    }
}
