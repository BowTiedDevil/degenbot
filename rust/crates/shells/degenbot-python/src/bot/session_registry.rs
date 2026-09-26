//! `SessionObject` — the Python projection of the Rust session object registry.
//!
//! Thin translation only. Everything a caller needs to NAME a pool or token is
//! converted here (address parsing, family tag, V4 pool id, chain-scope
//! check) and the answer is built here, but the decision — "is this the
//! identity this session already holds?" — belongs to
//! [`SessionObjectRegistry`], and the GIL is released around the registry call
//! so a Python-held GIL never sits in front of a session's one identity map.
//! No construction choreography lives on this seam: registering a live pool,
//! resolving token metadata, and reading state stay on the
//! `build_*_pool` / `get_pool` / `register_token` surfaces.
//!
//! A `SessionObject` is a read-only NAME, not an owner: the registry holds the
//! object for the session's lifetime, and two consumers that name one identity
//! get handles whose [`key`](PySessionObject::key) is equal. Python uses that
//! key to file the companion object it built; the key is minted here, so the
//! Python side never re-derives an identity of its own.

use crate::prelude::*;
use std::sync::Arc;

use alloy::primitives::Address;

use degenbot_bot::bot_core::session_registry::{
    ObjectRefusal, PoolIdentity, PoolObject, SessionObjectRegistry, TokenIdentity, TokenObject,
};

use super::{parse_address, PyBot};

/// A session object handle: the canonical identity of one pool or token, as the
/// session's registry names it.
///
/// Not a pool and not a token — no state, no provider, no journal. It is the
/// session's name for one object, and it holds no live data (that stays with
/// `BotState`), so it is safe to hold for the session and cheap to compare.
#[pyclass(name = "SessionObject", skip_from_py_object, module = "degenbot._ffi")]
pub struct PySessionObject {
    kind: &'static str,
    key: String,
    family: String,
    address: Address,
    chain_id: u64,
}

impl PySessionObject {
    /// The handle for a canonical pool object.
    fn from_pool(object: &PoolObject) -> Self {
        Self {
            kind: "pool",
            key: pool_key(object.identity()),
            family: object.family_tag().to_owned(),
            address: object.identity().address(),
            chain_id: object.chain_id(),
        }
    }

    /// The handle for a canonical token object.
    fn from_token(object: &TokenObject) -> Self {
        Self {
            kind: "token",
            key: token_key(object.identity()),
            family: "erc20".to_owned(),
            address: object.identity().address(),
            chain_id: object.chain_id(),
        }
    }
}

/// The canonical, session-unique name of a pool identity.
///
/// Minted from the identity the registry holds, never from the caller's own
/// spelling of it, so two consumers of one session always produce the same
/// key for the same pool. A V4 pool's key carries the
/// `(PoolManager, pool_id)` pair, because the manager address alone names many
/// pools.
fn pool_key(identity: &PoolIdentity) -> String {
    match identity.v4_pair() {
        Some((pool_manager, pool_id)) => format!(
            "pool:v4:{pool_manager}/{}",
            alloy::hex::encode_prefixed(pool_id)
        ),
        None => format!("pool:{}:{}", identity.family_tag(), identity.address()),
    }
}

/// The canonical, session-unique name of a token identity.
fn token_key(identity: &TokenIdentity) -> String {
    format!("token:erc20:{}", identity.address())
}

/// The family tags this seam can name, for an error that points at the fix.
const KNOWN_POOL_FAMILIES: &str =
    "v2, v3, v4, curve, balancer-weighted, balancer-stable, aerodrome-v2";

/// Convert a Python request into a canonical pool identity.
///
/// `address` is the pool's own contract address for every address-keyed
/// family, and the `PoolManager` for `v4` (whose identity is the
/// `(PoolManager, pool_id)` pair, so `pool_id` is required there and refused
/// everywhere else). The family tag is the vocabulary
/// `BotState::pool_family` reports, which is what a Python pool companion
/// carries on its live handle.
fn to_pool_identity(
    family: &str,
    address: &str,
    pool_id: Option<Vec<u8>>,
) -> PyResult<PoolIdentity> {
    let parsed = parse_address(address)?;
    if family == "v4" {
        let Some(pool_id) = pool_id else {
            return Err(pyo3::exceptions::PyValueError::new_err(
                "a v4 pool is named by (PoolManager, pool_id): pass pool_id",
            ));
        };
        let bytes: [u8; 32] = pool_id.as_slice().try_into().map_err(|_| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "a v4 pool_id is 32 bytes, got {}",
                pool_id.len()
            ))
        })?;
        return Ok(PoolIdentity::v4(parsed, bytes));
    }
    if pool_id.is_some() {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "only the v4 family is keyed by a pool id (got family {family:?} with one)"
        )));
    }
    PoolIdentity::for_address_family(family, parsed).ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "unknown pool family {family:?}; known families: {KNOWN_POOL_FAMILIES}"
        ))
    })
}

/// A resolve miss is the Python surface's `None`, not an exception: a registry
/// read is a question, and "this session does not hold that object" is its
/// answer. Every refusal variant is a "no" to that question, so the mapping is
/// total; every other failure stays an exception. The typed refusal is
/// deliberately NOT surfaced here — a Python caller that asked a yes/no
/// question about one identity has no branch to run on which kind of miss it
/// was.
fn pool_refusal_to_none(refusal: &ObjectRefusal) -> Option<PySessionObject> {
    match refusal {
        ObjectRefusal::UnknownPoolIdentity { .. }
        | ObjectRefusal::UnknownPoolAddress { .. }
        | ObjectRefusal::UnknownTokenIdentity { .. } => None,
    }
}

#[pymethods]
impl PySessionObject {
    /// The canonical, session-unique name of this object.
    ///
    /// Equal for every consumer that names this identity in this session, and
    /// unequal for every other object — the string a Python companion cache
    /// files its presentation handle under.
    #[getter]
    fn key(&self) -> &str {
        &self.key
    }

    /// `"pool"` or `"token"` — which kind of session object this names.
    #[getter]
    fn kind(&self) -> &str {
        self.kind
    }

    /// The family tag: a pool's registration family, or `"erc20"` for a token.
    #[getter]
    fn family(&self) -> &str {
        &self.family
    }

    /// The address component: the pool's own address, or for a V4 pool the
    /// `PoolManager` (which is not a pool address).
    #[getter]
    fn address(&self) -> String {
        self.address.to_string()
    }

    /// The chain whose session registered this object.
    #[getter]
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn __repr__(&self) -> String {
        format!("SessionObject({})", self.key)
    }
}

#[pymethods]
impl PyBot {
    /// Get-or-create: the session's canonical pool object for this identity,
    /// registering it if the session does not hold it yet. Any number of calls
    /// for one identity return the one object.
    #[pyo3(signature = (chain_id, family, address, pool_id=None))]
    fn get_or_create_session_pool(
        &self,
        py: Python<'_>,
        chain_id: u64,
        family: &str,
        address: &str,
        pool_id: Option<Vec<u8>>,
    ) -> PyResult<PySessionObject> {
        let identity = to_pool_identity(family, address, pool_id)?;
        self.check_session_chain(py, chain_id)?;
        let object = py.detach(|| self.bot.session_registry().get_or_create_pool(identity));
        Ok(PySessionObject::from_pool(&object))
    }

    /// The session's canonical pool object for this identity, or `None` when
    /// this session does not hold it. Registers nothing.
    #[pyo3(signature = (chain_id, family, address, pool_id=None))]
    fn resolve_session_pool(
        &self,
        py: Python<'_>,
        chain_id: u64,
        family: &str,
        address: &str,
        pool_id: Option<Vec<u8>>,
    ) -> PyResult<Option<PySessionObject>> {
        let identity = to_pool_identity(family, address, pool_id)?;
        self.check_session_chain(py, chain_id)?;
        let resolved = py.detach(|| self.bot.session_registry().resolve_pool(&identity));
        match resolved {
            Ok(object) => Ok(Some(PySessionObject::from_pool(&object))),
            Err(refusal) => Ok(pool_refusal_to_none(&refusal)),
        }
    }

    /// The session's canonical pool object at `address`, or `None` when no
    /// address-keyed identity in this session claims it.
    ///
    /// The read surface for a caller that knows a pool's address but not its
    /// family. It names the identity that registered the address first; a V4
    /// pool is never named here (a `PoolManager` is not a pool address).
    fn resolve_session_pool_by_address(
        &self,
        py: Python<'_>,
        chain_id: u64,
        address: &str,
    ) -> PyResult<Option<PySessionObject>> {
        let parsed = parse_address(address)?;
        self.check_session_chain(py, chain_id)?;
        let resolved = py.detach(|| self.bot.session_registry().resolve_pool_by_address(&parsed));
        match resolved {
            Ok(object) => Ok(Some(PySessionObject::from_pool(&object))),
            Err(refusal) => Ok(pool_refusal_to_none(&refusal)),
        }
    }

    /// Get-or-create: the session's canonical token object for this ERC-20
    /// address, registering it if the session does not hold it yet.
    fn get_or_create_session_token(
        &self,
        py: Python<'_>,
        chain_id: u64,
        address: &str,
    ) -> PyResult<PySessionObject> {
        let identity = TokenIdentity::erc20(parse_address(address)?);
        self.check_session_chain(py, chain_id)?;
        let object = py.detach(|| self.bot.session_registry().get_or_create_token(identity));
        Ok(PySessionObject::from_token(&object))
    }

    /// The session's canonical token object for this address, or `None` when
    /// this session does not hold it. Registers nothing.
    fn resolve_session_token(
        &self,
        py: Python<'_>,
        chain_id: u64,
        address: &str,
    ) -> PyResult<Option<PySessionObject>> {
        let identity = TokenIdentity::erc20(parse_address(address)?);
        self.check_session_chain(py, chain_id)?;
        let resolved = py.detach(|| self.bot.session_registry().resolve_token(&identity));
        match resolved {
            Ok(object) => Ok(Some(PySessionObject::from_token(&object))),
            Err(refusal) => Ok(pool_refusal_to_none(&refusal)),
        }
    }

    /// `(pool_count, token_count)` — how many canonical objects this session
    /// holds. One entry per identity, never one per request.
    fn session_object_counts(&self, py: Python<'_>) -> (usize, usize) {
        py.detach(|| {
            let registry: Arc<SessionObjectRegistry> = self.bot.session_registry();
            (registry.pool_count(), registry.token_count())
        })
    }
}

impl PyBot {
    /// Refuse a request scoped to another chain. Identity is session-local, so
    /// a session is one chain (ADR-006 D5): naming a different chain is a
    /// caller bug, not a second key space this seam will quietly mint.
    fn check_session_chain(&self, py: Python<'_>, chain_id: u64) -> PyResult<()> {
        let session_chain = py.detach(|| self.bot.session_registry().chain_id());
        if chain_id == session_chain {
            Ok(())
        } else {
            Err(pyo3::exceptions::PyValueError::new_err(format!(
                "chain_id {chain_id} is not this session's chain ({session_chain}); a session object registry is scoped to one chain"
            )))
        }
    }
}
