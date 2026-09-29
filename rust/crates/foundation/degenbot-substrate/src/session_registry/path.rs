//! The session's path kind: canonical path identity, and the adapter seam
//! the path-identity owner implements.
//!
//! # Identity, not policy
//!
//! A path object is the session's stable NAME for a route: the chain, the
//! ordered hops, and the id the path-identity owner allocated. Solver choice,
//! dispatch priority, submission posture, and any per-strategy admission rule
//! are deliberately absent — a strategy derives its own plan FROM the object
//! and keeps the plan itself, so two strategies that trade the same route
//! share one identity instead of forking it per strategy. The registry owning
//! solver or submission state is the failure mode this split exists to
//! prevent (`docs/architecture/session-object-registry.md`).
//!
//! # Hops are validated pool references
//!
//! A hop names a pool by the session's own pool identity ([`PoolIdentity`])
//! and resolves to that identity's canonical [`PoolObject`] — the session's one
//! object for that pool. So a path joins live state through the pool kind
//! rather than through a second pool-id map, and a hop naming a pool the
//! session does not hold is refused instead of minted.
//!
//! # One home for path state
//!
//! This module holds NO path map. The id space, the dedup signature index, and
//! the registered-path cap belong to whoever owns path identity today (the
//! engine's `PathRegistry`), and the session reaches them through
//! [`PathObjectAdapter`]. An adapter is a view over that one owner, so the
//! session can name a path without a second store that could disagree with it
//! about which route is which.

use std::fmt;
use std::sync::Arc;

use super::{ObjectRefusal, PoolIdentity, PoolObject};

/// One hop of a canonical path: a validated pool object plus the direction the
/// route takes through it.
///
/// The pool is an [`Arc`] to the session's canonical pool object, so a hop is
/// named by the same object every other consumer of that pool holds — a
/// consumer that copied a pool identity into a private value would fork the
/// join to live state.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PathHop {
    pool: Arc<PoolObject>,
    zero_for_one: bool,
}

impl PathHop {
    /// The hop over `pool` in the given direction.
    #[must_use]
    pub const fn new(pool: Arc<PoolObject>, zero_for_one: bool) -> Self {
        Self { pool, zero_for_one }
    }

    /// The canonical pool object this hop was validated to.
    #[must_use]
    pub const fn pool(&self) -> &Arc<PoolObject> {
        &self.pool
    }

    /// The pool identity this hop names.
    #[must_use]
    pub fn pool_identity(&self) -> &PoolIdentity {
        self.pool.identity()
    }

    /// Which way the route crosses this pool.
    #[must_use]
    pub const fn zero_for_one(&self) -> bool {
        self.zero_for_one
    }
}

/// Canonical identity of a path: its ordered `(pool, direction)` hop
/// signature.
///
/// Order and per-hop direction are both part of the key, so a route and its
/// reverse are two identities — the direction a route is traded decides which
/// route it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PathIdentity {
    hops: Vec<PathHop>,
}

impl PathIdentity {
    /// The identity of the route `hops`, in order.
    #[must_use]
    pub const fn new(hops: Vec<PathHop>) -> Self {
        Self { hops }
    }

    /// The route's hops, in order.
    #[must_use]
    pub fn hops(&self) -> &[PathHop] {
        &self.hops
    }

    /// How many hops the route has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hops.len()
    }

    /// Whether the route has no hops.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hops.is_empty()
    }
}

impl fmt::Display for PathIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.hops.is_empty() {
            return f.write_str("path with no hops");
        }
        f.write_str("path ")?;
        for (index, hop) in self.hops.iter().enumerate() {
            if index > 0 {
                f.write_str(" -> ")?;
            }
            write!(
                f,
                "{}{}",
                hop.pool.identity(),
                Self::arrow(hop.zero_for_one())
            )?;
        }
        Ok(())
    }
}

impl PathIdentity {
    /// The direction marker a hop renders as. Not a `Debug` of the bool: a
    /// refusal that reads "path v2 pool 0x..(false)" is unreadable, and the
    /// two spellings are the ones the swap math uses.
    fn arrow(zero_for_one: bool) -> &'static str {
        if zero_for_one {
            "(0->1)"
        } else {
            "(1->0)"
        }
    }
}

/// One session's canonical path object: identity, and nothing else.
///
/// Deliberately not a plan. What a strategy does with this route — which
/// solver, on what trigger, with which submission posture — is the strategy's
/// own derived value; putting any of it here would make the shared route
/// unforkable and give one arm's policy to the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathObject {
    chain_id: u64,
    identity: PathIdentity,
    path_id: u64,
}

impl PathObject {
    /// The chain whose session named this object.
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// The id the path-identity owner allocated for this route.
    ///
    /// The engine's solve results, dispatcher, and encoder are keyed by this
    /// id, so it is the join a consumer needs — and it is the OWNER's id, not
    /// a session-local counter, which is what makes one route one identity.
    #[must_use]
    pub const fn path_id(&self) -> u64 {
        self.path_id
    }

    /// This object's canonical identity.
    #[must_use]
    pub const fn identity(&self) -> &PathIdentity {
        &self.identity
    }

    /// The route's hops, in order.
    #[must_use]
    pub fn hops(&self) -> &[PathHop] {
        self.identity.hops()
    }

    /// The object the path-identity owner mints for a route it has just
    /// allocated `path_id` to.
    ///
    /// Crate-private because the owner side of [`PathObjectAdapter`] is the
    /// only legitimate constructor: a session reaches its path objects through
    /// the registry, and the registry reaches them through the owner. The
    /// `chain_id` is an argument rather than a read of the first hop so that
    /// the SESSION stamps the scope — a hop object cannot be used to mint an
    /// object claiming a different chain.
    #[must_use]
    pub fn mint(chain_id: u64, identity: PathIdentity, path_id: u64) -> Self {
        Self {
            chain_id,
            identity,
            path_id,
        }
    }
}

/// The registry's side of the boundary with whoever owns canonical path
/// identity.
///
/// The registry holds no path map of its own, so the id space, the dedup
/// signature index, and the registered-path cap stay with their owner and
/// this trait is the only way the session reaches them. An implementation is a
/// VIEW over that one owner: it answers for ids the owner allocated and grows
/// the owner through the owner's own growth path, never around it.
///
/// The trait is the registry's dependency, not the owner's: the owner
/// implements it and the session never names the owner.
pub trait PathObjectAdapter: Send + Sync {
    /// Get-or-create: the canonical path object for `hops`, registering the
    /// route through the owner's own growth path when the owner does not hold
    /// it yet.
    ///
    /// # Errors
    ///
    /// A typed [`ObjectRefusal`]: a hop whose pool the owner cannot read, a
    /// route the owner refuses to register, or the owner's registered-path
    /// capacity. A refusal creates nothing.
    fn get_or_create_path(
        &self,
        chain_id: u64,
        hops: &PathIdentity,
    ) -> Result<Arc<PathObject>, ObjectRefusal>;

    /// The canonical path object for `hops`, without registering a route.
    ///
    /// # Errors
    ///
    /// [`ObjectRefusal::UnknownPathIdentity`] when the owner holds no such
    /// route. Answering for a route the owner already holds may RECORD the
    /// owner's session-object view of it (an id the owner allocated, never a
    /// new path); that is the first-sight recording an implementation needs to
    /// hand back one object per route, and it grows the path set not at all.
    fn resolve_path(
        &self,
        chain_id: u64,
        hops: &PathIdentity,
    ) -> Result<Arc<PathObject>, ObjectRefusal>;

    /// How many canonical paths the owner holds.
    fn path_count(&self) -> usize;
}
