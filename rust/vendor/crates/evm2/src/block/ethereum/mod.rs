//! Ethereum block execution.

use crate::{
    BaseEvmTypes, Evm, EvmFeatures, EvmTypes, SpecId, TxResult, TxResultWithState, TxStateChanges,
    block::{
        BlockExecutionError, BlockExecutionOutput, BlockExecutionResult, BlockExecutor, BlockState,
        BlockTransactionResult, BlockValidationError, BundleState, CommitChanges, ExecutableTx,
        GasOutput, ReceiptBuilder, ReceiptBuilderCtx, StateUpdate,
    },
    evm::{Bal, PendingState},
    interpreter::Host,
};
use alloc::{borrow::Cow, boxed::Box, sync::Arc, vec::Vec};
use alloy_consensus::{
    Eip658Value, EthereumTxEnvelope, Header, Receipt, ReceiptEnvelope, Transaction, TxEip4844,
    TxType,
    transaction::{TransactionEnvelope, TxHashRef},
};
use alloy_eip7928::BlockAccessIndex;
use alloy_eips::{eip2718::Typed2718, eip4895::Withdrawal, eip7685::Requests};
use alloy_hardforks::{EthereumHardfork, EthereumHardforks};
use alloy_primitives::{Address, B256};
use core::borrow::Borrow;

pub mod dao_fork;

pub mod execution;
use execution::{
    BlockExecutionContext, BlockSystemCalls, block_requests_from_receipts,
    commit_detached_transaction, commit_pending_state, dao_fork_state_changes,
    execute_transaction_without_commit, map_transaction_error, post_block_balance_state_changes,
    post_execution_system_call_state_changes, pre_execution_system_call_state_changes,
};

#[cfg(test)]
mod tests;

/// Block inputs of [`EthBlockExecutor`] that are not in the EVM's block environment.
#[derive(Clone, Debug, Default)]
pub struct EthBlockExecutionCtx<'a> {
    /// Optional transaction count hint.
    pub tx_count_hint: Option<usize>,
    /// Parent block hash.
    pub parent_hash: B256,
    /// Parent beacon block root.
    pub parent_beacon_block_root: Option<B256>,
    /// Ommer headers.
    pub ommers: Cow<'a, [Header]>,
    /// Withdrawals.
    pub withdrawals: Option<Cow<'a, [Withdrawal]>>,
}

/// Chain rules of [`EthBlockExecutor`] that depend on the block number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EthBlockRules {
    /// Block reward before the merge.
    pub base_block_reward: Option<u128>,
    /// Whether the block is the DAO fork block.
    pub dao_fork_transition: bool,
    /// EIP-6110 deposit contract. `None` means Ethereum mainnet's, which is wrong for other chains
    /// with deposits, such as Sepolia or Hoodi.
    pub deposit_contract_address: Option<Address>,
    /// Blob gas the block's transactions may use in total (EIP-4844). `None` leaves it unchecked.
    pub max_blob_gas_per_block: Option<u64>,
}

impl EthBlockRules {
    /// Reads the rules of block `block_number` from a chain's hardforks.
    pub fn new<C: EthereumHardforks + ?Sized>(
        chain: &C,
        block_number: u64,
        deposit_contract_address: Option<Address>,
    ) -> Self {
        Self {
            base_block_reward: execution::base_block_reward(chain, block_number),
            dao_fork_transition: chain
                .ethereum_fork_activation(EthereumHardfork::Dao)
                .transitions_at_block(block_number),
            deposit_contract_address,
            max_blob_gas_per_block: None,
        }
    }
}

/// Builds [`ReceiptEnvelope`]s for Ethereum transactions without blob sidecars.
///
/// Receipts carry an EIP-658 status, so their root matches headers only from Byzantium on; earlier
/// receipts carry the intermediate state root instead.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct EthReceiptBuilder;

impl ReceiptBuilder for EthReceiptBuilder {
    type Transaction = EthereumTxEnvelope<TxEip4844>;
    type Receipt = ReceiptEnvelope;

    fn build_receipt<T: EvmTypes>(
        &self,
        ctx: ReceiptBuilderCtx<TxType, TxResult<T>>,
    ) -> ReceiptEnvelope {
        let ReceiptBuilderCtx { tx_type, result, cumulative_gas_used } = ctx;
        let receipt = Receipt {
            status: Eip658Value::Eip658(result.is_success()),
            cumulative_gas_used,
            logs: result.logs,
        };
        ReceiptEnvelope::from_typed(tx_type, receipt.with_bloom())
    }
}

/// Detached Ethereum transaction result with the metadata needed for canonical receipt commit.
#[derive(Debug)]
pub struct EthTransactionResultWithState<T: EvmTypes = BaseEvmTypes, Ty = TxType> {
    result: TxResultWithState<T>,
    tx_type: Ty,
    blob_gas_used: u64,
}

impl<T: EvmTypes, Ty> EthTransactionResultWithState<T, Ty> {
    /// Creates a detached transaction result with its receipt metadata.
    pub const fn new(result: TxResultWithState<T>, tx_type: Ty, blob_gas_used: u64) -> Self {
        Self { result, tx_type, blob_gas_used }
    }

    /// Returns the detached EVM transaction result.
    pub const fn result(&self) -> &TxResultWithState<T> {
        &self.result
    }

    /// Splits the result into its parts.
    pub fn into_parts(self) -> (TxResultWithState<T>, Ty, u64) {
        (self.result, self.tx_type, self.blob_gas_used)
    }
}

impl<T: EvmTypes, Ty> BlockTransactionResult<T> for EthTransactionResultWithState<T, Ty> {
    fn result(&self) -> &TxResultWithState<T> {
        &self.result
    }

    fn into_result(self) -> TxResultWithState<T> {
        self.result
    }
}

type StateUpdateHook = Option<Box<dyn FnMut(StateUpdate) + Send>>;

/// Post-execution output of one block, before its receipts and state are taken.
#[derive(Debug)]
pub struct EthPostExecutionOutput {
    /// EIP-7685 requests of the block.
    pub requests: Requests,
    /// Gas used by the block.
    pub gas_used: u64,
    /// Blob gas used by the block.
    pub blob_gas_used: u64,
}

/// Configured Ethereum block executor backed by evm2.
#[expect(missing_debug_implementations)]
pub struct EthBlockExecutor<'a, T = BaseEvmTypes, R = EthReceiptBuilder>
where
    T: EvmTypes,
    R: ReceiptBuilder,
{
    /// EVM containing the execution overlay and optional block access list builder.
    pub evm: Evm<'a, T>,
    ctx: EthBlockExecutionCtx<'a>,
    rules: EthBlockRules,
    receipt_builder: R,
    spec_id: SpecId,
    /// Accumulated state changes, including state committed through the executor's hooks.
    pub block_state: BlockState,
    state_update_hook: StateUpdateHook,
    /// Transaction receipts in execution order.
    pub receipts: Vec<R::Receipt>,
    /// Total receipt gas used.
    pub cumulative_gas_used: u64,
    /// Regular gas used for Amsterdam block-capacity accounting.
    pub block_regular_gas_used: u64,
    /// State gas used for Amsterdam block-capacity accounting.
    pub block_state_gas_used: u64,
    separate_block_gas: bool,
    /// Total blob gas used.
    pub blob_gas_used: u64,
    bal_index_offset: u64,
}

impl<'a, T, R> EthBlockExecutor<'a, T, R>
where
    T: EvmTypes<Tx: Typed2718>,
    R: ReceiptBuilder<Transaction: Transaction + TxHashRef>,
{
    /// Creates an executor for the block in `evm`'s block environment.
    pub fn new(
        evm: Evm<'a, T>,
        ctx: EthBlockExecutionCtx<'a>,
        rules: EthBlockRules,
        receipt_builder: R,
    ) -> Self {
        // The executor stores evm2's base spec ID, while custom type families may expose a
        // richer host spec ID that converts into it.
        #[allow(clippy::useless_conversion)]
        let spec_id = evm.spec_id().into();
        let separate_block_gas = evm.version().feature(EvmFeatures::EIP8037);
        let receipts = Vec::with_capacity(ctx.tx_count_hint.unwrap_or_default());
        Self {
            evm,
            ctx,
            rules,
            receipt_builder,
            spec_id,
            block_state: BlockState::new(),
            state_update_hook: None,
            receipts,
            cumulative_gas_used: 0,
            block_regular_gas_used: 0,
            block_state_gas_used: 0,
            separate_block_gas,
            blob_gas_used: 0,
            bal_index_offset: 0,
        }
    }

    const fn block_context<'ctx>(
        rules: &EthBlockRules,
        parent_hash: B256,
        parent_beacon_block_root: Option<B256>,
        ommers: &'ctx [Header],
        withdrawals: Option<&'ctx [Withdrawal]>,
    ) -> BlockExecutionContext<'ctx> {
        BlockExecutionContext {
            system_calls: Some(BlockSystemCalls { parent_hash, parent_beacon_block_root }),
            ommers: Some(ommers),
            withdrawals,
            deposit_contract_address: rules.deposit_contract_address,
        }
    }

    const fn block_access_list_builder_enabled(&self) -> bool {
        self.evm.state().bal_builder().is_some()
    }

    /// Returns the block execution context.
    pub const fn context(&self) -> &EthBlockExecutionCtx<'a> {
        &self.ctx
    }

    /// Returns the chain rules of the block.
    pub const fn rules(&self) -> &EthBlockRules {
        &self.rules
    }

    /// Returns the accumulated block state.
    pub const fn block_state(&self) -> &BlockState {
        &self.block_state
    }

    /// Commits detached state without recording a transaction receipt or gas usage.
    pub fn commit_pending_state(&mut self, state: &PendingState) {
        let stream_state = self.state_update_hook.is_some();
        commit_pending_state(
            &mut self.evm,
            &mut self.block_state,
            stream_state,
            &mut |state| emit_state(&mut self.state_update_hook, state),
            state,
        );
    }

    /// Sets the Ethereum fork used for block system calls and deposit request processing.
    pub const fn set_spec_id(&mut self, spec_id: SpecId) {
        self.spec_id = spec_id;
    }

    /// Starts a new block on the same EVM and block state, as when executing several blocks as
    /// one: reads the spec and gas rules from the EVM's current configuration, installs `ctx`
    /// and `rules`, offsets the block access indices by `bal_index_offset`, and resets the gas
    /// counters. Receipts and block state carry over.
    ///
    /// Finish each block with [`Self::apply_post_execution_changes`] from its first receipt and the
    /// last one with [`BlockExecutor::into_state`]: [`BlockExecutor::finish`] applies
    /// post-execution changes again, over every receipt.
    pub fn start_block(
        &mut self,
        ctx: EthBlockExecutionCtx<'a>,
        rules: EthBlockRules,
        bal_index_offset: u64,
    ) {
        #[allow(clippy::useless_conversion)]
        let spec_id = self.evm.spec_id().into();
        self.spec_id = spec_id;
        self.separate_block_gas = self.evm.version().feature(EvmFeatures::EIP8037);
        self.ctx = ctx;
        self.rules = rules;
        self.bal_index_offset = bal_index_offset;
        self.reset_gas_used();
    }

    /// Resets the block's gas counters.
    pub const fn reset_gas_used(&mut self) {
        self.cumulative_gas_used = 0;
        self.block_regular_gas_used = 0;
        self.block_state_gas_used = 0;
        self.blob_gas_used = 0;
    }

    /// Applies the block's post-execution changes without consuming the executor: requests from
    /// the receipts from `receipts_start` on, rewards and withdrawals, then the request system
    /// calls (execution-specs order).
    pub fn apply_post_execution_changes(
        &mut self,
        receipts_start: usize,
    ) -> Result<EthPostExecutionOutput, BlockExecutionError> {
        self.set_transaction_block_access_index();
        let context = Self::block_context(
            &self.rules,
            self.ctx.parent_hash,
            self.ctx.parent_beacon_block_root,
            &self.ctx.ommers,
            None,
        );
        let mut requests =
            block_requests_from_receipts(self.spec_id, context, &self.receipts[receipts_start..])?;
        let stream_state = self.state_update_hook.is_some();
        let withdrawals =
            self.ctx.withdrawals.as_deref().filter(|_| self.spec_id.enables(SpecId::SHANGHAI));
        let context = Self::block_context(
            &self.rules,
            self.ctx.parent_hash,
            self.ctx.parent_beacon_block_root,
            &self.ctx.ommers,
            withdrawals,
        );
        let block_number = self.evm.block_env().number.to::<u64>();
        let block_beneficiary = self.evm.block_env().beneficiary;
        post_block_balance_state_changes(
            &mut self.evm,
            &mut self.block_state,
            stream_state,
            &mut |state| emit_state(&mut self.state_update_hook, state),
            self.spec_id,
            self.rules.base_block_reward,
            block_number,
            block_beneficiary,
            context.ommers,
            context.withdrawals,
        )?;

        post_execution_system_call_state_changes(
            &mut self.evm,
            &mut self.block_state,
            stream_state,
            &mut |state| emit_state(&mut self.state_update_hook, state),
            self.spec_id,
            context,
            &mut requests,
        )?;

        Ok(EthPostExecutionOutput {
            requests,
            gas_used: final_block_gas_used(
                self.separate_block_gas,
                self.cumulative_gas_used,
                self.block_regular_gas_used,
                self.block_state_gas_used,
            ),
            blob_gas_used: self.blob_gas_used,
        })
    }

    /// Validates that a transaction's blobs fit the block's remaining blob gas.
    fn validate_transaction_blob_gas(
        &self,
        transaction_blob_gas: u64,
    ) -> Result<(), BlockExecutionError> {
        let Some(max_blob_gas) = self.rules.max_blob_gas_per_block else { return Ok(()) };
        let block_available_blob_gas = max_blob_gas.saturating_sub(self.blob_gas_used);
        if transaction_blob_gas > block_available_blob_gas {
            return Err(BlockValidationError::TransactionBlobGasMoreThanAvailableBlockBlobGas {
                transaction_blob_gas,
                block_available_blob_gas,
            }
            .into());
        }
        Ok(())
    }

    #[inline]
    fn set_transaction_block_access_index(&mut self) {
        if self.block_access_list_builder_enabled() {
            let index = self.receipts.len() as u64 + 1 + self.bal_index_offset;
            self.set_block_access_index(BlockAccessIndex::new(index));
        }
    }

    fn record_receipt(
        &mut self,
        tx_type: <R::Transaction as TransactionEnvelope>::TxType,
        blob_gas_used: u64,
        outcome: TxResult<T>,
    ) -> GasOutput {
        let tx_gas_used = outcome.tx_gas_used();
        let regular_gas_used = outcome.execution_gas_spent();
        let state_gas_used = outcome.state_gas_spent();
        self.block_regular_gas_used = self.block_regular_gas_used.saturating_add(regular_gas_used);
        self.block_state_gas_used = self.block_state_gas_used.saturating_add(state_gas_used);
        self.cumulative_gas_used += tx_gas_used;
        self.blob_gas_used += blob_gas_used;
        self.receipts.push(self.receipt_builder.build_receipt::<T>(ReceiptBuilderCtx {
            tx_type,
            result: outcome,
            cumulative_gas_used: self.cumulative_gas_used,
        }));
        GasOutput::new_with_regular(tx_gas_used, regular_gas_used, state_gas_used)
    }
}

impl<'a, T, R> BlockExecutor for EthBlockExecutor<'a, T, R>
where
    T: EvmTypes<Tx: Typed2718>,
    R: ReceiptBuilder<Transaction: Transaction + TxHashRef>,
{
    type Transaction = R::Transaction;
    type Receipt = R::Receipt;
    type EvmTypes = T;
    type Evm = Evm<'a, T>;
    type TransactionResultWithState =
        EthTransactionResultWithState<T, <R::Transaction as TransactionEnvelope>::TxType>;

    fn evm(&self) -> &Self::Evm {
        &self.evm
    }

    fn evm_mut(&mut self) -> &mut Self::Evm {
        &mut self.evm
    }

    fn set_state_hook(&mut self, hook: impl FnMut(StateUpdate) + Send + 'static) -> bool {
        self.state_update_hook = Some(Box::new(hook));
        true
    }

    fn set_block_access_list(&mut self, block_access_list: Arc<Bal>) {
        self.evm.state_mut().set_bal(block_access_list);
    }

    fn set_block_access_index(&mut self, index: BlockAccessIndex) {
        self.evm.state_mut().set_bal_index(index);
    }

    fn enable_block_access_list_builder(&mut self) {
        self.evm.state_mut().enable_bal_builder();
        self.evm.state_mut().reset_bal_index();
    }

    fn take_block_access_list(&mut self) -> Option<Bal> {
        self.evm.state_mut().take_bal_builder()
    }

    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError> {
        let context = Self::block_context(
            &self.rules,
            self.ctx.parent_hash,
            self.ctx.parent_beacon_block_root,
            &self.ctx.ommers,
            None,
        );
        let block_number = self.evm.block_env().number.to::<u64>();
        let stream_state = self.state_update_hook.is_some();
        if self.rules.dao_fork_transition {
            core::hint::cold_path();
            dao_fork_state_changes(
                &mut self.evm,
                &mut self.block_state,
                stream_state,
                &mut |state| emit_state(&mut self.state_update_hook, state),
            )?;
        }
        pre_execution_system_call_state_changes(
            &mut self.evm,
            &mut self.block_state,
            stream_state,
            &mut |state| emit_state(&mut self.state_update_hook, state),
            self.spec_id,
            block_number,
            context,
        )
    }

    fn into_state(self) -> BundleState {
        self.block_state.into_bundle()
    }

    fn validate_transaction_gas_limit(
        &mut self,
        transaction_gas_limit: u64,
    ) -> Result<(), BlockExecutionError> {
        let block_gas_limit = self.evm.block_env().gas_limit.to::<u64>();
        let regular_limit = transaction_gas_limit.min(self.evm.version().tx_gas_limit_cap);
        let unavailable = if self.separate_block_gas {
            let regular_available = block_gas_limit.saturating_sub(self.block_regular_gas_used);
            let state_available = block_gas_limit.saturating_sub(self.block_state_gas_used);
            if regular_limit > regular_available {
                Some((transaction_gas_limit, regular_available))
            } else if transaction_gas_limit > state_available {
                Some((transaction_gas_limit, state_available))
            } else {
                None
            }
        } else {
            let available = block_gas_limit.saturating_sub(self.cumulative_gas_used);
            (regular_limit > available).then_some((transaction_gas_limit, available))
        };
        if let Some((transaction_gas_limit, block_available_gas)) = unavailable {
            return Err(BlockValidationError::TransactionGasLimitMoreThanAvailableBlockGas {
                transaction_gas_limit,
                block_available_gas,
            }
            .into());
        }
        Ok(())
    }

    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        commit: impl FnOnce(&TxResult<T>, TxStateChanges<'_>) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let (transaction, tx) = tx.into_parts();
        let tx = tx.borrow();
        self.set_transaction_block_access_index();
        self.validate_transaction_gas_limit(tx.gas_limit())?;
        let blob_gas_used = tx.blob_gas_used().unwrap_or_default();
        self.validate_transaction_blob_gas(blob_gas_used)?;
        let tx_type = tx.tx_type();
        let Some(outcome) = execution::execute_transaction_with_condition(
            &mut self.evm,
            &mut self.block_state,
            self.state_update_hook.is_some(),
            &mut |state| emit_state(&mut self.state_update_hook, state),
            &transaction,
            commit,
        )
        .map_err(|error| map_transaction_error(error, *tx.tx_hash()))?
        else {
            return Ok(None);
        };
        Ok(Some(self.record_receipt(tx_type, blob_gas_used, outcome)))
    }

    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::TransactionResultWithState, BlockExecutionError> {
        let (transaction, tx) = tx.into_parts();
        let tx = tx.borrow();
        self.set_transaction_block_access_index();
        self.validate_transaction_gas_limit(tx.gas_limit())?;
        let blob_gas_used = tx.blob_gas_used().unwrap_or_default();
        self.validate_transaction_blob_gas(blob_gas_used)?;
        let tx_type = tx.tx_type();
        let result = execute_transaction_without_commit(&mut self.evm, &transaction)
            .map_err(|err| map_transaction_error(err, *tx.tx_hash()))?;
        Ok(EthTransactionResultWithState::new(result, tx_type, blob_gas_used))
    }

    fn commit_transaction(
        &mut self,
        output: Self::TransactionResultWithState,
    ) -> Result<GasOutput, BlockExecutionError> {
        let EthTransactionResultWithState { result, tx_type, blob_gas_used } = output;
        self.set_transaction_block_access_index();
        let stream_state = self.state_update_hook.is_some();
        let outcome = commit_detached_transaction(
            &mut self.evm,
            &mut self.block_state,
            stream_state,
            &mut |state| emit_state(&mut self.state_update_hook, state),
            result,
        );
        Ok(self.record_receipt(tx_type, blob_gas_used, outcome))
    }

    fn receipts(&self) -> &[Self::Receipt] {
        &self.receipts
    }

    fn finish_with_block_access_list(
        mut self,
    ) -> Result<(BlockExecutionOutput<Self::Receipt>, Option<Bal>), BlockExecutionError> {
        let EthPostExecutionOutput { requests, gas_used, blob_gas_used } =
            self.apply_post_execution_changes(0)?;
        let block_access_list = self.take_block_access_list();
        let output = BlockExecutionOutput::new(
            BlockExecutionResult { receipts: self.receipts, requests, gas_used, blob_gas_used },
            self.block_state.into_bundle(),
        );
        Ok((output, block_access_list))
    }
}

/// Returns the block header's gas used: the larger of regular and state gas under EIP-8037,
/// receipt gas otherwise.
pub const fn final_block_gas_used(
    separate_block_gas: bool,
    cumulative_gas_used: u64,
    block_regular_gas_used: u64,
    block_state_gas_used: u64,
) -> u64 {
    if separate_block_gas {
        if block_regular_gas_used > block_state_gas_used {
            block_regular_gas_used
        } else {
            block_state_gas_used
        }
    } else {
        cumulative_gas_used
    }
}

fn emit_state(hook: &mut StateUpdateHook, state: StateUpdate) {
    if let Some(hook) = hook.as_mut() {
        hook(state);
    }
}
