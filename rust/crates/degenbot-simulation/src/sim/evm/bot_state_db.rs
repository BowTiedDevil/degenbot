//! `BotStateDb` — a thin `revm::DatabaseRef` wrapper over the RPC fallback.
//!
//! ## What this is today
//!
//! A forwarding newtype: every `DatabaseRef` method delegates to the
//! `fallback` (`WrapDatabaseAsync<AlloyDB>` in production). It does NOT serve
//! typed pool state from `Bot`'s registry — that path (option B, "the engine
//! state IS the EVM's `Database`") was deliberately not wired: serving the
//! snapshot's V2 reserves / V3 `slot0` against the on-chain slots the pool's
//! own `swap()` reads (fee growth, tick bitmap, `IERC20.balanceOf`) produced
//! K-invariant / `LOK` reverts from stale-vs-fresh state divergence. See the
//! historical note below.
//!
//! The wrapper persists because the live `BlockSimHandle` chain
//! (`simulator.rs`) references it as the `CacheDB` backing; collapsing it to
//! bare `WrapDatabaseAsync<AlloyDB>` is the Tier 1 refactor's scope
//! (ergo task `V5HCR5`), not this module's cleanup.
//!
//! ## Historical note — the retired slot encoders
//!
//! This module previously held a toolkit of slot encoders
//! (`encode_v2_reserves_slot`, `encode_v3_slot0`, `encode_v3_liquidity_slot`,
//! `encode_v3_tick_info_slot`, `tick_mapping_slot`, `sign_extend_*`) plus
//! `read_v2_slot`/`read_v3_slot`/`read_tracked_storage`/`SnapshotError`. These
//! are deleted (no consumer; the `read_*_slot` paths returned `None` so
//! `storage_ref` always fell through). If Tier 2 option B is ever pursued,
//! the encoders must be re-derived from the Solidity storage layout
//! — the old `parity_diagnostic_encoding.rs` tests that pinned them are also
//! gone. The default Tier 2 disposition is C (reject), so re-derivation is
//! not on any current path.
//!
//! ## Composition
//!
//! ```text
//! EVM transact -> CacheDB (sim-scoped overrides)
//!                 -> BotStateDb (forwarding wrapper; option B seam)
//!                 -> WrapDatabaseAsync<AlloyDB> (RPC fallback)
//! ```

use alloy::primitives::Address;
use degenbot_bot::bot_core::BotState;
use revm::database_interface::DatabaseRef;
use revm::primitives::{StorageKey, StorageValue, B256};
use revm::state::AccountInfo;

/// A thin `DatabaseRef` wrapper that forwards every read to the `fallback`.
///
/// Currently a no-op pass-through. The `bot_state` borrow is the option B
/// seam (the typed-state serving path); it is NOT read by any method today.
/// Whether this wrapper persists or collapses to bare `WrapDatabaseAsync<
/// AlloyDB>` is decided by the Tier 1 refactor (ergo task `V5HCR5`).
pub struct BotStateDb<'bot, ExtDb>
where
    ExtDb: DatabaseRef,
{
    /// The `Bot` typed-state read view. Retained as the option B seam; NOT
    /// read by the current forwarding impl.
    #[allow(dead_code)]
    pub bot_state: &'bot BotState,
    /// The RPC cold-miss fallback (`WrapDatabaseAsync<AlloyDB>` in production).
    pub fallback: ExtDb,
}

impl<'bot, ExtDb> BotStateDb<'bot, ExtDb>
where
    ExtDb: DatabaseRef,
{
    /// Wrap the cold-miss fallback `DatabaseRef`. The `bot_state` borrow is
    /// the option B seam (currently unread).
    #[must_use]
    pub fn new(bot_state: &'bot BotState, fallback: ExtDb) -> Self {
        Self {
            bot_state,
            fallback,
        }
    }
}

impl<ExtDb> DatabaseRef for BotStateDb<'_, ExtDb>
where
    ExtDb: DatabaseRef,
{
    type Error = ExtDb::Error;

    /// Forward to the fallback. The engine does not serve account info from
    /// typed state; cold-loaded once, cached by the outer `CacheDB`.
    ///
    /// # Errors
    ///
    /// Returns the fallback's error if the RPC `basic` fetch fails.
    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        self.fallback.basic_ref(address)
    }

    /// Forward to the fallback. Tracked-pool storage (V2 slot 8, V3 `slot0`/
    /// `liquidity`/`ticks`) is served from the RPC, NOT the snapshot — see
    /// the module doc for the K-invariant / stale-state-divergence reason.
    ///
    /// # Errors
    ///
    /// Returns the fallback's error if the RPC fetch fails.
    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        self.fallback.storage_ref(address, index)
    }

    /// Forward to the fallback. `code_by_hash` is **never invoked** if
    /// `basic` eagerly loads code (the spike-verified
    /// `code_by_hash` panic-safety invariant).
    fn code_by_hash_ref(&self, code_hash: B256) -> Result<revm::bytecode::Bytecode, Self::Error> {
        self.fallback.code_by_hash_ref(code_hash)
    }

    /// Block hashes are not in `Bot`'s domain — always fall through to
    /// `AlloyDB` (the live-network axis).
    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        self.fallback.block_hash_ref(number)
    }
}
