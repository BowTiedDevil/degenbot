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
    EvmTypes, TxResult,
    env::TxEnvExt,
    evm::handler::{DefaultTxHandlerHooks, GasSettlement, TxHandlerHooks},
    interpreter::GasTracker,
    registry::{HandlerResult, TxRequest},
};
use alloy_consensus::TxEip1559;
use alloy_primitives::U256;

/// Executes an EIP-1559 transaction using Ethereum rules.
pub fn handle<T: EvmTypes>(req: TxRequest<'_, '_, T, TxEip1559>) -> HandlerResult<TxResult<T>> {
    handle_with_hooks::<T, DefaultTxHandlerHooks>(req)
}

/// Executes an EIP-1559 transaction using Ethereum rules and custom handler hooks.
pub fn handle_with_hooks<T: EvmTypes, H: TxHandlerHooks<T>>(
    mut req: TxRequest<'_, '_, T, TxEip1559>,
) -> HandlerResult<TxResult<T>> {
    let prepared = prepare_with_hooks::<T, H>(&mut req)?;
    execute_prepared::<T, H>(req, prepared)
}

/// Validates an EIP-1559 transaction and applies its pre-execution state changes.
pub fn prepare_with_hooks<T: EvmTypes, H: TxHandlerHooks<T>>(
    req: &mut TxRequest<'_, '_, T, TxEip1559>,
) -> HandlerResult<PreparedTx> {
    let caller = req.tx.signer();
    let tx = req.tx.inner();
    let max_fee_per_gas = U256::from(tx.max_fee_per_gas);
    let max_priority_fee_per_gas = U256::from(tx.max_priority_fee_per_gas);
    let gas_price =
        effective_gas_price(max_fee_per_gas, max_priority_fee_per_gas, req.host.block.basefee);

    validate_priority_fee(req.host.version(), max_fee_per_gas, max_priority_fee_per_gas)?;
    validate_gas_price(req.host.version(), gas_price, req.host.block.basefee)?;
    validate_chain_id(req.host.version(), Some(tx.chain_id), false)?;
    validate_tx_gas_limit_cap(req.host.version(), tx.gas_limit)?;
    validate_block_gas_limit(req.host.version(), tx.gas_limit, req.host.block.gas_limit)?;
    validate_create_initcode(req.host.version(), tx.to, &tx.input)?;
    validate_nonce_not_overflow(tx.nonce)?;
    let (access_list_accounts, access_list_storage_keys) = access_list_counts(&tx.access_list);
    let (mut intrinsic, mut floor_gas) = intrinsic_and_floor_gas(
        req.host.version(),
        caller,
        tx.to,
        &tx.input,
        access_list_accounts,
        access_list_storage_keys,
        tx.value,
    );
    // EIP-2780: state-dependent gas is charged at the runtime gas phase, not the intrinsic phase.
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

    let max_gas_cost = U256::from(tx.gas_limit) * max_fee_per_gas;
    let sender_nonce = validate_sender_and_bump_nonce(
        req.host,
        caller,
        tx.nonce,
        max_gas_cost.saturating_add(tx.value),
    )?;

    warm_base_accounts(req.host, tx.to);
    warm_access_list(req.host, &tx.access_list);

    let effective_gas_cost = U256::from(tx.gas_limit) * gas_price;
    H::before_execution(req.host, req.envelope, caller, effective_gas_cost)?;

    Ok(PreparedTx { caller, gas_price, intrinsic, initial_state_gas, floor_gas, sender_nonce })
}

/// Executes and settles a prepared EIP-1559 transaction.
pub fn execute_prepared<T: EvmTypes, H: TxHandlerHooks<T>>(
    req: TxRequest<'_, '_, T, TxEip1559>,
    prepared: PreparedTx,
) -> HandlerResult<TxResult<T>> {
    let PreparedTx { caller, gas_price, intrinsic, initial_state_gas, floor_gas, sender_nonce } =
        prepared;
    let tx = req.tx.inner();

    let (execution_gas_limit, reservoir) =
        initial_gas_and_reservoir(req.host.version(), tx.gas_limit, intrinsic, initial_state_gas);
    *req.host.tx_env_mut() = TxEnvExt {
        origin: caller,
        gas_price,
        chain_id: U256::from(req.host.version().chain_id),
        ..TxEnvExt::default()
    };
    let mut tx_gas =
        GasTracker::new_with_execution_gas_and_reservoir(execution_gas_limit, reservoir);
    let frame = prepare_initial_frame(
        req.host,
        caller,
        sender_nonce,
        tx.to,
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
