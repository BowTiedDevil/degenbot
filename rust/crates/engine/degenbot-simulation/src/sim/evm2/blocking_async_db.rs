//! The synchronous driver for [`AlloyAsyncDb`] — the evm2 mirror of revm's
//! `WrapDatabaseAsync<AlloyDB>`: every read blocks the current tokio task on
//! the async fetch (`block_in_place` + `Handle::block_on`), so the whole
//! sync verb surface (`Evm2SimSession::run`/`run_sequence`,
//! `Evm2ScratchEvm::replay*`) drives the production stack unchanged.
//!
//! evm2's async-native path (`transact_async` on stackful coroutines) is the
//! post-cutover coroutine fan-out in the performance roadmap; until it
//! lands, the blocking adapter keeps the evm2 production stack
//! behavior-identical to the revm one it replaces, including the same
//! construction failure mode ([`BlockingAsyncDb::new`] returns `None` with
//! no ambient multi-threaded runtime).

use alloy::primitives::{Address, B256};

use ::evm2::bytecode::Bytecode;
use ::evm2::evm::{r#async::AsyncDatabase, AccountInfo, Database};
use ::evm2::interpreter::Word;

use super::alloy_async_db::AlloyAsyncDb;

/// A synchronous `Database` over the async RPC fallback (see the module doc).
pub struct BlockingAsyncDb<'p> {
    inner: AlloyAsyncDb<'p>,
}

impl<'p> BlockingAsyncDb<'p> {
    /// Wrap the async adapter, or return `None` when there is no ambient
    /// multi-threaded tokio runtime to block on (mirrors
    /// `WrapDatabaseAsync::new`'s failure surface).
    #[must_use]
    pub fn new(inner: AlloyAsyncDb<'p>) -> Option<Self> {
        let handle = tokio::runtime::Handle::try_current().ok()?;
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
            return None;
        }
        Some(Self { inner })
    }
}

/// Block the current task on the async read (the runtime is multi-threaded
/// — [`BlockingAsyncDb::new`] refused anything else). A free function so
/// the future's `&mut` borrow of the adapter is the only borrow in play.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
}

impl Database for BlockingAsyncDb<'_> {
    type Error = <AlloyAsyncDb<'static> as AsyncDatabase>::Error;

    fn get_account(&mut self, address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
        block_on(self.inner.get_account(*address))
    }

    fn get_code_by_hash(&mut self, code_hash: &B256) -> Result<Bytecode, Self::Error> {
        block_on(self.inner.get_code_by_hash(*code_hash))
    }

    fn get_storage(&mut self, address: &Address, key: &Word) -> Result<Word, Self::Error> {
        block_on(self.inner.get_storage(*address, *key))
    }

    fn get_block_hash(&mut self, number: &Word) -> Result<B256, Self::Error> {
        block_on(self.inner.get_block_hash(*number))
    }
}
