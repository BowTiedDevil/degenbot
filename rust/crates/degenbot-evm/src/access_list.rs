//! EIP-2930 access-list emission from the revm `State` journal.
//!
//! Retires `eth_createAccessList`. The in-process `transact` (NOT
//! `transact_one`, which discards state) returns `ResultAndState { result,
//! state }` where `state: EvmState = AddressMap<Account>`. Iterate the
//! touched accounts' `storage` maps to collect the warmed slots → an
//! `AccessList` ready for the *submitted* `execute()` transaction's
//! `TxParams.accessList` (consumed by `degenbot-submission`).
//!
//! # Filled by task `ED3Q7R`.
//!
//! See `docs/spikes/revm-composition-api-and-cold-miss-latency.md` §5 for the
//! verified `ResultAndState.state` API surface.

use alloy::rpc::types::eth::AccessList;

/// Emit the EIP-2930 access list the `execute()` call warmed, from the revm
/// `transact` result's `state` journal.
///
/// Iterates `state.iter()` (an `EvmState = AddressMap<Account>`); for each
/// `Account` with touched storage, collects the `storage.keys()` into an
/// `AccessListItem { address, storage_keys }`. No `eth_createAccessList` RPC.
///
/// # Filled by task `ED3Q7R`.
#[must_use]
pub fn emit_access_list_from_state<S>(_state: &S) -> AccessList
where
    S: AccessListStateView,
{
    // TODO(ED3Q7R): iterate state.iter(), collect touched storage.keys()
    // into AccessListItem { address, storage_keys }, return AccessList::from(...).
    AccessList::default()
}

/// The minimal view of the revm `EvmState` (`AddressMap<Account>`) this leaf
/// reads to emit the access list — a trait so the emitter is testable without
/// depending on the concrete revm `State` type (which carries the full journal).
///
/// Filled by task `ED3Q7R` with a concrete impl over `revm_state::EvmState`.
pub trait AccessListStateView {
    /// Iterate (address, touched-slot-keys) in storage order.
    fn iter_touched(
        &self,
    ) -> impl Iterator<Item = (alloy::primitives::Address, Vec<alloy::primitives::B256>)>;
}
