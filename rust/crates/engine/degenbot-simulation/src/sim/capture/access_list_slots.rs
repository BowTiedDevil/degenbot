//! The collected-slots buffer behind the access-list capture — the
//! `address -> touched slot set` map the engine's `step`-hook collector
//! writes and the caller drains into an EIP-2930 [`AccessList`].
//!
//! Accounts with no touched storage (a bare balance touch) contribute no
//! `AccessListItem` — the access list's value is the storage keys it
//! pre-warms.

use alloy::primitives::map::AddressHashMap;
use alloy::primitives::{Address, B256};
use alloy::rpc::types::eth::AccessList;
use std::collections::BTreeSet;

/// The owned interior slot map (see the module doc).
#[derive(Debug, Default)]
pub struct CollectedSlots {
    /// `address ->` the set of storage slot keys (B256, the EIP-2930 wire
    /// type) the run touched. `BTreeSet` for deterministic emission.
    touched: AddressHashMap<BTreeSet<B256>>,
}

impl CollectedSlots {
    /// Record one `(address, slot)` touch (an `SLOAD`/`SSTORE` observed at
    /// the `step` hook).
    pub fn record(&mut self, address: Address, slot: B256) {
        self.touched.entry(address).or_default().insert(slot);
    }

    /// Drain the collected slot set into an EIP-2930 [`AccessList`],
    /// resetting the collector to empty for reuse on the next path. Accounts
    /// with no touched storage contribute no `AccessListItem`.
    pub fn take_access_list(&mut self) -> AccessList {
        let touched = std::mem::take(&mut self.touched);
        let items: Vec<alloy::rpc::types::eth::AccessListItem> = touched
            .into_iter()
            .filter_map(|(address, slots)| {
                if slots.is_empty() {
                    None
                } else {
                    Some(alloy::rpc::types::eth::AccessListItem {
                        address,
                        storage_keys: slots.into_iter().collect(),
                    })
                }
            })
            .collect();
        AccessList::from(items)
    }
}
