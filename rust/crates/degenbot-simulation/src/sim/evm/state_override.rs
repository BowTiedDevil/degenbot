//! Sim-scoped override application on a `CacheDB`.
//!
//! Applies the owner-funded-100-ETH + injected-executor+runtime-bytecode +
//! warmup-slots + WETH9 `balanceOf` override into `CacheDB::insert_account_storage` /
//! `insert_account_info` calls, preserving the **explicit-balance-wins** merge
//! documented on the source leaf (the executor's 10-ETH `balance` override
//! must NOT be clobbered by the warmup's residual-`0x0` balance; the WETH9
//! `balanceOf` slot IS overwritten by the warmup-to-10-ETH raise).
//!
//! This adaptor is **backing-agnostic** — it only calls `CacheDB::*` methods
//! that work identically whether the backing `Database` is `WrapDatabaseAsync<
//! AlloyDB>` (option A) or `BotStateDb<WrapDatabaseAsync<AlloyDB>>` (option B).
//!
//! See ADR-019 for the verified `CacheDB` insertion API (the detailing spike
//! doc was removed in the stale-docs cleanup `71ec78b2`).

use degenbot_executor::WarmupSlots;
use revm::database::CacheDB;
use revm::database_interface::DatabaseRef;
use revm::primitives::{Address, Bytes, U256};
use revm::state::AccountInfo;

/// The owner's ETH-funding amount for gas (mirrors the Python oracle's literal
/// at `amounts::OWNER_BALANCE_ETH`).
pub const OWNER_FUND_ETH: u64 = 100;

/// The injected executor's ETH-funding amount for V4 settlement + V3 callback
/// WETH payments (mirrors `amounts::INJECTED_BALANCE_ETH`).
pub const EXECUTOR_FUND_ETH: u64 = 10;

/// The 1-wei warmup touch value (ERC6909 + WETH9 slot pre-warming).
pub const ONE_WEI: U256 = U256::from_limbs([1, 0, 0, 0]);

/// Parameters for the simulation state-override application, supplied by
/// the dispatch leaf. Carries the addresses + runtime bytecode + the warmup
/// slots computed by `degenbot-executor`.
#[derive(Debug, Clone)]
pub struct SimulationOverrideParams {
    /// The operator key's address — the owner funded with ETH for gas.
    pub owner: Address,
    /// Whether to inject the executor runtime bytecode at `injected_address`.
    pub inject_code: bool,
    /// The address to inject the executor bytecode at (used iff `inject_code`).
    pub injected_address: Option<Address>,
    /// The executor runtime bytecode (injected when `inject_code` is `true`).
    pub runtime_bytecode: Bytes,
    /// The warmup slots (WETH9 `balanceOf`, PoolManager ERC6909 `balanceOf`)
    /// from `degenbot_executor::compute_simulation_warmup_slots`.
    pub warmup: WarmupSlots,
    /// WETH9 contract address.
    pub weth_address: Address,
    /// Uniswap V4 PoolManager address.
    pub pool_manager_address: Address,
}

/// Apply the simulation state-overrides onto a `CacheDB`, field-for-field.
///
/// The merge is **explicit-balance-wins**: an existing `balance` is preserved;
/// only absent balances are filled from the warmup. The WETH9 `balanceOf` slot
/// IS overwritten to the operational 10-ETH amount. See the source leaf's
/// doc-comment for the merge subtlety.
///
/// # Cold-load side effect
///
/// `insert_account_storage` calls `load_account`, which fetches the account
/// from the backing `Database` if not yet cached. For WETH9 + PoolManager this
/// triggers one `basic` cold-load each (3 concurrent RPCs under `AlloyDB`),
/// populating the code cache so subsequent sim calls pay zero RPC. The owner +
/// injected executor are inserted via `insert_account_info` (no backing
/// fetch — the owner is the funded EOA; the injected executor is fresh).
///
/// # Errors
///
/// Returns [`OverrideError`] if a backing `DatabaseRef` fetch fails
/// (propagated from `CacheDB::insert_account_storage`'s `load_account`).
///
/// # Panics
///
/// Panics if `params.inject_code` is `true` but `params.injected_address`
/// is `None` — the caller must supply the injection address when requesting
/// code injection (mirrors the source leaf's `.expect`).
pub fn apply_simulation_overrides<ExtDb>(
    cache_db: &mut CacheDB<ExtDb>,
    params: &SimulationOverrideParams,
) -> Result<(), OverrideError>
where
    ExtDb: DatabaseRef,
    <ExtDb as DatabaseRef>::Error: std::fmt::Display,
{
    // Fund the executor owner with ETH for gas.
    cache_db.insert_account_info(
        params.owner,
        AccountInfo {
            balance: eth_amount(OWNER_FUND_ETH),
            ..AccountInfo::default()
        },
    );

    if params.inject_code {
        #[expect(clippy::expect_used)] // inject_code => injected_address required (guarded above)
        let injected = params.injected_address.expect(
            "inject_code is true but injected_address is None — supply the injection address",
        );

        // Inject the runtime bytecode at the fresh address + fund it with ETH
        // for V4 settlement and V3-callback WETH payments (the deployed
        // contract wraps 10 ETH at construction; code injection skips that, so
        // the balance is set explicitly here). insert_account_info's
        // insert_contract fills code_hash from the runtime bytecode.
        cache_db.insert_account_info(
            injected,
            AccountInfo {
                balance: eth_amount(EXECUTOR_FUND_ETH),
                code: Some(revm::bytecode::Bytecode::new_raw(
                    params.runtime_bytecode.clone(),
                )),
                ..AccountInfo::default()
            },
        );

        // Merge the warmup storage slots (1-wei touches) onto WETH9 + PM.
        // The injected executor's 10-ETH balance survives (it was set above and
        // the warmup only touches STORAGE, never balance, on the executor).
        merge_warmup_slots(
            cache_db,
            &params.warmup,
            params.weth_address,
            params.pool_manager_address,
        )?;

        // Raise the WETH9 balanceOf slot from the warmup's 1-wei touch to the
        // operational 10-ETH amount for V3 callbacks.
        insert_weth_balance_override(
            cache_db,
            params.weth_address,
            params.warmup.weth_balance,
            eth_amount(EXECUTOR_FUND_ETH),
        )?;
    }

    Ok(())
}

/// Merge the warmup slot entries (1-wei touches) onto the WETH9 + PoolManager
/// accounts in the `CacheDB`. Mirrors `merge_warmup` in the source leaf, but
/// writes to `CacheDB::insert_account_storage` instead of `stateDiff` dicts.
///
/// Three slot writes land: WETH9 `balanceOf` -> 1 wei, PM ERC6909 (weth-id) ->
/// 1 wei, PM ERC6909 (native-id) -> 1 wei. The executor's 10-ETH balance (set
/// above) is untouched — `insert_account_storage` only writes STORAGE.
fn merge_warmup_slots<ExtDb>(
    cache_db: &mut CacheDB<ExtDb>,
    warmup: &WarmupSlots,
    weth_address: Address,
    pool_manager_address: Address,
) -> Result<(), OverrideError>
where
    ExtDb: DatabaseRef,
    <ExtDb as DatabaseRef>::Error: std::fmt::Display,
{
    // WETH9 balanceOf(executor) slot — 1-wei warmup touch.
    cache_db
        .insert_account_storage(weth_address, warmup.weth_balance, ONE_WEI)
        .map_err(|e| OverrideError::Insertion(e.to_string()))?;

    // PoolManager ERC6909 balanceOf(executor, weth_id) — 1-wei warmup touch.
    cache_db
        .insert_account_storage(pool_manager_address, warmup.erc6909_weth, ONE_WEI)
        .map_err(|e| OverrideError::Insertion(e.to_string()))?;

    // PoolManager ERC6909 balanceOf(executor, native_id) — 1-wei warmup touch.
    cache_db
        .insert_account_storage(pool_manager_address, warmup.erc6909_native, ONE_WEI)
        .map_err(|e| OverrideError::Insertion(e.to_string()))?;

    Ok(())
}

/// Overwrite the WETH9 `balanceOf` slot with the operational ETH amount. The
/// warmup wrote 1 wei there for slot-warming; this raises it to 10 ETH.
fn insert_weth_balance_override<ExtDb>(
    cache_db: &mut CacheDB<ExtDb>,
    weth_address: Address,
    weth_balance_slot: U256,
    balance: U256,
) -> Result<(), OverrideError>
where
    ExtDb: DatabaseRef,
    <ExtDb as DatabaseRef>::Error: std::fmt::Display,
{
    cache_db
        .insert_account_storage(weth_address, weth_balance_slot, balance)
        .map_err(|e| OverrideError::Insertion(e.to_string()))
}

/// A whole-ETH `U256` balance (`eth * 10**18`). Mirrors the source leaf's
/// `eth_amount` helper.
fn eth_amount(eth: u64) -> U256 {
    U256::from(eth) * U256::from(10u128.pow(18))
}

/// Errors raised by `apply_simulation_overrides`.
#[derive(Debug, thiserror::Error)]
pub enum OverrideError {
    /// A `CacheDB` insertion's backing fetch failed (propagated from
    /// `load_account` -> `DatabaseRef::basic_ref` — e.g. an `AlloyDB` RPC
    /// failure for a WETH9/PM cold-load).
    #[error("CacheDB insertion backing fetch failed: {0}")]
    Insertion(String),
}

#[expect(clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;
    use degenbot_executor::compute_simulation_warmup_slots;
    use revm::database::CacheDB;
    use revm::database_interface::EmptyDB;
    use revm::primitives::Address;

    /// Canonical mainnet WETH9 + PoolManager (per the Python oracle constants).
    const WETH: Address = address!("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2");
    const PM: Address = address!("000000000004444c5dc75cb358380d2e3de08a90");

    /// The executor owner + a fresh injected address (parity corpus addresses
    /// from `degenbot-simulation`'s §4.2 golden-value tests).
    const OWNER: Address = address!("9c56a29c7231974c269e24f9fb3c29203039089e");
    const INJECTED: Address = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    /// A minimal non-trivial runtime bytecode stand-in for parity (the real
    /// bytecode is read from disk by the Python file-load; here opaque bytes the
    /// Rust leaf carries through verbatim).
    const RUNTIME_BYTECODE: &[u8] = &0xDEAD_BEEF_u32.to_be_bytes();

    /// Read back a `CacheDB` storage slot as a `U256` for parity comparison.
    fn read_storage(db: &CacheDB<EmptyDB>, addr: Address, slot: U256) -> U256 {
        db.storage_ref(addr, slot).unwrap_or_default()
    }

    /// The §4.2 parity assertion: the `CacheDB` state written by
    /// `apply_simulation_overrides` matches the oracle's emitted values
    /// field-for-field — `balance`, `code`, and each warmup slot value — over
    /// the corpus `inject_code` True/False.
    ///
    /// The `EmptyDB` backing isolates the port's writes from the backing's
    /// reads (every untracked account returns not-existing; the parity
    /// assertion is over the OVERRIDE writes only, which is what the JSON
    /// oracle specifies).
    #[test]
    fn apply_simulation_overrides_writes_match_oracle_values() {
        for inject_code in [false, true] {
            let warmup = compute_simulation_warmup_slots(INJECTED, WETH);
            let runtime = Bytes::from_static(RUNTIME_BYTECODE);

            let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
            apply_simulation_overrides(
                &mut cache_db,
                &SimulationOverrideParams {
                    owner: OWNER,
                    inject_code,
                    injected_address: (inject_code).then_some(INJECTED),
                    runtime_bytecode: runtime.clone(),
                    warmup,
                    weth_address: WETH,
                    pool_manager_address: PM,
                },
            )
            .expect("apply_simulation_overrides with EmptyDB backing cannot fail");

            // ── owner: funded with 100 ETH, no code injected.
            let owner_info = cache_db.basic_ref(OWNER).expect("owner loaded");
            assert_eq!(
                owner_info.as_ref().map(|i| i.balance),
                Some(eth_amount(OWNER_FUND_ETH)),
                "owner balance: inject_code={inject_code}",
            );

            if inject_code {
                // ── injected executor: 10 ETH + the runtime bytecode.
                let injected_info = cache_db
                    .basic_ref(INJECTED)
                    .expect("injected loaded")
                    .expect("injected account present");
                assert_eq!(
                    injected_info.balance,
                    eth_amount(EXECUTOR_FUND_ETH),
                    "injected balance: inject_code={inject_code}",
                );
                assert_eq!(
                    injected_info
                        .code
                        .expect("injected code present")
                        .original_bytes(),
                    runtime,
                    "injected runtime bytecode carried verbatim: inject_code={inject_code}",
                );

                // ── WETH9 balanceOf slot: warmed to 1 wei then RAISED to 10 ETH
                //    (the explicit-balance-wins + raise semantics).
                assert_eq!(
                    read_storage(&cache_db, WETH, warmup.weth_balance),
                    eth_amount(EXECUTOR_FUND_ETH),
                    "WETH9 balanceOf slot raised to 10 ETH: inject_code={inject_code}",
                );

                // ── PM ERC6909 slots: both warmed to 1 wei.
                assert_eq!(
                    read_storage(&cache_db, PM, warmup.erc6909_weth),
                    ONE_WEI,
                    "PM ERC6909 (weth-id) warmed to 1 wei: inject_code={inject_code}",
                );
                assert_eq!(
                    read_storage(&cache_db, PM, warmup.erc6909_native),
                    ONE_WEI,
                    "PM ERC6909 (native-id) warmed to 1 wei: inject_code={inject_code}",
                );
            }
        }
    }

    /// When `inject_code` is false, no warmup slots are written (only the owner
    /// is funded). This pins the no-inject branch.
    #[test]
    fn apply_simulation_overrides_skip_when_no_code_injection() {
        let warmup = compute_simulation_warmup_slots(INJECTED, WETH);
        let mut cache_db: CacheDB<EmptyDB> = CacheDB::new(EmptyDB::default());
        apply_simulation_overrides(
            &mut cache_db,
            &SimulationOverrideParams {
                owner: OWNER,
                inject_code: false,
                injected_address: None,
                runtime_bytecode: Bytes::new(),
                warmup,
                weth_address: WETH,
                pool_manager_address: PM,
            },
        )
        .expect("EmptyDB backing cannot fail");

        // Owner funded; nothing else written.
        let owner_info = cache_db.basic_ref(OWNER).expect("owner loaded");
        assert_eq!(
            owner_info.as_ref().map(|i| i.balance),
            Some(eth_amount(OWNER_FUND_ETH)),
        );
        // WETH9 slot was NOT written (merge_warmup + raise skipped) -> ZERO.
        assert_eq!(
            read_storage(&cache_db, WETH, warmup.weth_balance),
            U256::ZERO,
            "WETH9 slot untouched when inject_code=false",
        );
    }
}
