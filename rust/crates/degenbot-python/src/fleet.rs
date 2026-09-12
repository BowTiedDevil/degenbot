//! The fleet operator seam (JCI2FW Part B): the runtime re-tune channel
//! over the ONE process-level posture owner
//! ([`degenbot_workers::posture::process`]).
//!
//! Two pyfunctions on the real Python submodule `degenbot._ffi.fleet`:
//!
//! - [`set_posture_policy`] — the partial-patch write: a `dict` over the six
//!   typed cordon-threshold keys (the degenbot-config `fleet` section; the
//!   `DEGENBOT_FLEET_CORDON_ENTER_EVENTS` env family), at least one
//!   required. The patch reaches
//!   the core as a [`degenbot_workers::posture::PosturePolicyPatch`], whose
//!   `validate()` is the ONE encoding of the semantic rules (windows > 0 ms,
//!   duty percent in the sane (0.0, 100.0] range, `enter_events >= 1`,
//!   floor >= 1 when set) — REJECT with the typed
//!   [`PostureRetuneError`], never clamp silently. The patched policy =
//!   the owner's CURRENT policy + supplied fields, applied through
//!   `PostureOwner::retune` (the atomic swap that keeps posture state and
//!   the trailing sample window). The effective policy (all six fields +
//!   the current `Nominal|Cordoned` posture) is echoed back as a dict.
//! - [`current_posture_policy`] — the read: the same effective-policy dict
//!   without a write (the `get_fleet_posture` op / `fleet posture show` CLI).
//!
//! Every successful change emits ONE loud `tracing::warn!` line listing
//! old -> new per changed key — an operator-visible POLICY decision on the
//! log surface, not an exception (never `record_exception_keyed`).
//!
//! Boot config stays the default source: `FleetBoot::from_config`
//! installed the process owner from the typed keys; this seam only
//! re-tunes the LIVE policy on top of it.
//!
//! The Python mirror home is `degenbot.fleet` (the ADR-013 Pydantic
//! barrier: the first Python consumer mints the home; no leaf imports
//! `degenbot._ffi` directly).

use pyo3::create_exception;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyModule};

use degenbot_workers::posture::{FleetPosture, PosturePolicy, PosturePolicyPatch};

// The six typed threshold key names — the exact degenbot-config `fleet`
// vocabulary the wire speaks. One constant per key so the wire dict keys,
// the patch mapping, and the echo dict share ONE spelling.
const CORDON_ENTER_EVENTS: &str = "cordon_enter_events";
const CORDON_ENTER_WINDOW_MS: &str = "cordon_enter_window_ms";
const CORDON_DUTY_PERCENT: &str = "cordon_duty_percent";
const CORDON_DUTY_WINDOW_MS: &str = "cordon_duty_window_ms";
const CORDON_EXIT_CLEAN_MS: &str = "cordon_exit_clean_ms";
const CORDON_SIM_INTAKE_FLOOR: &str = "cordon_sim_intake_floor";

/// The posture key of the effective-policy dict (not a threshold — never
/// accepted in a patch).
const POSTURE: &str = "posture";

// The typed channel error: subclasses `ValueError` so broad handlers keep
// working, but callers classify by type (the engine seam's
// `PoolRegistrationError` pattern). Raised for unknown keys, wrong value
// types, empty patches, and out-of-range thresholds alike.
create_exception!(
    degenbot._ffi.fleet,
    PostureRetuneError,
    PyValueError,
    "The fleet posture re-tune channel refused the patch (unknown key, non-dict patch, empty patch, or a threshold outside its typed range)."
);

/// The wire-shape refusal for one patch entry.
fn refused(key: &str, detail: &str) -> PyErr {
    PostureRetuneError::new_err(format!("{key}: {detail}"))
}

/// A Python type name for a rejection message (never `unwrap`s — a broken
/// type name degrades to `?`, the refusal text still lands).
fn type_name_of(value: &Bound<'_, PyAny>) -> String {
    value
        .get_type()
        .name()
        .map_or_else(|_| "?".to_string(), |name| name.to_string())
}

/// Reject `bool` before any numeric extraction: `True` is an `int` in
/// Python, and a silent `True -> 1` coercion would smuggle a threshold
/// past the typed wire (REJECT, never coerce).
fn reject_bool(key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
    if value.cast::<PyBool>().is_ok() {
        return Err(refused(
            key,
            &format!("expected a number, got bool ({})", type_name_of(value)),
        ));
    }
    Ok(())
}

/// Extract one `usize` threshold (an int on the wire; bools and
/// non-ints are refused, not coerced).
fn threshold_usize(key: &str, value: &Bound<'_, PyAny>) -> PyResult<usize> {
    reject_bool(key, value)?;
    value.extract().map_err(|_| {
        refused(
            key,
            &format!("expected an int, got {}", type_name_of(value)),
        )
    })
}

/// Extract one `f64` threshold (int or float on the wire; bools and
/// non-numbers are refused, not coerced).
fn threshold_f64(key: &str, value: &Bound<'_, PyAny>) -> PyResult<f64> {
    reject_bool(key, value)?;
    value.extract().map_err(|_| {
        refused(
            key,
            &format!("expected a number, got {}", type_name_of(value)),
        )
    })
}

/// Map the wire patch dict onto the typed [`PosturePolicyPatch`]. Unknown
/// keys and wrong value types are refused HERE (the FFI boundary), so a
/// caller can never smuggle a second threshold vocabulary past the channel.
fn patch_from_dict(patch: &Bound<'_, PyDict>) -> PyResult<PosturePolicyPatch> {
    let mut out = PosturePolicyPatch::default();
    for (key, value) in patch.iter() {
        let key: String = key.extract().map_err(|_| {
            refused(
                "patch",
                &format!("keys must be strings, got {}", type_name_of(&key)),
            )
        })?;
        match key.as_str() {
            CORDON_ENTER_EVENTS => {
                out.enter_events = Some(threshold_usize(&key, &value)?);
            }
            CORDON_ENTER_WINDOW_MS => {
                out.enter_window_ms = Some(
                    threshold_usize(&key, &value)?
                        .try_into()
                        .map_err(|_| refused(&key, "window ms overflowed the typed u64 range"))?,
                );
            }
            CORDON_DUTY_PERCENT => {
                out.duty_percent = Some(threshold_f64(&key, &value)?);
            }
            CORDON_DUTY_WINDOW_MS => {
                out.duty_window_ms = Some(
                    threshold_usize(&key, &value)?
                        .try_into()
                        .map_err(|_| refused(&key, "window ms overflowed the typed u64 range"))?,
                );
            }
            CORDON_EXIT_CLEAN_MS => {
                out.exit_clean_ms = Some(
                    threshold_usize(&key, &value)?
                        .try_into()
                        .map_err(|_| refused(&key, "window ms overflowed the typed u64 range"))?,
                );
            }
            CORDON_SIM_INTAKE_FLOOR => {
                // The key's typed type is `opt usize`: an explicit `None`
                // value is the operator CLEARING the override (back to half
                // the slot cap); an int sets it; absence (key not in the
                // dict) leaves it untouched.
                if value.is_none() {
                    out.sim_intake_floor_override = Some(None);
                } else {
                    out.sim_intake_floor_override = Some(Some(threshold_usize(&key, &value)?));
                }
            }
            POSTURE => {
                return Err(refused(
                    &key,
                    "is read-only (the current posture is echoed, never patched)",
                ));
            }
            _ => {
                return Err(refused(
                    &key,
                    "unknown fleet-posture threshold key (expected one of the six cordon_* keys)",
                ));
            }
        }
    }
    Ok(out)
}

/// `"3"` / `"none"` spelling of the sim-intake floor for the loud line.
fn floor_display(floor: Option<usize>) -> String {
    match floor {
        Some(n) => n.to_string(),
        None => "none".to_string(),
    }
}

/// The loud line's `old -> new` fragment: one `key: old -> new` entry per
/// CHANGED threshold, wire-key spelling.
fn policy_changes(old: &PosturePolicy, new: &PosturePolicy) -> String {
    let mut changed: Vec<String> = Vec::new();
    if old.enter_events != new.enter_events {
        changed.push(format!(
            "{CORDON_ENTER_EVENTS}: {} -> {}",
            old.enter_events, new.enter_events
        ));
    }
    if old.enter_window_ms != new.enter_window_ms {
        changed.push(format!(
            "{CORDON_ENTER_WINDOW_MS}: {} -> {}",
            old.enter_window_ms, new.enter_window_ms
        ));
    }
    #[expect(
        clippy::float_cmp,
        reason = "change DETECTOR over operator-supplied values, not computed floats: the                   patched value lands verbatim or it did not land"
    )]
    if old.duty_percent != new.duty_percent {
        changed.push(format!(
            "{CORDON_DUTY_PERCENT}: {} -> {}",
            old.duty_percent, new.duty_percent
        ));
    }
    if old.duty_window_ms != new.duty_window_ms {
        changed.push(format!(
            "{CORDON_DUTY_WINDOW_MS}: {} -> {}",
            old.duty_window_ms, new.duty_window_ms
        ));
    }
    if old.exit_clean_ms != new.exit_clean_ms {
        changed.push(format!(
            "{CORDON_EXIT_CLEAN_MS}: {} -> {}",
            old.exit_clean_ms, new.exit_clean_ms
        ));
    }
    if old.sim_intake_floor_override != new.sim_intake_floor_override {
        changed.push(format!(
            "{CORDON_SIM_INTAKE_FLOOR}: {} -> {}",
            floor_display(old.sim_intake_floor_override),
            floor_display(new.sim_intake_floor_override)
        ));
    }
    changed.join(", ")
}

/// The posture enum's wire spelling.
#[must_use]
pub const fn posture_name(posture: FleetPosture) -> &'static str {
    match posture {
        FleetPosture::Nominal => "Nominal",
        FleetPosture::Cordoned => "Cordoned",
    }
}

/// The effective-policy dict: all six typed fields (wire-key spelling) +
/// the current posture. The ONE echo shape both verbs return.
fn effective_policy_dict(
    py: Python<'_>,
    policy: PosturePolicy,
    posture: FleetPosture,
) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item(CORDON_ENTER_EVENTS, policy.enter_events)?;
    dict.set_item(CORDON_ENTER_WINDOW_MS, policy.enter_window_ms)?;
    dict.set_item(CORDON_DUTY_PERCENT, policy.duty_percent)?;
    dict.set_item(CORDON_DUTY_WINDOW_MS, policy.duty_window_ms)?;
    dict.set_item(CORDON_EXIT_CLEAN_MS, policy.exit_clean_ms)?;
    dict.set_item(CORDON_SIM_INTAKE_FLOOR, policy.sim_intake_floor_override)?;
    dict.set_item(POSTURE, posture_name(posture))?;
    Ok(dict.into())
}

/// `degenbot._ffi.fleet.set_posture_policy(patch: dict) -> dict`
///
/// Apply the operator's partial patch to the LIVE process posture policy
/// and echo the effective policy. See the module docs for the full
/// contract.
///
/// # Errors
///
/// [`PostureRetuneError`] on an unknown key, a non-dict patch, an empty
/// patch, a wrong-typed value, or an out-of-range threshold (all REJECT —
/// the live policy is untouched when any of these fire).
#[pyfunction]
pub fn set_posture_policy(patch: &Bound<'_, PyDict>) -> PyResult<Py<PyDict>> {
    let py = patch.py();
    let patch = patch_from_dict(patch)?;
    patch
        .validate()
        .map_err(|err| PostureRetuneError::new_err(err.to_string()))?;

    let owner = degenbot_workers::posture::process();
    let current = owner.policy();
    let effective = current.patched_with(patch);
    owner.retune(effective);

    // Loud, operator-visible policy decision — ONE warn line listing
    // old -> new per changed key. A retune that changes nothing (an
    // identical value) is silent: the policy did not change.
    let changed = policy_changes(&current, &effective);
    if !changed.is_empty() {
        tracing::warn!(
            target: "degenbot::fleet",
            changed = %changed,
            "[fleet-posture] operator retune — cordon thresholds changed and are LIVE"
        );
        // TB4QGX T9 (retune wake gap): a live retune can change the
        // admission guard with no message in flight. Wake every parked host
        // so the fleet re-reads the owner now, not at the backstop bound.
        degenbot_bot::arb_engine::fleet_wake::wake_hosts();
    }

    effective_policy_dict(py, effective, owner.current())
}

/// `degenbot._ffi.fleet.current_posture_policy() -> dict`
///
/// The read side of the channel: the effective policy (all six fields) +
/// the current posture, with no write.
///
/// # Errors
///
/// Only on dict construction failure (never refuses a read).
#[pyfunction]
pub fn current_posture_policy(py: Python<'_>) -> PyResult<Py<PyDict>> {
    let owner = degenbot_workers::posture::process();
    effective_policy_dict(py, owner.policy(), owner.current())
}

/// Register the fleet seam on the real Python submodule
/// `degenbot._ffi.fleet`.
///
/// # Errors
///
/// Returns `PyErr` if the submodule, the typed exception, or a function
/// fails to register on the module.
pub fn add_fleet_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    let submod = PyModule::new(py, "degenbot._ffi.fleet")?;
    submod.add("PostureRetuneError", py.get_type::<PostureRetuneError>())?;
    submod.add_function(wrap_pyfunction!(set_posture_policy, &submod)?)?;
    submod.add_function(wrap_pyfunction!(current_posture_policy, &submod)?)?;
    m.add_submodule(&submod)?;
    py.import("sys")?
        .getattr("modules")?
        .set_item("degenbot._ffi.fleet", &submod)?;
    Ok(())
}
