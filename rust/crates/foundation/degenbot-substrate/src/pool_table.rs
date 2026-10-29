//! The pool table: one owner for pool identity.
//!
//! The registry core composed the pool tables beside the token registry as
//! plain fields, so every module in the crate could reach them and each
//! registration arm reproduced identity allocation — bump the id counter,
//! insert the entry, bind the key — plus its own duplicate policy (a typed
//! error, an `assert!` panic, or a pre-check upstream written only to stop
//! the panic). [`PoolTable`] owns the tables instead, and the operations that
//! change identity are its own: [`PoolTable::allocate`] is the one monotonic
//! allocation and key binding, [`PoolTable::register`] the one duplicate
//! refusal, [`PoolTable::quarantine`] / [`PoolTable::release`] the one
//! transition that dirties a pool's state nonce.
//!
//! What stays a read is a read: a family dispatch borrows one entry by id
//! (`get` / `get_mut`) or resolves one key (`pool_id_by_address`,
//! `pool_id_by_key`), and both key shapes answer from the indexes registration
//! writes — so a caller holding an id, an address, or a `(PoolManager,
//! pool_id)` pair cannot reach a second mirror of the table.

use alloy::primitives::Address;
use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_pools::RegisteredPoolFamily;
use hashbrown::{HashMap, HashSet};

use crate::pool_lookup::PoolLookup;
use crate::registration_door::RegistrationVerdict;
use crate::{PoolEntry, RegisterRefusal};

/// What a registration claims: the identity a pool is keyed by, and the family
/// its verdict carries.
///
/// The table never sees a family's params — an arm claims the slot its params
/// name and hands over the entry it constructed — so no request's tick map or
/// balance vector is cloned to claim one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PoolClaim {
    /// The identity the pool is registered under.
    pub(crate) key: PoolLookup,
    /// The family the verdict tags.
    pub(crate) family: RegisteredPoolFamily,
}

impl PoolClaim {
    /// The claim of one identity for one family.
    #[must_use]
    pub(crate) const fn new(key: PoolLookup, family: RegisteredPoolFamily) -> Self {
        Self { key, family }
    }
}

/// The one owner of pool identity: the entry table, the two key indexes, the
/// id counter, the reorg journal depth every family is constructed with, and
/// the ADR-040 quarantine set.
pub(crate) struct PoolTable {
    /// `pool_id` -> the registered entry.
    pools: HashMap<u64, PoolEntry>,
    /// Pool contract address -> `pool_id` for every address-keyed family.
    pool_addresses: HashMap<Address, u64>,
    /// `(pool_manager, pool_id)` -> `pool_id` for the V4 family, which one
    /// `PoolManager` hosts many pools under.
    v4_pool_ids: HashMap<(Address, V4PoolId), u64>,
    /// The next id to hand out; monotonic, so a retired id is never reused.
    next_pool_id: u64,
    /// Reorg journal depth (in blocks) for every pool — one mainnet epoch by
    /// default (ADR-003). Applied uniformly to every family.
    journal_depth: usize,
    /// ADR-040 quarantine set: pool ids currently EXCLUDED from solve
    /// resolution (tainted-by-desync surfaces). Family-agnostic — the gate sits
    /// in the projection dispatcher, not per state type — so the set spans
    /// every family this table registers.
    quarantined_pools: HashSet<u64>,
}

impl PoolTable {
    /// An empty table whose families construct with `journal_depth`.
    #[must_use]
    pub(crate) fn new(journal_depth: usize) -> Self {
        Self {
            pools: HashMap::new(),
            pool_addresses: HashMap::new(),
            v4_pool_ids: HashMap::new(),
            next_pool_id: 1,
            journal_depth,
            quarantined_pools: HashSet::new(),
        }
    }

    /// The reorg journal depth every family's state is constructed with.
    #[must_use]
    pub(crate) const fn journal_depth(&self) -> usize {
        self.journal_depth
    }

    /// The number of registered pools.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.pools.len()
    }

    /// Is this pool id registered?
    #[must_use]
    pub(crate) fn contains(&self, pool_id: u64) -> bool {
        self.pools.contains_key(&pool_id)
    }

    /// The registered entry for `pool_id`.
    #[must_use]
    pub(crate) fn get(&self, pool_id: u64) -> Option<&PoolEntry> {
        self.pools.get(&pool_id)
    }

    /// The registered entry for `pool_id`, mutably.
    pub(crate) fn get_mut(&mut self, pool_id: u64) -> Option<&mut PoolEntry> {
        self.pools.get_mut(&pool_id)
    }

    /// Every registered entry, family-dispatch order.
    pub(crate) fn values(&self) -> impl Iterator<Item = &PoolEntry> {
        self.pools.values()
    }

    /// Every registered pool id.
    pub(crate) fn keys(&self) -> impl Iterator<Item = u64> + '_ {
        self.pools.keys().copied()
    }

    /// Every registered `(pool_id, entry)` pair — the family-census iteration
    /// for a projection that needs the id beside the entry it borrows.
    pub(crate) fn entries(&self) -> impl Iterator<Item = (u64, &PoolEntry)> + '_ {
        self.pools.iter().map(|(&pool_id, entry)| (pool_id, entry))
    }

    /// The pool registered under this identity, or `None` when the key names no
    /// registration. The one resolution both key shapes answer from.
    #[must_use]
    pub(crate) fn pool_id_for(&self, key: &PoolLookup) -> Option<u64> {
        match key {
            PoolLookup::Address(address) => self.pool_id_by_address(address),
            PoolLookup::V4 {
                pool_manager,
                pool_id,
            } => self.pool_id_by_key(*pool_manager, pool_id),
        }
    }

    /// The pool registered at this contract address — the address-keyed
    /// families' index. V4 pools are not address-keyed and never answer here.
    #[must_use]
    pub(crate) fn pool_id_by_address(&self, address: &Address) -> Option<u64> {
        self.pool_addresses.get(address).copied()
    }

    /// The V4 pool registered under this `(pool_manager, pool_id)` pair.
    #[must_use]
    pub(crate) fn pool_id_by_key(&self, pool_manager: Address, pool_id: &V4PoolId) -> Option<u64> {
        self.v4_pool_ids.get(&(pool_manager, *pool_id)).copied()
    }

    /// The address index, for a projection that keys by contract address.
    pub(crate) fn address_index(&self) -> impl Iterator<Item = (&Address, &u64)> {
        self.pool_addresses.iter()
    }

    /// A snapshot of the address index — the sim-anchor projection's tracked-pool
    /// set, taken under the state lock and read after it is released.
    #[must_use]
    pub(crate) fn tracked_addresses(&self) -> HashMap<Address, u64> {
        self.pool_addresses.clone()
    }

    /// The registered V4 pools: `(pool_manager, pool_id)` -> internal pool id.
    pub(crate) fn v4_entries(&self) -> impl Iterator<Item = (&(Address, V4PoolId), &u64)> {
        self.v4_pool_ids.iter()
    }

    /// The number of pools registered under a `(pool_manager, pool_id)` key.
    #[must_use]
    pub(crate) fn v4_count(&self) -> usize {
        self.v4_pool_ids.len()
    }

    /// Register `entry` under `claim`, or refuse the duplicate.
    ///
    /// The ONE duplicate policy: a key the table already holds is refused as
    /// [`RegisterRefusal::AlreadyRegistered`] with the claimed identity, and
    /// nothing is inserted. Every family's registration reaches this, so no
    /// family can answer a duplicate differently from another.
    #[must_use]
    pub(crate) fn register(&mut self, claim: PoolClaim, entry: PoolEntry) -> RegistrationVerdict {
        if self.pool_id_for(&claim.key).is_some() {
            return RegistrationVerdict::refused(claim.family, already_registered(&claim.key));
        }
        RegistrationVerdict::registered(self.allocate(claim, entry), claim.family)
    }

    /// Allocate the next pool id and bind `claim`'s key to it in one step.
    ///
    /// Ids are monotonic: a retired id is never handed out again, so a handle
    /// that outlived its registration can never name a different pool. A caller
    /// that has established the key is free ([`Self::register`] does) allocates
    /// directly; the debug assertions pin that precondition.
    pub(crate) fn allocate(&mut self, claim: PoolClaim, entry: PoolEntry) -> u64 {
        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;
        let displaced = match claim.key {
            PoolLookup::Address(address) => self.pool_addresses.insert(address, pool_id),
            PoolLookup::V4 {
                pool_manager,
                pool_id: v4_pool_id,
            } => self.v4_pool_ids.insert((pool_manager, v4_pool_id), pool_id),
        };
        debug_assert!(
            displaced.is_none(),
            "allocation takes a key the table does not already hold"
        );
        debug_assert!(
            self.pools.insert(pool_id, entry).is_none(),
            "allocation takes a fresh pool id"
        );
        pool_id
    }

    /// Remove the pool registered under `key` — its entry and its key binding.
    /// Returns the retired pool id, or `None` when the key names no
    /// registration. The id is retired, not recycled.
    pub(crate) fn unregister(&mut self, key: &PoolLookup) -> Option<u64> {
        let pool_id = match key {
            PoolLookup::Address(address) => self.pool_addresses.remove(address)?,
            PoolLookup::V4 {
                pool_manager,
                pool_id,
            } => self.v4_pool_ids.remove(&(*pool_manager, *pool_id))?,
        };
        self.pools.remove(&pool_id);
        Some(pool_id)
    }

    /// ADR-040: exclude the pool from solve resolution. Returns `true` when this
    /// call CHANGED the state — an unknown pool id and a repeat are both
    /// `false`, so a caller can log the transition and not the attempt.
    pub(crate) fn quarantine(&mut self, pool_id: u64) -> bool {
        self.set_quarantined(pool_id, true)
    }

    /// ADR-040: re-admit the pool to solve resolution. Returns `true` when this
    /// call CHANGED the state.
    pub(crate) fn release(&mut self, pool_id: u64) -> bool {
        self.set_quarantined(pool_id, false)
    }

    /// The ONE quarantine transition, both directions.
    ///
    /// The gate's whole value is that a cached hop projection or an in-flight
    /// solver snapshot taken before the transition cannot be used after it, so
    /// a transition that changed the set marks the pool dirty by bumping its
    /// `state_nonce`. That is why the two directions share this body: a release
    /// that skipped the bump would leave a stale `Invalid(Quarantined)` entry
    /// readable.
    fn set_quarantined(&mut self, pool_id: u64, quarantined: bool) -> bool {
        if quarantined && !self.pools.contains_key(&pool_id) {
            return false;
        }
        let changed = if quarantined {
            self.quarantined_pools.insert(pool_id)
        } else {
            self.quarantined_pools.remove(&pool_id)
        };
        if !changed {
            return false;
        }
        if let Some(entry) = self.pools.get_mut(&pool_id) {
            entry.bump_state_nonce();
        }
        true
    }

    /// ADR-040: is the pool currently quarantined (excluded from solve)?
    #[must_use]
    pub(crate) fn is_quarantined(&self, pool_id: u64) -> bool {
        self.quarantined_pools.contains(&pool_id)
    }

    /// ADR-040: the quarantine depth (the scrape gauge's backing count).
    #[must_use]
    pub(crate) fn quarantined_count(&self) -> usize {
        self.quarantined_pools.len()
    }
}

/// The ONE duplicate refusal: the claimed identity, its rendering the historical
/// registration message.
fn already_registered(key: &PoolLookup) -> RegisterRefusal {
    RegisterRefusal::AlreadyRegistered {
        address: match key {
            PoolLookup::Address(address) => Some(*address),
            PoolLookup::V4 { .. } => None,
        },
        pool_key: match key {
            PoolLookup::Address(_) => None,
            PoolLookup::V4 {
                pool_manager,
                pool_id,
            } => Some((*pool_manager, *pool_id)),
        },
        detail: format!("pool already registered: {key}"),
    }
}
