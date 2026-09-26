//! The session's position seam: canonical position identity, and a read that
//! refuses instead of fabricating.
//!
//! # Identity is canonical; the value is not
//!
//! A position is the one session-object candidate whose value DECAYS, so the
//! session keeps its identity — `(chain, market, account)`, stamped with the
//! session's own chain — and reaches the value through an observer that may
//! refuse. There is deliberately no `get_or_create_position`: creating a
//! position would mean inventing the value a failed read could not produce, and
//! caching one would hide its age from a strategy about to act on it.
//!
//! # No store
//!
//! This module holds no collection. The registry keeps a HANDLE to one
//! observer, exactly as the path kind keeps a handle to its identity owner, so
//! there is no position map here for a failed read to fall back on and no
//! second authority that could disagree with the integration about what a
//! position is. A caller that wants a position held across a solve caches it as
//! its own derived value.

use std::sync::Arc;

use alloy::primitives::Address;

use degenbot_core::op_error;

use super::SessionObjectRegistry;

pub use degenbot_core::session_positions::{
    Freshness, HealthFactor, PositionIdentity, PositionObserver, PositionReading, PositionRefusal,
};

impl SessionObjectRegistry {
    /// The canonical identity of the position `account` holds in the `market`
    /// pool contract, stamped with THIS session's chain.
    ///
    /// The registry names the identity rather than accepting a caller-built
    /// one, because the chain is part of the key and a session is scoped to
    /// exactly one: two sessions on the same chain agree on the identity, and a
    /// session never hands out an identity for a chain it does not serve.
    #[must_use]
    pub fn position_identity(&self, market: Address, account: Address) -> PositionIdentity {
        PositionIdentity::new(self.chain_id(), market, account)
    }

    /// Install the session's position observer — the component that reads
    /// positions for this session's chain and markets.
    ///
    /// First install wins, and a second install is refused with the observer
    /// handed back: two observers in one session could answer the same
    /// identity from different sources, which is the drift the registry exists
    /// to remove. One session installs one observer, once.
    ///
    /// The refusal is REPORTED at ERROR here rather than left to the call site.
    /// The path owner logs at its composition site because the engine IS that
    /// site; for positions it is not, because the observer belongs to a lending
    /// integration and the two halves can only meet at a composition root — the
    /// bot boot, or a consumer's own `main`
    /// (`docs/architecture/session-object-registry.md`, *Who installs it, and
    /// where*). Neither is a place that can be assumed to run, and the boot's
    /// install is unconditional, so the registry is the one component that always
    /// learns of a fork. The refused observer is still handed back so the caller
    /// can release it: the log is the operator signal, not a substitute for the
    /// `Err`.
    ///
    /// # Errors
    ///
    /// `Err` carrying `observer` when an observer is already installed.
    pub fn install_position_observer(
        &self,
        observer: Arc<dyn PositionObserver>,
    ) -> Result<(), Arc<dyn PositionObserver>> {
        self.positions.set(observer).inspect_err(|_| {
            op_error!(
                domain = state,
                session_chain_id = self.chain_id(),
                "a second position observer was refused: this session keeps the first one, so a position the refused observer would have served is read from the other source"
            );
        })
    }

    /// Whether this session has a position observer installed.
    #[must_use]
    pub fn has_position_observer(&self) -> bool {
        self.positions.get().is_some()
    }

    /// Read the position `identity` names, as fresh as `freshness` requires.
    ///
    /// Three refusals happen before the observer is asked or its reading is
    /// returned, so a caller cannot be handed a value that is out of scope, out
    /// of date, or invented:
    /// - the chain in `identity` is not this session's chain
    ///   ([`PositionRefusal::ChainScopeMismatch`]),
    /// - no observer is installed ([`PositionRefusal::NoPositionOwner`]),
    /// - the observation the observer produced is older than `freshness`
    ///   allows ([`PositionRefusal::StaleObservation`]).
    ///
    /// The freshness check is the SESSION's, not the observer's, so a lax
    /// implementation cannot pass a decaying snapshot through as current. Each
    /// read reaches the observer: this registry caches nothing, so two
    /// consumers reading one identity may (correctly) see two different blocks.
    ///
    /// # Errors
    ///
    /// A typed [`PositionRefusal`]. There is no path here that returns a
    /// reading alongside a fault, and none that substitutes a default.
    pub fn read_position(
        &self,
        identity: &PositionIdentity,
        freshness: &Freshness,
    ) -> Result<PositionReading, PositionRefusal> {
        if identity.chain_id() != self.chain_id() {
            return Err(PositionRefusal::ChainScopeMismatch {
                identity: *identity,
                session_chain_id: self.chain_id(),
            });
        }
        let observer = self
            .positions
            .get()
            .ok_or(PositionRefusal::NoPositionOwner)?;
        let reading = observer.read_position(identity, freshness)?;
        if !freshness.accepts(Some(reading.observed_block())) {
            return Err(PositionRefusal::StaleObservation {
                identity: *identity,
                observed_block: Some(reading.observed_block()),
                required: *freshness,
            });
        }
        Ok(reading)
    }
}
