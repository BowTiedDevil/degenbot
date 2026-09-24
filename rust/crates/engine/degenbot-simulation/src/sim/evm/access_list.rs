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

use alloy::primitives::map::AddressHashMap;
use alloy::primitives::B256;
use alloy::rpc::types::eth::AccessList;
use revm::bytecode::opcode::{SLOAD, SSTORE};
use revm::inspector::Inspector;
use revm::interpreter::interpreter_types::{InputsTr, Jumps, StackTr};
use revm::interpreter::{Interpreter, InterpreterTypes};
use revm::state::{Account, EvmState};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

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

// ─────────────────────────────────────────────────────────────────────────
// The in-process access-list collector (ADR-019 D3)
// ─────────────────────────────────────────────────────────────────────────

/// The owned interior slot map shared between an [`AccessListCollector`]
/// (the [`Inspector`] that writes during `execute()`) + its [`AccessListHandle`]
/// (the handle `simulate_path_on_evm` reads from after the run).
#[derive(Debug, Default)]
struct CollectedSlots {
    /// `address ->` the set of storage slot keys (B256, the EIP-2930 wire
    /// type) `execute()` touched. `BTreeSet` for deterministic emission.
    touched: AddressHashMap<BTreeSet<B256>>,
}

/// An [`Inspector`] that collects the EIP-2930 access list as a byproduct of
/// the FIRST `inspect_one`/`inspect_one_tx` run, by recording every `SLOAD`/
/// `SSTORE` opcode's `(address, slot)` pair in real-time.
///
/// Replaces the post-re-`transact` + [`emit_access_list_from_state`] as the
/// production access-list source (ADR-019 D3). The re-run executed `execute()`
/// twice; this collector drains the warmed-slot set from the first run, so
/// `execute()` runs once.
///
/// # Why per-opcode capture (not journal diff)
///
/// The 7-call profit-detection loop accumulates ALL seven calls' touches into
/// one journal (pre reads → `execute()` → post reads). The submitted
/// `execute()` transaction's access list needs `execute()`-ONLY touched slots,
/// so a journal-`finalize` diff would include the balance-read slots too. The
/// collector is attached ONLY to `execute()`'s `inspect_one` (the balance
/// reads use `transact_one`, which does not invoke the inspector), so it sees
/// exactly `execute()`'s `SLOAD`/`SSTORE` opcodes — `execute()`-only by
/// construction, no diffing needed.
///
/// # The shared-handle shape
///
/// `inspect_one(tx, collector)` MOVES the collector into the EVM (it becomes
/// `InspectEvm::Inspector`), so the caller can't read it back without the
/// low-level `InspectorEvmTr::inspector()` getter (whose associated-type
/// bounds would cascade onto the generic `simulate_path_on_evm`). Instead,
/// [`AccessListCollector::new`] returns the collector + an [`AccessListHandle`]
/// sharing an `Rc<RefCell<CollectedSlots>>`; the collector writes during
/// `execute()`, the handle drains after — no `InspectorEvmTr` bound needed.
///
/// # Parity with [`emit_access_list_from_state`]
///
/// Both produce the same `(address, storage_keys)` set for a given
/// `execute()` call: `emit_access_list_from_state` reads the post-`transact`
/// `State` journal's `account.storage.keys()` (slots revm inserted on cold
/// access); the collector records the same slots at the `SLOAD`/`SSTORE`
/// opcode. The parity test pins them equal over a fixture.
#[derive(Debug, Clone)]
pub struct AccessListCollector {
    /// Shared with the [`AccessListHandle`] returned from [`Self::new`].
    slots: Rc<RefCell<CollectedSlots>>,
}

/// A read/drain handle to an [`AccessListCollector`] that was moved into an
/// EVM via `inspect_one`. Holds the same `Rc<RefCell<CollectedSlots>>` so the
/// caller can drain the warmed-slot set after the run without retrieving the
/// collector back out of the EVM.
#[derive(Debug, Clone)]
pub struct AccessListHandle {
    slots: Rc<RefCell<CollectedSlots>>,
}

impl Default for AccessListCollector {
    /// A collector with no handle — the placeholder baked into the per-block
    /// EVM via `build_mainnet_with_inspector` (its type fixes
    /// `InspectEvm::Inspector = AccessListCollector`; `inspect_one` swaps in a
    /// fresh collector-with-handle per `execute()` run, so the baked-in one is
    /// never read).
    fn default() -> Self {
        Self {
            slots: Rc::new(RefCell::new(CollectedSlots::default())),
        }
    }
}

impl AccessListCollector {
    /// Create a collector + its drain handle (shared `Rc<RefCell<...>>`).
    #[must_use]
    pub fn new() -> (Self, AccessListHandle) {
        let slots = Rc::new(RefCell::new(CollectedSlots::default()));
        (
            Self {
                slots: Rc::clone(&slots),
            },
            AccessListHandle { slots },
        )
    }
}

impl AccessListHandle {
    /// Drain the collected slot set into an EIP-2930 [`AccessList`],
    /// resetting the collector to empty for reuse on the next path.
    ///
    /// Accounts with no touched storage (a bare balance touch) contribute no
    /// `AccessListItem` — the access list's value is the storage keys it
    /// pre-warms (mirrors [`emit_access_list_from_state`]).
    #[must_use]
    pub fn take_access_list(&self) -> AccessList {
        let touched = std::mem::take(&mut self.slots.borrow_mut().touched);
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

/// `Inspector` impl — record the slot on every `SLOAD`/`SSTORE`.
///
/// The `step` hook fires BEFORE the opcode executes, so the slot key is still
/// the stack top. revm's stack Vec grows by appending (TOP = `data().last()`),
/// so the slot is `last()` for both `SLOAD` (slot = top) and `SSTORE`
/// (slot = top, value = second-from-top). The touched address is the current
/// frame's `target_address` (`interp.input`).
impl<CTX, INTR: InterpreterTypes> Inspector<CTX, INTR> for AccessListCollector {
    fn step(&mut self, interp: &mut Interpreter<INTR>, _context: &mut CTX) {
        let opcode = interp.bytecode.opcode();
        if opcode == SLOAD || opcode == SSTORE {
            let address = interp.input.target_address();
            // revm's stack Vec grows by appending, so the TOP (the slot for
            // both `SLOAD` + `SSTORE`) is `data().last()`, not `first()`.
            if let Some(&slot) = interp.stack.data().last() {
                self.slots
                    .borrow_mut()
                    .touched
                    .entry(address)
                    .or_default()
                    .insert(B256::from(slot));
            }
        }
    }
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, Bytes, B256, U256};
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

    // ── ADR-019 D3 parity: the Inspector collector + the State-journal
    //    emitter produce the same `(address, storage_keys)` set for a given
    //    contract execution. fixture: a contract that does SLOAD(slot 1) +
    //    SSTORE(slot 2 = 0x99), so both slots 1 + 2 are warmed at the contract
    //    address. Both methods must emit exactly that.
    // ──────────────────────────────────────────────────────────────────────

    use revm::bytecode::opcode;
    use revm::bytecode::Bytecode;
    use revm::context::Context as RevmContext;
    use revm::context::TxEnv;
    use revm::database::CacheDB;
    use revm::database_interface::EmptyDB;
    use revm::primitives::TxKind;
    use revm::state::AccountInfo;
    use revm::{ExecuteEvm, InspectEvm, MainBuilder, MainContext};

    /// Build a CacheDB with a single funded contract at `contract_addr`
    /// carrying the SLOAD/SSTORE fixture bytecode.
    fn fixture_db(contract_addr: Address, code: Bytecode) -> CacheDB<EmptyDB> {
        let mut cache_db = CacheDB::new(EmptyDB::default());
        cache_db.insert_account_info(
            contract_addr,
            AccountInfo {
                balance: U256::from(1_000_000_000_000_000_000u64),
                code: Some(code),
                ..Default::default()
            },
        );
        cache_db
    }

    /// Fixture bytecode: `PUSH1 1, SLOAD, PUSH1 0x99, PUSH1 2, SSTORE, STOP`.
    /// Touches storage slot 1 (read) + slot 2 (write) at its own address.
    fn fixture_bytecode() -> Bytes {
        alloy::primitives::Bytes::from(vec![
            opcode::PUSH1,
            0x01,
            opcode::SLOAD, // read slot 1
            opcode::PUSH1,
            0x99,
            opcode::PUSH1,
            0x02,
            opcode::SSTORE, // write slot 2 = 0x99
            opcode::STOP,
        ])
    }

    /// The EIP-2930 access list the [`AccessListCollector`] records during a
    /// single `inspect_one` run MATCHES the access list
    /// [`emit_access_list_from_state`] emits from the post-`transact` `State`
    /// journal — same address + same storage-key set. ADR-019 D3 parity.
    #[test]
    fn access_list_collector_matches_state_journal_emitter() {
        let contract_addr = Address::repeat_byte(0x42);
        let tx = TxEnv::builder()
            .kind(TxKind::Call(contract_addr))
            .gas_limit(100_000)
            .build()
            .expect("well-formed tx");

        // Path A — Inspector collector on the first (and only) run.
        let db_a = fixture_db(contract_addr, Bytecode::new_raw(fixture_bytecode()));
        let evm_a = RevmContext::mainnet()
            .with_db(db_a)
            .build_mainnet_with_inspector(AccessListCollector::default());
        let mut evm_a = evm_a;
        let (collector, handle) = AccessListCollector::new();
        let result_a = evm_a
            .inspect_one(tx.clone(), collector)
            .expect("inspect runs");
        assert!(result_a.is_success(), "fixture must succeed");
        let al_collector = handle.take_access_list();

        // Path B — `transact` + `emit_access_list_from_state` (the retired
        // production path, now an engine-generic primitive).
        let db_b = fixture_db(contract_addr, Bytecode::new_raw(fixture_bytecode()));
        let mut evm_b = RevmContext::mainnet()
            .with_db(db_b)
            .build_mainnet_with_inspector(AccessListCollector::default());
        let result_b = evm_b.transact(tx).expect("transact runs");
        assert!(result_b.result.is_success(), "fixture must succeed");
        let al_state = emit_access_list_from_state(&result_b.state);

        // Both must surface exactly the contract address + the {slot 1, slot 2}
        // SET (ADR-019 D3 parity — “same addresses + storage keys”). The
        // collector emits sorted (BTreeSet); the state-journal emitter emits
        // storage-map iteration order, so compare the SETS, not ordered Vecs.
        let collector_addrs: Vec<Address> = al_collector.iter().map(|i| i.address).collect();
        let state_addrs: Vec<Address> = al_state.iter().map(|i| i.address).collect();
        assert_eq!(
            collector_addrs,
            vec![contract_addr],
            "collector surfaces contract only"
        );
        assert_eq!(
            state_addrs,
            vec![contract_addr],
            "state emitter surfaces contract only"
        );

        let mut collector_slots = al_collector[0].storage_keys.clone();
        let mut state_slots = al_state[0].storage_keys.clone();
        collector_slots.sort();
        state_slots.sort();
        let mut expected = vec![B256::from(U256::from(1u64)), B256::from(U256::from(2u64))];
        expected.sort();
        assert_eq!(
            collector_slots, expected,
            "collector slots {collector_slots:?}"
        );
        assert_eq!(state_slots, expected, "state emitter slots {state_slots:?}");
        assert_eq!(
            collector_slots, state_slots,
            "ADR-019 D3 parity: collector AL must equal state-journal AL"
        );
    }
}
