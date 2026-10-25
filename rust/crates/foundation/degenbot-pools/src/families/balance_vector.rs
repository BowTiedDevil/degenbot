//! Family-blind event vocabulary shared by the three balance-vector family
//! read modules — [`super::curve_stableswap`], [`super::balancer_weighted`],
//! and [`super::balancer_stable`] (ADR-059 D6: what the families genuinely
//! share lives beside them, never between their projections).
//!
//! The balance-vector structural family's three solve families carry the same
//! live-state shape (a balance vector + a two-stamp clock + a reorg journal,
//! ADR-017 D1) and therefore the same event vocabulary: one buffered balance
//! update lands on the slice through the family's apply door
//! ([`super::door::FamilyApply`]). The probe vocabulary the observe doors
//! answer is the shared [`super::door`] contract. The per-family swap math,
//! resolve projections, and calc behavior are deliberately NOT here — each
//! family read module owns its own.

use alloy::primitives::U256;

/// One buffered balance-vector event (a Curve `Exchange` or a Balancer Vault
/// `PoolBalanceChanged`): the new balance vector and the block it landed at.
/// The event type pairs with the family's state slice at the apply door; the
/// slice's own [`crate::state::state_history::BalanceVectorPoolState::apply_balance_update`]
/// owns the journal + clock discipline (ADR-017 D1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceVectorEvent {
    /// The new balances (one per token — the slice asserts the arity).
    pub balances: Vec<U256>,
    /// The block the update landed at (advances the price clock).
    pub block_number: u64,
}
