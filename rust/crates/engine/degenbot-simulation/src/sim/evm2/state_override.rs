//! The evm2 port of `apply_simulation_overrides` — the sim-scoped owner/executor
//! funding + bytecode injection + warmup-slot merge + capital-plan application
//! onto a `CacheDB`.
//!
//! One behavioral difference is forced by evm2's `CacheDB`: its
//! `insert_account_storage` writes the cache entry WITHOUT the backing
//! `load_account` fetch revm's version performs, so the cold-load side effect
//! (the WETH9/PoolManager code prefetch) does not happen here — those accounts'
//! code loads on first real read instead.
//!
//! # Capital targeting is its own decision
//!
//! A settlement warmup declares the WETH9 / PoolManager identities a deployment
//! accounts against plus the slots its contract touches; WHICH account those
//! slots name belongs to the warmup, and the built-in binding keys them on the
//! deployment's executor in both deployment arms (declared code and deployed
//! code). The capital overrides never inherit that answer. They are the ONE
//! [`crate::CapitalOverridePlan`] a fan-out composed, and every write it carries
//! was already resolved — contract, slot, and value — from the account the plan
//! names (see [`crate::sim::capital`]). This function applies them and decides
//! nothing: no token list, no slot arithmetic, no mode.
//!
//! # Order
//!
//! Owner funding, declared code, settlement warmup, then the capital plan. The
//! plan goes LAST so it is authoritative for the assets it names: a warmup's
//! 1-wei touch or its operational WETH9 balance can never reintroduce capital a
//! zero-balance clearing removed, and can never overwrite a self-funded
//! requirement the built Plan declared.
//!
//! The one account read this function may perform is a capital target's own: a
//! deployed executor's account info (its code hash, above all) lives in the
//! chain, so it is materialized exactly as the EVM's own first read would (see
//! [`set_native_balance`]).

use alloy::primitives::{Address, U256};

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{AccountInfo, CacheDB, DynDatabase};

use crate::sim::capital::{CapitalOverride, CapitalTarget};
use crate::sim::state_override::{
    SimulationOverrideParams, EXECUTOR_FUND_ETH, ONE_WEI, OWNER_FUND_ETH,
};

/// Apply the simulation state-overrides onto an evm2 `CacheDB`.
///
/// The settlement warmup touches apply only for a deployment that declares the
/// identities ([`SimulationOverrideParams::settlement`]); a declared code
/// override installs its runtime at the declaration's own address. The capital
/// plan's writes apply last, at the account the plan names (see the module doc).
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
        // operational amount for V3 callbacks. A capital plan that names this
        // account's WETH9 balance overwrites it below — the plan is the
        // authority for every asset it declares.
        cache_db.insert_account_storage(
            &settlement.weth_address,
            &settlement.slots.weth_balance,
            &eth_amount(EXECUTOR_FUND_ETH),
        );
    }

    apply_capital_plan(cache_db, params);
}

/// Apply the composed capital plan: every resolved write, in plan order.
///
/// The account and its writes are ONE value on the plan, so "writes present,
/// no account to apply them to" is unrepresentable rather than a state this
/// function would have to skip in release builds. `Production` carries no write
/// at all, so an ordinary simulation keeps the chain's own state — including the
/// executor's balance, which is never materialized here.
fn apply_capital_plan<ExtDb: DynDatabase>(
    cache_db: &mut CacheDB<ExtDb>,
    params: &SimulationOverrideParams,
) {
    let Some((holder, capital_writes)) = params.capital.writes() else {
        return;
    };
    for capital_write in capital_writes {
        apply_capital_override(cache_db, holder, capital_write);
    }
}

/// Apply one resolved override at the plan's account.
fn apply_capital_override<ExtDb: DynDatabase>(
    cache_db: &mut CacheDB<ExtDb>,
    holder: Address,
    capital_write: &CapitalOverride,
) {
    match capital_write.target() {
        // The balance word, set through the account: a clearing replaces the
        // balance and keeps the code and nonce the arm executes, a funding
        // installs exactly the declared amount.
        CapitalTarget::NativeBalance => {
            set_native_balance(cache_db, holder, capital_write.value());
        }
        CapitalTarget::Storage { contract, slot } => {
            cache_db.insert_account_storage(&contract, &slot, &capital_write.value());
        }
    }
}

/// Set an account's native ETH balance while preserving the account itself.
///
/// A capital probe models the executor's own holdings in BOTH deployment arms,
/// so the write cannot ride on a declared-code override: a deployed contract's
/// account info — its code hash above all — is the chain's, and only the balance
/// may change.
///
/// A cache that has not materialized the account yet loads it exactly as the
/// EVM's own first read would; a load failure is left to that read, which fails
/// at the same account with the same database error rather than letting a
/// rewritten account shadow it. An account the chain reports absent is simply
/// created with the requested balance (the EVM would read exactly that).
fn set_native_balance<ExtDb: DynDatabase>(
    cache_db: &mut CacheDB<ExtDb>,
    account: Address,
    balance: U256,
) {
    if cache_db.account_info(&account).is_none() && cache_db.get_account(&account).is_err() {
        return;
    }
    match cache_db.account_info(&account).cloned() {
        Some(existing) => {
            cache_db.insert_account_info(
                &account,
                AccountInfo {
                    balance,
                    ..existing
                },
            );
        }
        // The chain reports the account absent, so it already reads a zero
        // balance: a clearing has nothing to write.
        None if balance.is_zero() => {}
        // A funded account the chain does not hold is created with exactly the
        // declared balance — the code hash of an absent account is empty, which
        // is what the EVM would read.
        None => {
            cache_db.insert_account_info(&account, AccountInfo::default().with_balance(balance));
        }
    }
}

/// A whole-ETH `U256` balance (`eth * 10**18`).
pub(crate) fn eth_amount(eth: u64) -> U256 {
    U256::from(eth) * U256::from(10u128.pow(18))
}

#[cfg(test)]
#[expect(clippy::expect_used)] // test-only fixture reads: the accounts are inserted by the fixture
mod tests {
    use super::*;
    use alloy::primitives::{keccak256, Bytes};
    use degenbot_execution::{CapitalRequirement, HopInfo, PathInfo, V2HopInfo, V4HopInfo};

    use ::evm2::evm::Database;

    use crate::sim::capital::{
        CapitalAsset, CapitalLayouts, CapitalPath, CapitalPlanError, ProbeCapital, USDC_MAINNET,
        USDC_MAINNET_BALANCE_SLOT, WBTC_MAINNET, WBTC_MAINNET_BALANCE_SLOT,
    };

    const OWNER: Address = Address::new([0x24; 20]);
    const EXECUTOR: Address = Address::new([0x42; 20]);
    const LAB_WETH: Address = Address::new([0x01; 20]);
    const LAB_TOKEN: Address = Address::new([0x03; 20]);
    const LAB_POOL_MANAGER: Address = Address::new([0x02; 20]);
    const BYSTANDER: Address = Address::new([0x77; 20]);

    /// The layouts this fixture declares EXPLICITLY: its lab WETH at slot 3, its
    /// lab token at slot 5, and its lab PoolManager at the canonical ERC-6909
    /// base — beside the pinned mainnet set.
    fn layouts() -> CapitalLayouts {
        CapitalLayouts::verified()
            .with_token(LAB_WETH, 3)
            .with_token(LAB_TOKEN, 5)
            .with_manager(LAB_POOL_MANAGER, 4)
    }

    fn slot_for(base: u64, account: Address) -> U256 {
        degenbot_executor::mapping_slot(
            U256::from(base),
            U256::from_be_bytes(account.into_word().into()),
        )
    }

    fn claim_slot(base: u64, account: Address, currency: Address) -> U256 {
        degenbot_executor::nested_mapping_slot(
            U256::from(base),
            U256::from_be_bytes(account.into_word().into()),
            degenbot_executor::erc6909_id(currency),
        )
    }

    fn lab_path() -> PathInfo {
        PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: Address::from([0xa1; 20]),
                token0_address: LAB_WETH,
                token1_address: LAB_TOKEN,
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: Address::from([0xa2; 20]),
                token0_address: LAB_TOKEN,
                token1_address: LAB_WETH,
                fee: 30,
                zfo: true,
            }),
        ])
    }

    /// A path naming the pinned mainnet USDC + WBTC beside the lab WETH, so the
    /// clearing covers the production tokens AND the fixture's own.
    fn mixed_path() -> PathInfo {
        PathInfo::new(vec![
            HopInfo::V2(V2HopInfo {
                pool_address: Address::from([0xa1; 20]),
                token0_address: LAB_WETH,
                token1_address: USDC_MAINNET,
                fee: 30,
                zfo: true,
            }),
            HopInfo::V2(V2HopInfo {
                pool_address: Address::from([0xa2; 20]),
                token0_address: USDC_MAINNET,
                token1_address: WBTC_MAINNET,
                fee: 30,
                zfo: true,
            }),
        ])
    }

    fn v4_path() -> PathInfo {
        PathInfo::new(vec![HopInfo::V4(V4HopInfo {
            pool_manager_address: LAB_POOL_MANAGER,
            pool_id_hex: "0x01".to_string(),
            currency0_address: LAB_WETH,
            currency1_address: LAB_TOKEN,
            fee: 500,
            tick_spacing: 10,
            hook_address: Address::ZERO,
            zfo: true,
        })])
    }

    /// The external initial capital a deployment's Encode declares: the lab WETH
    /// entry.
    fn requirements() -> Vec<CapitalRequirement> {
        vec![CapitalRequirement {
            currency: LAB_WETH,
            amount: 1_000_000,
        }]
    }

    fn plan(
        mode: ProbeCapital,
        path: &PathInfo,
        requirements: &[CapitalRequirement],
    ) -> crate::CapitalOverridePlan {
        mode.override_plan(EXECUTOR, &[CapitalPath { path, requirements }], &layouts())
            .expect("the fixture declares its layouts")
    }

    fn cache_lookup<ExtDb: DynDatabase>(
        db: &CacheDB<ExtDb>,
        token: Address,
        slot: U256,
    ) -> Option<U256> {
        db.cache
            .storage
            .get(&token)
            .and_then(|storage| storage.slots.get(&slot))
            .copied()
    }

    fn executor_warmup() -> crate::SettlementWarmup {
        crate::SettlementWarmup {
            weth_address: LAB_WETH,
            pool_manager_address: Some(LAB_POOL_MANAGER),
            slots: degenbot_executor::compute_simulation_warmup_slots(EXECUTOR, LAB_WETH),
        }
    }

    #[test]
    fn zero_balance_strips_every_touched_asset_but_preserves_owner_gas() {
        let path = lab_path();
        let mut db = ::evm2::evm::InMemoryDB::default();
        apply_simulation_overrides(
            &mut db,
            &SimulationOverrideParams {
                owner: OWNER,
                code: Some(crate::DeclaredCode {
                    address: EXECUTOR,
                    runtime: Bytes::from_static(&[0x00]),
                }),
                settlement: Some(executor_warmup()),
                capital: plan(ProbeCapital::ZeroBalance, &path, &[]),
            },
        );
        assert_eq!(
            db.account_info(&EXECUTOR).map(|account| account.balance),
            Some(U256::ZERO),
            "native ETH is cleared at the executor"
        );
        assert_eq!(
            db.account_info(&OWNER).map(|account| account.balance),
            Some(eth_amount(OWNER_FUND_ETH)),
            "owner gas funding survives"
        );
        for (token, slot) in [
            (LAB_WETH, slot_for(3, EXECUTOR)),
            (LAB_TOKEN, slot_for(5, EXECUTOR)),
        ] {
            assert_eq!(
                cache_lookup(&db, token, slot),
                Some(U256::ZERO),
                "every touched token word is cleared at the executor"
            );
        }
        for (token, slot) in [
            (LAB_WETH, slot_for(3, OWNER)),
            (LAB_TOKEN, slot_for(5, OWNER)),
        ] {
            assert_eq!(
                cache_lookup(&db, token, slot),
                None,
                "the owner's own words are never a capital target"
            );
        }
        // This path touches no V4 currency, so the warmup's own claim touches
        // are the only ones present — and they stay warmup facts, never capital.
        let slots = degenbot_executor::compute_simulation_warmup_slots(EXECUTOR, LAB_WETH);
        for slot in [slots.erc6909_weth, slots.erc6909_native] {
            assert_eq!(cache_lookup(&db, LAB_POOL_MANAGER, slot), Some(ONE_WEI));
        }
    }

    /// PoolManager claim coverage for a V4 path: both currencies' claims are
    /// cleared, including the non-WETH one, and each asset is named once.
    #[test]
    fn zero_balance_clears_pool_manager_claims_for_v4_currencies() {
        let path = v4_path();
        let composed = plan(ProbeCapital::ZeroBalance, &path, &[]);
        let claim = |currency| CapitalAsset::Erc6909Claim {
            manager: LAB_POOL_MANAGER,
            currency,
        };
        assert_eq!(
            composed.assets().collect::<Vec<_>>(),
            vec![
                CapitalAsset::Native,
                CapitalAsset::Erc20 { token: LAB_WETH },
                claim(LAB_WETH),
                CapitalAsset::Erc20 { token: LAB_TOKEN },
                claim(LAB_TOKEN),
            ],
            "native, each currency once, and each claim once"
        );
        let mut db = ::evm2::evm::InMemoryDB::default();
        apply_simulation_overrides(
            &mut db,
            &SimulationOverrideParams {
                owner: OWNER,
                capital: composed,
                ..Default::default()
            },
        );
        for currency in [LAB_WETH, LAB_TOKEN] {
            assert_eq!(
                cache_lookup(&db, LAB_POOL_MANAGER, claim_slot(4, EXECUTOR, currency)),
                Some(U256::ZERO),
                "the claim of {currency} is cleared at the executor"
            );
        }
    }

    #[test]
    fn self_fund_installs_the_declared_requirement_over_the_warmup() {
        let path = lab_path();
        let requirements = requirements();
        let mut db = ::evm2::evm::InMemoryDB::default();
        apply_simulation_overrides(
            &mut db,
            &SimulationOverrideParams {
                owner: OWNER,
                settlement: Some(executor_warmup()),
                capital: plan(ProbeCapital::SelfFund, &path, &requirements),
                ..Default::default()
            },
        );
        assert_eq!(
            cache_lookup(&db, LAB_WETH, slot_for(3, EXECUTOR)),
            Some(U256::from(1_000_000u64)),
            "the declared requirement wins over the warmup's operational WETH balance"
        );
        assert_eq!(
            cache_lookup(&db, LAB_TOKEN, slot_for(5, EXECUTOR)),
            Some(U256::ZERO),
            "a touched-but-unrequired token is cleared, never granted a blanket balance"
        );
        assert_eq!(
            cache_lookup(
                &db,
                USDC_MAINNET,
                slot_for(USDC_MAINNET_BALANCE_SLOT, EXECUTOR)
            ),
            None,
            "an untouched production token is never written at all"
        );
        assert!(
            db.account_info(&EXECUTOR)
                .is_none_or(|account| account.balance.is_zero()),
            "the touched native balance is cleared (an absent account already reads zero)"
        );
    }

    /// The production arm of the ONE capital value: no override at all.
    #[test]
    fn production_applies_no_capital_override() {
        let mut db = ::evm2::evm::InMemoryDB::default();
        apply_simulation_overrides(
            &mut db,
            &SimulationOverrideParams {
                owner: OWNER,
                capital: ProbeCapital::Production
                    .override_plan(EXECUTOR, &[], &layouts())
                    .expect("production declares nothing"),
                ..Default::default()
            },
        );
        for (token, slot) in [
            (LAB_WETH, slot_for(3, EXECUTOR)),
            (LAB_TOKEN, slot_for(5, EXECUTOR)),
            (USDC_MAINNET, slot_for(USDC_MAINNET_BALANCE_SLOT, EXECUTOR)),
            (WBTC_MAINNET, slot_for(WBTC_MAINNET_BALANCE_SLOT, EXECUTOR)),
        ] {
            assert_eq!(
                cache_lookup(&db, token, slot),
                None,
                "production installs no probe capital"
            );
        }
        assert_eq!(
            db.account_info(&EXECUTOR).map(|info| info.balance),
            None,
            "production never materializes (or clears) the executor's balance"
        );
        assert_eq!(
            db.account_info(&OWNER).map(|info| info.balance),
            Some(eth_amount(OWNER_FUND_ETH)),
            "owner gas funding is unchanged"
        );
    }

    /// The two deployment arms every capital override must treat identically.
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
    const DECLARED_RUNTIME: &[u8] = &[0xfe];
    const BYSTANDER_CODE: &[u8] = &[0x00];
    const BYSTANDER_ETH: u64 = 6;
    const BYSTANDER_SLOT_VALUE: u64 = 5;
    const EXECUTOR_DEPLOYED_ETH: u64 = 3;
    const EXECUTOR_DEPLOYED_NONCE: u64 = 7;

    /// The chain the sim reads through: the executor's deployed account (code,
    /// native balance, nonce), the settlement contracts' words keyed on the
    /// executor, the OWNER's own words, and an unrelated account the overrides
    /// must not touch.
    fn chain() -> ::evm2::evm::InMemoryDB {
        let mut db = ::evm2::evm::InMemoryDB::default();
        db.insert_account_info(
            &EXECUTOR,
            AccountInfo::default()
                .with_balance(eth_amount(EXECUTOR_DEPLOYED_ETH))
                .with_nonce(EXECUTOR_DEPLOYED_NONCE)
                .with_code(Bytecode::new_raw(Bytes::from_static(DEPLOYED_RUNTIME))),
        );
        db.insert_account_info(
            &BYSTANDER,
            AccountInfo::default()
                .with_balance(eth_amount(BYSTANDER_ETH))
                .with_code(Bytecode::new_raw(Bytes::from_static(BYSTANDER_CODE))),
        );
        db.insert_account_storage(
            &BYSTANDER,
            &U256::from(1),
            &U256::from(BYSTANDER_SLOT_VALUE),
        );

        let executor_slots = degenbot_executor::compute_simulation_warmup_slots(EXECUTOR, LAB_WETH);
        db.insert_account_storage(&LAB_WETH, &executor_slots.weth_balance, &eth_amount(5));
        db.insert_account_storage(
            &LAB_POOL_MANAGER,
            &executor_slots.erc6909_weth,
            &U256::from(7),
        );
        db.insert_account_storage(
            &LAB_POOL_MANAGER,
            &executor_slots.erc6909_native,
            &U256::from(9),
        );
        db.insert_account_storage(&LAB_TOKEN, &slot_for(5, EXECUTOR), &eth_amount(11));
        db.insert_account_storage(
            &USDC_MAINNET,
            &slot_for(USDC_MAINNET_BALANCE_SLOT, EXECUTOR),
            &eth_amount(13),
        );
        db.insert_account_storage(
            &WBTC_MAINNET,
            &slot_for(WBTC_MAINNET_BALANCE_SLOT, EXECUTOR),
            &eth_amount(17),
        );

        // The owner's own WETH9 and claim words: the owner funds gas and holds
        // no contract capital, so no override may key on it.
        let owner_slots = degenbot_executor::compute_simulation_warmup_slots(OWNER, LAB_WETH);
        db.insert_account_storage(&LAB_WETH, &owner_slots.weth_balance, &eth_amount(4));
        db.insert_account_storage(
            &LAB_POOL_MANAGER,
            &owner_slots.erc6909_weth,
            &U256::from(43),
        );
        db.insert_account_storage(
            &LAB_POOL_MANAGER,
            &owner_slots.erc6909_native,
            &U256::from(44),
        );
        db
    }

    /// The overrides one deployment arm applies over the fixture chain: one
    /// capital target in BOTH arms, and a declared runtime only in the declared
    /// one.
    fn arm_params(arm: Arm, path: &PathInfo) -> SimulationOverrideParams {
        SimulationOverrideParams {
            owner: OWNER,
            code: match arm {
                Arm::Deployed => None,
                Arm::Declared => Some(crate::DeclaredCode {
                    address: EXECUTOR,
                    runtime: Bytes::from_static(DECLARED_RUNTIME),
                }),
            },
            settlement: Some(executor_warmup()),
            capital: plan(ProbeCapital::ZeroBalance, path, &[]),
        }
    }

    /// The parity acceptance: every capital override targets the EXECUTOR in both
    /// deployment arms, with a distinct owner, while owner gas funding, the
    /// deployed code, and every unrelated account survive.
    #[test]
    fn capital_overrides_target_the_executor_in_both_deployment_arms() {
        for (arm, expected_runtime, expected_nonce) in [
            (Arm::Deployed, DEPLOYED_RUNTIME, EXECUTOR_DEPLOYED_NONCE),
            (Arm::Declared, DECLARED_RUNTIME, 0),
        ] {
            let path = mixed_path();
            let params = arm_params(arm, &path);
            let mut db: CacheDB<::evm2::evm::InMemoryDB> = CacheDB::new(chain());
            apply_simulation_overrides(&mut db, &params);
            assert_cleared_at_the_executor(arm, expected_runtime, expected_nonce, &mut db);
            assert_owner_words_survive(arm, &db);

            let bystander =
                <CacheDB<::evm2::evm::InMemoryDB> as Database>::get_account(&mut db, &BYSTANDER)
                    .expect("the unrelated account reads")
                    .expect("the unrelated account exists");
            assert_eq!(
                bystander.balance,
                eth_amount(BYSTANDER_ETH),
                "{arm:?}: an unrelated account's balance is untouched"
            );
            assert_eq!(
                bystander.code_hash,
                keccak256(BYSTANDER_CODE),
                "{arm:?}: an unrelated account's code is untouched"
            );
            assert_eq!(
                <CacheDB<::evm2::evm::InMemoryDB> as Database>::get_storage(
                    &mut db,
                    &BYSTANDER,
                    &U256::from(1)
                )
                .expect("the unrelated account's storage reads"),
                U256::from(BYSTANDER_SLOT_VALUE),
                "{arm:?}: an unrelated account's storage is untouched"
            );
        }
    }

    /// The executor-side acceptance: the balance, the code the arm executes, the
    /// nonce, and every touched token word.
    fn assert_cleared_at_the_executor(
        arm: Arm,
        expected_runtime: &'static [u8],
        expected_nonce: u64,
        db: &mut CacheDB<::evm2::evm::InMemoryDB>,
    ) {
        let info = db
            .account_info(&EXECUTOR)
            .expect("the capital target is materialized in both arms")
            .clone();
        assert_eq!(
            info.balance,
            U256::ZERO,
            "{arm:?}: native ETH is cleared at the executor whether or not code is declared"
        );
        assert_eq!(
            info.code_hash,
            keccak256(expected_runtime),
            "{arm:?}: clearing preserves the code the arm executes"
        );
        assert_eq!(
            <CacheDB<::evm2::evm::InMemoryDB> as Database>::get_code_by_hash(db, &info.code_hash)
                .expect("the preserved code hash resolves")
                .original_bytes(),
            Bytes::from_static(expected_runtime),
            "{arm:?}: the executed runtime resolves from its preserved code hash"
        );
        assert_eq!(
            info.nonce, expected_nonce,
            "{arm:?}: the nonce is the arm's own"
        );
        for (label, token, slot) in [
            ("WETH9 balance", LAB_WETH, slot_for(3, EXECUTOR)),
            (
                "USDC balance",
                USDC_MAINNET,
                slot_for(USDC_MAINNET_BALANCE_SLOT, EXECUTOR),
            ),
            (
                "WBTC balance",
                WBTC_MAINNET,
                slot_for(WBTC_MAINNET_BALANCE_SLOT, EXECUTOR),
            ),
        ] {
            assert_eq!(
                cache_lookup(db, token, slot),
                Some(U256::ZERO),
                "{arm:?}: the {label} is cleared at the executor"
            );
        }
    }

    /// The owner-side acceptance: gas funding survives, and no owner-held word is
    /// a capital target.
    fn assert_owner_words_survive(arm: Arm, db: &CacheDB<::evm2::evm::InMemoryDB>) {
        assert_eq!(
            db.account_info(&OWNER).map(|account| account.balance),
            Some(eth_amount(OWNER_FUND_ETH)),
            "{arm:?}: owner gas funding survives the clearing"
        );
        let owner_slots = degenbot_executor::compute_simulation_warmup_slots(OWNER, LAB_WETH);
        for (label, token, slot) in [
            ("WETH9 balance", LAB_WETH, owner_slots.weth_balance),
            (
                "PoolManager WETH claim",
                LAB_POOL_MANAGER,
                owner_slots.erc6909_weth,
            ),
            (
                "PoolManager native claim",
                LAB_POOL_MANAGER,
                owner_slots.erc6909_native,
            ),
        ] {
            assert_ne!(
                cache_lookup(db, token, slot),
                Some(U256::ZERO),
                "{arm:?}: the owner's {label} is never a capital target"
            );
        }
    }

    /// The warmup separation: a deployment whose declared warmup slots key on the
    /// OWNER (the shape the built-in binding used to produce for a deployed
    /// executor) cannot redirect the clearing, and its own key keeps only the
    /// warmup's touches.
    #[test]
    fn clearing_follows_the_capital_target_not_the_warmup_subject() {
        let owner_slots = degenbot_executor::compute_simulation_warmup_slots(OWNER, LAB_WETH);
        let path = lab_path();
        let params = SimulationOverrideParams {
            owner: OWNER,
            code: None,
            settlement: Some(crate::SettlementWarmup {
                weth_address: LAB_WETH,
                pool_manager_address: Some(LAB_POOL_MANAGER),
                slots: owner_slots,
            }),
            capital: plan(ProbeCapital::ZeroBalance, &path, &[]),
        };
        let mut db: CacheDB<::evm2::evm::InMemoryDB> = CacheDB::new(chain());
        apply_simulation_overrides(&mut db, &params);

        assert_eq!(
            cache_lookup(&db, LAB_WETH, slot_for(3, EXECUTOR)),
            Some(U256::ZERO),
            "the executor's WETH9 balance is cleared from the executor's own key"
        );
        assert_eq!(
            cache_lookup(&db, LAB_WETH, owner_slots.weth_balance),
            Some(eth_amount(EXECUTOR_FUND_ETH)),
            "the clearing does not follow the warmup's key"
        );
        for slot in [owner_slots.erc6909_weth, owner_slots.erc6909_native] {
            assert_eq!(
                cache_lookup(&db, LAB_POOL_MANAGER, slot),
                Some(ONE_WEI),
                "the warmup's claim touches stay warmup touches"
            );
        }
    }

    /// The fail-closed boundary: an unverified token refuses the plan, so no
    /// override is ever applied and the seeded holding survives.
    #[test]
    fn an_unverified_token_refuses_before_any_override_is_applied() {
        let unknown = Address::from([0x99; 20]);
        let path = PathInfo::new(vec![HopInfo::V2(V2HopInfo {
            pool_address: Address::from([0xa1; 20]),
            token0_address: LAB_WETH,
            token1_address: unknown,
            fee: 30,
            zfo: true,
        })]);
        let refusal = ProbeCapital::ZeroBalance
            .override_plan(
                EXECUTOR,
                &[CapitalPath {
                    path: &path,
                    requirements: &[],
                }],
                &layouts(),
            )
            .expect_err("the token's layout is unverified");
        assert_eq!(
            refusal,
            CapitalPlanError::UnverifiedLayout {
                asset: CapitalAsset::Erc20 { token: unknown }
            }
        );
        assert!(
            refusal.to_string().contains("no verified storage layout"),
            "the refusal names the gap: {refusal}"
        );

        let mut db: CacheDB<::evm2::evm::InMemoryDB> = CacheDB::new(chain());
        apply_simulation_overrides(
            &mut db,
            &SimulationOverrideParams {
                owner: OWNER,
                capital: ProbeCapital::Production
                    .override_plan(EXECUTOR, &[], &layouts())
                    .expect("production declares nothing"),
                ..Default::default()
            },
        );
        assert_ne!(
            cache_lookup(&db, LAB_WETH, slot_for(3, EXECUTOR)),
            Some(U256::ZERO),
            "a refused plan never clears anything"
        );
    }
}
