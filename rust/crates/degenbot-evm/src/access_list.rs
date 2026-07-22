//! EIP-2930 access-list emission from the revm `State` journal.
//!
//! Retires `eth_createAccessList`. The in-process `transact` (NOT
//! `transact_one`, which discards state) returns `ResultAndState { result,
//! state }` where `state: EvmState = AddressMap<Account>`. Iterate the
//! touched accounts' `storage` maps to collect the warmed slots → an
//! `AccessList` ready for the *submitted* `execute()` transaction's
//! `TxParams.accessList` (consumed by `degenbot-submission`).
//!
//! # API surface (spike QGJGWI §5 — pinned)
//!
//! ```text
//! // revm-context-interface-41.0.0/src/result.rs
//! pub struct ExecResultAndState<R, S = EvmState> { pub result: R, pub state: S }
//! pub type ResultAndState<H = HaltReason, S = EvmState> = ExecResultAndState<ExecutionResult<H>, S>;
//! // revm-state-41.0.0/src/types.rs
//! pub type EvmState = AddressMap<Account>;     // Address → Account
//! pub type EvmStorage = StorageKeyMap<EvmStorageSlot>;  // slot key → slot
//! // revm-state-41.0.0/src/account.rs (Account)
//! pub struct Account { pub info: AccountInfo, pub storage: EvmStorage, pub status: AccountStatus, … }
//! ```
//! `transact_one` discards state (finalize clears the journal) — use `transact`
//! for the access-list path, OR call `finalize()` after `transact_one` runs to
//! drain the accumulated journal into an `EvmState`.

// Solidity/rpc/revm identifiers (AccessList, EIP-2930, WETH9, PoolManager,
// storage, journaled_state, etc.) are ubiquitous here.
#![allow(clippy::doc_markdown)]

use alloy::primitives::B256;
use alloy::rpc::types::eth::AccessList;
use revm::state::{Account, EvmState};

/// Emit the EIP-2930 access list the `execute()` call warmed, from the revm
/// `transact` result's `state` journal.
///
/// Iterates `state.iter()` (an `EvmState = AddressMap<Account>`); for each
/// `Account` flagged `Touched` (revm's `AccountStatus::Touched` — set by
/// `mark_touch()` whenever a call reads/writes the account's storage or
/// balance), collects the touched `storage.keys()` into an
/// `AccessListItem { address, storage_keys }`. Accounts with no touched storage
/// are skipped (a bare `Touched` from a balance-only touch contributes no
/// access-list slots — the access list's value is the STORAGE keys it pre-warms).
///
/// The `StorageKey` (a `U256`) is widened to `B256` (the EIP-2930 wire type)
/// via `B256::from(u256)` — left-pad-preserving (the high 32 bytes are the
/// U256, the low bytes zero). This matches `eth_createAccessList`'s output
/// shape (the RPC returns `B256` storage keys).
///
/// No `eth_createAccessList` RPC — the warmed-slot set is a free byproduct of
/// the in-process `transact`.
///
/// # Which slots are included
///
/// Per the spike (§5 sketch: `acc.storage.keys()`), ALL slots present in the
/// account's `storage` map at `finalize` time — revm populates `storage` only
/// for slots the call actually accessed (a cold SLOAD/SSTORE inserts a slot
/// entry); untouched slots never land in the map. So `storage.keys()` IS the
/// warmed-slot set (no further `is_cold` filtering needed). The round-trip
/// validation (emit → re-simulate-with-access-list → `gas_used` matches the
/// no-access-list `transact`) pins this — warm slots are warm both ways.
///
/// # Arguments
///
/// - `state` — the revm `EvmState` from a `transact(...).state` (or
///   `finalize()` after `transact_one`). Borrowed; not consumed.
#[must_use]
pub fn emit_access_list_from_state(state: &EvmState) -> AccessList {
    let items: Vec<alloy::rpc::types::eth::AccessListItem> = state
        .iter()
        .filter(|(_, account)| account.is_touched())
        .filter_map(|(address, account)| {
            let storage_keys: Vec<B256> = storage_keys_for(account);
            if storage_keys.is_empty() {
                None
            } else {
                Some(alloy::rpc::types::eth::AccessListItem {
                    address: *address,
                    storage_keys,
                })
            }
        })
        .collect();
    AccessList::from(items)
}

/// Collect the touched storage slot keys for an `Account`, widened from
/// `StorageKey` (`U256`) to `B256` (the EIP-2930 wire type).
///
/// Only slots actually accessed by the call land in `account.storage` (revm
/// inserts a slot entry on first cold access); un-accessed slots are absent.
/// So `storage.keys()` IS the warmed-slot set — no `is_cold` filtering.
fn storage_keys_for(account: &Account) -> Vec<B256> {
    account.storage.keys().map(|key| B256::from(*key)).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use alloy::primitives::{Address, B256, U256};
    use revm::state::{
        Account, AccountStatus, EvmState, EvmStorage, EvmStorageSlot, TransactionId,
    };

    /// Insert a touched account with the given storage slots into an `EvmState`.
    fn state_with_touched_storage(addr: Address, slots: &[U256]) -> EvmState {
        let mut storage = EvmStorage::default();
        for &slot in slots {
            storage.insert(slot, EvmStorageSlot::new(slot, TransactionId::ZERO));
        }
        let mut account = Account::default();
        account.storage = storage;
        account.status = AccountStatus::Touched;
        let mut state = EvmState::default();
        state.insert(addr, account);
        state
    }

    /// A touched account with one warmed storage slot emits a single
    /// `AccessListItem { address, [slot] }`.
    #[test]
    fn emit_access_list_collects_touched_storage_slots() {
        let addr = Address::ZERO;
        let slot = U256::from(0x42u64);
        let state = state_with_touched_storage(addr, &[slot]);

        let access_list = emit_access_list_from_state(&state);
        assert_eq!(access_list.len(), 1, "one touched account");
        let item = &access_list[0];
        assert_eq!(item.address, addr);
        assert_eq!(item.storage_keys, vec![B256::from(slot)]);
    }

    /// A touched account with NO touched storage (a balance-only touch, e.g.
    /// the owner funded with ETH) emits NO `AccessListItem` — the access list's
    /// value is the storage keys it pre-warms, and a bare balance touch
    /// contributes none.
    #[test]
    fn emit_access_list_skips_touched_account_with_no_storage() {
        let addr = Address::ZERO;
        let state = state_with_touched_storage(addr, &[]);

        let access_list = emit_access_list_from_state(&state);
        assert!(
            access_list.is_empty(),
            "balance-only touch contributes no slots"
        );
    }

    /// An UNtouched account in the state (present but not flagged `Touched`)
    /// emits nothing — only touched accounts contribute to the access list.
    #[test]
    fn emit_access_list_skips_untouched_account() {
        let addr = Address::ZERO;
        let slot = U256::from(0x99u64);
        // Same construction as `state_with_touched_storage`, but status is
        // `default()` (no `Touched` flag set).
        let mut storage = EvmStorage::default();
        storage.insert(slot, EvmStorageSlot::new(slot, TransactionId::ZERO));
        let mut account = Account::default();
        account.storage = storage;
        account.status = AccountStatus::default();
        let mut state = EvmState::default();
        state.insert(addr, account);

        let access_list = emit_access_list_from_state(&state);
        assert!(access_list.is_empty(), "untouched account skipped");
    }

    /// Multiple touched accounts each contribute their own storage keys — the
    /// emitted list preserves the per-account grouping (one item per address,
    /// each carrying its slot set).
    #[test]
    fn emit_access_list_groups_slots_per_address() {
        let addr_a = Address::ZERO;
        let addr_b = Address::repeat_byte(0x11);
        let slot_first = U256::from(1u64);
        let slot_second = U256::from(2u64);
        let slot_third = U256::from(3u64);

        let state_a = state_with_touched_storage(addr_a, &[slot_first, slot_second]);
        let state_b = state_with_touched_storage(addr_b, &[slot_third]);
        let mut state = state_a;
        state.extend(state_b);

        let access_list = emit_access_list_from_state(&state);
        assert_eq!(access_list.len(), 2, "two touched accounts");
        // The AddressMap iteration order is not guaranteed; find each address.
        let item_a = access_list
            .iter()
            .find(|i| i.address == addr_a)
            .expect("addr_a present");
        assert_eq!(item_a.storage_keys.len(), 2);
        assert!(item_a.storage_keys.contains(&B256::from(slot_first)));
        assert!(item_a.storage_keys.contains(&B256::from(slot_second)));
        let item_b = access_list
            .iter()
            .find(|i| i.address == addr_b)
            .expect("addr_b present");
        assert_eq!(item_b.storage_keys, vec![B256::from(slot_third)]);
    }

    /// An empty state (no accounts touched) emits an empty access list.
    #[test]
    fn emit_access_list_empty_state_is_empty_list() {
        let state = EvmState::default();
        let access_list = emit_access_list_from_state(&state);
        assert!(access_list.is_empty());
    }
}
