//! Session object registry — the one per-session owner of object identity.
//!
//! An **Object** is a thing the session names (a pool, an ERC-20 token): a
//! canonical identity plus, for pool/token kinds, the live state its existing
//! owner holds. This module is the **Session object registry** for the pools
//! and tokens of ONE session — the lifetime of one
//! [`Bot`](crate::bot_core::Bot) on one chain (ADR-006 D5). It owns
//! **Canonical identity** and nothing else, and its only growth path is
//! **Get-or-create**.
//!
//! # Identity, not live state
//!
//! The registry is deliberately shallow. **Live state** — reserves, liquidity,
//! reorg journals — stays with its existing owner ([`BotState`], which remains
//! the sole writer), and construction I/O stays behind the
//! [`ConstructionIo`](crate::bot_core::construction_io::ConstructionIo) seam.
//! A registry that absorbed live state would be a second `BotState` with a
//! second writer on the same reorg journals, so the two concerns are kept
//! apart: this module answers "which pool is this?", and reading or advancing
//! the state behind that answer is a separate step. Rationale and the current
//! owner table live in `docs/architecture/session-object-registry.md`.
//!
//! # Handles, not borrows
//!
//! Every read surface hands back an owned shared handle — an **Object
//! reference** as `Arc<PoolObject>` / `Arc<TokenObject>`. Two consumers hold
//! the same canonical object at once and neither one can remove it: the
//! registry owns the entry for the session's lifetime, and a consumer's handle
//! going away is not a deregistration. The map is a [`DashMap`] of `Arc`s, so
//! an entry has no stable address to borrow from; `Arc` is also the honest
//! shape, because a consumer holds the object beyond the call that produced it.
//!
//! # Scope
//!
//! Identity is **session-local**. A canonical key is meaningful only inside its
//! session, so the chain the registry is constructed for is a constructor
//! argument and a registry built for another chain is a different key space,
//! not a filtered view of the same one. The kinds here are pools, ERC-20
//! tokens, and paths, plus the position identity and read below.
//!
//! # The position kind is an identity and a read
//!
//! A position is not a canonical session object: its value decays, so the
//! session names the position's IDENTITY and reaches the value through an
//! observer that can refuse — [`PositionIdentity`] and [`PositionObserver`]
//! re-exported from the [`position`] submodule. There is no position map here
//! and no get-or-create: a read that fails is a typed [`PositionRefusal`],
//! never a fabricated position. The seam's types live in `degenbot-core` so a
//! lending integration can implement the observer without depending on the
//! engine; see the [`position`] submodule for the reasoning.
//!
//! # The path kind is reached, not stored
//!
//! Pools and tokens are registered here, so their identity has one home. A
//! path is different: its id space, its dedup signature index, and its cap
//! already have an owner, so the registry reaches them through
//! [`PathObjectAdapter`] and holds no path map of its own — a second map keyed
//! by hop signature is exactly the drift the registry exists to remove. The
//! canonical object is minted by that owner and is identity only; a strategy's
//! plan is derived from it. See the [`path`] submodule for the split.

mod path;
mod position;

pub use path::{PathHop, PathIdentity, PathObject, PathObjectAdapter};
pub use position::{
    Freshness, HealthFactor, PositionIdentity, PositionObserver, PositionReading, PositionRefusal,
};

use std::fmt;
use std::sync::{Arc, OnceLock};

use alloy::primitives::Address;
use dashmap::DashMap;
use degenbot_decoders::v4_swap_decoder::V4PoolId;

/// Canonical identity of one session's pool object.
///
/// The family is part of the key, so a V2 and a V3 pool at the same address
/// are two identities, and a V4 pool is keyed by its `(PoolManager, pool_id)`
/// pair — one `PoolManager` contract hosts many pools, so the pair (never the
/// manager address alone) names one.
///
/// Distinct from the live-state identity types
/// ([`V4PoolIdentity`](degenbot_pools::v4_state::V4PoolIdentity) and
/// siblings), which describe a *registered pool's* immutable registration data
/// inside [`BotState`]. This type describes which object the session names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PoolIdentity {
    /// A Uniswap V2 pool: its contract address, family-scoped.
    V2(Address),
    /// A Uniswap V3 pool: its contract address, family-scoped.
    V3(Address),
    /// A Uniswap V4 pool: the `PoolManager` contract that hosts it plus its
    /// on-chain `pool_id`.
    V4 {
        /// The V4 `PoolManager` contract hosting this pool.
        pool_manager: Address,
        /// The pool's on-chain id, unique only within its `PoolManager`.
        pool_id: V4PoolId,
    },
    /// An Aerodrome V2 pool: its contract address, family-scoped.
    AerodromeV2(Address),
    /// A Balancer V2 weighted pool: its contract address, family-scoped.
    BalancerWeighted(Address),
    /// A Balancer V2 stable pool: its contract address, family-scoped.
    BalancerStable(Address),
    /// A Curve stableswap pool: its contract address, family-scoped.
    Curve(Address),
}

impl PoolIdentity {
    /// The identity of the V2 pool at `address`.
    #[must_use]
    pub const fn v2(address: Address) -> Self {
        Self::V2(address)
    }

    /// The identity of the V3 pool at `address`.
    #[must_use]
    pub const fn v3(address: Address) -> Self {
        Self::V3(address)
    }

    /// The identity of the V4 pool `pool_id` under `pool_manager`.
    #[must_use]
    pub const fn v4(pool_manager: Address, pool_id: V4PoolId) -> Self {
        Self::V4 {
            pool_manager,
            pool_id,
        }
    }

    /// The address-keyed identity for the family named by `family_tag`.
    ///
    /// `family_tag` is the same kebab-case vocabulary
    /// [`PoolIdentity::family_tag`] and
    /// [`BotState::pool_family`](crate::bot_core::BotState::pool_family)
    /// report, so a consumer that already knows a pool's family (a Python
    /// companion reading its live handle, a core registration result) names the
    /// identity without this module having to expose a constructor per family.
    /// `None` for a tag this registry does not model, and for `"v4"` — whose
    /// identity is a `(PoolManager, pool_id)` pair, not an address; a V4
    /// consumer uses [`PoolIdentity::v4`].
    #[must_use]
    pub const fn for_address_family(family_tag: &str, address: Address) -> Option<Self> {
        match family_tag.as_bytes() {
            b"v2" => Some(Self::V2(address)),
            b"v3" => Some(Self::V3(address)),
            b"curve" => Some(Self::Curve(address)),
            b"balancer-weighted" => Some(Self::BalancerWeighted(address)),
            b"balancer-stable" => Some(Self::BalancerStable(address)),
            b"aerodrome-v2" => Some(Self::AerodromeV2(address)),
            _ => None,
        }
    }

    /// The address component of the identity.
    ///
    /// For every address-keyed family this is the pool's own contract
    /// address. For V4 it is the `PoolManager` contract, which is NOT a pool
    /// address — a V4 consumer naming a pool reads [`Self::v4_pair`], and the
    /// registry never indexes a V4 identity by this address.
    #[must_use]
    pub const fn address(&self) -> Address {
        match self {
            Self::V2(address)
            | Self::V3(address)
            | Self::AerodromeV2(address)
            | Self::BalancerWeighted(address)
            | Self::BalancerStable(address)
            | Self::Curve(address) => *address,
            Self::V4 { pool_manager, .. } => *pool_manager,
        }
    }

    /// Whether this identity is a V4 pool — the one family whose key is a
    /// `(PoolManager, pool_id)` pair rather than an address, and therefore the
    /// one family the registry keeps out of the address index.
    #[must_use]
    pub const fn is_v4(&self) -> bool {
        matches!(self, Self::V4 { .. })
    }

    /// The `(pool_manager, pool_id)` pair, for a V4 identity. `None` for the
    /// address-keyed families.
    #[must_use]
    pub const fn v4_pair(&self) -> Option<(Address, V4PoolId)> {
        match self {
            Self::V4 {
                pool_manager,
                pool_id,
            } => Some((*pool_manager, *pool_id)),
            Self::V2(..)
            | Self::V3(..)
            | Self::AerodromeV2(..)
            | Self::BalancerWeighted(..)
            | Self::BalancerStable(..)
            | Self::Curve(..) => None,
        }
    }

    /// The family tag (`"v2"` / `"v3"` / `"v4"` / `"curve"` /
    /// `"balancer-weighted"` / `"balancer-stable"` / `"aerodrome-v2"`) — the
    /// same tag vocabulary
    /// [`BotState::pool_family`](crate::bot_core::BotState::pool_family)
    /// reports, so an identity-to-live-state join reads one family name and a
    /// consumer that already knows a family names its identity through
    /// [`PoolIdentity::for_address_family`].
    #[must_use]
    pub const fn family_tag(&self) -> &'static str {
        match self {
            Self::V2(..) => "v2",
            Self::V3(..) => "v3",
            Self::V4 { .. } => "v4",
            Self::AerodromeV2(..) => "aerodrome-v2",
            Self::BalancerWeighted(..) => "balancer-weighted",
            Self::BalancerStable(..) => "balancer-stable",
            Self::Curve(..) => "curve",
        }
    }
}

impl fmt::Display for PoolIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V2(address) => write!(f, "v2 pool {address}"),
            Self::V3(address) => write!(f, "v3 pool {address}"),
            Self::V4 {
                pool_manager,
                pool_id,
            } => write!(
                f,
                "v4 pool {pool_manager}/{}",
                alloy::hex::encode_prefixed(pool_id)
            ),
            Self::AerodromeV2(address)
            | Self::BalancerWeighted(address)
            | Self::BalancerStable(address)
            | Self::Curve(address) => {
                write!(f, "{} pool {address}", self.family_tag())
            }
        }
    }
}

/// Canonical identity of one session's token object: a chain-scoped ERC-20
/// contract address. The chain is not a key component because the registry is
/// constructed for exactly one chain, so it is already implied by the registry
/// a request arrives on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TokenIdentity {
    /// An ERC-20 token at `address`.
    Erc20(Address),
}

impl TokenIdentity {
    /// The identity of the ERC-20 token at `address`.
    #[must_use]
    pub const fn erc20(address: Address) -> Self {
        Self::Erc20(address)
    }

    /// The token's contract address.
    #[must_use]
    pub const fn address(&self) -> Address {
        match self {
            Self::Erc20(address) => *address,
        }
    }
}

impl fmt::Display for TokenIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Erc20(address) => write!(f, "erc20 token {address}"),
        }
    }
}

/// Why a resolve request for an object this session does not hold was refused.
///
/// Typed by kind so a caller branches without parsing a message. A refusal
/// creates nothing: the identity stays unregistered until a get-or-create
/// legitimately registers it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ObjectRefusal {
    /// No pool object is registered for this identity in this session.
    #[error("no pool object is registered for {identity} in this session")]
    UnknownPoolIdentity {
        /// The identity the request named.
        identity: PoolIdentity,
    },
    /// No token object is registered for this identity in this session.
    #[error("no token object is registered for {identity} in this session")]
    UnknownTokenIdentity {
        /// The identity the request named.
        identity: TokenIdentity,
    },
    /// No address-keyed pool object is registered at this address in this
    /// session. The address view of a pool: it resolves a caller that knows
    /// only an address to whichever family registered that address first, and
    /// a V4 pool is deliberately not in it.
    #[error("no address-keyed pool object is registered at {address} in this session")]
    UnknownPoolAddress {
        /// The address the request named.
        address: Address,
    },
    /// No canonical path object is registered for this hop signature. The
    /// path-identity owner does not hold the route, so a resolve that may not
    /// register answers with this rather than creating one.
    #[error("no canonical path object is registered for {hops} in this session")]
    UnknownPathIdentity {
        /// The route the request named.
        hops: PathIdentity,
    },
    /// The session has no path-identity owner installed, so it cannot answer
    /// for a path at all. Distinct from a missing pool: every hop of the
    /// request resolved, and the refusal is about the owner being unwired.
    #[error("no path-identity owner is installed on this session's registry")]
    NoPathOwner,
    /// The path-identity owner refused the route: the hop shape cannot be
    /// routed. The owner's own reason, kept verbatim so the diagnosis does not
    /// get re-worded in transit.
    #[error("the path-identity owner refused {hops}: {reason}")]
    UnroutablePath {
        /// The route the request named.
        hops: PathIdentity,
        /// The owner's reason for the refusal.
        reason: String,
    },
    /// The path-identity owner's registered-path capacity is reached. A benign
    /// stop signal for discovery, not a fault: the route is refused and the
    /// owner keeps every path it already holds.
    #[error("the path-identity owner's registered-path cap is reached ({registered}/{cap})")]
    PathCapacityReached {
        /// The owner's configured capacity.
        cap: usize,
        /// The owner's registered-path count at refusal.
        registered: usize,
    },
}

/// One session's pool object: canonical identity, and nothing else.
///
/// Deliberately not a pool. Pool behavior and live state stay on the existing
/// pool types — the V2/V3/V4 state structs in `degenbot-pools`, registered and
/// advanced through [`BotState`]. This object is the stable name the session's
/// consumers agree on; it holds no reserves, no liquidity, no journal, and no
/// I/O handle.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PoolObject {
    chain_id: u64,
    identity: PoolIdentity,
}

impl PoolObject {
    /// The chain whose session registered this object. Carried on the object
    /// rather than read back out of a registry, so a handle handed to a
    /// consumer names its own scope.
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// This object's canonical identity.
    #[must_use]
    pub const fn identity(&self) -> &PoolIdentity {
        &self.identity
    }

    /// The family tag (`"v2"` / `"v3"` / `"v4"`) of this object's identity.
    #[must_use]
    pub const fn family_tag(&self) -> &'static str {
        self.identity.family_tag()
    }

    /// The object a registry mints for `identity` in a `chain_id` session.
    /// Private because the registry owns the get-or-create seam: a consumer
    /// gets its object from the session, never constructs one.
    const fn from_identity(chain_id: u64, identity: PoolIdentity) -> Self {
        Self { chain_id, identity }
    }
}

/// One session's token object: canonical identity, and nothing else.
///
/// As with [`PoolObject`], the token's live metadata stays with its existing
/// owner ([`BotState`]'s token entries); this object is the session's name for
/// the token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenObject {
    chain_id: u64,
    identity: TokenIdentity,
}

impl TokenObject {
    /// The chain whose session registered this object.
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// This object's canonical identity.
    #[must_use]
    pub const fn identity(&self) -> &TokenIdentity {
        &self.identity
    }

    /// The object a registry mints for `identity` in a `chain_id` session.
    /// Private for the same reason as [`PoolObject::from_identity`].
    const fn from_identity(chain_id: u64, identity: TokenIdentity) -> Self {
        Self { chain_id, identity }
    }
}

/// The session object registry: one per session, owning canonical identity for
/// the session's pools, tokens, and paths.
///
/// The registry is the single growth path — get-or-create — so a second
/// registration seam cannot produce a second object for one identity, and a
/// racing registration converges on the first rather than duplicating it. It
/// holds no provider, DB handle, tick fetcher, or construction I/O, and it
/// never writes [`BotState`]; it is identity only.
pub struct SessionObjectRegistry {
    /// The chain this session is scoped to. Identity is session-local, so this
    /// is a property of the registry rather than a key component: a registry
    /// built for another chain is a different key space.
    chain_id: u64,
    /// Pool identities → the one canonical object per identity.
    pools: DashMap<PoolIdentity, Arc<PoolObject>>,
    /// Address-keyed pool identity → the identity that address resolved to,
    /// so a consumer that knows only an address (
    /// [`Self::resolve_pool_by_address`]) joins the same object instead of
    /// keeping a second address map of its own. An INDEX over `pools`, not a
    /// second authority: it holds identities, never objects, and the identity
    /// it names is the one already in `pools`. First registration for an
    /// address wins, which is the order an address-keyed consumer observes.
    /// V4 identities are absent by design — a `PoolManager` address is not a
    /// pool address.
    pool_addresses: DashMap<Address, PoolIdentity>,
    /// Token identities → the one canonical object per identity.
    tokens: DashMap<TokenIdentity, Arc<TokenObject>>,
    /// The path-identity owner, installed once. A handle rather than a map
    /// because this registry does not store paths: it asks the owner, which
    /// holds the one path id space, dedup index, and cap.
    paths: OnceLock<Arc<dyn PathObjectAdapter>>,
    /// The session's position observer, installed once. A handle rather than a
    /// map because a position is a perishable READ, not a session-resident
    /// object: the registry names the identity and asks the observer, and holds
    /// no position of its own to cache or fall back on.
    positions: OnceLock<Arc<dyn PositionObserver>>,
}

impl fmt::Debug for SessionObjectRegistry {
    /// Counts, not contents: a registry handle is shared and an object's
    /// address is not part of its identity, so dumping entries would report
    /// allocation trivia where the question is scope.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionObjectRegistry")
            .field("chain_id", &self.chain_id)
            .field("pools", &self.pools.len())
            .field("pool_addresses", &self.pool_addresses.len())
            .field("tokens", &self.tokens.len())
            .field("has_path_owner", &self.paths.get().is_some())
            .field("has_position_observer", &self.positions.get().is_some())
            .finish()
    }
}

impl SessionObjectRegistry {
    /// A registry scoped to `chain_id`, holding no objects yet.
    #[must_use]
    pub fn new(chain_id: u64) -> Self {
        Self {
            chain_id,
            pools: DashMap::new(),
            pool_addresses: DashMap::new(),
            tokens: DashMap::new(),
            paths: OnceLock::new(),
            positions: OnceLock::new(),
        }
    }

    /// The chain this session's identities are scoped to.
    #[must_use]
    pub const fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Get-or-create: the canonical pool object for `identity`, registering it
    /// if this session does not hold it yet.
    ///
    /// Any number of calls for one identity — sequential or racing across
    /// threads — return the same object and leave one entry. The insert is the
    /// only growth path, and it happens under the map's own shard lock, so no
    /// caller-side lock is needed and none would be correct to add.
    #[must_use]
    pub fn get_or_create_pool(&self, identity: PoolIdentity) -> Arc<PoolObject> {
        let seed = identity.clone();
        let entry = self
            .pools
            .entry(identity)
            .or_insert_with(move || Arc::new(PoolObject::from_identity(self.chain_id, seed)));
        let object = Arc::clone(entry.value());
        if !object.identity().is_v4() {
            // First identity to claim an address is the one an address-only
            // consumer resolves to, so a second family at the same address
            // stays a distinct object without disturbing the address view.
            self.pool_addresses
                .entry(object.identity().address())
                .or_insert_with(|| object.identity().clone());
        }
        object
    }

    /// The canonical pool object at `address`, resolved through the address
    /// index — the read surface for a consumer that knows a pool's address but
    /// not its family (a pool lookup by address, a tracker's per-block pool
    /// read).
    ///
    /// It answers with the object whose identity first claimed the address, so
    /// the answer is a property of the session rather than of the call: when
    /// two families share an address, the address view names the first
    /// registered one and a caller that needs the other must name its family
    /// ([`Self::resolve_pool`]). A V4 pool is not address-keyed and is never
    /// named here.
    ///
    /// # Errors
    ///
    /// [`ObjectRefusal::UnknownPoolAddress`] when no address-keyed identity in
    /// this session claims `address`. The refusal registers nothing.
    pub fn resolve_pool_by_address(
        &self,
        address: &Address,
    ) -> Result<Arc<PoolObject>, ObjectRefusal> {
        let identity = self
            .pool_addresses
            .get(address)
            .map(|entry| entry.value().clone())
            .ok_or(ObjectRefusal::UnknownPoolAddress { address: *address })?;
        self.resolve_pool(&identity)
    }

    /// The canonical pool object for `identity`, without registering anything.
    ///
    /// # Errors
    ///
    /// [`ObjectRefusal::UnknownPoolIdentity`] when this session does not hold
    /// the identity. The refusal registers nothing, and does not pre-empt the
    /// get-or-create that may legitimately register it next.
    pub fn resolve_pool(&self, identity: &PoolIdentity) -> Result<Arc<PoolObject>, ObjectRefusal> {
        self.pools
            .get(identity)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| ObjectRefusal::UnknownPoolIdentity {
                identity: identity.clone(),
            })
    }

    /// How many pool objects this session holds — one per identity, never one
    /// per request.
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Get-or-create: the canonical token object for `identity`, registering it
    /// if this session does not hold it yet. The token twin of
    /// [`Self::get_or_create_pool`].
    #[must_use]
    pub fn get_or_create_token(&self, identity: TokenIdentity) -> Arc<TokenObject> {
        let seed = identity.clone();
        let entry = self
            .tokens
            .entry(identity)
            .or_insert_with(move || Arc::new(TokenObject::from_identity(self.chain_id, seed)));
        Arc::clone(entry.value())
    }

    /// The canonical token object for `identity`, without registering anything.
    ///
    /// # Errors
    ///
    /// [`ObjectRefusal::UnknownTokenIdentity`] when this session does not hold
    /// the identity. The refusal registers nothing.
    pub fn resolve_token(
        &self,
        identity: &TokenIdentity,
    ) -> Result<Arc<TokenObject>, ObjectRefusal> {
        self.tokens
            .get(identity)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| ObjectRefusal::UnknownTokenIdentity {
                identity: identity.clone(),
            })
    }

    /// How many token objects this session holds — one per identity, never one
    /// per request.
    #[must_use]
    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    /// Install the session's path-identity owner — the one component that
    /// holds the path id space, the dedup signature index, and the
    /// registered-path cap.
    ///
    /// First install wins, and a second install is refused with the adapter
    /// handed back: two owners in one session would be two id spaces, so the
    /// same route could get a different id depending on which consumer asked,
    /// which is the drift the registry exists to remove. One session therefore
    /// installs one owner, once.
    ///
    /// # Errors
    ///
    /// `Err` carrying `adapter` when an owner is already installed.
    pub fn install_path_objects(
        &self,
        adapter: Arc<dyn PathObjectAdapter>,
    ) -> Result<(), Arc<dyn PathObjectAdapter>> {
        self.paths.set(adapter)
    }

    /// Whether this session has a path-identity owner installed.
    #[must_use]
    pub fn has_path_owner(&self) -> bool {
        self.paths.get().is_some()
    }

    /// How many canonical paths this session's owner holds — zero while no
    /// owner is installed, because a session with no owner holds no paths.
    #[must_use]
    pub fn path_count(&self) -> usize {
        self.paths.get().map_or(0, |owner| owner.path_count())
    }

    /// Get-or-create: the canonical path object for the ordered
    /// `(pool, direction)` hops, registering the route through the
    /// path-identity owner when it does not hold it yet.
    ///
    /// Each hop is validated against this session's pool objects FIRST, so a
    /// hop naming a pool the session does not hold is refused before the owner
    /// is asked to register anything. The twin of
    /// [`Self::get_or_create_pool`] over hops instead of pools: a strategy
    /// names the route it wants and gets back the one object every other
    /// consumer of that route holds.
    ///
    /// # Errors
    ///
    /// A typed [`ObjectRefusal`]:
    /// [`ObjectRefusal::UnknownPoolIdentity`] for a hop over a pool this
    /// session does not hold, [`ObjectRefusal::NoPathOwner`] while no owner is
    /// installed, and the owner's own typed refusal otherwise (an unroutable
    /// hop shape, or the registered-path cap). A refusal registers nothing.
    pub fn get_or_create_path(
        &self,
        hops: &[(PoolIdentity, bool)],
    ) -> Result<Arc<PathObject>, ObjectRefusal> {
        let identity = self.path_identity(hops)?;
        let owner = self.path_owner()?;
        owner.get_or_create_path(self.chain_id, &identity)
    }

    /// The canonical path object for the ordered hops, without registering a
    /// route. A resolve that may not grow the owner's path set.
    ///
    /// # Errors
    ///
    /// A typed [`ObjectRefusal`], as for [`Self::get_or_create_path`], with
    /// [`ObjectRefusal::UnknownPathIdentity`] when the owner holds no such
    /// route. The refusal registers nothing and does not pre-empt the
    /// get-or-create that may legitimately register it next.
    pub fn resolve_path(
        &self,
        hops: &[(PoolIdentity, bool)],
    ) -> Result<Arc<PathObject>, ObjectRefusal> {
        let identity = self.path_identity(hops)?;
        let owner = self.path_owner()?;
        owner.resolve_path(self.chain_id, &identity)
    }

    /// The hop signature for a path request, with every hop resolved to the
    /// session's canonical pool object. This is where "validated pool
    /// references" is enforced on the registry side.
    fn path_identity(&self, hops: &[(PoolIdentity, bool)]) -> Result<PathIdentity, ObjectRefusal> {
        hops.iter()
            .map(|(identity, zero_for_one)| {
                self.resolve_pool(identity)
                    .map(|pool| PathHop::new(pool, *zero_for_one))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(PathIdentity::new)
    }

    /// The installed path-identity owner.
    fn path_owner(&self) -> Result<&Arc<dyn PathObjectAdapter>, ObjectRefusal> {
        self.paths.get().ok_or(ObjectRefusal::NoPathOwner)
    }
}
