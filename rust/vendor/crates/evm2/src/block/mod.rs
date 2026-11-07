//! Block execution.
//!
//! [`BlockExecutor`] executes the transactions of one block on an [`Evm`](crate::Evm), applies
//! the block's pre- and post-execution changes, and collects receipts and the block's
//! [`BundleState`]. [`ethereum::EthBlockExecutor`] implements it with Ethereum's block rules.

use crate::{EvmTypes, EvmTypesHost, TxResult, TxResultWithState, TxStateChanges, evm::Bal};
use alloc::{sync::Arc, vec::Vec};
use alloy_consensus::transaction::{Recovered, TransactionEnvelope};
use alloy_eip7928::BlockAccessIndex;
use alloy_eips::eip7685::Requests;
use alloy_primitives::Log;
use core::borrow::Borrow;

pub mod bundle_state;
pub use bundle_state::BundleState;

mod error;
pub use error::{
    BlockExecutionError, BlockValidationError, InternalBlockExecutionError, InvalidTxError,
    map_database_error, map_handler_error,
};

pub mod ethereum;

mod state;
pub use state::{AccountUpdate, BlockState, BundleSource, StateUpdate, TransactionChanges};

/// The transaction environment executed by a [`BlockExecutor`].
pub type EvmTxOf<E> = <<E as BlockExecutor>::EvmTypes as EvmTypesHost>::Tx;

/// Gas used by a successfully executed transaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GasOutput {
    /// Gas used for receipt accounting.
    tx_gas_used: u64,
    /// Regular gas used for Amsterdam block-capacity accounting.
    regular_gas_used: u64,
    /// Gas used for state-capacity accounting.
    state_gas_used: u64,
}

impl GasOutput {
    /// Creates a new gas output.
    pub const fn new(tx_gas_used: u64, state_gas_used: u64) -> Self {
        Self { tx_gas_used, regular_gas_used: tx_gas_used, state_gas_used }
    }

    /// Creates a gas output with independent receipt, regular, and state gas accounting.
    pub const fn new_with_regular(
        tx_gas_used: u64,
        regular_gas_used: u64,
        state_gas_used: u64,
    ) -> Self {
        Self { tx_gas_used, regular_gas_used, state_gas_used }
    }

    /// Returns transaction gas used for receipt accounting.
    pub const fn tx_gas_used(&self) -> u64 {
        self.tx_gas_used
    }

    /// Returns regular gas used for Amsterdam block-capacity accounting.
    pub const fn regular_gas_used(&self) -> u64 {
        self.regular_gas_used
    }

    /// Returns state gas used for state-capacity accounting.
    pub const fn state_gas_used(&self) -> u64 {
        self.state_gas_used
    }
}

impl From<u64> for GasOutput {
    fn from(gas_used: u64) -> Self {
        Self::new(gas_used, gas_used)
    }
}

/// Whether a block executor commits an executed transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitChanges {
    /// Transaction should be committed into block executor's state.
    Yes,
    /// Transaction should not be committed.
    No,
}

impl CommitChanges {
    /// Returns `true` if transaction should be committed into block executor's state.
    pub const fn should_commit(self) -> bool {
        matches!(self, Self::Yes)
    }
}

/// Context for building a transaction receipt from an execution result.
#[derive(Clone, Debug)]
pub struct ReceiptBuilderCtx<TxType, TransactionResult> {
    /// Transaction type used by the receipt.
    pub tx_type: TxType,
    /// Raw transaction execution result.
    pub result: TransactionResult,
    /// Cumulative gas used by this transaction and all prior transactions in the block.
    pub cumulative_gas_used: u64,
}

/// Builds chain-specific receipts from raw transaction execution results.
#[auto_impl::auto_impl(&, Arc)]
pub trait ReceiptBuilder {
    /// Consensus transaction type accepted by the executor.
    type Transaction: TransactionEnvelope;
    /// Receipt produced by this builder.
    type Receipt: alloy_consensus::TxReceipt<Log = Log>;

    /// Builds a receipt for the transaction execution result.
    fn build_receipt<T: EvmTypes>(
        &self,
        ctx: ReceiptBuilderCtx<<Self::Transaction as TransactionEnvelope>::TxType, TxResult<T>>,
    ) -> Self::Receipt;
}

/// A transaction a [`BlockExecutor`] can execute: the EVM transaction environment with its
/// recovered signer, and the consensus transaction it was built from.
pub trait ExecutableTx<E: BlockExecutor> {
    /// Consensus transaction accessor.
    type Tx: Borrow<E::Transaction>;

    /// Splits the transaction into its EVM environment and consensus transaction.
    fn into_parts(self) -> (Recovered<EvmTxOf<E>>, Self::Tx);
}

impl<E, Tx> ExecutableTx<E> for (Recovered<EvmTxOf<E>>, Tx)
where
    E: BlockExecutor,
    Tx: Borrow<E::Transaction>,
{
    type Tx = Tx;

    fn into_parts(self) -> (Recovered<EvmTxOf<E>>, Tx) {
        self
    }
}

impl<'a, E> ExecutableTx<E> for &'a Recovered<E::Transaction>
where
    E: BlockExecutor<Transaction: Clone>,
    EvmTxOf<E>: From<E::Transaction>,
{
    type Tx = &'a E::Transaction;

    fn into_parts(self) -> (Recovered<EvmTxOf<E>>, &'a E::Transaction) {
        let tx = Recovered::new_unchecked(self.clone_inner().into(), self.signer());
        (tx, self.inner())
    }
}

/// A detached block transaction output containing its raw execution result.
pub trait BlockTransactionResult<T: EvmTypes> {
    /// Returns the raw transaction execution result.
    fn result(&self) -> &TxResultWithState<T>;

    /// Consumes the output and returns the raw transaction execution result.
    fn into_result(self) -> TxResultWithState<T>;
}

/// The result of executing a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockExecutionResult<T> {
    /// All the receipts of the transactions in the block.
    pub receipts: Vec<T>,
    /// All the EIP-7685 requests in the block.
    pub requests: Requests,
    /// The total gas used by the block.
    pub gas_used: u64,
    /// Blob gas used by the block.
    pub blob_gas_used: u64,
}

impl<T> Default for BlockExecutionResult<T> {
    fn default() -> Self {
        Self { receipts: Vec::new(), requests: Requests::default(), gas_used: 0, blob_gas_used: 0 }
    }
}

/// [`BlockExecutionResult`] combined with state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockExecutionOutput<T> {
    /// All the receipts of the transactions in the block.
    pub result: BlockExecutionResult<T>,
    /// The changed state of the block after execution.
    pub state: BundleState,
}

impl<T> BlockExecutionOutput<T> {
    /// Combines execution results with their persistent state.
    pub const fn new(result: BlockExecutionResult<T>, state: BundleState) -> Self {
        Self { result, state }
    }
}

/// A configured block executor.
pub trait BlockExecutor: Sized {
    /// Consensus transaction type executed by this executor.
    type Transaction;
    /// Receipt type produced by this executor.
    type Receipt;
    /// Runtime EVM type family.
    type EvmTypes: EvmTypes;
    /// EVM instance used by this executor.
    type Evm;
    /// Owned transaction execution result and detached state changes.
    type TransactionResultWithState: BlockTransactionResult<Self::EvmTypes>;

    /// Returns the underlying EVM.
    fn evm(&self) -> &Self::Evm;

    /// Returns the underlying EVM mutably.
    fn evm_mut(&mut self) -> &mut Self::Evm;

    /// Sets a hook for state updates emitted during block execution, including pre-execution
    /// block changes and transaction changes.
    ///
    /// Returns `true` if the hook was installed.
    fn set_state_hook(&mut self, _hook: impl FnMut(StateUpdate) + Send + 'static) -> bool {
        false
    }

    /// Attaches an EIP-7928 block access list for indexed state reads.
    fn set_block_access_list(&mut self, block_access_list: Arc<Bal>);

    /// Sets the EIP-7928 block access index used by state reads and BAL construction.
    fn set_block_access_index(&mut self, index: BlockAccessIndex);

    /// Enables construction of an EIP-7928 block access list from committed state changes.
    fn enable_block_access_list_builder(&mut self);

    /// Takes the block access list built from committed state changes.
    fn take_block_access_list(&mut self) -> Option<Bal>;

    /// Applies pre-execution block changes.
    fn apply_pre_execution_changes(&mut self) -> Result<(), BlockExecutionError>;

    /// Consumes the executor and returns the state accumulated so far without applying
    /// post-execution block changes.
    fn into_state(self) -> BundleState;

    /// Executes a transaction, invokes `f` with the borrowed execution result and state changes,
    /// and commits changes when `f` returns [`CommitChanges::Yes`].
    fn execute_transaction_with_commit_condition(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&TxResult<Self::EvmTypes>, TxStateChanges<'_>) -> CommitChanges,
    ) -> Result<Option<GasOutput>, BlockExecutionError> {
        let output = self.execute_transaction_without_commit(tx)?;
        let result = output.result();
        if f(&result.result, result.pending_state.changes()).should_commit() {
            self.commit_transaction(output).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Validates transaction gas admission against the executor's current block budget.
    fn validate_transaction_gas_limit(&mut self, gas_limit: u64)
    -> Result<(), BlockExecutionError>;

    /// Executes a transaction and detaches its state changes without committing them.
    fn execute_transaction_without_commit(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<Self::TransactionResultWithState, BlockExecutionError>;

    /// Commits detached transaction state and records its receipt and gas accounting.
    fn commit_transaction(
        &mut self,
        output: Self::TransactionResultWithState,
    ) -> Result<GasOutput, BlockExecutionError>;

    /// Executes a transaction, invokes `f` with the borrowed execution result, and commits
    /// changes.
    fn execute_transaction_with_result_closure(
        &mut self,
        tx: impl ExecutableTx<Self>,
        f: impl FnOnce(&TxResult<Self::EvmTypes>),
    ) -> Result<GasOutput, BlockExecutionError> {
        self.execute_transaction_with_commit_condition(tx, |result, _| {
            f(result);
            CommitChanges::Yes
        })
        .map(Option::unwrap_or_default)
    }

    /// Executes a transaction and commits changes.
    fn execute_transaction(
        &mut self,
        tx: impl ExecutableTx<Self>,
    ) -> Result<GasOutput, BlockExecutionError> {
        self.execute_transaction_with_result_closure(tx, |_| {})
    }

    /// Returns receipts accumulated so far.
    fn receipts(&self) -> &[Self::Receipt];

    /// Finishes block execution and returns the output with the block access list built during
    /// execution, if its builder was enabled.
    fn finish_with_block_access_list(
        self,
    ) -> Result<(BlockExecutionOutput<Self::Receipt>, Option<Bal>), BlockExecutionError>;

    /// Finishes block execution and returns the output.
    fn finish(self) -> Result<BlockExecutionOutput<Self::Receipt>, BlockExecutionError> {
        self.finish_with_block_access_list().map(|(output, _)| output)
    }
}
