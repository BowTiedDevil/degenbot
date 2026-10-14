//! TEST-ONLY configuration seam.
//!
//! Production first-wins is unchanged: the module-init boot installs the
//! env+file-loaded `BotConfig` into `degenbot_config::holder` exactly once, and
//! every later construction observes that snapshot. This module is the
//! sanctioned TEST SEAM that lets the Python suite re-govern that snapshot, so
//! a DECLARED captured environment governs the next construction instead of
//! the process's first resolution. The suite never writes `DEGENBOT_*` into
//! the process environment: it declares its environment as data and installs
//! it through this seam (the installing sibling of the ADR-013 hypothetical
//! door).
//!
//! Convention: like [`crate::config::resolve_hypothetical`] (ADR-013), this is
//! a raw `_ffi` surface — importing the extension installs nothing, the
//! functions are deliberately NOT re-exported into the `degenbot` Python
//! package, and the only caller is the test harness (`tests/conftest.py`). No
//! production boot path reaches them.
//!
//! Safety contract (see also `degenbot_config::holder`): previously-constructed
//! engines keep their minted snapshots; [`reinstall_config_holder_for_tests`]
//! re-governs only future resolutions. Resetting while engines exist is
//! safe-by-design — every engine reads the holder at CONSTRUCTION
//! (`boot_host`, the engine construction pack step), and the process-lifetime
//! resources sized from the first snapshot (the shared io runtime, the
//! solve/io worker pools) are intentionally not re-sized.

use crate::prelude::*;

/// TEST-ONLY: load a CAPTURED environment (`env` + the operator file it names,
/// or the explicit `file`) and install that snapshot into the typed-config
/// holder (and the shell's published verdict).
///
/// The installing sibling of [`crate::config::resolve_hypothetical`]: pass the
/// complete captured environment as data and the NEXT resolution observes it,
/// while engines already built keep their minted snapshots. Nothing reads or
/// writes the process environment. See the module docs for the safety
/// contract.
///
/// # Errors
///
/// `ValueError` carrying the loader's fail-closed refusal when the captured
/// environment + file do not load (an unparsable value, an unknown key, an
/// unreadable named file).
#[pyfunction]
#[pyo3(signature = (env, file=None))]
pub fn reinstall_config_holder_for_tests(
    env: ::std::collections::BTreeMap<String, String>,
    file: Option<&str>,
) -> PyResult<()> {
    let loaded = crate::config::hypothetical_layers(env, file)
        .map_err(|error| ::pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    let _ =
        ::degenbot_config::holder::reinstall_for_tests(std::sync::Arc::new(loaded.config.clone()));
    crate::config::republish_loaded(loaded);
    Ok(())
}

/// TEST-ONLY read-only view: the node endpoint the CURRENT holder snapshot
/// resolves for `chain_id` and `scope` (`"request"` or `"subscription"`).
///
/// Reads `degenbot_config::holder::config()` — the SAME snapshot
/// `boot_host` reads at construction — so an assertion tracks what a fresh
/// engine would observe, not the module-init verdict. The endpoint is resolved
/// through the core resolver (`resolve_node_uri`) over that snapshot's node
/// tables, with no explicit override and no provenance (the holder carries the
/// typed value, not the layers).
///
/// # Errors
///
/// `ValueError` when `scope` names no capability, or when no layer in the
/// snapshot supplied an endpoint for `chain_id`.
#[pyfunction]
#[pyo3(signature = (chain_id, scope))]
pub fn resolved_node_uri_for_tests(chain_id: u64, scope: &str) -> PyResult<String> {
    let scope = scope
        .parse::<::degenbot_config::NodeScope>()
        .map_err(|error| ::pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    let layers = ::degenbot_config::LoadedConfig {
        config: ::degenbot_config::holder::config().clone(),
        provenance: ::std::collections::BTreeMap::new(),
        entry_provenance: ::std::collections::BTreeMap::new(),
    };
    let resolved = ::degenbot_config::resolve_node_uri(
        &layers,
        chain_id,
        scope,
        &::degenbot_config::NodeOverrides::new(),
    )
    .map_err(|error| ::pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    Ok(resolved.value)
}
