//! Transaction execution lifecycle and result types.

use super::{
    AccountChanges, BlockStateAccumulator, Evm, PendingState, StateChangeSink, StateChangeSource,
    TxStateChanges,
};
use crate::{EvmTypesHost, interpreter::InstrStop};
use alloc::vec::Vec;
use alloy_primitives::{Address, Bytes, Log};
use core::fmt;
use derive_where::derive_where;

/// Transaction execution result without an owned state diff for an EVM type family.
pub type TxResult<T = crate::BaseEvmTypes> = TxResultExt<<T as EvmTypesHost>::TxResultExt>;

/// Transaction execution result without an owned state diff, parameterized by extension data.
///
/// This is the result-only half of transaction execution: status, gas used, output, stop reason,
/// logs, and extension data. Logs live here because they are execution
/// output, not database state. Use [`ExecutedTx::detach`] only when an owned [`PendingState`]
/// value is required.
#[must_use = "transaction results contain execution status, gas, logs, and errors"]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TxResultExt<E = ()> {
    /// Whether execution succeeded.
    pub status: bool,
    /// Total gas spent (execution + state) before refund. The receipt gas-used value is
    /// [`Self::tx_gas_used`].
    pub total_gas_spent: u64,
    /// State gas consumed by the transaction (EIP-8037): storage creation, account creation, code
    /// deposit, the top-level create's initial state gas, and the EIP-7702 per-authorization
    /// state gas, net of the EIP-7702 per-authorization state-gas refund. Zero when EIP-8037 is
    /// disabled.
    pub state_gas_spent: u64,
    /// Gas refund (capped per EIP-3529), before the EIP-7623 floor adjustment.
    pub refunded: u64,
    /// EIP-7623 floor gas. Zero when not applicable.
    pub floor_gas: u64,
    /// Interpreter stop reason.
    pub stop: InstrStop,
    /// Return or revert output.
    pub output: Bytes,
    /// Created contract address for successful create transactions.
    pub created_address: Option<Address>,
    /// Logs emitted by the transaction.
    ///
    /// Empty when [`EvmFeatures::LOG_COLLECTION`](crate::EvmFeatures::LOG_COLLECTION) is disabled.
    pub logs: Vec<Log>,
    /// EVM type-specific extension data.
    pub ext: E,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl<E> TxResultExt<E> {
    /// Replaces the extension, preserving all other transaction result fields.
    #[inline]
    pub fn with_ext<F>(self, ext: F) -> TxResultExt<F> {
        self.map_ext(|_| ext)
    }

    /// Transforms the extension, preserving all other transaction result fields.
    #[inline]
    pub fn map_ext<F>(self, f: impl FnOnce(E) -> F) -> TxResultExt<F> {
        let Self {
            status,
            total_gas_spent,
            state_gas_spent,
            refunded,
            floor_gas,
            stop,
            output,
            created_address,
            logs,
            ext,
            _non_exhaustive,
        } = self;
        TxResultExt {
            status,
            total_gas_spent,
            state_gas_spent,
            refunded,
            floor_gas,
            stop,
            output,
            created_address,
            logs,
            ext: f(ext),
            _non_exhaustive,
        }
    }

    /// Returns whether execution succeeded.
    #[inline]
    pub const fn is_success(&self) -> bool {
        self.status
    }

    /// Returns whether execution ended with the `REVERT` opcode.
    ///
    /// Unlike [`InstrStop::is_frame_revert`], this does not count [`InstrStop::CallTooDeep`],
    /// [`InstrStop::OutOfFunds`] or [`InstrStop::NonceOverflow`]. Those are call-frame failures
    /// that the interpreter treats as reverts, but a transaction result reports them as halts.
    #[inline]
    pub const fn is_revert(&self) -> bool {
        !self.status && self.stop.is_revert()
    }

    /// Returns whether execution failed for a reason other than the `REVERT` opcode.
    #[inline]
    pub const fn is_halt(&self) -> bool {
        !self.status && !self.is_revert()
    }

    /// Returns the receipt gas-used value: `max(total_gas_spent - refunded, floor_gas)`.
    #[inline]
    pub const fn tx_gas_used(&self) -> u64 {
        // `max(spent - refunded, floor_gas)`, spelled out because `Ord::max` is not const-stable.
        let spent_sub_refunded = self.total_gas_spent.saturating_sub(self.refunded);
        if spent_sub_refunded > self.floor_gas { spent_sub_refunded } else { self.floor_gas }
    }

    /// Returns this transaction's execution (non-state) block gas:
    /// `max(total_gas_spent - state_gas_spent, floor_gas)`, pre-refund.
    ///
    /// Together with [`Self::state_gas_spent()`] this is the per-transaction split that callers add
    /// to the block's separate execution- and state-gas counters (EIP-8037 + EIP-7778). The
    /// EIP-7623 calldata floor is not discounted by state gas, so it binds against the
    /// execution component (ethereum/EIPs#11836); the refund still only affects
    /// [`Self::tx_gas_used`].
    #[inline]
    pub const fn execution_gas_spent(&self) -> u64 {
        let execution = self.total_gas_spent.saturating_sub(self.state_gas_spent);
        if execution > self.floor_gas { execution } else { self.floor_gas }
    }

    /// Returns this transaction's state gas (EIP-8037) — the stored `state_gas_spent` field,
    /// exposed as the counterpart to [`Self::execution_gas_spent`] for the per-transaction
    /// block-gas split.
    #[inline]
    pub const fn state_gas_spent(&self) -> u64 {
        self.state_gas_spent
    }

    /// Returns the effective gas refund after the EIP-7623 floor adjustment.
    ///
    /// [`Self::refunded`] is capped per EIP-3529 but still pre-floor. When the floor binds —
    /// `total_gas_spent - refunded < floor_gas` — the floor charge absorbs the refund entirely
    /// and this returns zero; otherwise it returns [`Self::refunded`]. Consumers computing net
    /// cost as `total_gas_spent - refund` should use this rather than the raw field; the receipt
    /// value is [`Self::tx_gas_used`].
    #[inline]
    pub const fn final_refunded(&self) -> u64 {
        if self.total_gas_spent.saturating_sub(self.refunded) < self.floor_gas {
            0
        } else {
            self.refunded
        }
    }
}

/// Whether the transaction scratch is still pending resolution. Not the owned
/// [`PendingState`] a transaction detaches into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StateStatus {
    Present,
    Cleared,
}

/// A transaction whose post-finalization state is not resolved yet.
///
/// `ExecutedTx` borrows the EVM mutably until the caller chooses what to do with the
/// transaction scratch:
///
/// - [`Self::commit`] accepts the state into the internal accepted overlay;
/// - [`Self::commit_to`] accepts the state and records it in a block accumulator;
/// - [`Self::commit_with`] accepts the state and first streams it to an external sink;
/// - [`Self::discard`] drops the state and keeps only the result;
/// - [`Self::discard_with`] streams the state to an external sink and then drops it;
/// - [`Self::detach`] moves the pending transaction overlay out as a [`PendingState`] without
///   committing it.
///
/// Dropping `ExecutedTx` without calling one of those methods is equivalent to [`Self::discard`].
#[must_use = "executed transaction state must be committed, discarded, or detached"]
pub struct ExecutedTx<'evm, 'host, T: EvmTypesHost = crate::BaseEvmTypes> {
    evm: &'evm mut Evm<'host, T>,
    result: Option<TxResult<T>>,
    state: StateStatus,
}

impl<T: EvmTypesHost> fmt::Debug for ExecutedTx<'_, '_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutedTx")
            .field("has_pending_state", &self.has_pending_state())
            .finish_non_exhaustive()
    }
}

impl<'evm, 'host, T: EvmTypesHost> ExecutedTx<'evm, 'host, T> {
    #[inline]
    pub(crate) const fn from_result(
        evm: &'evm mut Evm<'host, T>,
        result: TxResult<T>,
        has_pending_state: bool,
    ) -> Self {
        Self {
            evm,
            result: Some(result),
            state: if has_pending_state { StateStatus::Present } else { StateStatus::Cleared },
        }
    }

    #[inline]
    fn has_pending_state(&self) -> bool {
        self.state == StateStatus::Present
    }

    #[inline]
    const fn take_result(&mut self) -> TxResult<T> {
        self.result.take().expect("executed transaction result was already taken")
    }

    #[inline]
    fn clear_pending_state(&mut self) {
        if self.has_pending_state() {
            self.evm.state.clear_transaction_state();
            self.state = StateStatus::Cleared;
        }
    }

    #[inline]
    fn commit_pending_state(&mut self) {
        if self.has_pending_state() {
            self.evm.state.commit_transaction();
            self.evm.state.clear_transaction_state();
            self.state = StateStatus::Cleared;
        }
    }

    #[inline]
    fn take_pending_state(&mut self) -> PendingState {
        if self.has_pending_state() {
            let pending = self.evm.state.take_pending_state();
            self.evm.state.clear_transaction_state();
            self.state = StateStatus::Cleared;
            pending
        } else {
            PendingState::default()
        }
    }

    /// Returns the transaction result without resolving state changes.
    #[inline]
    pub fn result(&self) -> &TxResult<T> {
        match &self.result {
            Some(result) => result,
            None => unreachable!("executed transaction result was already taken"),
        }
    }

    /// Returns each account loaded by the transaction with its changes, in an unspecified order.
    ///
    /// This borrows the transaction state without detaching it, so a caller can apply the changes
    /// and then [`commit`](Self::commit) or [`discard`](Self::discard) the transaction.
    #[inline]
    pub fn account_changes(&self) -> impl Iterator<Item = AccountChanges<'_>> {
        self.changes().account_changes()
    }

    /// Returns a borrowed view of the transaction's state changes.
    ///
    /// Like [`account_changes`](Self::account_changes), this borrows the transaction state in
    /// place, so a caller can inspect what the transaction wrote, including single accounts and
    /// storage slots, before deciding to [`commit`](Self::commit) or [`discard`](Self::discard).
    #[inline]
    pub fn changes(&self) -> TxStateChanges<'_> {
        if self.has_pending_state() {
            self.evm.state.transaction_changes()
        } else {
            TxStateChanges::empty()
        }
    }

    /// Accepts the transaction state into the internal accepted overlay.
    ///
    /// This makes the transaction's state effects visible to later transactions executed by the
    /// same EVM. It clears transaction scratch and returns the result-only [`TxResult`].
    pub fn commit(mut self) -> TxResult<T> {
        self.commit_pending_state();
        self.take_result()
    }

    /// Accepts the transaction state and records its changes in a block accumulator.
    ///
    /// This streams transaction changes into `block_state`, commits them to the accepted overlay,
    /// and returns the result-only [`TxResult`]. The transaction overlay is not detached.
    pub fn commit_to(self, block_state: &mut BlockStateAccumulator) -> TxResult<T> {
        let Ok(result) = self.commit_with(block_state);
        result
    }

    /// Streams transaction changes into `sink`, then accepts the transaction.
    ///
    /// If the sink returns an error, the transaction is not committed and the executed handle is
    /// dropped, which discards the transaction scratch. Use infallible sinks on the block hot path.
    pub fn commit_with<S: StateChangeSink>(
        mut self,
        sink: &mut S,
    ) -> Result<TxResult<T>, S::Error> {
        if self.has_pending_state() {
            self.evm.state.transaction_changes().visit(sink)?;
            self.commit_pending_state();
        }
        Ok(self.take_result())
    }

    /// Discards the transaction state and returns the result.
    ///
    /// Discarding does not mutate the accepted overlay and does not detach the transaction
    /// overlay. This is the intended path for result-only execution such as `eth_call`.
    pub fn discard(mut self) -> TxResult<T> {
        self.clear_pending_state();
        self.take_result()
    }

    /// Streams transaction changes into `sink`, then discards the transaction state.
    ///
    /// This observes the same pending writes as [`Self::commit_with`], but does not mutate the
    /// accepted overlay. If the sink returns an error, the executed handle is dropped, which
    /// discards the transaction scratch.
    pub fn discard_with<S: StateChangeSink>(
        mut self,
        sink: &mut S,
    ) -> Result<TxResult<T>, S::Error> {
        if self.has_pending_state() {
            self.evm.state.transaction_changes().visit(sink)?;
            self.clear_pending_state();
        }
        Ok(self.take_result())
    }

    /// Detaches the transaction into its owned pending state without committing it.
    ///
    /// Detaching moves the transaction overlay out of the EVM as a [`PendingState`], clears
    /// transaction scratch, and returns a [`TxResultWithState`] that can be moved or stored. The
    /// pending state folds into an EIP-7928 Block Access List via
    /// [`Bal::commit`](crate::evm::Bal::commit) and streams its changes
    /// to a [`StateChangeSink`] via
    /// [`StateChangeSource::visit`](super::StateChangeSource::visit). The detached state is not
    /// accepted into this EVM's internal overlay unless the caller commits it separately.
    pub fn detach(mut self) -> TxResultWithState<T> {
        let pending_state = self.take_pending_state();
        let result = self.take_result();
        TxResultWithState { result, pending_state, _non_exhaustive: () }
    }
}

impl<T: EvmTypesHost> StateChangeSource for ExecutedTx<'_, '_, T> {
    /// Visits the transaction's changes in place, as [`PendingState`]'s implementation does for
    /// detached state.
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        self.changes().visit(sink)
    }
}

impl<T: EvmTypesHost> Drop for ExecutedTx<'_, '_, T> {
    #[inline]
    fn drop(&mut self) {
        self.clear_pending_state();
    }
}

/// Result of executing a transaction with its owned pending state.
///
/// This is the detached shape produced by [`ExecutedTx::detach`]. It pairs a [`TxResult`] with
/// the owned [`PendingState`] the transaction was left in. Prefer resolving [`Evm::transact`] with
/// [`ExecutedTx::commit`] or [`ExecutedTx::discard`] when an owned write-set is unnecessary.
#[derive_where(Clone, Debug, Default, PartialEq, Eq; T::TxResultExt)]
pub struct TxResultWithState<T: EvmTypesHost = crate::BaseEvmTypes> {
    /// Execution result produced by the transaction.
    pub result: TxResult<T>,
    /// Pending state the transaction detached into.
    pub pending_state: PendingState,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(
        total_gas_spent: u64,
        state_gas_spent: u64,
        refunded: u64,
        floor_gas: u64,
    ) -> TxResultExt {
        TxResultExt {
            total_gas_spent,
            state_gas_spent,
            refunded,
            floor_gas,
            ..TxResultExt::default()
        }
    }

    #[test]
    fn gas_breakdown_getters() {
        // Floor inactive: tx_gas_used = total_gas_spent - refunded, refund is effective.
        let r = result(100_000, 30_000, 8_000, 21_000);
        assert_eq!(r.tx_gas_used(), 92_000);
        // Per-tx split: execution + state == total.
        assert_eq!(r.execution_gas_spent(), 70_000);
        assert_eq!(r.state_gas_spent(), 30_000);
        assert_eq!(r.execution_gas_spent() + r.state_gas_spent(), r.total_gas_spent);
    }

    #[test]
    fn status_helpers_classify_stop_reasons() {
        let with_stop = |status, stop| TxResultExt::<()> { status, stop, ..TxResultExt::default() };

        let success = with_stop(true, InstrStop::Return);
        assert!(success.is_success() && !success.is_revert() && !success.is_halt());

        let revert = with_stop(false, InstrStop::Revert);
        assert!(!revert.is_success() && revert.is_revert() && !revert.is_halt());

        // Counted by `InstrStop::is_frame_revert`, but not a top-level `REVERT`.
        for stop in [
            InstrStop::OutOfFunds,
            InstrStop::CallTooDeep,
            InstrStop::NonceOverflow,
            InstrStop::OutOfGas,
        ] {
            let halt = with_stop(false, stop);
            assert!(!halt.is_success() && !halt.is_revert() && halt.is_halt(), "{stop:?}");
        }
    }

    #[test]
    fn floor_gas_absorbs_refund() {
        // Floor active: spent - refunded < floor, so floor wins.
        let r = result(50_000, 0, 40_000, 21_000);
        assert_eq!(r.tx_gas_used(), 21_000);
    }
}
