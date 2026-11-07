use super::{
    PreparedTx, access_list_counts, effective_gas_price, execute_initial_frame,
    initial_gas_and_reservoir, intrinsic_and_floor_gas, prepare_initial_frame,
    validate_block_gas_limit, validate_chain_id, validate_create_initcode,
    validate_execution_gas_limit_cap, validate_floor_gas, validate_gas_price,
    validate_intrinsic_gas, validate_nonce_not_overflow, validate_priority_fee,
    validate_sender_and_bump_nonce, validate_tx_gas_limit_cap, warm_access_list,
    warm_base_accounts,
};
use crate::{
    EvmFeatures, EvmTypes, TxResult,
    env::TxEnvExt,
    evm::handler::{DefaultTxHandlerHooks, GasSettlement, TxHandlerHooks},
    interpreter::GasTracker,
    registry::{HandlerError, HandlerResult, TxRequest},
};
use alloy_consensus::transaction::TxEip4844Variant;
use alloy_eips::eip4844::{DATA_GAS_PER_BLOB, VERSIONED_HASH_VERSION_KZG};
use alloy_primitives::U256;

/// Executes an EIP-4844 transaction using Ethereum rules.
pub fn handle<T: EvmTypes>(
    req: TxRequest<'_, '_, T, TxEip4844Variant>,
) -> HandlerResult<TxResult<T>> {
    handle_with_hooks::<T, DefaultTxHandlerHooks>(req)
}

/// Executes an EIP-4844 transaction using Ethereum rules and custom handler hooks.
pub fn handle_with_hooks<T: EvmTypes, H: TxHandlerHooks<T>>(
    mut req: TxRequest<'_, '_, T, TxEip4844Variant>,
) -> HandlerResult<TxResult<T>> {
    let prepared = prepare_with_hooks::<T, H>(&mut req)?;
    execute_prepared::<T, H>(req, prepared)
}

/// Validates an EIP-4844 transaction and applies its pre-execution state changes.
pub fn prepare_with_hooks<T: EvmTypes, H: TxHandlerHooks<T>>(
    req: &mut TxRequest<'_, '_, T, TxEip4844Variant>,
) -> HandlerResult<PreparedTx> {
    let caller = req.tx.signer();
    let tx = req.tx.inner().tx();
    let max_fee_per_gas = U256::from(tx.max_fee_per_gas);
    let max_priority_fee_per_gas = U256::from(tx.max_priority_fee_per_gas);
    let gas_price =
        effective_gas_price(max_fee_per_gas, max_priority_fee_per_gas, req.host.block.basefee);
    let max_fee_per_blob_gas = U256::from(tx.max_fee_per_blob_gas);

    validate_priority_fee(req.host.version(), max_fee_per_gas, max_priority_fee_per_gas)?;
    validate_gas_price(req.host.version(), gas_price, req.host.block.basefee)?;
    validate_chain_id(req.host.version(), Some(tx.chain_id), false)?;
    if req.host.feature(EvmFeatures::BLOB_BASE_FEE_CHECK) {
        validate_blob_fee(max_fee_per_blob_gas, req.host.block.blob_basefee)?;
    }
    validate_blobs(&tx.blob_versioned_hashes, req.host.version().max_blobs_per_tx)?;
    validate_tx_gas_limit_cap(req.host.version(), tx.gas_limit)?;
    validate_block_gas_limit(req.host.version(), tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(req.host.version(), tx.to.into(), &tx.input)?;
    validate_nonce_not_overflow(tx.nonce)?;
    let (access_list_accounts, access_list_storage_keys) = access_list_counts(&tx.access_list);
    let (mut intrinsic, mut floor_gas) = intrinsic_and_floor_gas(
        req.host.version(),
        caller,
        tx.to.into(),
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
        tx.value,
    );
    // Blob transactions are always calls; EIP-2780 charges any state-dependent gas at the runtime
    // gas phase, so no state gas is charged at the intrinsic phase.
    let mut initial_state_gas = 0;
    H::adjust_intrinsic_gas(
        req.host,
        req.envelope,
        &mut intrinsic,
        &mut initial_state_gas,
        &mut floor_gas,
    )?;
    validate_intrinsic_gas(tx.gas_limit, intrinsic, initial_state_gas)?;
    validate_floor_gas(tx.gas_limit, floor_gas)?;
    validate_execution_gas_limit_cap(req.host.version(), tx.gas_limit, intrinsic, floor_gas)?;

    let blob_gas_cost = U256::from(DATA_GAS_PER_BLOB) * U256::from(tx.blob_versioned_hashes.len());
    let max_blob_gas_cost = blob_gas_cost * max_fee_per_blob_gas;
    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    let sender_nonce = validate_sender_and_bump_nonce(
        req.host,
        caller,
        tx.nonce,
        max_gas_cost.saturating_add(max_blob_gas_cost).saturating_add(tx.value),
    )?;

    warm_base_accounts(req.host, tx.to.into());
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    let blob_basefee_cost = blob_gas_cost * req.host.block.blob_basefee;
    H::before_execution(req.host, req.envelope, caller, effective_gas_cost + blob_basefee_cost)?;

    Ok(PreparedTx { caller, gas_price, intrinsic, initial_state_gas, floor_gas, sender_nonce })
}

/// Executes and settles a prepared EIP-4844 transaction.
pub fn execute_prepared<T: EvmTypes, H: TxHandlerHooks<T>>(
    req: TxRequest<'_, '_, T, TxEip4844Variant>,
    prepared: PreparedTx,
) -> HandlerResult<TxResult<T>> {
    let PreparedTx { caller, gas_price, intrinsic, initial_state_gas, floor_gas, sender_nonce } =
        prepared;
    let tx = req.tx.inner().tx();

    let (execution_gas_limit, reservoir) =
        initial_gas_and_reservoir(req.host.version(), tx.gas_limit, intrinsic, initial_state_gas);
    *req.host.tx_env_mut() = TxEnvExt {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        blob_hashes: tx.blob_versioned_hashes.iter().copied().map(Into::into).collect(),
        ext: T::TxEnvExt::default(),
        _non_exhaustive: (),
    };
    let mut tx_gas =
        GasTracker::new_with_execution_gas_and_reservoir(execution_gas_limit, reservoir);
    let frame = prepare_initial_frame(
        req.host,
        caller,
        sender_nonce,
        tx.to.into(),
        &tx.input,
        tx.value,
        &mut tx_gas,
    )?;
    let result =
        execute_initial_frame(req.host, frame, &mut tx_gas, execution_gas_limit, reservoir)?;
    H::settle_transaction(
        req.host,
        req.envelope,
        GasSettlement {
            caller,
            gas_price,
            gas_limit: tx.gas_limit,
            floor_gas,
            initial_state_gas,
            state_refund: 0,
            result,
        },
    )
}

fn validate_blob_fee(max_fee_per_blob_gas: U256, blob_basefee: U256) -> HandlerResult<()> {
    if max_fee_per_blob_gas < blob_basefee {
        return Err(HandlerError::BlobFeeCapLessThanBlobBaseFee {
            max_fee_per_blob_gas,
            blob_base_fee: blob_basefee,
        });
    }
    Ok(())
}

fn validate_blobs(blobs: &[alloy_primitives::B256], max_blobs: usize) -> HandlerResult<()> {
    if blobs.is_empty() {
        return Err(HandlerError::EmptyBlobs);
    }
    if blobs.len() > max_blobs {
        return Err(HandlerError::TooManyBlobs { have: blobs.len(), max: max_blobs });
    }
    if blobs.iter().any(|blob| blob[0] != VERSIONED_HASH_VERSION_KZG) {
        return Err(HandlerError::BlobVersionNotSupported);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes, Evm, ExecutionConfig, Precompiles, SpecId, Version,
        env::BlockEnvExt,
        ethereum::{TxEnvelope, ethereum_tx_registry},
        evm::{AccountInfo, InMemoryDB},
    };
    use alloc::vec;
    use alloy_consensus::{TxEip4844, transaction::Recovered};
    use alloy_primitives::{Address, B256};

    #[rstest::rstest]
    fn blob_fee_validation_policy(
        #[values(true, false)] base_fee_check: bool,
        #[values(true, false)] blob_base_fee_check: bool,
        #[values(0, 6, 7, 8)] max_fee_per_blob_gas: u128,
    ) {
        let caller = Address::with_last_byte(0xaa);
        let mut version = Version::new(SpecId::CANCUN);
        version.features.set(EvmFeatures::BASE_FEE_CHECK, base_fee_check);
        version.features.set(EvmFeatures::BLOB_BASE_FEE_CHECK, blob_base_fee_check);
        let mut db = InMemoryDB::default();
        db.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(U256::from(10_000_000)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::CANCUN, version),
            SpecId::CANCUN,
            BlockEnvExt {
                basefee: U256::from(2),
                blob_basefee: U256::from(7),
                ..Default::default()
            },
            ethereum_tx_registry(SpecId::CANCUN),
            db,
            Precompiles::base(SpecId::CANCUN),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Eip4844(
                TxEip4844 {
                    chain_id: 1,
                    gas_limit: 21_000,
                    max_fee_per_gas: 2,
                    max_fee_per_blob_gas,
                    blob_versioned_hashes: vec![B256::repeat_byte(1)],
                    to: Address::with_last_byte(0xbb),
                    ..Default::default()
                }
                .into(),
            ),
            caller,
        );
        let expected = if blob_base_fee_check && max_fee_per_blob_gas < 7 {
            Err(HandlerError::BlobFeeCapLessThanBlobBaseFee {
                max_fee_per_blob_gas: U256::from(max_fee_per_blob_gas),
                blob_base_fee: U256::from(7),
            })
        } else {
            Ok(())
        };
        assert_eq!(evm.validate_tx(&tx), expected);
        let result = evm.transact(&tx).map(|executed| executed.commit());
        assert_eq!(result.as_ref().map(|_| ()).map_err(Clone::clone), expected);
        if let Ok(result) = result {
            assert!(result.is_success());
            assert_eq!(result.tx_gas_used(), 21_000);
            // Disabling validation does not disable charging the actual blob base fee.
            let account = evm.state_mut().account(&caller).unwrap();
            assert_eq!(account.nonce(), 1);
            assert_eq!(account.balance(), U256::from(10_000_000 - 21_000 * 2 - 131_072 * 7));
        }
    }

    #[rstest::rstest]
    #[case(vec![], HandlerError::EmptyBlobs)]
    #[case(vec![B256::ZERO], HandlerError::BlobVersionNotSupported)]
    #[case(vec![B256::repeat_byte(1); 7], HandlerError::TooManyBlobs { have: 7, max: 6 })]
    fn blob_fee_simulation_still_validates_blobs(
        #[case] blobs: alloc::vec::Vec<B256>,
        #[case] expected: HandlerError,
    ) {
        let mut version = Version::new(SpecId::CANCUN);
        version.features.remove(EvmFeatures::BLOB_BASE_FEE_CHECK);
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::CANCUN, version),
            SpecId::CANCUN,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            InMemoryDB::default(),
            Precompiles::base(SpecId::CANCUN),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Eip4844(
                TxEip4844 {
                    chain_id: 1,
                    gas_limit: 21_000,
                    blob_versioned_hashes: blobs,
                    ..Default::default()
                }
                .into(),
            ),
            Address::with_last_byte(0xaa),
        );
        assert_eq!(evm.validate_tx(&tx), Err(expected.clone()));
        assert_eq!(evm.transact(&tx).map(|executed| executed.discard()), Err(expected));
    }
}
