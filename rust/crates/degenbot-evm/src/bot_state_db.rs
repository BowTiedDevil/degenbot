//! `BotStateDb` — a `revm::DatabaseRef` impl over `Bot`'s typed pool state.
//!
//! The move that makes the engine state *be* the EVM's `Database` (option B,
//! chosen by the operator post-spike QGJGWI). A hand-written `DatabaseRef`
//! impl reads `Bot`'s typed pool state (`V2PoolState` reserves, `V3PoolState`/
//! `V4PoolState` `slot0`/`liquidity`/tick-data) and ABI-encodes it to EVM slots
//! **on demand** — no long-lived encoded copy. `WrapDatabaseAsync<AlloyDB>` is
//! the cold-miss fallback for contracts the engine does not track.
//!
//! `DatabaseRef` is `&self` (vs `Database`'s `&mut self`) — no `Mutex` needed
//! (`WrapDatabaseAsync<AlloyDB>` impls `DatabaseRef` via `&self`, blocking
//! internally on a tokio runtime). Composes under `CacheDB<BotStateDb<…>>`:
//!
//! ```text
//! EVM transact → CacheDB (sim-scoped overrides)
//!                 → BotStateDb (engine typed state, encode-on-demand)
//!                 → WrapDatabaseAsync<AlloyDB> (RPC fallback)
//! ```
//!
//! # Filled by task `EGMSNS`.
//!
//! See `docs/spikes/revm-composition-api-and-cold-miss-latency.md` §2.3 for
//! the verified composition shape + §4 for the `code_by_hash` panic-safety
//! invariant (`basic` must eagerly load code).

use revm::database_interface::DatabaseRef;
use revm::primitives::{Address, B256, U256};
use revm::state::AccountInfo;

/// The engine-state read view that is the EVM's `Database`.
///
/// Carries a `BotStateSnapshot` (the typed-state read view, populated by
/// `Bot::pool_state_snapshot`) + the cold-miss `fallback` (a
/// `WrapDatabaseAsync<AlloyDB>`). The `Snapshot` type parameter is filled by
/// task `EGMSNS` with the `Bot`-derived snapshot type.
///
/// # Filled by task `EGMSNS`.
#[derive(Debug)]
pub struct BotStateDb<Snap, ExtDb>
where
    Snap: BotStateSnapshot,
    ExtDb: DatabaseRef,
{
    /// The `Bot` typed-state read view, encoding typed fields to EVM slots
    /// on demand (single source of truth: the typed fields in `Bot`).
    pub snapshot: Snap,
    /// The RPC cold-miss fallback (`WrapDatabaseAsync<AlloyDB>` in production).
    pub fallback: ExtDb,
}

/// The engine-state read view trait — a `&self` snapshot of `Bot`'s typed pool
/// state that answers EVM-slot reads by encoding typed fields on demand.
///
/// Implemented by the `Bot`-derived snapshot type in task `EGMSNS`.
pub trait BotStateSnapshot: std::fmt::Debug + Send + Sync {
    /// Get account info (nonce + balance + [`Bytecode`] + `code_hash`) for the
    /// address, or `None` if not tracked.
    ///
    /// **Must eagerly load `code`** (the `code_by_hash` panic-safety invariant
    /// — see spike §4). Return full `AccountInfo { code }` with a correct
    /// `code_hash` for tracked contracts; untracked contracts return `None`
    /// (= fall through to the `AlloyDB` fallback).
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] if the account lookup fails (snapshot
    /// unreachable — e.g. RwLock poisoned) or signals an unmapped address.
    fn basic(&self, _address: Address) -> Result<Option<AccountInfo>, SnapshotError>;

    /// Get the storage value at `slot` for `address`, or `None` if not tracked.
    ///
    /// For tracked pools: `storage(pool, RESERVE0_SLOT)` reads
    /// `V2PoolState.reserves.0` + ABI-encodes to a 32-byte word; same for
    /// V3/V4 `slot0`/`liquidity`/tick slots. Untracked storage -> `None`
    /// (= fall through).
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::UnmappedSlot`] if a slot was requested that the
    /// snapshot does not map (signals a layout drift), or
    /// [`SnapshotError::AccountLookup`] if the account lookup fails.
    fn storage(&self, _address: Address, _slot: U256) -> Result<Option<U256>, SnapshotError>;
}

/// Errors raised by `BotStateSnapshot` reads (storage-layout lookup failures —
/// unreachable for valid slots, surfaces drift as a typed error).
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// A storage slot was requested that the snapshot does not map (signals a
    /// layout drift — the storage-layout mapping missed a slot the EVM read).
    #[error("Snapshot has no storage layout for slot {slot} on address {address}")]
    UnmappedSlot {
        /// The EVM storage slot index.
        slot: U256,
        /// The contract address.
        address: Address,
    },
    /// The account lookup failed (snapshot unreachable — e.g. RwLock poisoned).
    #[error("Snapshot account lookup failed: {0}")]
    AccountLookup(String),
}

impl<Snap, ExtDb> DatabaseRef for BotStateDb<Snap, ExtDb>
where
    Snap: BotStateSnapshot,
    ExtDb: DatabaseRef,
    <ExtDb as DatabaseRef>::Error: std::convert::From<SnapshotError>,
{
    type Error = ExtDb::Error;

    /// Tracked contracts served from the snapshot (encode-on-demand); untracked
    /// fall through to the `AlloyDB` fallback.
    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        match self.snapshot.basic(address) {
            Ok(Some(info)) => Ok(Some(info)),
            Ok(None) => self.fallback.basic_ref(address),
            Err(e) => Err(e.into()),
        }
    }

    /// `code_by_hash` is **never invoked** if `basic` eagerly loads code (the
    /// spike-verified `code_by_hash` panic-safety invariant). Falls through to
    /// the fallback for any (unreachable) cold-path.
    fn code_by_hash_ref(&self, code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        self.fallback.code_by_hash_ref(code_hash)
    }

    /// Tracked storage served from the snapshot; untracked fall through.
    fn storage_ref(
        &self,
        address: Address,
        index: revm::primitives::StorageKey,
    ) -> Result<revm::primitives::StorageValue, Self::Error> {
        match self.snapshot.storage(address, index) {
            Ok(Some(value)) => Ok(value),
            Ok(None) => self.fallback.storage_ref(address, index),
            Err(e) => Err(e.into()),
        }
    }

    /// Block hashes are not in `Bot`'s domain — always fall through to
    /// `AlloyDB` (the live-network axis).
    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.fallback.block_hash_ref(number)
    }
}
