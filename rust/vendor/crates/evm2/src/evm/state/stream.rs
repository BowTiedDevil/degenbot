//! Borrowed state-change streaming traits and adapters.

use super::{AccountInfo, StorageOverlay};
use crate::{bytecode::Bytecode, interpreter::Word};
use alloy_primitives::{Address, B256};
use auto_impl::auto_impl;
use core::convert::Infallible;

/// Borrowed account change passed to [`StateChangeSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccountChangeRef<'a> {
    /// Account address.
    pub address: Address,
    /// Account at the start of the source's aggregation boundary.
    pub original: Option<&'a AccountInfo>,
    /// Account after the change. `None` is an explicit deletion.
    pub current: Option<&'a AccountInfo>,
    /// Whether the account was created during the transaction.
    ///
    /// Only transaction-level sources report this; block-level aggregation loses per-transaction
    /// lifecycle flags and reports `false`.
    pub created: bool,
    /// Whether the account was selfdestructed during the transaction.
    ///
    /// Only transaction-level sources report this; block-level aggregation loses per-transaction
    /// lifecycle flags and reports `false`.
    pub selfdestructed: bool,
}

/// One loaded account's transaction changes: its metadata, new bytecode, and storage overlay.
///
/// Returned by [`PendingState::account_changes`](super::PendingState::account_changes) and
/// [`ExecutedTx::account_changes`](crate::evm::ExecutedTx::account_changes).
#[derive(Clone, Copy, Debug)]
pub struct AccountChanges<'a> {
    /// Account address.
    pub address: Address,
    /// Account at the start of the transaction.
    pub original: Option<&'a AccountInfo>,
    /// Account after the transaction. `None` means the account does not exist.
    pub current: Option<&'a AccountInfo>,
    /// Whether the account was created during the transaction.
    pub created: bool,
    /// Whether the account was selfdestructed during the transaction.
    pub selfdestructed: bool,
    /// Whether the account was touched by the transaction, even if [`Self::is_changed`] is false.
    ///
    /// Loaded-only accounts are not touched. Reverted touches are excluded, except for the
    /// EIP-161 precompile-3 rule. This metadata survives transaction finalization and detachment.
    /// It is not emitted through [`StateChangeSink`].
    pub touched: bool,
    /// The account's new bytecode keyed by code hash, when its code changed to non-empty code.
    pub code: Option<(B256, &'a Bytecode)>,
    /// The account's loaded storage slots and wipe marker.
    pub storage: &'a StorageOverlay,
}

impl<'a> AccountChanges<'a> {
    /// Returns whether the account info or lifecycle changed: different info, creation, or
    /// selfdestruct. Storage is not considered; see [`Self::is_changed`].
    #[inline]
    pub fn is_info_changed(&self) -> bool {
        self.original != self.current || self.created || self.selfdestructed
    }

    /// Returns whether the account changes state at all, through its info or its storage.
    #[inline]
    pub fn is_changed(&self) -> bool {
        self.is_info_changed() || self.storage.is_changed()
    }

    /// Emits these changes to `sink`: new bytecode, then the storage wipe, changed slots and slot
    /// reads, then the metadata as a change or a read.
    pub fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error> {
        if let Some((code_hash, code)) = self.code {
            sink.bytecode(code_hash, code)?;
        }
        let address = self.address;
        let storage = self.storage;
        if storage.wiped {
            sink.storage_wipe(address)?;
        }
        for (&key, slot) in &storage.slots {
            let value = &slot.value;
            if slot.is_changed(storage.wiped) {
                sink.storage(StorageChange {
                    address,
                    key,
                    original: value.original,
                    current: value.current,
                })?;
            } else {
                sink.storage_read(address, key, value.original)?;
            }
        }
        if self.is_info_changed() {
            sink.account(AccountChangeRef {
                address,
                original: self.original,
                current: self.current,
                created: self.created,
                selfdestructed: self.selfdestructed,
            })
        } else {
            sink.account_read(address, self.current)
        }
    }
}

/// Storage slot change passed to [`StateChangeSink`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageChange {
    /// Account address.
    pub address: Address,
    /// Storage slot key.
    pub key: Word,
    /// Slot value at the start of the source's aggregation boundary.
    pub original: Word,
    /// Slot value after the change.
    pub current: Word,
}

/// Consumer of borrowed transaction or block state changes.
#[auto_impl(&mut, Box)]
pub trait StateChangeSink {
    /// Error returned by this sink.
    type Error;

    /// Observes bytecode keyed by code hash.
    #[inline]
    fn bytecode(&mut self, _code_hash: B256, _code: &Bytecode) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes an account change.
    #[inline]
    fn account(&mut self, _change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage wipe marker for an account.
    ///
    /// Sources emit this before any storage slot changes for the same account so sinks can apply
    /// the wipe once, then apply subsequent slot writes.
    #[inline]
    fn storage_wipe(&mut self, _address: Address) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage slot change.
    #[inline]
    fn storage(&mut self, _change: StorageChange) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes an account the transaction loaded but left unchanged. `None` means the account
    /// was loaded as non-existent.
    ///
    /// Only transaction-level sources report reads; sinks that persist changes can ignore them.
    #[inline]
    fn account_read(
        &mut self,
        _address: Address,
        _info: Option<&AccountInfo>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Observes a storage slot the transaction loaded but left unchanged.
    ///
    /// `value` is the slot's value at the start of the transaction. After a storage wipe the
    /// slot's value after the transaction is zero.
    ///
    /// Only transaction-level sources report reads; sinks that persist changes can ignore them.
    #[inline]
    fn storage_read(
        &mut self,
        _address: Address,
        _key: Word,
        _value: Word,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Source of borrowed state changes.
pub trait StateChangeSource {
    /// Visits all changes. Ordering is source-defined and not guaranteed to be deterministic.
    ///
    /// Sources that track reads also report loaded-but-unchanged entries through
    /// [`StateChangeSink::account_read`] and [`StateChangeSink::storage_read`].
    fn visit<S: StateChangeSink>(&self, sink: &mut S) -> Result<(), S::Error>;
}

/// Sink that ignores all changes.
#[derive(Clone, Debug, Default)]
#[allow(missing_copy_implementations)]
pub struct NoopChangeSink(());

impl StateChangeSink for NoopChangeSink {
    type Error = Infallible;
}

/// Sink that forwards each change to two sinks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tee<A, B> {
    a: A,
    b: B,
}

impl<A, B> Tee<A, B> {
    /// Creates a new tee sink.
    #[inline]
    pub const fn new(a: A, b: B) -> Self {
        Self { a, b }
    }
}

impl<A, B> StateChangeSink for Tee<A, B>
where
    A: StateChangeSink,
    B: StateChangeSink<Error = A::Error>,
{
    type Error = A::Error;

    #[inline]
    fn bytecode(&mut self, code_hash: B256, code: &Bytecode) -> Result<(), Self::Error> {
        self.a.bytecode(code_hash, code)?;
        self.b.bytecode(code_hash, code)
    }

    #[inline]
    fn account(&mut self, change: AccountChangeRef<'_>) -> Result<(), Self::Error> {
        self.a.account(change)?;
        self.b.account(change)
    }

    #[inline]
    fn storage_wipe(&mut self, address: Address) -> Result<(), Self::Error> {
        self.a.storage_wipe(address)?;
        self.b.storage_wipe(address)
    }

    #[inline]
    fn storage(&mut self, change: StorageChange) -> Result<(), Self::Error> {
        self.a.storage(change)?;
        self.b.storage(change)
    }

    #[inline]
    fn account_read(
        &mut self,
        address: Address,
        info: Option<&AccountInfo>,
    ) -> Result<(), Self::Error> {
        self.a.account_read(address, info)?;
        self.b.account_read(address, info)
    }

    #[inline]
    fn storage_read(
        &mut self,
        address: Address,
        key: Word,
        value: Word,
    ) -> Result<(), Self::Error> {
        self.a.storage_read(address, key, value)?;
        self.b.storage_read(address, key, value)
    }
}
