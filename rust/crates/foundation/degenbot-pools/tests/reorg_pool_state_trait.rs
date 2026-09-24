//! Spike for a `ReorgPoolState` trait on balance-vector state structs
//! (ADR-014 D3 refinement candidate).
//!
//! Goal: prove that `restore_before_block` can return `()` (no family-specific
//! return type) with the field-write absorbed *into* the pool's own impl, so a
//! single non-generic trait covers the balance-vector family without re-deriving
//! a per-family return type (the no-op trap ADR-014 rejected for the cross-
//! family `PoolFamilyReg`).
//!
//! This spike covers ALL THREE balance-vector structs
//! (Curve, `BalancerWeighted`, `BalancerStable`),
//! which share byte-identical `ReorgPoolState` impls modulo the struct name.
//! V2/Aerodrome (reserve-pair) and V3/V4 (concentrated-liquidity) are out of
//! scope for the spike; V3 in particular keeps `V3RestoreResult` (a genuinely
//! different restore algorithm), so a trait-absorbing restore there is a
//! separate, harder question this spike does not answer.
//!
//! Behavior under test (seam = the trait on the public state struct):
//! `restore_before_block` pops deltas at/after the target, writes the landed-at
//! balances/block into the struct's own fields, returns `()`.
//! `discard_before_block` drops deltas earlier than the target, returns `()`.
//! `journal_len` reports the delta count (already generic on `ReorgJournal`).
//!
//! Oracle: worked example with literal expected values. Registration (genesis)
//! at block 10 with balances [1000, 1000]; a swap lands at block 12 producing
//! balances [1100, 909]; a reorg restores to block 11 → the landed-at state is
//! the genesis balances [1000, 1000] at block 10. The test asserts through the
//! struct's PUBLIC `balances`/`update_block` fields after restore — no internals
//! mocked, no `BotState`.

use alloy::primitives::{aliases::U112, Address, U256};
use degenbot_pools::aerodrome_v2_state::AerodromeV2PoolState;
use degenbot_pools::balancer_stable_state::BalancerStablePoolState;
use degenbot_pools::balancer_weighted_state::BalancerWeightedPoolState;
use degenbot_pools::curve_state::CurvePoolState;
use degenbot_pools::state_history::{BalancesBlockDelta, JournalError, ReorgJournal, V2BlockDelta};
use degenbot_pools::v2_state::V2PoolState;
use degenbot_pools::ReorgPoolState;

const GENESIS_BLOCK: u64 = 10;
const SWAP_BLOCK: u64 = 12;

fn genesis_balances() -> Vec<U256> {
    vec![U256::from(1000u64), U256::from(1000u64)]
}

fn after_swap_balances() -> Vec<U256> {
    vec![U256::from(1100u64), U256::from(909u64)]
}

/// A balance-vector state with a genesis anchor at `GENESIS_BLOCK` and a swap
/// transition at `SWAP_BLOCK`, with live fields set to the post-swap state.
struct BalanceVectorPostSwap {
    genesis: Vec<U256>,
    after: Vec<U256>,
}

impl BalanceVectorPostSwap {
    fn new() -> Self {
        Self {
            genesis: genesis_balances(),
            after: after_swap_balances(),
        }
    }

    fn build_curve(&self) -> CurvePoolState {
        self.build_curve_with_genesis()
    }

    fn build_curve_with_genesis(&self) -> CurvePoolState {
        let mut journal = ReorgJournal::<BalancesBlockDelta>::new(8);
        // Genesis anchor at block 10.
        journal.push_delta(BalancesBlockDelta {
            block: GENESIS_BLOCK,
            balances_before: self.genesis.clone(),
            balances_after: self.genesis.clone(),
        });
        // Swap transition at block 12.
        journal.push_delta(BalancesBlockDelta {
            block: SWAP_BLOCK,
            balances_before: self.genesis.clone(),
            balances_after: self.after.clone(),
        });
        CurvePoolState {
            balances: self.after.clone().into(),
            update_block: SWAP_BLOCK,
            journal,
            data_provider: None,
            state_nonce: 0,
        }
    }

    fn build_weighted(&self) -> BalancerWeightedPoolState {
        let mut journal = ReorgJournal::<BalancesBlockDelta>::new(8);
        journal.push_delta(BalancesBlockDelta {
            block: GENESIS_BLOCK,
            balances_before: self.genesis.clone(),
            balances_after: self.genesis.clone(),
        });
        journal.push_delta(BalancesBlockDelta {
            block: SWAP_BLOCK,
            balances_before: self.genesis.clone(),
            balances_after: self.after.clone(),
        });
        BalancerWeightedPoolState {
            balances: self.after.clone().into(),
            update_block: SWAP_BLOCK,
            journal,
            state_nonce: 0,
        }
    }

    fn build_stable(&self) -> BalancerStablePoolState {
        let mut journal = ReorgJournal::<BalancesBlockDelta>::new(8);
        journal.push_delta(BalancesBlockDelta {
            block: GENESIS_BLOCK,
            balances_before: self.genesis.clone(),
            balances_after: self.genesis.clone(),
        });
        journal.push_delta(BalancesBlockDelta {
            block: SWAP_BLOCK,
            balances_before: self.genesis.clone(),
            balances_after: self.after.clone(),
        });
        BalancerStablePoolState {
            balances: self.after.clone().into(),
            update_block: SWAP_BLOCK,
            journal,
            rate_provider: None,
            state_nonce: 0,
        }
    }

    /// A state with ONLY the genesis anchor (no swap) — for discard + no-op
    /// + hard-error tests that don't need a transition to roll back.
    fn build_curve_genesis_only(&self) -> CurvePoolState {
        let mut journal = ReorgJournal::<BalancesBlockDelta>::new(8);
        journal.push_delta(BalancesBlockDelta {
            block: GENESIS_BLOCK,
            balances_before: self.genesis.clone(),
            balances_after: self.genesis.clone(),
        });
        CurvePoolState {
            balances: self.genesis.clone().into(),
            update_block: GENESIS_BLOCK,
            journal,
            data_provider: None,
            state_nonce: 0,
        }
    }

    fn build_weighted_genesis_only(&self) -> BalancerWeightedPoolState {
        let mut journal = ReorgJournal::<BalancesBlockDelta>::new(8);
        journal.push_delta(BalancesBlockDelta {
            block: GENESIS_BLOCK,
            balances_before: self.genesis.clone(),
            balances_after: self.genesis.clone(),
        });
        BalancerWeightedPoolState {
            balances: self.genesis.clone().into(),
            update_block: GENESIS_BLOCK,
            journal,
            state_nonce: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Curve — restore writes fields, returns ()
// ---------------------------------------------------------------------------

#[test]
fn curve_restore_before_block_returns_unit_and_writes_landed_at_fields() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_curve();
    assert_eq!(state.balances.as_ref(), fixture.after.as_slice());
    assert_eq!(state.update_block, SWAP_BLOCK);
    assert_eq!(state.journal_len(), 2);

    let result: Result<(), JournalError> = state.restore_before_block(GENESIS_BLOCK + 1);

    assert!(result.is_ok());
    assert_eq!(
        state.balances.as_ref(),
        fixture.genesis.as_slice(),
        "balances roll back to genesis"
    );
    assert_eq!(state.update_block, GENESIS_BLOCK, "update_block rolls back");
    assert_eq!(state.journal_len(), 1, "swap delta was popped");
}

#[test]
fn curve_restore_before_block_no_op_when_newest_before_target() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_curve();

    let result = state.restore_before_block(SWAP_BLOCK + 100);

    assert!(result.is_ok());
    assert_eq!(
        state.balances.as_ref(),
        fixture.after.as_slice(),
        "post-swap state survives no-op"
    );
    assert_eq!(state.update_block, SWAP_BLOCK);
    assert_eq!(state.journal_len(), 2, "journal untouched on no-op");
}

#[test]
fn curve_restore_before_block_at_genesis_is_hard_error() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_curve_genesis_only();

    let result = state.restore_before_block(GENESIS_BLOCK);

    assert!(
        matches!(result, Err(JournalError::NoStatePriorToBlock { .. })),
        "target == genesis block is a hard error"
    );
    assert_eq!(
        state.balances.as_ref(),
        fixture.genesis.as_slice(),
        "genesis state untouched on error"
    );
}

#[test]
fn curve_discard_before_block_returns_unit_and_drops_old_deltas() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_curve();

    let result: Result<(), JournalError> = state.discard_before_block(SWAP_BLOCK);

    assert!(result.is_ok());
    assert_eq!(state.journal_len(), 1, "genesis delta discarded");
    assert_eq!(
        state.balances.as_ref(),
        fixture.after.as_slice(),
        "discard does not mutate live state"
    );
    assert_eq!(
        state.update_block, SWAP_BLOCK,
        "discard does not mutate update_block"
    );
}

// ---------------------------------------------------------------------------
// BalancerWeighted — restore writes fields, returns ()
// ---------------------------------------------------------------------------

#[test]
fn balancer_weighted_restore_before_block_returns_unit_and_writes_landed_at_fields() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_weighted();
    assert_eq!(state.balances.as_ref(), fixture.after.as_slice());
    assert_eq!(state.update_block, SWAP_BLOCK);
    assert_eq!(state.journal_len(), 2);

    let result: Result<(), JournalError> = state.restore_before_block(GENESIS_BLOCK + 1);

    assert!(result.is_ok());
    assert_eq!(state.balances.as_ref(), fixture.genesis.as_slice());
    assert_eq!(state.update_block, GENESIS_BLOCK);
    assert_eq!(state.journal_len(), 1, "swap delta was popped");
}

#[test]
fn balancer_weighted_restore_before_block_no_op_when_newest_before_target() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_weighted();

    let result = state.restore_before_block(SWAP_BLOCK + 100);

    assert!(result.is_ok());
    assert_eq!(
        state.balances.as_ref(),
        fixture.after.as_slice(),
        "post-swap state survives no-op"
    );
    assert_eq!(state.update_block, SWAP_BLOCK);
    assert_eq!(state.journal_len(), 2, "journal untouched on no-op");
}

#[test]
fn balancer_weighted_restore_before_block_at_genesis_is_hard_error() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_weighted_genesis_only();

    let result = state.restore_before_block(GENESIS_BLOCK);

    assert!(
        matches!(result, Err(JournalError::NoStatePriorToBlock { .. })),
        "target == genesis block is a hard error"
    );
    assert_eq!(
        state.balances.as_ref(),
        fixture.genesis.as_slice(),
        "genesis state untouched on error"
    );
}

#[test]
fn balancer_weighted_discard_before_block_returns_unit_and_drops_old_deltas() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_weighted();

    let result: Result<(), JournalError> = state.discard_before_block(SWAP_BLOCK);

    assert!(result.is_ok());
    assert_eq!(state.journal_len(), 1, "genesis delta discarded");
    assert_eq!(
        state.balances.as_ref(),
        fixture.after.as_slice(),
        "discard does not mutate live state"
    );
    assert_eq!(state.update_block, SWAP_BLOCK);
}

// ---------------------------------------------------------------------------
// BalancerStable — restore writes fields, returns ()
// (originally the sole spike struct; retained here so all three siblings are
//  covered by one file, proving the three-way byte-identical-impl witness.)
// ---------------------------------------------------------------------------

#[test]
fn balancer_stable_restore_before_block_returns_unit_and_writes_landed_at_fields() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_stable();
    assert_eq!(state.balances.as_ref(), fixture.after.as_slice());
    assert_eq!(state.update_block, SWAP_BLOCK);
    assert_eq!(state.journal_len(), 2);

    let result: Result<(), JournalError> = state.restore_before_block(GENESIS_BLOCK + 1);

    assert!(result.is_ok());
    assert_eq!(state.balances.as_ref(), fixture.genesis.as_slice());
    assert_eq!(state.update_block, GENESIS_BLOCK);
    assert_eq!(state.journal_len(), 1, "swap delta was popped");
}

#[test]
fn balancer_stable_restore_before_block_no_op_when_newest_before_target() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_stable();

    let result = state.restore_before_block(SWAP_BLOCK + 100);

    assert!(result.is_ok());
    assert_eq!(
        state.balances.as_ref(),
        fixture.after.as_slice(),
        "post-swap state survives no-op"
    );
    assert_eq!(state.update_block, SWAP_BLOCK);
    assert_eq!(state.journal_len(), 2, "journal untouched on no-op");
}

#[test]
fn balancer_stable_restore_before_block_at_genesis_is_hard_error() {
    let fixture = BalanceVectorPostSwap::new();
    // build_stable already pushes a swap delta on top of genesis; use a
    // genesis-only construction for the hard-error case.
    let mut state = {
        let params = degenbot_pools::balancer_stable_state::RegisterBalancerStablePoolParams {
            address: Address::ZERO,
            vault: Address::ZERO,
            pool_id: [0u8; 32],
            tokens: vec![Address::ZERO; 2],
            amp: 200u128,
            scaling_factors: vec![U256::from(1u64); 2],
            swap_fee: 0u128,
            bpt_idx: None,
            invariant_version: 1u8,
            balances: fixture.genesis.clone(),
            update_block: GENESIS_BLOCK,
            rate_provider: None,
        };
        BalancerStablePoolState::from_params(params, 8).1
    };

    let result = state.restore_before_block(GENESIS_BLOCK);

    assert!(
        matches!(result, Err(JournalError::NoStatePriorToBlock { .. })),
        "target == genesis block is a hard error"
    );
    assert_eq!(
        state.balances.as_ref(),
        fixture.genesis.as_slice(),
        "genesis state untouched on error"
    );
}

#[test]
fn balancer_stable_discard_before_block_returns_unit_and_drops_old_deltas() {
    let fixture = BalanceVectorPostSwap::new();
    let mut state = fixture.build_stable();

    let result: Result<(), JournalError> = state.discard_before_block(SWAP_BLOCK);

    assert!(result.is_ok());
    assert_eq!(state.journal_len(), 1, "genesis delta discarded");
    assert_eq!(
        state.balances.as_ref(),
        fixture.after.as_slice(),
        "discard does not mutate live state"
    );
    assert_eq!(state.update_block, SWAP_BLOCK);
}

// ---------------------------------------------------------------------------
// V2 (reserve-pair) — restore writes reserve fields, returns ()
// ---------------------------------------------------------------------------

fn genesis_reserves() -> (U112, U112) {
    (U112::from(1000u64), U112::from(1000u64))
}

fn after_swap_reserves() -> (U112, U112) {
    (U112::from(1100u64), U112::from(909u64))
}

fn v2_post_swap() -> V2PoolState {
    let (r0, r1) = genesis_reserves();
    let (a0, a1) = after_swap_reserves();
    let mut journal = ReorgJournal::<V2BlockDelta>::new(8);
    journal.push_delta(V2BlockDelta {
        block: GENESIS_BLOCK,
        reserve0_before: r0,
        reserve1_before: r1,
        reserve0_after: r0,
        reserve1_after: r1,
    });
    journal.push_delta(V2BlockDelta {
        block: SWAP_BLOCK,
        reserve0_before: r0,
        reserve1_before: r1,
        reserve0_after: a0,
        reserve1_after: a1,
    });
    V2PoolState {
        reserve0: a0,
        reserve1: a1,
        update_block: SWAP_BLOCK,
        journal,
        state_nonce: 0,
    }
}

#[test]
fn v2_restore_before_block_returns_unit_and_writes_landed_at_fields() {
    let (r0, r1) = genesis_reserves();
    let (a0, a1) = after_swap_reserves();
    let mut state = v2_post_swap();
    assert_eq!(state.reserve0, a0);
    assert_eq!(state.reserve1, a1);
    assert_eq!(state.update_block, SWAP_BLOCK);
    assert_eq!(state.journal_len(), 2);

    let result: Result<(), JournalError> = state.restore_before_block(GENESIS_BLOCK + 1);

    assert!(result.is_ok());
    assert_eq!(state.reserve0, r0, "reserve0 rolls back to genesis");
    assert_eq!(state.reserve1, r1, "reserve1 rolls back to genesis");
    assert_eq!(state.update_block, GENESIS_BLOCK);
    assert_eq!(state.journal_len(), 1, "swap delta was popped");
}

#[test]
fn v2_restore_before_block_no_op_when_newest_before_target() {
    let mut state = v2_post_swap();
    let result = state.restore_before_block(SWAP_BLOCK + 100);
    assert!(result.is_ok());
    let (a0, a1) = after_swap_reserves();
    assert_eq!(state.reserve0, a0, "post-swap state survives no-op");
    assert_eq!(state.reserve1, a1);
    assert_eq!(state.journal_len(), 2, "journal untouched on no-op");
}

// ---------------------------------------------------------------------------
// Aerodrome (reserve-pair) — restore writes reserve fields, returns ()
// ---------------------------------------------------------------------------

fn aerodrome_post_swap() -> AerodromeV2PoolState {
    let (r0, r1) = genesis_reserves();
    let (a0, a1) = after_swap_reserves();
    let mut journal = ReorgJournal::<V2BlockDelta>::new(8);
    journal.push_delta(V2BlockDelta {
        block: GENESIS_BLOCK,
        reserve0_before: r0,
        reserve1_before: r1,
        reserve0_after: r0,
        reserve1_after: r1,
    });
    journal.push_delta(V2BlockDelta {
        block: SWAP_BLOCK,
        reserve0_before: r0,
        reserve1_before: r1,
        reserve0_after: a0,
        reserve1_after: a1,
    });
    AerodromeV2PoolState {
        reserve0: a0,
        reserve1: a1,
        update_block: SWAP_BLOCK,
        journal,
        state_nonce: 0,
    }
}

#[test]
fn aerodrome_restore_before_block_returns_unit_and_writes_landed_at_fields() {
    let (r0, r1) = genesis_reserves();
    let mut state = aerodrome_post_swap();

    let result: Result<(), JournalError> = state.restore_before_block(GENESIS_BLOCK + 1);

    assert!(result.is_ok());
    assert_eq!(state.reserve0, r0, "reserve0 rolls back to genesis");
    assert_eq!(state.reserve1, r1, "reserve1 rolls back to genesis");
    assert_eq!(state.update_block, GENESIS_BLOCK);
    assert_eq!(state.journal_len(), 1, "swap delta was popped");
}

#[test]
fn aerodrome_discard_before_block_returns_unit_and_drops_old_deltas() {
    let (a0, a1) = after_swap_reserves();
    let mut state = aerodrome_post_swap();
    let result: Result<(), JournalError> = state.discard_before_block(SWAP_BLOCK);
    assert!(result.is_ok());
    assert_eq!(state.journal_len(), 1, "genesis delta discarded");
    assert_eq!(state.reserve0, a0, "discard does not mutate live state");
    assert_eq!(state.reserve1, a1);
}
