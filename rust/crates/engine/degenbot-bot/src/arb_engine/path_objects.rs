//! The engine's path-object ADAPTER — the owner side of
//! [`PathObjectAdapter`].
//!
//! # One store, reached from the session
//!
//! Canonical path identity already has an owner: the engine's `PathRegistry`
//! holds the id space, the dedup signature index, the reverse index, and the
//! registered-path cap. This module is a thin view over that one owner. The
//! session asks its registry for a path, the registry asks this adapter, and
//! the adapter grows the engine through the engine's OWN registration path —
//! so a session-named path is a path the engine resolves and solves, and no
//! second store can disagree with the engine about which route is which.
//!
//! # The two joins this makes
//!
//! - **Identity → live state.** Each hop's session [`PoolObject`] carries a
//!   [`PoolIdentity`], and the engine hops on a `pool_id`. The id is DERIVED
//!   from the registration tables `BotState` already owns
//!   (`BotState::pool_id_for_identity`), so a hop the session has a name for
//!   but the engine has no pool to trade is refused rather than registered.
//! - **Session → engine.** `chain_id` is the session's, stamped by the
//!   registry; the `path_id` is the engine's, allocated by the engine.
//!
//! Lock order is engine-then-core, the order the engine already documents:
//! this adapter takes the engine mutex, then reads the core.

use std::sync::Arc;

use ::degenbot_solvers::mixed::PoolHop;
use parking_lot::Mutex;

use crate::arb_engine::lifecycle::{register_path, PathRegistrationError};
use crate::arb_engine::ArbitrageEngine;
use crate::bot_core::session_registry::{
    ObjectRefusal, PathIdentity, PathObject, PathObjectAdapter,
};
use crate::bot_core::state_lock::LockSite;

/// The session's handle on the engine's path identity.
///
/// Holds the engine handle rather than the registry: the engine mutex is what
/// makes growth and deregistration atomic with respect to the engine's own
/// path state, and a handle to the engine is the only thing that can reach it.
pub struct EnginePathObjects {
    engine: Arc<Mutex<ArbitrageEngine>>,
}

impl EnginePathObjects {
    /// The adapter over `engine`'s path registry.
    pub(crate) fn new(engine: Arc<Mutex<ArbitrageEngine>>) -> Self {
        Self { engine }
    }
}

impl PathObjectAdapter for EnginePathObjects {
    fn get_or_create_path(
        &self,
        chain_id: u64,
        hops: &PathIdentity,
    ) -> Result<Arc<PathObject>, ObjectRefusal> {
        let mut engine = self.engine.lock();
        let pool_hops = engine_hops(&engine, hops)?;
        // The engine's own registration: dedup by hop signature, the
        // registered-path cap, pool validation, and the resolve snapshot the
        // solve cycle reads. Going around it would make a session-named path
        // a different thing than a solved one.
        let path_id =
            register_path(&mut engine, pool_hops).map_err(|error| refusal(error, hops))?;
        Ok(engine.registry.canonical_path(chain_id, path_id, hops))
    }

    fn resolve_path(
        &self,
        chain_id: u64,
        hops: &PathIdentity,
    ) -> Result<Arc<PathObject>, ObjectRefusal> {
        let mut engine = self.engine.lock();
        let pool_hops = engine_hops(&engine, hops)?;
        let signature: Vec<(u64, bool)> = pool_hops
            .iter()
            .map(|hop| (hop.pool_id, hop.zero_for_one))
            .collect();
        let path_id = engine
            .registry
            .lookup(&signature)
            .ok_or_else(|| ObjectRefusal::UnknownPathIdentity { hops: hops.clone() })?;
        Ok(engine.registry.canonical_path(chain_id, path_id, hops))
    }

    fn path_count(&self) -> usize {
        self.engine.lock().registry.len()
    }
}

/// The engine's pool hops for a session hop signature — the join from
/// canonical pool identity to the id the engine trades, read from the
/// registration tables `BotState` owns.
fn engine_hops(
    engine: &ArbitrageEngine,
    hops: &PathIdentity,
) -> Result<Vec<PoolHop>, ObjectRefusal> {
    let core = engine.core.read_at(LockSite::Registration);
    hops.hops()
        .iter()
        .map(|hop| {
            let identity = hop.pool_identity();
            let pool_id = core.pool_id_for_identity(identity).ok_or_else(|| {
                ObjectRefusal::UnknownPoolIdentity {
                    identity: identity.clone(),
                }
            })?;
            Ok(PoolHop {
                pool_id,
                zero_for_one: hop.zero_for_one(),
            })
        })
        .collect()
}

/// The engine's registration refusal, restated in the session registry's
/// refusal vocabulary. The unroutable case keeps the engine's own reason
/// verbatim, because it names the hop and the structural fault and re-wording
/// it would only lose that.
fn refusal(error: PathRegistrationError, hops: &PathIdentity) -> ObjectRefusal {
    match error {
        PathRegistrationError::Invalid(reason) => ObjectRefusal::UnroutablePath {
            hops: hops.clone(),
            reason,
        },
        PathRegistrationError::RegistryFull { cap, registered } => {
            ObjectRefusal::PathCapacityReached { cap, registered }
        }
    }
}
