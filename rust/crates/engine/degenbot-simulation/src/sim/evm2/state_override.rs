//! The evm2 port of `apply_simulation_overrides`
//! (`sim::evm::state_override`) — the sim-scoped owner/executor funding +
//! bytecode injection + warmup-slot merge onto a `CacheDB`, field-for-field
//! with the same explicit-balance-wins merge (the WETH9 `balanceOf` slot IS
//! overwritten to the operational amount).
//!
//! One behavioral difference is forced by evm2's `CacheDB`: its
//! `insert_account_storage` writes the cache entry WITHOUT the backing
//! `load_account` fetch revm's version performs, so the cold-load side
//! effect (the WETH9/PoolManager code prefetch) does not happen here — those
//! accounts' code loads on first real read instead.
//!
//! # Capital targeting is its own decision
//!
//! A settlement warmup declares the WETH9 / PoolManager identities a deployment
//! accounts against plus the slots its contract touches; WHICH account those
//! slots name belongs to the warmup, and the built-in binding keys them on the
//! deployment's executor in both deployment arms (declared code and deployed
//! code). The capital overrides never inherit that answer. Each one names its
//! own account — [`SimulationOverrideParams::zero_balance`] for the clearing
//! set, [`SimulationOverrideParams::self_fund_usdc`] for the funded USDC
//! holder — and derives the WETH9 balance and PoolManager ERC6909 claim slots
//! from that account, so a probe's capital lands on the executor it names
//! whatever the deployment declares (code or none) and whichever account the
//! warmup keyed.
//!
//! The one account read this function may perform is the clearing target's own:
//! a deployed executor's account info (its code hash, above all) lives in the
//! chain, so it is materialized exactly as the EVM's own first read would
//! (see [`clear_native_balance`]).

use alloy::primitives::{Address, U256};

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, CacheDB, DynDatabase};

use degenbot_executor::compute_simulation_warmup_slots;

use crate::sim::state_override::{
    SimulationOverrideParams, EXECUTOR_FUND_ETH, ONE_WEI, OWNER_FUND_ETH, USDC_MAINNET,
    USDC_MAINNET_BALANCE_SLOT, WBTC_MAINNET, WBTC_MAINNET_BALANCE_SLOT,
};

/// Apply the simulation state-overrides onto an evm2 `CacheDB`,
/// field-for-field with the revm-side original (see the module doc).
///
/// The settlement warmup touches apply only for a deployment that declares the
/// identities ([`SimulationOverrideParams::settlement`]); a declared code
/// override installs its runtime at the declaration's own address. The capital
/// overrides target the account their own parameter names (see the module doc).
pub fn apply_simulation_overrides<ExtDb: DynDatabase>(
    cache_db: &mut CacheDB<ExtDb>,
    params: &SimulationOverrideParams,
) {
    cache_db.insert_account_info(
        &params.owner,
        AccountInfo::default().with_balance(eth_amount(OWNER_FUND_ETH)),
    );

    if let Some(code) = &params.code {
        cache_db.insert_account_info(
            &code.address,
            AccountInfo::default()
                .with_balance(eth_amount(EXECUTOR_FUND_ETH))
                .with_code(Bytecode::new_raw(code.runtime.clone())),
        );
    }

    if let Some(holder) = params.self_fund_usdc {
        let slot = degenbot_executor::mapping_slot(
            U256::from(USDC_MAINNET_BALANCE_SLOT),
            U256::from_be_bytes(holder.into_word().into()),
        );
        cache_db.insert_account_storage(&USDC_MAINNET, &slot, &eth_amount(EXECUTOR_FUND_ETH));
    }

    if let Some(settlement) = &params.settlement {
        // The three 1-wei warmup touches (WETH9 balanceOf, PoolManager
        // ERC6909 weth-id + native-id) — storage only, the executor's
        // balance survives.
        cache_db.insert_account_storage(
            &settlement.weth_address,
            &settlement.slots.weth_balance,
            &ONE_WEI,
        );
        if let Some(pool_manager) = settlement.pool_manager_address {
            cache_db.insert_account_storage(
                &pool_manager,
                &settlement.slots.erc6909_weth,
                &ONE_WEI,
            );
            cache_db.insert_account_storage(
                &pool_manager,
                &settlement.slots.erc6909_native,
                &ONE_WEI,
            );
        }

        // Raise the WETH9 balanceOf slot from the 1-wei touch to the
        // operational amount for V3 callbacks.
        cache_db.insert_account_storage(
            &settlement.weth_address,
            &settlement.slots.weth_balance,
            &eth_amount(EXECUTOR_FUND_ETH),
        );
    }

    if let Some(executor) = params.zero_balance {
        // Native ETH first: the clearing target is the account named here, in
        // BOTH deployment arms. A declared-code build keeps the runtime the
        // code override installed; a deployed executor keeps the code the chain
        // holds. Only the balance is replaced.
        clear_native_balance(cache_db, executor);

        for (token, base) in [
            (USDC_MAINNET, USDC_MAINNET_BALANCE_SLOT),
            (WBTC_MAINNET, WBTC_MAINNET_BALANCE_SLOT),
        ] {
            let slot = degenbot_executor::mapping_slot(
                U256::from(base),
                U256::from_be_bytes(executor.into_word().into()),
            );
            cache_db.insert_account_storage(&token, &slot, &U256::ZERO);
        }
        // The WETH9 balance and the PoolManager ERC6909 claims are keyed on
        // the clearing target itself, never on whichever account the declared
        // warmup's slots were derived for: a warmup keyed on the owner would
        // otherwise zero the owner's claim slots and leave the executor's
        // capital in place.
        if let Some(settlement) = &params.settlement {
            let slots = compute_simulation_warmup_slots(executor, settlement.weth_address);
            cache_db.insert_account_storage(
                &settlement.weth_address,
                &slots.weth_balance,
                &U256::ZERO,
            );
            if let Some(pool_manager) = settlement.pool_manager_address {
                cache_db.insert_account_storage(&pool_manager, &slots.erc6909_weth, &U256::ZERO);
                cache_db.insert_account_storage(&pool_manager, &slots.erc6909_native, &U256::ZERO);
            }
        }
    }
}

/// Clear an account's native ETH balance while preserving the account itself.
///
/// The zero-balance probe models an executor holding no capital in BOTH
/// deployment arms, so the clear cannot ride on a declared-code override: a
/// deployed contract's account info — its code hash above all — is the chain's,
/// and only the balance may change.
///
/// A cache that has not materialized the account yet loads it exactly as the
/// EVM's own first read would; a load failure is left to that read, which fails
/// at the same account with the same database error rather than letting a
/// cleared account shadow it. An account the chain reports absent already has a
/// zero balance and is left absent.
fn clear_native_balance<ExtDb: DynDatabase>(cache_db: &mut CacheDB<ExtDb>, account: Address) {
    if cache_db.account_info(&account).is_none() && cache_db.get_account(&account).is_err() {
        return;
    }
    let Some(existing) = cache_db.account_info(&account).cloned() else {
        return;
    };
    cache_db.insert_account_info(
        &account,
        AccountInfo {
            balance: U256::ZERO,
            ..existing
        },
    );
}

/// A whole-ETH `U256` balance (`eth * 10**18`).
fn eth_amount(eth: u64) -> U256 {
    U256::from(eth) * U256::from(10u128.pow(18))
}

#[cfg(test)]
#[expect(clippy::expect_used)] // test-only fixture reads: the accounts are inserted by the fixture
mod tests {
    use super::*;
    use alloy::primitives::{keccak256, Address, Bytes};

    use ::evm2::evm::Database;

    /// The `balances(account)` slot of a token whose mapping base is `base`.
    fn slot_for(base: u64, account: Address) -> U256 {
        degenbot_executor::mapping_slot(
            U256::from(base),
            U256::from_be_bytes(account.into_word().into()),
        )
    }

    #[test]
    fn zero_balance_strips_token_capital_but_preserves_owner_gas() {
        let executor = Address::from([0x42; 20]);
        let owner = Address::from([0x24; 20]);
        let weth = Address::from([0x01; 20]);
        let pm = Address::from([0x02; 20]);
        let executor_slots = compute_simulation_warmup_slots(executor, weth);
        let mut db = ::evm2::evm::InMemoryDB::default();
        apply_simulation_overrides(
            &mut db,
            &SimulationOverrideParams {
                owner,
                code: Some(crate::DeclaredCode {
                    address: executor,
                    runtime: alloy::primitives::Bytes::from_static(&[0x00]),
                }),
                settlement: Some(crate::SettlementWarmup {
                    weth_address: weth,
                    pool_manager_address: Some(pm),
                    slots: executor_slots,
                }),
                zero_balance: Some(executor),
                ..Default::default()
            },
        );
        assert_eq!(
            db.account_info(&executor).map(|a| a.balance),
            Some(U256::ZERO)
        );
        assert_eq!(
            db.account_info(&owner).map(|a| a.balance),
            Some(eth_amount(OWNER_FUND_ETH))
        );
        for (token, slot) in [
            (weth, executor_slots.weth_balance),
            (USDC_MAINNET, slot_for(USDC_MAINNET_BALANCE_SLOT, executor)),
            (WBTC_MAINNET, slot_for(WBTC_MAINNET_BALANCE_SLOT, executor)),
            (pm, executor_slots.erc6909_weth),
            (pm, executor_slots.erc6909_native),
        ] {
            assert_eq!(
                db.cache
                    .storage
                    .get(&token)
                    .and_then(|s| s.slots.get(&slot)),
                Some(&U256::ZERO),
                "every capital slot is cleared at the executor",
            );
        }
        let owner_slots = compute_simulation_warmup_slots(owner, weth);
        for (token, slot) in [
            (weth, owner_slots.weth_balance),
            (pm, owner_slots.erc6909_weth),
            (pm, owner_slots.erc6909_native),
        ] {
            assert_eq!(
                db.cache
                    .storage
                    .get(&token)
                    .and_then(|s| s.slots.get(&slot)),
                None,
                "no capital slot keys on the owner",
            );
        }
    }

    #[test]
    fn usdc_capital_is_scoped_to_the_executor_and_opt_in() {
        let executor = Address::from([0x42; 20]);
        let owner = Address::from([0x24; 20]);
        let slot = degenbot_executor::mapping_slot(
            U256::from(USDC_MAINNET_BALANCE_SLOT),
            U256::from_be_bytes(executor.into_word().into()),
        );
        let mut db = ::evm2::evm::InMemoryDB::default();
        let params = SimulationOverrideParams {
            owner,
            self_fund_usdc: Some(executor),
            ..Default::default()
        };
        apply_simulation_overrides(&mut db, &params);
        assert_eq!(
            db.cache
                .storage
                .get(&USDC_MAINNET)
                .and_then(|s| s.slots.get(&slot)),
            Some(&eth_amount(EXECUTOR_FUND_ETH)),
            "the deployed executor, not the owner fallback, receives probe capital",
        );
        let mut ordinary = ::evm2::evm::InMemoryDB::default();
        apply_simulation_overrides(&mut ordinary, &SimulationOverrideParams::default());
        assert_eq!(
            ordinary
                .cache
                .storage
                .get(&USDC_MAINNET)
                .and_then(|s| s.slots.get(&slot)),
            None,
            "ordinary solver simulation must not receive USDC",
        );
    }

    /// The two deployment arms every capital override must treat identically.
    ///
    /// The runtime the EVM executes at the executor comes from the declaration
    /// in one arm and from the chain in the other; nothing about capital
    /// targeting may depend on which.
    #[derive(Clone, Copy, Debug)]
    enum Arm {
        /// `code = None`: the executor's account info is the chain's.
        Deployed,
        /// `code = Some(..)`: the declared runtime is installed at the executor.
        Declared,
    }

    /// The runtime the chain holds at the deployed executor — deliberately not
    /// the declared runtime, so a cleared account that lost its code hash cannot
    /// pass by accident.
    const DEPLOYED_RUNTIME: &[u8] = &[0x60, 0x2a, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3];

    /// A declared build's runtime.
    const DECLARED_RUNTIME: &[u8] = &[0xfe];

    /// An unrelated account's runtime.
    const BYSTANDER_CODE: &[u8] = &[0x00];

    /// The chain the sim reads through: the executor's deployed account (code,
    /// native balance, nonce), the settlement contracts' balances keyed on the
    /// executor, the OWNER's own WETH9/claim slots, and an unrelated account the
    /// overrides must not touch.
    struct Chain {
        db: ::evm2::evm::InMemoryDB,
        owner: Address,
        executor: Address,
        weth: Address,
        pool_manager: Address,
        bystander: Address,
    }

    /// The unrelated account's native balance and its one storage slot.
    const BYSTANDER_ETH: u64 = 6;
    const BYSTANDER_SLOT_VALUE: u64 = 5;

    /// The executor's on-chain holdings the clearing must erase.
    const EXECUTOR_DEPLOYED_ETH: u64 = 3;
    const EXECUTOR_DEPLOYED_NONCE: u64 = 7;

    impl Chain {
        fn new() -> Self {
            let owner = Address::from([0x24; 20]);
            let executor = Address::from([0x42; 20]);
            let weth = Address::from([0x01; 20]);
            let pool_manager = Address::from([0x02; 20]);
            let bystander = Address::from([0x77; 20]);

            let mut db = ::evm2::evm::InMemoryDB::default();
            db.insert_account_info(
                &executor,
                AccountInfo::default()
                    .with_balance(eth_amount(EXECUTOR_DEPLOYED_ETH))
                    .with_nonce(EXECUTOR_DEPLOYED_NONCE)
                    .with_code(Bytecode::new_raw(Bytes::from_static(DEPLOYED_RUNTIME))),
            );
            db.insert_account_info(
                &bystander,
                AccountInfo::default()
                    .with_balance(eth_amount(BYSTANDER_ETH))
                    .with_code(Bytecode::new_raw(Bytes::from_static(BYSTANDER_CODE))),
            );
            db.insert_account_storage(
                &bystander,
                &U256::from(1),
                &U256::from(BYSTANDER_SLOT_VALUE),
            );

            let executor_slots = compute_simulation_warmup_slots(executor, weth);
            db.insert_account_storage(&weth, &executor_slots.weth_balance, &eth_amount(5));
            db.insert_account_storage(&pool_manager, &executor_slots.erc6909_weth, &U256::from(7));
            db.insert_account_storage(
                &pool_manager,
                &executor_slots.erc6909_native,
                &U256::from(9),
            );
            db.insert_account_storage(
                &USDC_MAINNET,
                &slot_for(USDC_MAINNET_BALANCE_SLOT, executor),
                &eth_amount(11),
            );
            db.insert_account_storage(
                &WBTC_MAINNET,
                &slot_for(WBTC_MAINNET_BALANCE_SLOT, executor),
                &eth_amount(13),
            );

            // The owner's own WETH9 and claim slots: the owner funds gas and
            // holds no contract capital, so no override may key on it.
            let owner_slots = compute_simulation_warmup_slots(owner, weth);
            db.insert_account_storage(&weth, &owner_slots.weth_balance, &eth_amount(4));
            db.insert_account_storage(&pool_manager, &owner_slots.erc6909_weth, &U256::from(43));
            db.insert_account_storage(&pool_manager, &owner_slots.erc6909_native, &U256::from(44));

            Self {
                db,
                owner,
                executor,
                weth,
                pool_manager,
                bystander,
            }
        }
    }

    /// The parity acceptance: the native, WETH9, USDC, WBTC, and
    /// PoolManager-claim overrides target the EXECUTOR in both deployment arms,
    /// with a distinct owner, while the owner's gas funding, the deployed code,
    /// and every unrelated account survive.
    ///
    /// The two pinned defects: native ETH clearing used to be conditional on
    /// declared code (so the deployed arm kept the executor's on-chain ETH), and
    /// the WETH9 / ERC6909 slots used to be taken from whatever account the
    /// declared warmup keyed (the owner, in the deployed arm) instead of from the
    /// executor the clear names.
    #[test]
    fn capital_overrides_target_the_executor_in_both_deployment_arms() {
        for (arm, expected_runtime, expected_nonce) in [
            (Arm::Deployed, DEPLOYED_RUNTIME, EXECUTOR_DEPLOYED_NONCE),
            (Arm::Declared, DECLARED_RUNTIME, 0),
        ] {
            let mut chain = Chain::new();
            let params = arm_params(arm, &chain);
            let mut db: CacheDB<::evm2::evm::InMemoryDB> =
                CacheDB::new(std::mem::take(&mut chain.db));
            apply_simulation_overrides(&mut db, &params);

            assert_executor_cleared(arm, &chain, &mut db, expected_runtime, expected_nonce);
            assert_capital_slots_cleared(arm, &chain, &db);
            assert_bystander_untouched(arm, &chain, &mut db);
        }
    }

    /// The overrides one deployment arm applies over a [`Chain`]: one capital
    /// target in BOTH arms, and a declared runtime only in the declared one.
    fn arm_params(arm: Arm, chain: &Chain) -> SimulationOverrideParams {
        SimulationOverrideParams {
            owner: chain.owner,
            code: match arm {
                Arm::Deployed => None,
                Arm::Declared => Some(crate::DeclaredCode {
                    address: chain.executor,
                    runtime: Bytes::from_static(DECLARED_RUNTIME),
                }),
            },
            settlement: Some(crate::SettlementWarmup {
                weth_address: chain.weth,
                pool_manager_address: Some(chain.pool_manager),
                slots: compute_simulation_warmup_slots(chain.executor, chain.weth),
            }),
            zero_balance: Some(chain.executor),
            ..Default::default()
        }
    }

    /// The account-side acceptance: the executor's native ETH is cleared whether
    /// or not code is declared, the code the arm executes survives with it (the
    /// chain's for a deployed executor, the declared runtime otherwise), and
    /// owner gas funding is untouched.
    fn assert_executor_cleared(
        arm: Arm,
        chain: &Chain,
        db: &mut CacheDB<::evm2::evm::InMemoryDB>,
        expected_runtime: &'static [u8],
        expected_nonce: u64,
    ) {
        let info = db
            .account_info(&chain.executor)
            .expect("the capital target is materialized in both arms")
            .clone();
        assert_eq!(
            info.balance,
            U256::ZERO,
            "{arm:?}: native ETH is cleared at the executor whether or not code is declared",
        );
        assert_eq!(
            info.code_hash,
            keccak256(expected_runtime),
            "{arm:?}: clearing preserves the code the arm executes",
        );
        assert_eq!(
            <CacheDB<::evm2::evm::InMemoryDB> as Database>::get_code_by_hash(db, &info.code_hash)
                .expect("the preserved code hash resolves")
                .original_bytes(),
            Bytes::from_static(expected_runtime),
            "{arm:?}: the executed runtime resolves from its preserved code hash",
        );
        assert_eq!(
            info.nonce, expected_nonce,
            "{arm:?}: the nonce is the arm's own (the chain's for a deployed executor)",
        );
        assert_eq!(
            db.account_info(&chain.owner).map(|a| a.balance),
            Some(eth_amount(OWNER_FUND_ETH)),
            "{arm:?}: owner gas funding survives the clearing",
        );
    }

    /// The slot-side acceptance: every capital slot the EXECUTOR owns is zeroed
    /// and no slot keyed on the OWNER is.
    fn assert_capital_slots_cleared(
        arm: Arm,
        chain: &Chain,
        db: &CacheDB<::evm2::evm::InMemoryDB>,
    ) {
        let executor_slots = compute_simulation_warmup_slots(chain.executor, chain.weth);
        for (label, token, slot) in [
            ("WETH9 balance", chain.weth, executor_slots.weth_balance),
            (
                "PoolManager WETH claim",
                chain.pool_manager,
                executor_slots.erc6909_weth,
            ),
            (
                "PoolManager native claim",
                chain.pool_manager,
                executor_slots.erc6909_native,
            ),
            (
                "USDC balance",
                USDC_MAINNET,
                slot_for(USDC_MAINNET_BALANCE_SLOT, chain.executor),
            ),
            (
                "WBTC balance",
                WBTC_MAINNET,
                slot_for(WBTC_MAINNET_BALANCE_SLOT, chain.executor),
            ),
        ] {
            assert_eq!(
                db.cache
                    .storage
                    .get(&token)
                    .and_then(|s| s.slots.get(&slot)),
                Some(&U256::ZERO),
                "{arm:?}: the {label} is cleared at the executor",
            );
        }

        let owner_slots = compute_simulation_warmup_slots(chain.owner, chain.weth);
        for (label, token, slot) in [
            ("WETH9 balance", chain.weth, owner_slots.weth_balance),
            (
                "PoolManager WETH claim",
                chain.pool_manager,
                owner_slots.erc6909_weth,
            ),
            (
                "PoolManager native claim",
                chain.pool_manager,
                owner_slots.erc6909_native,
            ),
        ] {
            assert_eq!(
                db.cache
                    .storage
                    .get(&token)
                    .and_then(|s| s.slots.get(&slot)),
                None,
                "{arm:?}: the owner's {label} is never a capital target",
            );
        }
    }

    /// The unrelated account keeps its balance, its code hash, and its storage.
    fn assert_bystander_untouched(
        arm: Arm,
        chain: &Chain,
        db: &mut CacheDB<::evm2::evm::InMemoryDB>,
    ) {
        let bystander =
            <CacheDB<::evm2::evm::InMemoryDB> as Database>::get_account(db, &chain.bystander)
                .expect("the unrelated account reads")
                .expect("the unrelated account exists");
        assert_eq!(
            bystander.balance,
            eth_amount(BYSTANDER_ETH),
            "{arm:?}: an unrelated account's balance is untouched",
        );
        assert_eq!(
            bystander.code_hash,
            keccak256(BYSTANDER_CODE),
            "{arm:?}: an unrelated account's code is untouched",
        );
        assert_eq!(
            <CacheDB<::evm2::evm::InMemoryDB> as Database>::get_storage(
                db,
                &chain.bystander,
                &U256::from(1)
            )
            .expect("the unrelated account's storage reads"),
            U256::from(BYSTANDER_SLOT_VALUE),
            "{arm:?}: an unrelated account's storage is untouched",
        );
    }

    /// The warmup separation: a deployment whose declared warmup slots key on
    /// the OWNER (the shape the built-in binding used to produce for a deployed
    /// executor) cannot redirect the clearing. The executor's WETH9 balance and
    /// both PoolManager claims are the ones zeroed; the warmup's own key keeps
    /// only the warmup's touches.
    #[test]
    fn clearing_follows_the_capital_target_not_the_warmup_subject() {
        let chain = Chain::new();
        let owner_slots = compute_simulation_warmup_slots(chain.owner, chain.weth);
        let params = SimulationOverrideParams {
            owner: chain.owner,
            code: None,
            settlement: Some(crate::SettlementWarmup {
                weth_address: chain.weth,
                pool_manager_address: Some(chain.pool_manager),
                slots: owner_slots,
            }),
            zero_balance: Some(chain.executor),
            ..Default::default()
        };
        let mut db: CacheDB<::evm2::evm::InMemoryDB> = CacheDB::new(chain.db);
        apply_simulation_overrides(&mut db, &params);

        let executor_slots = compute_simulation_warmup_slots(chain.executor, chain.weth);
        for (label, token, slot) in [
            ("WETH9 balance", chain.weth, executor_slots.weth_balance),
            (
                "PoolManager WETH claim",
                chain.pool_manager,
                executor_slots.erc6909_weth,
            ),
            (
                "PoolManager native claim",
                chain.pool_manager,
                executor_slots.erc6909_native,
            ),
        ] {
            assert_eq!(
                db.cache
                    .storage
                    .get(&token)
                    .and_then(|s| s.slots.get(&slot)),
                Some(&U256::ZERO),
                "the executor's {label} is cleared from the executor's own key",
            );
        }

        // The warmup's own key is left to the warmup: the operational WETH9
        // balance the settlement funds, and the two 1-wei claim touches.
        assert_eq!(
            db.cache
                .storage
                .get(&chain.weth)
                .and_then(|s| s.slots.get(&owner_slots.weth_balance)),
            Some(&eth_amount(EXECUTOR_FUND_ETH)),
            "the clearing does not follow the warmup's key",
        );
        for slot in [owner_slots.erc6909_weth, owner_slots.erc6909_native] {
            assert_eq!(
                db.cache
                    .storage
                    .get(&chain.pool_manager)
                    .and_then(|s| s.slots.get(&slot)),
                Some(&ONE_WEI),
                "the warmup's claim touches stay warmup touches",
            );
        }
    }
}
