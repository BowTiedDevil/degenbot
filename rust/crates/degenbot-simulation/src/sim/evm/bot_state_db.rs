//! `BotStateDb` — a thin `revm::DatabaseRef` wrapper over the RPC fallback
//! with an env-gated serving seam + divergence probe.
//!
//! ## What this is today
//!
//! `storage_ref` forwards to the `fallback` (`WrapDatabaseAsync<AlloyDB>` in
//! production) and, between the fallback read and the return, runs two
//! DEFAULT-OFF env-gated diagnostics on tracked pool slots:
//!
//! - Divergence probe (`super::divergence_probe`): pure observation — compares
//!   the engine's packed typed state against the just-fetched RPC value, logs
//!   a `[sim-divergence]` line on mismatch, accumulates the tally. Never
//!   changes what the sim reads.
//! - Serving seam (`super::serving`): behavior change — for tracked slots the
//!   engine carries authoritatively, returns the engine's packed word instead
//!   of the RPC value. Gated OFF by default; enabling requires the engine to
//!   carry the FULL slot set the pool's `swap()` callback reads, or a partial
//!   serve reintroduces the documented K-invariant / `LOK` reverts (the engine
//!   doesn't carry `feeGrowthGlobal`/`tickBitmap`/per-pair balances that the
//!   same `swap()` callback reads).
//!
//! The serving seam is a POC retained for the divergence investigation. Its
//! premise ("stale engine state causes `CurrencyNotSettled`") was REFUTED by
//! mainnet data — V3 hops matched the actual swap output exactly (engine state
//! is correct) while only the V4 swap diverged by 1-8 units (a solver calc
//! rounding divergence, not stale state). See
//! `docs/architecture/sim_v4_swap_step_rounding.md`. The seam stays gated off
//! in production; it is a dead switch kept for future re-probing.
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
//! `storage_ref` always fell through). Re-derivation (if a serving path is
//! re-pursued) starts from the Solidity storage layout — the old
//! `parity_diagnostic_encoding.rs` tests that pinned them are also gone.
//!
//! ## Composition
//!
//! ```text
//! EVM transact -> CacheDB (sim-scoped overrides)
//!                 -> BotStateDb (forwarding wrapper + serving seam)
//!                 -> WrapDatabaseAsync<AlloyDB> (RPC fallback)
//! ```

use alloy::primitives::Address;
use degenbot_bot::bot_core::BotState;
use revm::database_interface::DatabaseRef;
use revm::primitives::{StorageKey, StorageValue, B256};
use revm::state::AccountInfo;

/// A thin `DatabaseRef` wrapper that forwards every read to the `fallback`.
///
/// Forwards to the fallback, with env-gated serving + divergence-probe
/// diagnostics layered onto tracked pool slots (see the module docs). The
/// `bot_state` borrow backs both diagnostics. Whether this wrapper persists
/// or collapses to bare `WrapDatabaseAsync<AlloyDB>` is decided by the
/// Tier 1 refactor (ergo task `V5HCR5`).
pub struct BotStateDb<'bot, ExtDb>
where
    ExtDb: DatabaseRef,
{
    /// The `Bot` typed-state read view backing the serving seam + divergence
    /// probe (both env-gated, default off).
    pub bot_state: &'bot BotState,
    /// The RPC cold-miss fallback (`WrapDatabaseAsync<AlloyDB>` in production).
    pub fallback: ExtDb,
}

impl<'bot, ExtDb> BotStateDb<'bot, ExtDb>
where
    ExtDb: DatabaseRef,
{
    /// Wrap the cold-miss fallback `DatabaseRef`. The `bot_state` borrow backs
    /// the env-gated serving seam + divergence probe.
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

    /// Serve tracked-pool storage from the engine's typed state, falling
    /// through to the RPC for everything else.
    ///
    /// Two env-gated diagnostics run here, both DEFAULT OFF so production
    /// behavior is unchanged (single atomic load each — zero per-SLOAD work):
    ///
    /// - `DEGENBOT_SIM_DIVERGENCE_LOG=1` ([`super::divergence_probe`]): pure
    ///   observation — compares the engine's packed typed state against the
    ///   RPC value, logs a `[sim-divergence]` line on mismatch, accumulates
    ///   the tally. Never changes what the sim reads.
    /// - `DEGENBOT_SIM_SERVE_ENGINE_STATE=1` ([`super::serving`]): the serving
    ///   seam — returns the engine's packed word for tracked slots
    ///   (V2 reserves / V3/V4 `slot0`/`liquidity`/`ticks(tick)`) instead of
    ///   the RPC value. Premise (stale state) was REFUTED; the seam stays
    ///   gated off in production (a partial serve reintroduces the documented
    ///   K-invariant / `LOK` reverts (the engine doesn't carry
    ///   `feeGrowthGlobal`/`tickBitmap`/per-pair balances the same swap
    ///   callback reads).
    ///
    /// # Errors
    ///
    /// Returns the fallback's error if the RPC fetch fails (the RPC value is
    /// always fetched even when serving is on, so it can be logged as the
    /// delta baseline).
    fn storage_ref(
        &self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        let rpc_value = self.fallback.storage_ref(address, index)?;
        // Observation first (env-gated, pure — compares engine vs RPC, logs
        // divergence, never changes what the sim reads). Independent of the
        // serving gate below.
        super::divergence_probe::observe_storage_read(self.bot_state, address, index, rpc_value);
        // Serving seam (env-gated, DEFAULT OFF): if `(address, index)` maps
        // to a tracked pool slot the engine carries authoritatively, return
        // the engine's packed word instead of the RPC value (the sim's swap
        // callback reads the engine's state, matching what the solver read).
        // Premise (stale engine state) was REFUTED — V3 hops matched exactly
        // while V4 diverged by 1-8 units (solver calc rounding, not state).
        // The seam stays gated off in production; a partial serve (slot0/
        // liquidity WITHOUT feeGrowth/bitmap) reintroduces the documented
        // K-invariant / LOK reverts.
        let served = super::serving::serve_tracked_slot(self.bot_state, address, index, rpc_value);
        Ok(served.unwrap_or(rpc_value))
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
