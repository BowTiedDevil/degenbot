//! Pure-Rust `eth_simulateV1` `stateOverrides` construction.
//!
//! A pyo3-free *core leaf* that ports
//! `examples/eth_backrun_v2_v3_v4_rust.py::build_simulation_state_overrides`
//! (L416–L508). Builds the Alloy [`AccountOverride`] / [`StateOverride`] map
//! fed under `stateOverrides` in an `eth_simulateV1` call so the simulated
//! executor sees:
//!
//! - the **owner** funded with ETH for gas (100 ETH),
//! - the **injected executor** funded with ETH for V4 settlement + V3 callback
//!   WETH payments (10 ETH) and carrying the runtime bytecode, and
//! - the **warmup storage slots** (ERC6909 + WETH9) pre-warmed to 1 wei so the
//!   injected bytecode avoids cold-SSTORE penalties, with the WETH9
//!   `balanceOf` slot finally overridden to the operational 10-ETH amount.
//!
//! The warmup **slot addresses** are provided by
//! [`degenbot_executor::compute_simulation_warmup_slots`] (the §62H23D leaf);
//! they cross the crate boundary as the [`WarmupSlots`] struct. The 1-wei
//! warmed values + the `stateDiff` dict shape are constructed here (they are
//! trivial: the warmup intent is "touch the slot, value irrelevant below 1
//! wei"), mirroring the PyO3 adapter that the Python oracle calls through.
//!
//! # The merge subtlety
//!
//! When the warmup `stateDiff` entries are merged into `stateOverrides`, the
//! executor's own 10-ETH `balance` override must NOT be clobbered by the
//! warmup's residual-`0x0` balance entry for the same address. The merge is
//! **explicit-balance-wins**: an existing `balance` is preserved; only absent
//! balances are filled from the warmup. The WETH9 `balanceOf` slot, by
//! contrast, IS overwritten — the warmup wrote 1 wei there for
//! slot-warming; this leaf raises it to the operational 10 ETH.
//!
//! # Parity (§4.2)
//!
//! Byte-for-byte parity vs the Python oracle over the fixture corpus
//! (`inject_code` True/False × with/without warmup). The Rust output matches
//! the Python `stateOverrides` dict field-for-field: `balance` (owner 100 ETH;
//! injected executor 10 ETH), `code` (runtime bytecode at injected address),
//! `stateDiff` (merged warmup 1-wei entries + the WETH9 `balanceOf` slot
//! raised to 10 ETH).
//!
//! # Standalone-Rust consumer
//!
//! `cargo add degenbot-simulation` gives the full `stateOverrides` surface
//! with zero `pyo3` dependency (ADR-005 standalone-core). The runtime-bytecode
//! file-load (`_load_executor_runtime_bytecode`) stays Python — the bytes are
//! accepted as a `Bytes` arg (disposition A2 `stays-python`).

// Solidity/RPC identifiers (balanceOf, stateDiff, eth_simulateV1, ERC6909,
// WETH9, PoolManager, …) are ubiquitous in this crate's docs; allow the
// pedantic doc-markdown lint to match the peer core crates.
#![allow(clippy::doc_markdown)]

use alloy::primitives::{
    map::{AddressHashMap, B256HashMap},
    Address, Bytes, U256,
};
use alloy::rpc::types::eth::state::{AccountOverride, StateOverride};
use degenbot_executor::WarmupSlots;

/// Balance-call calldata builders (B1): WETH9 `balanceOf`, Multicall3
/// `getEthBalance`, PoolManager ERC6909 `balanceOf`, + `wrap_execute_calldata`.
/// Moved to `degenbot-evm` (shared with `simulate_in_process`); re-exported
/// here so existing `use degenbot_simulation::calldata::...` call sites stay
/// unchanged.
pub use degenbot_evm::calldata;

/// The `eth_simulateV1` `SimulatePayload` 7-call builder (B2).
pub mod payload;

/// The typed RPC dispatch leaf (B3/B4/B5): `eth_simulateV1` /
/// `eth_createAccessList` / `eth_feeHistory` over the typed
/// `degenbot_rpc::AlloyProvider` surfaces (the ZUZANP leaf), with the
/// response parse into typed [`dispatch::SimulationResult`] /
/// [`dispatch::BlockPriorityFees`].
pub mod dispatch;

/// The `simulate_one` per-path async orchestration (C1 capstone) + the
/// gross/net profit arithmetic (C2) + the market-aware age-decay priority
/// fee (C4) + the int128 guard (C3) + the `_tally_fail` revert-bucket
/// accumulator. Consumes the sibling A (TCTUAW) / B (PGA5S3) leaves + the
/// WWC4DL encode (YQORTM) + the WWC4DL revert taxonomy (SYI3PG).
pub mod simulate_one;

/// The `dispatch_profitable_results` fan-out + categorization (D-row). Fans
/// `simulate_one` out across a tokio `JoinSet` with a `MAX_SIMULATE_CONCURRENT`
/// semaphore, gathers with exception tolerance, categorizes into gas-
/// profitable / gas-unprofitable / exception, and integrates the thin-margin
/// pre-filter (SYI3PG) + `PathSuppression` (M756BN, consumed from
/// `degenbot-submission`).
pub mod dispatch_profitable;

// Re-export the most-used types at the crate root for ergonomic access
// (mirrors how `degenbot_executor` surfaces `WarmupSlots` / `mapping_slot`).
pub use degenbot_evm::{
    compute_priority_fee, fits_int128, BlockPriorityFees, FailBuckets, SimFailure, SimResult,
    SimulateContext, SimulatePath, AGE_DECAY_CONSTANT, EXECUTE_CONFIG, GAS_SAFETY_MARGIN,
    INITIAL_EXECUTE_GAS, INT128_MAX, INT128_MIN, MAX_PRIORITY_FEE_PERCENTILE,
    MIN_PRIORITY_FEE_PERCENTILE, TARGET_PROFIT_RATIO,
};
pub use dispatch::{SimulatedCall, SimulationResult};
pub use payload::{build_simulate_payload, SimulationParams, SIM_CALL_COUNT};
pub use simulate_one::simulate_one;

/// Operational funding amounts + the warmup 1-wei touch value.
///
/// All come straight from the Python oracle's literals: the owner gets 100 ETH
/// for gas; the injected executor gets 10 ETH for V4 settlement + V3 callback
/// WETH payments; the warmup slots are touched at exactly 1 wei.
mod amounts {
    use alloy::primitives::U256;

    /// ETH funded to the executor owner for gas.
    pub const OWNER_BALANCE_ETH: u64 = 100;
    /// ETH funded to the injected executor for V4 settlement + V3 callbacks.
    pub const INJECTED_BALANCE_ETH: u64 = 10;
    /// The 1-wei warmup touch value (ERC6909 + WETH9 slot pre-warming).
    pub const ONE_WEI: U256 = U256::from_limbs([1, 0, 0, 0]);
}

/// Build the `eth_simulateV1` `stateOverrides` map.
///
/// Ports `examples/eth_backrun_v2_v3_v4_rust.py::build_simulation_state_overrides`
/// (L416–L508). Returns a [`StateOverride`] (`AddressHashMap<AccountOverride>`)
/// applying all of: owner ETH funding, injected-executor code + ETH funding,
/// warmup-slot pre-warming (ERC6909 + WETH9, 1 wei), and the WETH9
/// `balanceOf` slot raised to 10 ETH.
///
/// # Arguments
///
/// - `executor_owner` — the operator key's address; funded with ETH for gas.
/// - `inject_code` — when `false`, only the owner is funded (mirrors the
///   Python `inject_code=False` branch). When `true`, the executor runtime
///   bytecode is injected at `injected_address` and the warmup/funding block
///   runs.
/// - `injected_address` — the fresh address at which to inject the executor.
///   Required when `inject_code` is `true`; ignored otherwise.
/// - `runtime_bytecode` — the executor runtime bytecode (CBOR + immutables
///   appended, per `contracts/recompile.py`). The file-load stays Python
///   (disposition A2); the bytes cross the FFI boundary here.
/// - `warmup` — the three warmup slot addresses from
///   [`degenbot_executor::compute_simulation_warmup_slots`] (the §62H23D
///   leaf). Consumed, not recomputed.
/// - `weth_address` — the WETH9 contract address (balanceOf override target).
/// - `pool_manager_address` — the Uniswap V4 PoolManager address (ERC6909
///   balance host). Present for parity with the Python oracle's merge.
///
/// # Merge ordering (the subtlety)
///
/// The warmup supplies a residual `balance: 0x0` entry for the executor
/// address. Because the injected-executor override already sets a 10-ETH
/// `balance`, the warmup's `0x0` must NOT replace it: [`merge_warmup`] only
/// fills a `balance` when none is present. The WETH9 `balanceOf` slot, by
/// contrast, is overwritten last to the operational 10 ETH regardless of the
/// warmup's 1-wei touch.
///
/// # Panics
///
/// Panics if `inject_code` is `true` and `injected_address` is `None`. Callers
/// must supply the injected address when requesting code injection (mirrors
/// the Python oracle's `if inject_code and injected_address` guard).
#[must_use]
pub fn build_simulation_state_overrides(
    executor_owner: Address,
    inject_code: bool,
    injected_address: Option<Address>,
    runtime_bytecode: Bytes,
    warmup: WarmupSlots,
    weth_address: Address,
    pool_manager_address: Address,
) -> StateOverride {
    let mut overrides: AddressHashMap<AccountOverride> = AddressHashMap::default();

    // Fund the executor owner with ETH for gas.
    overrides.insert(
        executor_owner,
        AccountOverride {
            balance: Some(eth_amount(amounts::OWNER_BALANCE_ETH)),
            ..Default::default()
        },
    );

    if inject_code {
        let injected = injected_address.expect(
            "inject_code is true but injected_address is None — supply the injection address",
        );

        // Inject the runtime bytecode at the fresh address + fund it with ETH
        // for V4 settlement and V3-callback WETH payments (the deployed
        // contract wraps 10 ETH at construction; code injection skips that, so
        // the balance is set explicitly here).
        overrides.insert(
            injected,
            AccountOverride {
                code: Some(runtime_bytecode),
                balance: Some(eth_amount(amounts::INJECTED_BALANCE_ETH)),
                ..Default::default()
            },
        );

        // Merge the warmup stateDiff entries (1-wei touches) into the overrides.
        // The executor's 10-ETH balance survives: merge_warmup only fills an
        // ABSENT balance, never replacing one already set.
        let weth_balance_slot = warmup.weth_balance;
        merge_warmup(
            &mut overrides,
            &warmup,
            weth_address,
            pool_manager_address,
            injected,
        );

        // Raise the WETH9 balanceOf slot from the warmup's 1-wei touch to the
        // operational 10-ETH amount for V3 callbacks.
        override_weth_balance(
            &mut overrides,
            weth_address,
            weth_balance_slot,
            eth_amount(amounts::INJECTED_BALANCE_ETH),
        );
    }

    StateOverride::from(overrides)
}

/// Merge the warmup slot entries into the override map.
///
/// Three entries land (mirroring the PyO3 adapter's dict shape):
/// - `WETH9`: `stateDiff` = `{weth_balance_slot: 1 wei}`,
/// - `PoolManager`: `stateDiff` = `{erc6909_weth_slot: 1 wei,
///   erc6909_native_slot: 1 wei}`,
/// - `executor`: residual `balance: 0` (kept only when no balance is present
///   yet — the merge subtlety: the injected executor's 10-ETH balance wins).
fn merge_warmup(
    overrides: &mut AddressHashMap<AccountOverride>,
    warmup: &WarmupSlots,
    weth_address: Address,
    pool_manager_address: Address,
    executor_address: Address,
) {
    // WETH9 entry.
    let weth_entry = overrides.entry(weth_address).or_default();
    set_state_diff(weth_entry, warmup.weth_balance, amounts::ONE_WEI);

    // PoolManager entry (two ERC6909 slots).
    let pm_entry = overrides.entry(pool_manager_address).or_default();
    set_state_diff(pm_entry, warmup.erc6909_weth, amounts::ONE_WEI);
    set_state_diff(pm_entry, warmup.erc6909_native, amounts::ONE_WEI);

    // Executor residual-balance entry: only fill when no balance is set. The
    // injected-executor override already carries 10 ETH, so this `0` is
    // dropped — the explicit funding wins (the merge subtlety).
    let exec_entry = overrides.entry(executor_address).or_default();
    if exec_entry.balance.is_none() {
        exec_entry.balance = Some(U256::ZERO);
    }
}

/// Set a single `stateDiff` slot→value pair on an override, creating the
/// `stateDiff` map if absent and merging into it otherwise.
fn set_state_diff(entry: &mut AccountOverride, slot: U256, value: U256) {
    let map = entry.state_diff.get_or_insert_with(B256HashMap::default);
    map.insert(u256_to_b256(slot), u256_to_b256(value));
}

/// Overwrite the WETH9 `balanceOf` slot with the operational ETH amount,
/// creating the WETH9 override + its `stateDiff` map if absent.
fn override_weth_balance(
    overrides: &mut AddressHashMap<AccountOverride>,
    weth_address: Address,
    weth_balance_slot: U256,
    balance: U256,
) {
    let entry = overrides.entry(weth_address).or_default();
    let map = entry.state_diff.get_or_insert_with(B256HashMap::default);
    map.insert(u256_to_b256(weth_balance_slot), u256_to_b256(balance));
}

/// A whole-ETH `U256` balance (`eth * 10**18`).
fn eth_amount(eth: u64) -> U256 {
    U256::from(eth) * U256::from(10u64.pow(18))
}

/// Convert a `U256` to its 32-byte big-endian `B256` slot key/value form.
///
/// Mirrors the Python oracle's `f"0x{value:064x}"` /
/// `"0x" + value.to_bytes(32, "big").hex()` — the canonical `stateDiff` slot
/// hex representation.
fn u256_to_b256(value: U256) -> alloy::primitives::B256 {
    alloy::primitives::B256::from(value.to_be_bytes::<32>())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use alloy::primitives::{address, Bytes, U256};

    /// Canonical mainnet WETH9 + PoolManager (per the Python oracle constants).
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");

    /// The executor owner + a fresh injected address (parity corpus addresses).
    const OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const INJECTED: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    /// A minimal non-trivial runtime bytecode stand-in for parity (the real
    /// bytecode is read from disk by the Python file-load; here it is opaque
    /// bytes the Rust leaf carries through verbatim).
    const RUNTIME_BYTECODE: &[u8] = &0xDEAD_BEEF_u32.to_be_bytes();

    fn warmup() -> WarmupSlots {
        degenbot_executor::compute_simulation_warmup_slots(INJECTED, WETH, PM)
    }

    // ---------------------------------------------------------------------
    // §4.2 golden values captured directly from the Python oracle
    // `build_simulation_state_overrides` (L416–L508) over the corpus addresses
    // above. These pin byte-for-byte parity: the Rust leaf must emit exactly
    // these slot hexes + balance values the Python oracle emits. Generated by
    // running the oracle (see the parity generator in the commit notes).
    // ---------------------------------------------------------------------

    /// 100 ETH, the owner's gas balance (Python: `100 * 10**18`).
    const OWNER_BALANCE: u128 = 100_000_000_000_000_000_000;
    /// 10 ETH, the injected executor's settlement + WETH-callback balance.
    const INJECTED_BALANCE: u128 = 10_000_000_000_000_000_000;

    // -------------------------------------------------------------------------
    // §4.2 parity fixtures — generated from the Python oracle
    // `build_simulation_state_overrides` (L416–L508). Each row pins the exact
    // override map the Python oracle emits for the corpus inputs. The Python
    // reference emits a dict keyed by checksummed addresses with string-hex
    // slot/value entries; the Rust leaf emits the typed `StateOverride` map.
    // The assertions below check field-for-field equality: which addresses are
    // present, their `balance` / `code` / `stateDiff` entries, and the merge
    // subtlety (the executor's 10-ETH balance survives the warmup `0x0`).
    // -------------------------------------------------------------------------

    #[test]
    fn inject_code_false_funds_only_owner() {
        let overrides =
            build_simulation_state_overrides(OWNER, false, None, Bytes::new(), warmup(), WETH, PM);

        // Owner funded with 100 ETH; no other overrides.
        assert_eq!(overrides.len(), 1);
        let owner = &overrides[&OWNER];
        assert_eq!(owner.balance, Some(eth_amount(amounts::OWNER_BALANCE_ETH)));
        assert!(owner.code.is_none());
        assert!(owner.state_diff.is_none());

        // No injected-executor / WETH9 / PoolManager entries.
        assert!(!overrides.contains_key(&INJECTED));
        assert!(!overrides.contains_key(&WETH));
        assert!(!overrides.contains_key(&PM));
    }

    #[test]
    fn inject_code_true_injects_code_and_funds_executor() {
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(INJECTED),
            Bytes::from_static(RUNTIME_BYTECODE),
            warmup(),
            WETH,
            PM,
        );

        // Owner funded with 100 ETH.
        let owner = &overrides[&OWNER];
        assert_eq!(owner.balance, Some(eth_amount(amounts::OWNER_BALANCE_ETH)));

        // Injected executor: code + 10 ETH balance (no stateDiff).
        let injected = &overrides[&INJECTED];
        assert_eq!(
            injected.code.as_ref().map(Bytes::as_ref),
            Some(RUNTIME_BYTECODE)
        );
        assert_eq!(
            injected.balance,
            Some(eth_amount(amounts::INJECTED_BALANCE_ETH))
        );
        assert!(injected.state_diff.is_none());
    }

    #[test]
    fn inject_code_true_warms_pool_manager_erc6909_slots_at_one_wei() {
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(INJECTED),
            Bytes::from_static(RUNTIME_BYTECODE),
            warmup(),
            WETH,
            PM,
        );

        let slots = warmup();
        let pm = overrides.get(&PM).expect("PoolManager override present");
        let sd = pm
            .state_diff
            .as_ref()
            .expect("PoolManager stateDiff present");

        // Two ERC6909 slots, each touched at exactly 1 wei.
        assert_eq!(
            sd.get(&u256_to_b256(slots.erc6909_weth)),
            Some(&u256_to_b256(U256::from(1)))
        );
        assert_eq!(
            sd.get(&u256_to_b256(slots.erc6909_native)),
            Some(&u256_to_b256(U256::from(1)))
        );
    }

    #[test]
    fn weth9_balanceof_slot_raised_to_operational_10_eth() {
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(INJECTED),
            Bytes::from_static(RUNTIME_BYTECODE),
            warmup(),
            WETH,
            PM,
        );

        let slots = warmup();
        let weth = overrides.get(&WETH).expect("WETH9 override present");
        let sd = weth.state_diff.as_ref().expect("WETH9 stateDiff present");

        // The warmup's 1-wei touch is OVERWRITTEN by the operational 10 ETH.
        assert_eq!(
            sd.get(&u256_to_b256(slots.weth_balance)),
            Some(&u256_to_b256(eth_amount(amounts::INJECTED_BALANCE_ETH)))
        );
        // No other slots on the WETH9 entry.
        assert_eq!(sd.len(), 1);
    }

    // ---------------------------------------------------------------------
    // The merge subtlety (called out explicitly per the AC):
    // ---------------------------------------------------------------------

    #[test]
    fn merge_subtlety_executor_10_eth_balance_not_clobbered_by_warmup_zero() {
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(INJECTED),
            Bytes::from_static(RUNTIME_BYTECODE),
            warmup(),
            WETH,
            PM,
        );

        // The warmup supplies a residual `balance: 0x0` for the executor
        // address. The injected-executor override already set 10 ETH. The merge
        // MUST preserve the 10 ETH and drop the 0x0 (explicit-balance-wins).
        let injected = &overrides[&INJECTED];
        assert_eq!(
            injected.balance,
            Some(eth_amount(amounts::INJECTED_BALANCE_ETH)),
            "executor balance must stay at 10 ETH, not be clobbered to 0 by warmup"
        );
        assert!(injected.code.is_some(), "code survives the merge too");
        assert!(injected.state_diff.is_none(), "executor gets no stateDiff");
    }

    #[test]
    #[should_panic(expected = "inject_code is true but injected_address is None")]
    fn inject_code_true_without_injected_address_panics() {
        let _ = build_simulation_state_overrides(
            OWNER,
            true,
            None,
            Bytes::from_static(RUNTIME_BYTECODE),
            warmup(),
            WETH,
            PM,
        );
    }

    // ---------------------------------------------------------------------
    // Internal helper parity: the eth_amount + u256_to_b256 round-trips match
    // the Python oracle's hex emission.
    // ---------------------------------------------------------------------

    #[test]
    fn eth_amount_matches_python_literals() {
        assert_eq!(eth_amount(100), U256::from(100_000_000_000_000_000_000u128));
        assert_eq!(eth_amount(10), U256::from(10_000_000_000_000_000_000u128));
    }

    #[test]
    fn one_wei_is_exactly_one() {
        assert_eq!(amounts::ONE_WEI, U256::from(1));
    }

    // ---------------------------------------------------------------------
    // §4.2 byte-for-byte golden-file parity vs the Python oracle. The slot
    // hexes + balance values are captured directly from the oracle's output
    // (see the `inject_true` / `inject_false` fixtures in the commit notes).
    // ---------------------------------------------------------------------

    /// The warmup slot `B256` keys the Python oracle emits (captured from
    /// `compute_simulation_warmup_slots` via the §62H23D PyO3 seam).
    const WETH_BALANCE_SLOT: alloy::primitives::B256 = alloy::primitives::b256!(
        "ca0453669a7127ce38f304ce121e552d78c30286022ebefeef6884684816084d"
    );
    const ERC6909_WETH_SLOT: alloy::primitives::B256 = alloy::primitives::b256!(
        "c87651b1e38cc90cedd910b68ad3a33c54f8132e8e50beaae3ca68aafafe854a"
    );
    const ERC6909_NATIVE_SLOT: alloy::primitives::B256 = alloy::primitives::b256!(
        "27b77f9e86613e8da78f13fe38575cf532b4167ab8d8d5303c689cc9fd0ed7ff"
    );

    #[test]
    fn golden_inject_code_false_matches_python_oracle() {
        let overrides =
            build_simulation_state_overrides(OWNER, false, None, Bytes::new(), warmup(), WETH, PM);

        // Exactly one entry: the owner with 100 ETH, nothing else.
        assert_eq!(overrides.len(), 1);
        let owner = &overrides[&OWNER];
        assert_eq!(owner.balance, Some(U256::from(OWNER_BALANCE)));
        assert!(owner.code.is_none());
        assert!(owner.state_diff.is_none());
        assert!(owner.state.is_none());
        assert!(owner.nonce.is_none());
    }

    #[test]
    fn golden_inject_code_true_matches_python_oracle_byte_for_byte() {
        let overrides = build_simulation_state_overrides(
            OWNER,
            true,
            Some(INJECTED),
            Bytes::from_static(RUNTIME_BYTECODE),
            warmup(),
            WETH,
            PM,
        );

        // Four entries: owner, injected executor, WETH9, PoolManager.
        assert_eq!(overrides.len(), 4);

        // Owner: 100 ETH, nothing else.
        let owner = &overrides[&OWNER];
        assert_eq!(owner.balance, Some(U256::from(OWNER_BALANCE)));
        assert!(owner.code.is_none());
        assert!(owner.state_diff.is_none());

        // Injected executor: 10 ETH balance + the runtime bytecode, no
        // stateDiff (the warmup's residual `0x0` balance was dropped by the
        // merge subtlety — explicit-balance-wins).
        let injected = &overrides[&INJECTED];
        assert_eq!(injected.balance, Some(U256::from(INJECTED_BALANCE)));
        assert_eq!(
            injected.code.as_ref().map(Bytes::as_ref),
            Some(RUNTIME_BYTECODE)
        );
        assert!(injected.state_diff.is_none());

        // WETH9: exactly one stateDiff slot, the `balanceOf` raised to 10 ETH
        // (the warmup's 1-wei touch overwritten).
        let weth = &overrides[&WETH];
        let weth_sd = weth.state_diff.as_ref().expect("WETH9 stateDiff");
        assert_eq!(weth_sd.len(), 1);
        let ten_eth = eth_amount(amounts::INJECTED_BALANCE_ETH);
        assert_eq!(
            weth_sd.get(&WETH_BALANCE_SLOT),
            Some(&u256_to_b256(ten_eth)),
            "WETH9 balanceOf slot must hold 10 ETH (0x8ac7230489e80000), got {weth_sd:?}"
        );
        assert!(weth.balance.is_none());

        // PoolManager: exactly two ERC6909 stateDiff slots, each at 1 wei.
        let pm = &overrides[&PM];
        let pm_sd = pm.state_diff.as_ref().expect("PoolManager stateDiff");
        assert_eq!(pm_sd.len(), 2);
        assert_eq!(
            pm_sd.get(&ERC6909_WETH_SLOT),
            Some(&u256_to_b256(U256::from(1))),
            "erc6909_weth slot must hold 1 wei"
        );
        assert_eq!(
            pm_sd.get(&ERC6909_NATIVE_SLOT),
            Some(&u256_to_b256(U256::from(1))),
            "erc6909_native slot must hold 1 wei"
        );
        assert!(pm.balance.is_none());
    }

    #[test]
    fn golden_warmup_slots_match_python_oracle_compute() {
        // The warmup slot addresses the leaf consumes must equal the
        // addresses the Python oracle computes for the same inputs (the §62H23D
        // leaf's own parity; cross-checked here end-to-end).
        let slots = warmup();
        assert_eq!(u256_to_b256(slots.weth_balance), WETH_BALANCE_SLOT);
        assert_eq!(u256_to_b256(slots.erc6909_weth), ERC6909_WETH_SLOT);
        assert_eq!(u256_to_b256(slots.erc6909_native), ERC6909_NATIVE_SLOT);
    }
}
