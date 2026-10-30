//! The evm2 port of the RPC cold-miss fallback — the
//! `WrapDatabaseAsync<AlloyDB>` replacement.
//!
//! revm's seam is sync: `WrapDatabaseAsync` blocks an internal tokio handle
//! on every `AlloyDB` read. evm2's seam is async-native: an [`AsyncDatabase`]
//! returns futures, the `AsyncDb` adapter objectifies them into the
//! synchronous `DynDatabase` surface, and `Evm::transact_async` drives them
//! on stackful coroutines — no manual runtime blocking, no per-read
//! `block_on`. The engine's per-block discipline (fresh `CacheDB` per block,
//! settle-and-discard) is unchanged; only the cold-miss layer differs.
//!
//! The read semantics mirror `AlloyDB`: account reads pin one block and
//! eager-load code (so `get_code_by_hash` is unreachable, the same invariant
//! the revm side relies on), and an account with zero balance, zero nonce,
//! and no code reads as non-existent.

use alloy::primitives::{Address, B256};
use degenbot_rpc::provider::AlloyProvider;

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::r#async::AsyncDatabase;
use ::evm2::evm::AccountInfo;
use ::evm2::interpreter::Word;

/// The adapter's error type — the provider's RPC failure, or a block the
/// node cannot serve (evm2 treats a benign block-hash miss as a database
/// failure, matching the trait contract).
#[derive(Debug, thiserror::Error)]
pub enum AlloyAsyncDbError {
    /// The underlying RPC read failed.
    #[error("RPC read failed: {0}")]
    Rpc(#[from] degenbot_core::errors::ProviderError),
    /// `eth_getBlockByNumber` returned null for a requested block.
    #[error("block {0} not found")]
    BlockNotFound(u64),
}

/// An `AsyncDatabase` over the shared `AlloyProvider`, pinning every read to
/// one block — the per-block cold-miss fallback in the target evm2 stack
/// (`CacheDB<WarmCodeCache<BotStateDb<AlloyAsyncDb>>>`).
pub struct AlloyAsyncDb<'p> {
    /// The shared provider (same instance the revm engine's `AlloyDB`
    /// consumes, via the same retry/timeout policy).
    provider: &'p AlloyProvider,
    /// The block every read pins (the pumped sim block).
    block: u64,
}

impl<'p> AlloyAsyncDb<'p> {
    /// Wrap the provider, pinning reads to `block`.
    #[must_use]
    pub fn new(provider: &'p AlloyProvider, block: u64) -> Self {
        Self { provider, block }
    }
}

impl AsyncDatabase for AlloyAsyncDb<'_> {
    type Error = AlloyAsyncDbError;

    /// Balance + nonce + code at the pinned block, eager-loaded like
    /// `AlloyDB::basic_ref`. The all-empty account reads as non-existent.
    async fn get_account(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let block = Some(self.block);
        let balance = self.provider.get_balance(&address, block).await?;
        let nonce = self.provider.get_transaction_count(&address, block).await?;
        let code = self.provider.get_code(&address, block).await?;
        if balance.is_zero() && nonce == 0 && code.is_empty() {
            return Ok(None);
        }
        // `new_raw` (never `new_legacy`): foreign code may be an EIP-7702
        // delegation designator, and only the raw constructor auto-detects
        // it — a legacy-wrapped 7702 account fails validation
        // (`RejectCallerWithCode`) and decodes wrong downstream.
        Ok(Some(
            AccountInfo::default()
                .with_balance(balance)
                .with_nonce(nonce)
                .with_code(Bytecode::new_raw(code)),
        ))
    }

    /// Unreachable: `get_account` eager-loads code, the same invariant the
    /// revm side relies on (`code_by_hash` is never invoked when `basic`
    /// returns code inline). Standard JSON-RPC has no code-by-hash read, so
    /// there is no honest implementation.
    #[expect(clippy::panic)]
    async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
        panic!("get_account eager-loads code — get_code_by_hash is never invoked");
    }

    /// One storage slot at the pinned block.
    async fn get_storage(&mut self, address: Address, key: Word) -> Result<Word, Self::Error> {
        let value = self
            .provider
            .get_storage_at(&address, key, Some(self.block))
            .await?;
        Ok(Word::from_be_bytes(value.0))
    }

    /// One block hash. A missing block is a database failure (the BLOCKHASH
    /// window contract), never a benign zero.
    async fn get_block_hash(&mut self, number: Word) -> Result<B256, Self::Error> {
        let number = number.to::<u64>();
        let block = self.provider.get_block(number).await?;
        let block = block.ok_or(AlloyAsyncDbError::BlockNotFound(number))?;
        Ok(block.header.hash)
    }
}
