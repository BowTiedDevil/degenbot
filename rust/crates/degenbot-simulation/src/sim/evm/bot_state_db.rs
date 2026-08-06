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
use revm::primitives::{StorageKey, StorageValue, B256, KECCAK_EMPTY};
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

    /// Forward to the fallback, with a LOUD invalidity tripwire for tracked
    /// pools (ADR-021: detect/classify/stop loudly, never auto-repair).
    ///
    /// # Errors
    ///
    /// Returns the fallback's error if the RPC `basic` fetch fails.
    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let info = self.fallback.basic_ref(address)?;
        // A tracked pool address MUST resolve to a contract with code. If the
        // RPC fallback reports it as non-existent (`None`) or code-less
        // (``KECCAK_EMPTY``), every later per-block sim would execute the pool
        // as an EOA → zero reserves → the mysterious empty-revert family
        // (path-205: `0xb01C29F3` BNB/WETH read `reserve0=0` in sim while
        // healthy on-chain). A registered pool can never legitimately be
        // code-less, so this is an impossibility — fail loudly HERE. Because
        // this is the value-origin point (the warm cache only caches what this
        // returns), the poisoned `None`/empty can never reach the warm cache
        // to be served on a later block's hit.
        if self.bot_state.pool_id_by_address(&address).is_some() {
            let invalid = match &info {
                None => true,
                Some(acc) => acc.code_hash == KECCAK_EMPTY,
            };
            if invalid {
                let state = if info.is_none() {
                    "non-existent (None)"
                } else {
                    "code-less (KECCAK_EMPTY)"
                };
                panic!(
                    "Sim DB invariant: tracked pool {address} resolved as {state} by the \
                     RPC fallback — refusing to simulate a code-less pool"
                );
            }
        }
        Ok(info)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use alloy::primitives::aliases::U112;
    use alloy::primitives::{Address, U256};
    use degenbot_bot::bot_core::{BotState, RegisterV2PoolParams};
    use degenbot_uniswap::dex_identity::DexVariant;
    use revm::bytecode::Bytecode;
    use revm::primitives::B256;
    use std::cell::Cell;

    /// The tracked-pool address (matches the bot-core test fixture shape).
    const POOL: Address = Address::new([0xaa; 20]);
    /// A NON-pool address — an EOA / warmup-victim basic read must stay exempt.
    const EOA: Address = Address::new([0xee; 20]);

    /// A `BotState` with one tracked V2 pool at [`POOL`], so
    /// `pool_id_by_address(POOL)` resolves (and `EOA` does not).
    fn bot_state_with_pool() -> BotState {
        let mut core = BotState::new();
        core.register_v2_pool(&RegisterV2PoolParams {
            address: POOL,
            token0: Address::from([0xbb; 20]),
            token1: Address::from([0xcc; 20]),
            reserve0: U112::from(1000),
            reserve1: U112::from(2000),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::from([0xdd; 20]),
            update_block: 0,
            variant: DexVariant::UniswapV2,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        })
        .expect("test setup: V2 registration");
        core
    }

    /// A minimal `DatabaseRef` fallback whose `basic_ref` returns the test's
    /// scripted result and counts calls.
    struct ScriptedDb {
        result: Option<AccountInfo>,
        calls: Cell<u64>,
    }
    impl DatabaseRef for ScriptedDb {
        type Error = core::convert::Infallible;
        fn basic_ref(&self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.result.clone())
        }
        fn code_by_hash_ref(&self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::new_legacy(alloy::primitives::Bytes::new()))
        }
        fn storage_ref(
            &self,
            _address: Address,
            _index: StorageKey,
        ) -> Result<StorageValue, Self::Error> {
            Ok(StorageValue::ZERO)
        }
        fn block_hash_ref(&self, _number: u64) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    fn code_info() -> AccountInfo {
        let code = Bytecode::new_legacy(alloy::primitives::Bytes::from_static(&[0x60, 0x00]));
        AccountInfo::new(U256::from(1), 0, code.hash_slow(), code)
    }
    fn codeless_info() -> AccountInfo {
        AccountInfo::new(U256::from(1), 0, KECCAK_EMPTY, Bytecode::default())
    }

    // ── Green 1: a tracked pool resolving to `None` (non-existent) PANICS ──

    #[test]
    #[should_panic(expected = "Sim DB invariant")]
    fn tracked_pool_resolving_none_panics() {
        let core = bot_state_with_pool();
        let db = ScriptedDb {
            result: None,
            calls: Cell::new(0),
        };
        let bsd = BotStateDb::new(&core, db);
        let _ = bsd.basic_ref(POOL).unwrap();
    }

    // ── Green 2: a tracked pool resolving to CODE-LESS PANICS ──

    #[test]
    #[should_panic(expected = "Sim DB invariant")]
    fn tracked_pool_resolving_codeless_panics() {
        let core = bot_state_with_pool();
        let db = ScriptedDb {
            result: Some(codeless_info()),
            calls: Cell::new(0),
        };
        let bsd = BotStateDb::new(&core, db);
        let _ = bsd.basic_ref(POOL).unwrap();
    }

    // ── Green 3: a tracked pool resolving WITH code passes through ──

    #[test]
    fn tracked_pool_with_code_passes_through() {
        let core = bot_state_with_pool();
        let db = ScriptedDb {
            result: Some(code_info()),
            calls: Cell::new(0),
        };
        let bsd = BotStateDb::new(&core, db);
        let got = bsd.basic_ref(POOL).unwrap();
        assert!(got.is_some(), "tracked pool with code is forwarded");
        assert_ne!(got.unwrap().code_hash, KECCAK_EMPTY);
    }

    // ── Green 4: NON-pool addresses (EOA/warmup) are EXEMPT from the tripwire ──

    #[test]
    fn non_pool_none_is_exempt() {
        let core = bot_state_with_pool();
        let db = ScriptedDb {
            result: None,
            calls: Cell::new(0),
        };
        let bsd = BotStateDb::new(&core, db);
        // EOA is not a pool: `None` is a legitimate EOA read, forwarded no-panic.
        assert!(bsd.basic_ref(EOA).unwrap().is_none());
    }
}
