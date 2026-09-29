//! `PyO3` wrapper for the engine stage surface — the pool-identity
//! `#[pymethods]` slice.
//!
//! The Python registration pipeline used to keep its own address → engine
//! `pool_id` maps per family. Those mirrors are gone: a caller that holds a
//! pool's IDENTITY (a family tag plus an address, or a V4 `PoolManager` plus
//! pool id) asks the shared `BotState` here, through
//! `EngineDriver::pool_id_for_identity`, and gets the id the engine hops on —
//! or `None` when no pool with that identity is registered. The answer comes
//! from the registration tables the state owner already keeps, so no Python
//! dict can disagree with it.
//!
//! String parsing and error typing live here (the driver boundary is typed:
//! `Address` / `V4PoolId`); the decision is the core's.

use super::{hex_string_to_pool_id, Address, PyArbEngine};
use crate::prelude::*;

use degenbot_substrate::session_registry::PoolIdentity;

#[pymethods]
impl PyArbEngine {
    /// The registered engine `pool_id` for an address-keyed pool identity, or
    /// `None` when this session holds no pool with that identity.
    ///
    /// `family_tag` is the vocabulary the live pool handle carries (`"v2"`,
    /// `"v3"`, `"curve"`, `"balancer-weighted"`, `"balancer-stable"`,
    /// `"aerodrome-v2"`). An unknown tag is a `ValueError`: a caller that
    /// cannot name a family must not receive some other family's id.
    #[pyo3(signature = (family_tag, address))]
    fn pool_id_for_pool(
        &self,
        py: Python<'_>,
        family_tag: &str,
        address: &str,
    ) -> PyResult<Option<u64>> {
        let parsed: Address = address.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool address: {e}"))
        })?;
        let identity = PoolIdentity::for_address_family(family_tag, parsed).ok_or_else(|| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "unknown pool family {family_tag:?}: expected one of v2, v3, curve, \
                 balancer-weighted, balancer-stable, aerodrome-v2"
            ))
        })?;
        Ok(py.detach(|| self.driver.pool_id_for_identity(&identity)))
    }

    /// The registered engine `pool_id` for a V4 `(PoolManager, pool_id)` pair,
    /// or `None` when this session holds no pool with that pair. One
    /// `PoolManager` hosts many pools, so the pair — never the manager address
    /// alone — is the identity.
    #[pyo3(signature = (pool_manager, pool_id_hex))]
    fn pool_id_for_v4_pool(
        &self,
        py: Python<'_>,
        pool_manager: &str,
        pool_id_hex: &str,
    ) -> PyResult<Option<u64>> {
        let manager: Address = pool_manager.parse().map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Invalid pool_manager: {e}"))
        })?;
        let pool_id = hex_string_to_pool_id(pool_id_hex)?;
        let identity = PoolIdentity::v4(manager, pool_id);
        Ok(py.detach(|| self.driver.pool_id_for_identity(&identity)))
    }
}
