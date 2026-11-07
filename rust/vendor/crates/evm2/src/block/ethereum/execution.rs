//! EVM-backed Ethereum execution helpers.

use super::dao_fork;
use crate::{
    AnyError, Evm, EvmTypes, SpecId, TxResult, TxResultWithState, TxStateChanges,
    block::{
        BlockExecutionError, BlockState, BlockValidationError, CommitChanges,
        InternalBlockExecutionError, InvalidTxError, StateUpdate, map_database_error,
        map_handler_error,
    },
    evm::{
        AccountChangeRef, AccountInfo, BEACON_ROOTS_ADDRESS, BUILDER_DEPOSIT_REQUEST_ADDRESS,
        BUILDER_EXIT_REQUEST_ADDRESS, CONSOLIDATION_REQUEST_ADDRESS, HISTORY_STORAGE_ADDRESS,
        StateChangeSink, SystemTx, WITHDRAWAL_REQUEST_ADDRESS,
    },
    interpreter::InstrStop,
    registry::HandlerError,
};
use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use alloy_consensus::{
    BlockHeader, Header, TxReceipt, constants::ETH_TO_WEI, transaction::Recovered,
};
use alloy_eips::{
    eip2718::Typed2718,
    eip4895::Withdrawal,
    eip6110::{DEPOSIT_REQUEST_TYPE, MAINNET_DEPOSIT_CONTRACT_ADDRESS},
    eip7002::WITHDRAWAL_REQUEST_TYPE,
    eip7251::CONSOLIDATION_REQUEST_TYPE,
    eip7685::Requests,
};
use alloy_hardforks::EthereumHardforks;
use alloy_primitives::{Address, B256, Bytes, Log, U256, b256, map::AddressMap};
use core::any::Any;

/// Size of one encoded EIP-6110 deposit request.
pub const DEPOSIT_BYTES_SIZE: usize = 48 + 32 + 8 + 96 + 8;
/// EIP-8282 builder deposit request type.
pub const BUILDER_DEPOSIT_REQUEST_TYPE: u8 = 0x03;
/// EIP-8282 builder exit request type.
pub const BUILDER_EXIT_REQUEST_TYPE: u8 = 0x04;

/// First topic of the deposit contract's `DepositEvent`.
const DEPOSIT_EVENT_SIGNATURE_HASH: B256 =
    b256!("0x649bbc62d0e31342afea4e5cd82d4049e7e1ee912fc0889aa790803be39038c5");

/// ABI layout of a `DepositEvent` payload: the offset of each dynamic field's length word and
/// the field's size, in the order the consensus layer consumes them (public key, withdrawal
/// credentials, amount, signature, index).
const DEPOSIT_EVENT_FIELDS: [(usize, usize); 5] =
    [(160, 48), (256, 32), (320, 8), (384, 96), (512, 8)];

/// Length of an ABI-encoded `DepositEvent` payload.
const DEPOSIT_EVENT_LENGTH: usize = 576;

/// Ethereum transaction validation error returned by evm2 handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EthInvalidTxError(HandlerError);

impl core::fmt::Display for EthInvalidTxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl core::error::Error for EthInvalidTxError {}

impl InvalidTxError for EthInvalidTxError {
    fn is_nonce_too_low(&self) -> bool {
        matches!(self.0, HandlerError::InvalidNonce { expected, got } if got < expected)
    }

    fn is_gas_limit_too_high(&self) -> bool {
        matches!(
            self.0,
            HandlerError::GasLimitMoreThanBlock { .. }
                | HandlerError::TxGasLimitGreaterThanCap { .. }
        )
    }

    fn is_gas_limit_too_low(&self) -> bool {
        matches!(
            self.0,
            HandlerError::IntrinsicGasTooLow { .. } | HandlerError::GasLimitBelowFloor { .. }
        )
    }

    /// Exposes the wrapped [`HandlerError`] so RPC layers can recover the structured error.
    fn as_any(&self) -> &(dyn Any + 'static) {
        &self.0
    }
}

impl TryFrom<HandlerError> for EthInvalidTxError {
    type Error = HandlerError;

    fn try_from(error: HandlerError) -> Result<Self, Self::Error> {
        match error {
            HandlerError::Database(_)
            | HandlerError::Fatal(_)
            | HandlerError::WrongTransactionType { .. } => Err(error),
            HandlerError::External(_)
            | HandlerError::UnsupportedTransactionType(_)
            | HandlerError::InvalidNonce { .. }
            | HandlerError::InvalidChainId { .. }
            | HandlerError::MissingChainId
            | HandlerError::IntrinsicGasTooLow { .. }
            | HandlerError::GasLimitBelowFloor { .. }
            | HandlerError::IntrinsicGasGreaterThanCap { .. }
            | HandlerError::InsufficientFunds { .. }
            | HandlerError::RejectCallerWithCode
            | HandlerError::NonceOverflow
            | HandlerError::GasLimitMoreThanBlock { .. }
            | HandlerError::TxGasLimitGreaterThanCap { .. }
            | HandlerError::CreateInitCodeSizeLimit { .. }
            | HandlerError::FeeCapLessThanBaseFee { .. }
            | HandlerError::EmptyAuthorizationList
            | HandlerError::BlobFeeCapLessThanBlobBaseFee { .. }
            | HandlerError::EmptyBlobs
            | HandlerError::TooManyBlobs { .. }
            | HandlerError::BlobVersionNotSupported
            | HandlerError::PriorityFeeGreaterThanMaxFee => Ok(Self(error)),
        }
    }
}

/// Additional block-level execution context.
#[derive(Debug, Clone, Copy, Default)]
pub struct BlockExecutionContext<'a> {
    /// Pre-block system calls to run before transaction execution.
    pub system_calls: Option<BlockSystemCalls>,
    /// Pre-merge ommer headers included in the block.
    pub ommers: Option<&'a [Header]>,
    /// Post-block withdrawals to apply after transaction execution.
    pub withdrawals: Option<&'a [Withdrawal]>,
    /// Deposit contract address used to derive EIP-6110 deposit requests from receipts.
    pub deposit_contract_address: Option<Address>,
}

/// Inputs required by Ethereum pre-block system calls.
#[derive(Debug, Clone, Copy)]
pub struct BlockSystemCalls {
    /// Parent block hash for EIP-2935 history storage.
    pub parent_hash: B256,
    /// Parent beacon block root for EIP-4788 beacon roots.
    pub parent_beacon_block_root: Option<B256>,
}

/// Converts a transaction's handler error into a block error with its hash.
pub fn map_transaction_error(error: HandlerError, hash: B256) -> BlockExecutionError {
    let error = match EthInvalidTxError::try_from(error) {
        Ok(error) => {
            return BlockValidationError::InvalidTx { hash, error: Box::new(error) }.into();
        }
        Err(error) => error,
    };
    if let HandlerError::Database(database_error) = &error
        && !database_error.is_fatal()
    {
        BlockValidationError::EVM { hash, error: AnyError::new(error) }.into()
    } else {
        InternalBlockExecutionError::EVM { hash, error: AnyError::new(error) }.into()
    }
}

fn send_state_update(state: StateUpdate, on_state_update: &mut impl FnMut(StateUpdate)) {
    if !state.is_empty() {
        on_state_update(state);
    }
}

/// Executes a transaction and commits it when `commit` accepts its result.
pub fn execute_transaction_with_condition<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    transaction: &Recovered<T::Tx>,
    commit: impl FnOnce(&TxResult<T>, TxStateChanges<'_>) -> CommitChanges,
) -> Result<Option<TxResult<T>>, HandlerError>
where
    T::Tx: Typed2718,
{
    let mut update = StateUpdate::default();
    let result = evm.transact(transaction).map(|executed| {
        if commit(executed.result(), executed.changes()).should_commit() {
            for changes in executed.account_changes() {
                block_state.commit_account_changes(changes, stream_state.then_some(&mut update));
            }
            Some(executed.commit())
        } else {
            let _ = executed.discard();
            None
        }
    });
    if stream_state {
        send_state_update(update, on_state_update);
    }
    result
}

/// Executes a transaction and detaches its state changes.
pub fn execute_transaction_without_commit<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    transaction: &Recovered<T::Tx>,
) -> Result<TxResultWithState<T>, HandlerError>
where
    T::Tx: Typed2718,
{
    evm.transact(transaction).map(|executed| executed.detach())
}

/// Commits a detached transaction into the EVM and the block state.
pub fn commit_detached_transaction<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    output: TxResultWithState<T>,
) -> TxResult<T> {
    let TxResultWithState { result, pending_state, .. } = output;
    accumulate_pending_state(block_state, stream_state, on_state_update, &pending_state);
    // Reattach the finalized transaction so evm2 retains its account capacity and recycles
    // storage maps for the next transaction instead of dropping the detached allocations.
    evm.state_mut().set_pending_state(pending_state);
    evm.state_mut().commit_transaction();
    result
}

/// Commits detached state into the EVM overlay and the block state.
pub fn commit_pending_state<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    pending_state: &evm2::evm::PendingState,
) {
    accumulate_pending_state(block_state, stream_state, on_state_update, pending_state);
    evm.overlay_db_mut().commit_pending(pending_state);
}

fn accumulate_pending_state(
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    pending_state: &evm2::evm::PendingState,
) {
    let mut update = StateUpdate::default();
    for changes in pending_state.account_changes() {
        block_state.commit_account_changes(changes, stream_state.then_some(&mut update));
    }
    if stream_state {
        send_state_update(update, on_state_update);
    }
}

/// Runs the EIP-2935 and EIP-4788 pre-block system calls.
pub fn pre_execution_system_call_state_changes<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    spec_id: SpecId,
    block_number: u64,
    context: BlockExecutionContext<'_>,
) -> Result<(), BlockExecutionError> {
    let Some(system_calls) = context.system_calls else {
        return Ok(());
    };

    if spec_id.enables(SpecId::CANCUN) {
        let parent_beacon_block_root = system_calls
            .parent_beacon_block_root
            .ok_or(BlockValidationError::MissingParentBeaconBlockRoot)?;

        if block_number == 0 {
            if !parent_beacon_block_root.is_zero() {
                return Err(BlockValidationError::CancunGenesisParentBeaconBlockRootNotZero {
                    parent_beacon_block_root,
                }
                .into());
            }
        } else {
            let _ = execute_system_call(
                evm,
                block_state,
                stream_state,
                on_state_update,
                BEACON_ROOTS_ADDRESS,
                parent_beacon_block_root.0.into(),
            )?;
        }
    }

    if spec_id.enables(SpecId::PRAGUE) && block_number != 0 {
        let _ = execute_system_call(
            evm,
            block_state,
            stream_state,
            on_state_update,
            HISTORY_STORAGE_ADDRESS,
            system_calls.parent_hash.0.into(),
        )?;
    }

    Ok(())
}

/// Collects the block's EIP-6110 deposit requests from its receipts.
pub fn block_requests_from_receipts<R>(
    spec_id: SpecId,
    context: BlockExecutionContext<'_>,
    receipts: &[R],
) -> Result<Requests, BlockExecutionError>
where
    R: TxReceipt<Log = Log>,
{
    let mut requests = Requests::default();
    if context.system_calls.is_none() || !spec_id.enables(SpecId::PRAGUE) {
        return Ok(requests);
    }

    let deposit_requests = parse_deposit_requests_from_receipts(
        context.deposit_contract_address.unwrap_or(MAINNET_DEPOSIT_CONTRACT_ADDRESS),
        receipts,
    )?;
    requests.push_request_with_type(DEPOSIT_REQUEST_TYPE, deposit_requests);

    Ok(requests)
}

// TODO: Replace the local deposit event and encoding with alloy-eips helpers once
// https://github.com/alloy-rs/alloy/pull/4244 is released.
fn parse_deposit_requests_from_receipts<R>(
    deposit_contract_address: Address,
    receipts: &[R],
) -> Result<Vec<u8>, BlockExecutionError>
where
    R: TxReceipt<Log = Log>,
{
    let mut out = Vec::new();
    for receipt in receipts {
        for log in receipt.logs() {
            if log.address != deposit_contract_address
                || log.topics().first() != Some(&DEPOSIT_EVENT_SIGNATURE_HASH)
            {
                continue;
            }
            extract_deposit_data(&log.data.data, &mut out)
                .map_err(BlockValidationError::DepositRequestDecode)?;
        }
    }

    Ok(out)
}

/// Strips the ABI framing from a `DepositEvent` payload, appending its fields to `out`. A payload
/// that is not the canonical encoding makes the block invalid (EIP-6110).
fn extract_deposit_data(data: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
    if data.len() != DEPOSIT_EVENT_LENGTH {
        return Err("invalid deposit event data length".to_string());
    }
    let word = |offset: usize| U256::from_be_slice(&data[offset..offset + 32]);
    for (index, (offset, _)) in DEPOSIT_EVENT_FIELDS.into_iter().enumerate() {
        if word(index * 32) != U256::from(offset) {
            return Err(alloc::format!("invalid offset for deposit field {index}"));
        }
    }
    out.reserve(DEPOSIT_BYTES_SIZE);
    for (index, (offset, size)) in DEPOSIT_EVENT_FIELDS.into_iter().enumerate() {
        if word(offset) != U256::from(size) {
            return Err(alloc::format!("invalid size for deposit field {index}"));
        }
        out.extend_from_slice(&data[offset + 32..offset + 32 + size]);
    }
    Ok(())
}

/// Runs the EIP-7002, EIP-7251 and EIP-8282 request system calls.
pub fn post_execution_system_call_state_changes<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    spec_id: SpecId,
    context: BlockExecutionContext<'_>,
    requests: &mut Requests,
) -> Result<(), BlockExecutionError> {
    if context.system_calls.is_none() || !spec_id.enables(SpecId::PRAGUE) {
        return Ok(());
    }

    let withdrawal_requests = execute_system_call(
        evm,
        block_state,
        stream_state,
        on_state_update,
        WITHDRAWAL_REQUEST_ADDRESS,
        Bytes::new(),
    )?;
    requests.push_request_with_type(
        WITHDRAWAL_REQUEST_TYPE,
        withdrawal_requests.output.iter().copied(),
    );

    let consolidation_requests = execute_system_call(
        evm,
        block_state,
        stream_state,
        on_state_update,
        CONSOLIDATION_REQUEST_ADDRESS,
        Bytes::new(),
    )?;
    requests.push_request_with_type(
        CONSOLIDATION_REQUEST_TYPE,
        consolidation_requests.output.iter().copied(),
    );

    if spec_id.enables(SpecId::AMSTERDAM) {
        let builder_deposit_requests = execute_system_call(
            evm,
            block_state,
            stream_state,
            on_state_update,
            BUILDER_DEPOSIT_REQUEST_ADDRESS,
            Bytes::new(),
        )?;
        requests.push_request_with_type(
            BUILDER_DEPOSIT_REQUEST_TYPE,
            builder_deposit_requests.output.iter().copied(),
        );

        let builder_exit_requests = execute_system_call(
            evm,
            block_state,
            stream_state,
            on_state_update,
            BUILDER_EXIT_REQUEST_ADDRESS,
            Bytes::new(),
        )?;
        requests.push_request_with_type(
            BUILDER_EXIT_REQUEST_TYPE,
            builder_exit_requests.output.iter().copied(),
        );
    }

    Ok(())
}

fn execute_system_call<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    address: Address,
    data: Bytes,
) -> Result<TxResult<T>, BlockExecutionError> {
    let executed = evm.system_call(SystemTx::new(address, data)).map_err(map_handler_error)?;
    // History and beacon-root calls accept reverts and halts, unlike request-producing calls.
    if !executed.result().is_success()
        && !matches!(address, BEACON_ROOTS_ADDRESS | HISTORY_STORAGE_ADDRESS)
    {
        let reason = executed.result().stop;
        let _ = executed.discard();
        return Err(match address {
            WITHDRAWAL_REQUEST_ADDRESS => BlockValidationError::WithdrawalRequestsContractCall {
                message: alloc::format!("{reason:?}"),
            },
            CONSOLIDATION_REQUEST_ADDRESS => {
                BlockValidationError::ConsolidationRequestsContractCall {
                    message: alloc::format!("{reason:?}"),
                }
            }
            _ => BlockValidationError::other(SystemCallFailed { address, reason }),
        }
        .into());
    }
    let outcome = executed.detach();
    Ok(commit_detached_transaction(evm, block_state, stream_state, on_state_update, outcome))
}

fn commit_state_changes<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    changes: &[(Address, Option<AccountInfo>, Option<AccountInfo>)],
) {
    let mut pending_state = evm2::evm::PendingState::default();
    for (address, original, current) in changes {
        let change = AccountChangeRef {
            address: *address,
            original: original.as_ref(),
            current: current.as_ref(),
            created: false,
            selfdestructed: false,
        };
        let Ok(()) = evm.overlay_db_mut().account(change);
        pending_state.insert_account(*address, original.clone(), current.clone());
    }
    accumulate_pending_state(block_state, stream_state, on_state_update, &pending_state);
}

/// Drains the DAO accounts into the refund beneficiary. Runs before the block's transactions.
///
/// The fork predates EIP-161, so drained accounts stay, empty or absent ones included.
pub fn dao_fork_state_changes<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
) -> Result<(), BlockExecutionError> {
    let mut drained_balance = U256::ZERO;
    let mut changes = Vec::new();
    for address in dao_fork::DAO_HARDFORK_ACCOUNTS {
        let original = evm.read_account_info(&address).map_err(map_database_error)?;
        let mut current = original.clone().unwrap_or_else(AccountInfo::empty);
        drained_balance = drained_balance.saturating_add(current.balance);
        current.balance = U256::ZERO;
        if original.as_ref() != Some(&current) {
            changes.push((address, original, Some(current)));
        }
    }
    commit_state_changes(evm, block_state, stream_state, on_state_update, &changes);

    // The beneficiary may also be drained, so read its balance after the drain.
    let mut balance_increments = AddressMap::default();
    balance_increments.insert(dao_fork::DAO_HARDFORK_BENEFICIARY, drained_balance);
    apply_balance_increments(
        evm,
        block_state,
        stream_state,
        on_state_update,
        false,
        balance_increments,
    )
}

/// Applies block and ommer rewards and withdrawals.
#[expect(clippy::too_many_arguments)]
pub fn post_block_balance_state_changes<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    spec_id: SpecId,
    base_block_reward: Option<u128>,
    block_number: u64,
    block_beneficiary: Address,
    ommers: Option<&[Header]>,
    withdrawals: Option<&[Withdrawal]>,
) -> Result<(), BlockExecutionError> {
    let mut balance_increments = AddressMap::<U256>::default();

    if let Some(base_block_reward) = base_block_reward {
        let ommers = ommers.unwrap_or_default();
        for ommer in ommers {
            *balance_increments.entry(ommer.beneficiary()).or_default() +=
                U256::from(ommer_reward(base_block_reward, block_number, ommer.number()));
        }
        *balance_increments.entry(block_beneficiary).or_default() +=
            U256::from(block_reward(base_block_reward, ommers.len()));
    }

    for withdrawal in withdrawals.into_iter().flatten() {
        *balance_increments.entry(withdrawal.address).or_default() += withdrawal.amount_wei();
    }

    apply_balance_increments(
        evm,
        block_state,
        stream_state,
        on_state_update,
        spec_id.enables(SpecId::SPURIOUS_DRAGON),
        balance_increments,
    )
}

/// Adds each increment to its account's balance. With `eip161`, a zero increment removes an empty
/// account.
fn apply_balance_increments<T: EvmTypes>(
    evm: &mut Evm<'_, T>,
    block_state: &mut BlockState,
    stream_state: bool,
    on_state_update: &mut impl FnMut(StateUpdate),
    eip161: bool,
    balance_increments: AddressMap<U256>,
) -> Result<(), BlockExecutionError> {
    let mut changes = Vec::with_capacity(balance_increments.len());
    for (address, increment) in balance_increments {
        let original = evm.read_account_info(&address).map_err(map_database_error)?;
        let current =
            if eip161 && increment.is_zero() && original.as_ref().is_none_or(AccountInfo::is_empty)
            {
                // The increment only touches the account, which EIP-161 removes when empty.
                None
            } else {
                let mut current = original.clone().unwrap_or_else(AccountInfo::empty);
                current.balance = current.balance.saturating_add(increment);
                Some(current)
            };
        if original == current {
            // Zero-value withdrawals still count as account accesses in the BAL.
            let Ok(()) = evm.overlay_db_mut().account_read(address, original.as_ref());
            continue;
        }
        changes.push((address, original, current));
    }
    commit_state_changes(evm, block_state, stream_state, on_state_update, &changes);
    Ok(())
}

/// Returns the base block reward at `block_number`, or `None` after the merge.
pub fn base_block_reward<C>(chain_spec: &C, block_number: u64) -> Option<u128>
where
    C: EthereumHardforks + ?Sized,
{
    if chain_spec.is_paris_active_at_block(block_number) {
        None
    } else if chain_spec.is_constantinople_active_at_block(block_number) {
        Some(ETH_TO_WEI * 2)
    } else if chain_spec.is_byzantium_active_at_block(block_number) {
        Some(ETH_TO_WEI * 3)
    } else {
        Some(ETH_TO_WEI * 5)
    }
}

/// Returns the block reward with the ommer inclusion bonus.
pub const fn block_reward(base_block_reward: u128, ommers: usize) -> u128 {
    base_block_reward + (base_block_reward >> 5) * ommers as u128
}

/// Returns the reward of an ommer included at `block_number`.
pub fn ommer_reward(base_block_reward: u128, block_number: u64, ommer_block_number: u64) -> u128 {
    let distance = 8u64.saturating_add(ommer_block_number).saturating_sub(block_number);
    (u128::from(distance) * base_block_reward) >> 3
}

/// A system contract reverted or halted without producing a successful result.
#[derive(Debug)]
struct SystemCallFailed {
    address: Address,
    reason: InstrStop,
}

impl core::fmt::Display for SystemCallFailed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "EVM system call to {} failed: {:?}", self.address, self.reason)
    }
}

impl core::error::Error for SystemCallFailed {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn deposit_log(data: Vec<u8>) -> Log {
        Log::new_unchecked(
            MAINNET_DEPOSIT_CONTRACT_ADDRESS,
            vec![DEPOSIT_EVENT_SIGNATURE_HASH],
            data.into(),
        )
    }

    /// Encodes a canonical `DepositEvent` payload whose fields are filled with `fill`.
    fn deposit_data(fill: u8) -> Vec<u8> {
        let mut data = vec![0; DEPOSIT_EVENT_LENGTH];
        for (index, (offset, size)) in DEPOSIT_EVENT_FIELDS.into_iter().enumerate() {
            data[index * 32..index * 32 + 32]
                .copy_from_slice(&U256::from(offset).to_be_bytes::<32>());
            data[offset..offset + 32].copy_from_slice(&U256::from(size).to_be_bytes::<32>());
            data[offset + 32..offset + 32 + size].fill(fill);
        }
        data
    }

    fn requests(log: Log) -> Result<Vec<u8>, BlockExecutionError> {
        let receipt = alloy_consensus::Receipt {
            status: true.into(),
            cumulative_gas_used: 0,
            logs: vec![log],
        };
        parse_deposit_requests_from_receipts(MAINNET_DEPOSIT_CONTRACT_ADDRESS, &[receipt])
    }

    #[test]
    fn parses_canonical_deposit() {
        assert_eq!(requests(deposit_log(deposit_data(7))).unwrap(), vec![7; DEPOSIT_BYTES_SIZE]);
    }

    #[test]
    fn rejects_non_canonical_deposits() {
        let mut short = deposit_data(7);
        short.pop();
        let mut bad_offset = deposit_data(7);
        bad_offset[31] = 0;
        let mut bad_size = deposit_data(7);
        bad_size[160 + 31] = 47;
        for data in [short, bad_offset, bad_size] {
            assert!(matches!(
                requests(deposit_log(data)),
                Err(BlockExecutionError::Validation(BlockValidationError::DepositRequestDecode(_)))
            ));
        }
    }

    #[test]
    fn classifies_gas_errors() {
        let floor = EthInvalidTxError(HandlerError::GasLimitBelowFloor { required: 2, got: 1 });
        assert!(floor.is_gas_limit_too_low());
        assert!(!floor.is_gas_limit_too_high());
        let cap =
            EthInvalidTxError(HandlerError::IntrinsicGasGreaterThanCap { required: 2, cap: 1 });
        assert!(!cap.is_gas_limit_too_low());
        assert!(!cap.is_gas_limit_too_high());
    }
}
