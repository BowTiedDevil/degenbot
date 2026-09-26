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
//! not a filtered view of the same one. The first cut is pools and ERC-20
//! tokens; paths and positions are not kinds here yet.

use std::fmt;
use std::sync::Arc;

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

    /// The address component of the identity.
    ///
    /// For V2/V3 this is the pool's own contract address. For V4 it is the
    /// `PoolManager` contract, which is NOT a pool address — a V4 consumer
    /// naming a pool reads [`Self::v4_pair`].
    #[must_use]
    pub const fn address(&self) -> Address {
        match self {
            Self::V2(address) | Self::V3(address) => *address,
            Self::V4 { pool_manager, .. } => *pool_manager,
        }
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
            Self::V2(..) | Self::V3(..) => None,
        }
    }

    /// The family tag (`"v2"` / `"v3"` / `"v4"`) — the same tag vocabulary
    /// [`BotState::pool_family`](crate::bot_core::BotState::pool_family)
    /// reports, so an identity-to-live-state join reads one family name.
    #[must_use]
    pub const fn family_tag(&self) -> &'static str {
        match self {
            Self::V2(..) => "v2",
            Self::V3(..) => "v3",
            Self::V4 { .. } => "v4",
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
}

/// One session's pool object: canonical identity, and nothing else.
///
/// Deliberately not a pool. Pool behavior and live state stay on the existing
/// pool types — the V2/V3/V4 state structs in `degenbot-pools`, registered and
/// advanced through [`BotState`]. This object is the stable name the session's
/// consumers agree on; it holds no reserves, no liquidity, no journal, and no
/// I/O handle.
#[derive(Clone, Debug, PartialEq, Eq)]
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
/// the session's pools and tokens.
///
/// The registry is the single growth path — get-or-create — so a second
/// registration seam cannot produce a second object for one identity, and a
/// racing registration converges on the first rather than duplicating it. It
/// holds no provider, DB handle, tick fetcher, or construction I/O, and it
/// never writes [`BotState`]; it is identity only.
#[derive(Debug)]
pub struct SessionObjectRegistry {
    /// The chain this session is scoped to. Identity is session-local, so this
    /// is a property of the registry rather than a key component: a registry
    /// built for another chain is a different key space.
    chain_id: u64,
    /// Pool identities → the one canonical object per identity.
    pools: DashMap<PoolIdentity, Arc<PoolObject>>,
    /// Token identities → the one canonical object per identity.
    tokens: DashMap<TokenIdentity, Arc<TokenObject>>,
}

impl SessionObjectRegistry {
    /// A registry scoped to `chain_id`, holding no objects yet.
    #[must_use]
    pub fn new(chain_id: u64) -> Self {
        Self {
            chain_id,
            pools: DashMap::new(),
            tokens: DashMap::new(),
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
        Arc::clone(entry.value())
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
}
