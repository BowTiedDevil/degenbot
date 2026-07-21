//! Aerodrome V2 pool state — the `BotState`-owned home for an Aerodrome V2
//! pool (Solidly-style constant-product AMM on Base / Arbitrum).
//!
//! ADR-005 slice — the Aerodrome state port: Aerodrome reserves + reorg
//! journal move OUT of the Python `StateCache` into Rust (the same migration
//! V2 underwent). Adds the Aerodrome family as a seventh `PoolEntry`
//! variant.
//!
//! ## State shape
//!
//! Aerodrome V2 pools are Solidly-style: two tokens, a **unidirectional**
//! fee (the same fraction applies in both swap directions), and a
//! `stable` flag selecting the Solidly stable invariant (vs. the volatile
//! constant-product invariant). Reserves are two `U256` values — same shape
//! as V2 — so the reorg journal reuses [`crate::state_history::V2BlockDelta`]
//! (a genesis anchor + one delta per `external_update`).

use alloy::primitives::{aliases::U112, Address};

use crate::state_history::{JournalError, ReorgJournal, ReorgPoolState, V2BlockDelta};
use degenbot_uniswap::dex_identity::DexVariant;

// ---------------------------------------------------------------------------
// Identity + state + params
// ---------------------------------------------------------------------------

/// Immutable Aerodrome V2 registration identity (ADR-005 identity slice).
///
/// Pure registration data — the pool address, tokens, factory, variant
/// (`AerodromeV2Volatile` / `AerodromeV2Stable`), `stable` mode flag, and the
/// unidirectional fee `(fee_numer, fee_denom)`. Set once at
/// `register_aerodrome_pool` and never mutated. Mirrors [`V2PoolIdentity`].
///
/// The fee is the **fee fraction** (`amount_in_after_fee = amount_in - amount_in * fee_numer / fee_denom`),
/// matching [`degenbot_solidly_math::calc_exact_in_volatile`]'s convention —
/// NOT the V2 retained-fraction `(gamma_numer, fee_denom)` convention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AerodromeV2PoolIdentity {
    /// Pool contract address.
    pub address: Address,
    /// Token0 contract address.
    pub token0: Address,
    /// Token1 contract address.
    pub token1: Address,
    /// Factory contract address.
    pub factory: Address,
    /// DEX+variant discriminator (`AerodromeV2Volatile` or `AerodromeV2Stable`).
    pub variant: DexVariant,
    /// Solidly stable-invariant flag (false ⇒ volatile constant-product).
    pub stable: bool,
    /// Unidirectional fee: `(fee_numer, fee_denom)`. The same fraction applies
    /// to both swap directions.
    pub fee: (u64, u64),
}

/// Mutable runtime state for an Aerodrome V2 pool.
///
/// Pure mutable data — the values that change as reserves are pumped and
/// rolled back. Immutable identity lives on [`AerodromeV2PoolIdentity`]; look
/// it up via [`crate::BotState::get_aerodrome_identity`]. The two
/// are paired in [`crate::PoolEntry::AerodromeV2`].
///
/// Reserves are typed `U112` to mirror the on-chain `uint112` storage width
/// (Solidly / Aerodrome pair contracts mirror v2-core's
/// `Sync(uint112,uint112)` reserve shape). See [`crate::V2PoolState`] for the
/// type-level enforcement rationale (ADR-012 / epic `ZPHT6X`).
#[derive(Clone, Debug)]
pub struct AerodromeV2PoolState {
    /// Current reserve of token0 (on-chain `uint112`).
    pub reserve0: U112,
    /// Current reserve of token1 (on-chain `uint112`).
    pub reserve1: U112,
    /// Block number of the last update.
    pub update_block: u64,
    /// Reorg journal — reuses the V2 delta shape (two reserves).
    pub journal: ReorgJournal<V2BlockDelta>,
}

/// Parameters for registering an Aerodrome V2 pool with `BotState`.
#[derive(Clone, Debug)]
pub struct RegisterAerodromeV2PoolParams {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub factory: Address,
    pub variant: DexVariant,
    pub stable: bool,
    /// Unidirectional fee: `(fee_numer, fee_denom)`.
    pub fee: (u64, u64),
    /// Reserves typed `U112` to mirror the on-chain `uint112` storage width
    /// (Solidly / Aerodrome pair contracts).
    pub reserve0: U112,
    pub reserve1: U112,
    /// Block number of the registration state — seeds the genesis reorg
    /// journal delta (mirror of V2's discipline).
    pub update_block: u64,
}

impl AerodromeV2PoolState {
    /// Construct `(identity, state)` from registration params + journal depth.
    ///
    /// Seeds the reorg journal with a genesis anchor (before == after ==
    /// registration reserves, at `update_block`) so
    /// [`crate::BotState::aerodrome_restore_before_block`] can land
    /// on the registration state — the same anchor discipline as V2/Curve.
    #[must_use]
    pub fn from_params(
        params: &RegisterAerodromeV2PoolParams,
        journal_depth: usize,
    ) -> (AerodromeV2PoolIdentity, AerodromeV2PoolState) {
        let identity = AerodromeV2PoolIdentity {
            address: params.address,
            token0: params.token0,
            token1: params.token1,
            factory: params.factory,
            variant: params.variant,
            stable: params.stable,
            fee: params.fee,
        };
        let mut journal = ReorgJournal::<V2BlockDelta>::new(journal_depth);
        journal.push_delta(V2BlockDelta {
            block: params.update_block,
            reserve0_before: params.reserve0,
            reserve1_before: params.reserve1,
            reserve0_after: params.reserve0,
            reserve1_after: params.reserve1,
        });
        let state = Self {
            reserve0: params.reserve0,
            reserve1: params.reserve1,
            update_block: params.update_block,
            journal,
        };
        (identity, state)
    }
}

impl AerodromeV2PoolState {
    /// Apply an Aerodrome `Sync` event to this pool's reserves, capturing the
    /// prior reserves into the reorg journal (ADR-014 D1 — relocated from
    /// `BotState::apply_aerodrome_sync_by_pool_id`). Aerodrome shares the
    /// `V2BlockDelta` full-state delta with V2 (Solidly mirrors v2-core's
    /// `Sync(uint112, uint112)`); the body is the same shape as
    /// `V2PoolState::apply_sync`.
    pub fn apply_sync(&mut self, reserve0: U112, reserve1: U112, block_number: u64) {
        self.journal.push_delta(V2BlockDelta {
            block: block_number,
            reserve0_before: self.reserve0,
            reserve1_before: self.reserve1,
            reserve0_after: reserve0,
            reserve1_after: reserve1,
        });
        self.reserve0 = reserve0;
        self.reserve1 = reserve1;
        self.update_block = block_number;
    }
}

// ADR-016 — pool-owned reorg rollback for the reserve-pair family. The
// field-write previously duplicated across the `BotState::aerodrome_*` /
// `v2_*` restore dispatchers is absorbed into the state struct; restore
// returns `()` so the trait is no-op-free. Byte-identical to the V2 impl
// modulo the struct name.
impl ReorgPoolState for AerodromeV2PoolState {
    fn restore_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        let (reserve0, reserve1, landed_block) = self.journal.restore_before_block(block)?;
        self.reserve0 = reserve0;
        self.reserve1 = reserve1;
        self.update_block = landed_block;
        Ok(())
    }

    fn discard_before_block(&mut self, block: u64) -> Result<(), JournalError> {
        self.journal.discard_before_block(block)
    }

    fn journal_len(&self) -> usize {
        self.journal.len()
    }

    fn newest_block(&self) -> Option<u64> {
        self.journal.newest_block()
    }
}

// ===========================================================================
// Tests for the relocated apply method (ADR-014 D1 — reserve-pair half of Q1).
// ===========================================================================
#[cfg(test)]
mod apply_inherent_tests {
    #![allow(unused_imports)]
    use super::*;
    use alloy::primitives::U256;

    fn state_with_reserves(r0: u64, r1: u64) -> AerodromeV2PoolState {
        AerodromeV2PoolState {
            reserve0: U112::from(r0),
            reserve1: U112::from(r1),
            update_block: 0,
            journal: ReorgJournal::<V2BlockDelta>::new(8),
        }
    }

    #[test]
    fn apply_sync_updates_reserves_advances_block_and_journals_priors() {
        // Aerodrome twin of the V2 test — Aerodrome shares V2BlockDelta +
        // the same reserve shape (Solidly mirrors v2-core's Sync(uint112)).
        let mut state = state_with_reserves(100, 200);
        let before_len = state.journal.len();

        state.apply_sync(U112::from(150), U112::from(250), 7);

        assert_eq!(state.journal.len(), before_len + 1);
        assert_eq!(state.journal.newest_block(), Some(7));
        assert_eq!(state.reserve0, U112::from(150));
        assert_eq!(state.reserve1, U112::from(250));
        assert_eq!(state.update_block, 7);
    }
}
