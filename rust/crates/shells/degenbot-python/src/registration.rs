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
    BuildFailure, BuildRefusal, HopSignature, RegistrationLedger, RegistrationOutcome,
};
use degenbot_pathfinding::PoolKind;

/// A memoized stable refusal of one pool, as the Python pipeline reads it.
#[pyclass(name = "UnregistrablePoolRecord", module = "degenbot._ffi")]
pub struct PyUnregistrablePoolRecord {
    outcome: String,
    counts_as_skip: bool,
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
    fn pool_verified(&self, py: Python<'_>, key: &str) -> bool {
        py.detach(|| self.lock().pool_verified(key))
    }

    /// Record a COMPLETED verify lifecycle (a pool fact).
    #[pyo3(signature = (key))]
    fn memoize_verified_pool(&self, py: Python<'_>, key: String) {
        py.detach(|| self.lock().memoize_verified_pool(key));
    }

    /// The memoized stable refusal for a pool key, or `None`.
    #[pyo3(signature = (key))]
    fn unregistrable_record(
        &self,
        py: Python<'_>,
        key: Option<&str>,
    ) -> Option<PyUnregistrablePoolRecord> {
        py.detach(|| {
            self.lock()
                .unregistrable_record(key)
                .map(|record| PyUnregistrablePoolRecord {
                    outcome: record.outcome.as_str().to_owned(),
                    counts_as_skip: record.counts_as_skip,
                })
        })
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
    Ok(RegistrationLedger::pool_memo_key(
        pool_kind, address, pool_hash,
    ))
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
    #![expect(clippy::unwrap_used)]

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
}
