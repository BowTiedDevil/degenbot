//! Async execution support for synchronous EVM hosts.
//!
//! This module runs the synchronous EVM on a native fiber. Synchronous host methods can then use
//! `block_on_current` to poll an async operation; if that operation is pending, the fiber is
//! suspended and the outer async task returns `Poll::Pending`.

use crate::{
    DatabaseError,
    bytecode::Bytecode,
    evm::{AccountInfo, DbResult, DynDatabase, NonStaticAny},
    interpreter::Word,
};
use alloy_primitives::{Address, B256};
use core::{fmt, future::Future, marker::PhantomData, pin::Pin, ptr::NonNull, task::Poll};
use corosensei::{Coroutine, CoroutineResult, Yielder, stack::DefaultStack};
use std::{cell::Cell, error::Error, io, task::Context};

type Resume = AsyncResult<NonNull<Context<'static>>>;
type Yield = ();
type Complete<R> = AsyncResult<R>;
type EvmFiber<R> = Coroutine<Resume, Yield, Complete<R>, DefaultStack>;

/// Fiber stack size, large enough for nested calls to reach the 1024 call depth limit.
///
/// Calls recurse on the native stack. Before EIP-150 a call can forward almost all of its gas, so a
/// self-calling contract reaches the depth limit with about 5.3M gas. Running such a transaction
/// with 1025 frames needs at least about 1.0 MiB of stack in release builds, 1.6 MiB with an
/// inspector, and 5.6 MiB with an inspector at opt-level 0 (about 5.5 KiB per frame). The stack is
/// reserved with `mmap` and committed lazily, so unused headroom only costs address space.
const DEFAULT_STACK_SIZE: usize = 16 * 1024 * 1024;

/// Reusable async EVM fiber stack storage.
#[derive(Default)]
pub(crate) struct FiberStack {
    stack: Option<DefaultStack>,
}

impl FiberStack {
    #[inline]
    fn take_or_new(&mut self) -> AsyncResult<DefaultStack> {
        match self.stack.take() {
            Some(stack) => Ok(stack),
            None => DefaultStack::new(DEFAULT_STACK_SIZE).map_err(AsyncError::Io),
        }
    }

    #[inline]
    fn put(&mut self, stack: DefaultStack) {
        debug_assert!(self.stack.is_none());
        self.stack = Some(stack);
    }
}

thread_local! {
    static CURRENT: Cell<Option<NonNull<CurrentFiber>>> = const { Cell::new(None) };
}

/// Result type used by async EVM execution helpers.
pub type AsyncResult<T, E = core::convert::Infallible> = Result<T, AsyncError<E>>;

/// Error returned by async EVM execution helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AsyncError<E = core::convert::Infallible> {
    /// The async EVM fiber was cancelled before execution completed.
    #[error("async EVM execution was cancelled")]
    Cancelled,
    /// An async host operation was called outside an async EVM fiber.
    #[error("async host operation requires EVM async fiber execution")]
    NotOnFiber,
    /// Async fiber stack setup failed.
    #[error(transparent)]
    Io(io::Error),
    /// The wrapped operation returned an error.
    #[error(transparent)]
    Inner(#[from] E),
}

impl AsyncError {
    fn with_inner_error<E>(self) -> AsyncError<E> {
        match self {
            Self::Cancelled => AsyncError::Cancelled,
            Self::NotOnFiber => AsyncError::NotOnFiber,
            Self::Io(error) => AsyncError::Io(error),
            Self::Inner(error) => match error {},
        }
    }
}

struct CurrentFiber {
    suspend: NonNull<Yielder<Resume, Yield>>,
    future_cx: NonNull<Context<'static>>,
    previous: Option<NonNull<Self>>,
    cancelled: bool,
}

impl CurrentFiber {
    #[inline]
    fn context(&mut self) -> &mut Context<'_> {
        unsafe { restore_context_lifetime(self.future_cx.as_mut()) }
    }

    #[inline]
    fn suspend(&mut self) -> AsyncResult<()> {
        let current = NonNull::from(&mut *self);
        CURRENT.set(self.previous);
        match unsafe { self.suspend.as_ref() }.suspend(()) {
            Ok(cx) => {
                CURRENT.set(Some(current));
                self.future_cx = cx;
                Ok(())
            }
            Err(error) => {
                CURRENT.set(Some(current));
                self.cancelled = true;
                Err(error)
            }
        }
    }

    #[inline]
    const fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

struct ResetCurrentFiber(Option<NonNull<CurrentFiber>>);

impl Drop for ResetCurrentFiber {
    fn drop(&mut self) {
        CURRENT.set(self.0);
    }
}

/// Runs `func` on a native fiber and awaits its completion.
///
/// Synchronous code running inside `func` may call [`block_on_current`] to wait for async host
/// operations without blocking the executor thread.
#[cfg(test)]
pub(crate) fn on_fiber_result<'a, R, E>(
    func: impl FnOnce() -> Result<R, E> + 'a,
) -> impl Future<Output = AsyncResult<R, E>> + Send + 'a
where
    R: 'a,
    E: 'a,
{
    OnFiber::new(func)
}

/// Runs `func` on a native fiber backed by a reusable EVM stack slot, returning a local future.
///
/// # Safety
///
/// `stack` must point to valid stack storage for the lifetime of the returned future. That storage
/// must not be accessed by anything else until the returned future is dropped.
pub(crate) unsafe fn on_local_fiber_result_with_stack<'a, R, E>(
    stack: NonNull<FiberStack>,
    func: impl FnOnce() -> Result<R, E> + 'a,
) -> impl Future<Output = AsyncResult<R, E>> + 'a
where
    R: 'a,
    E: 'a,
{
    LocalOnFiber::new(OnFiber::with_stack(stack, func))
}

/// Runs `func` on a native fiber backed by a reusable EVM stack slot.
///
/// # Safety
///
/// `stack` must point to valid stack storage for the lifetime of the returned future. That storage
/// must not be accessed by anything else until the returned future is dropped.
pub(crate) unsafe fn on_fiber_result_with_stack<'a, R, E>(
    stack: NonNull<FiberStack>,
    func: impl FnOnce() -> Result<R, E> + 'a,
) -> impl Future<Output = AsyncResult<R, E>> + Send + 'a
where
    R: 'a,
    E: 'a,
{
    OnFiber::with_stack(stack, func)
}

#[cfg(test)]
pub(crate) fn on_fiber<'a, R>(
    func: impl FnOnce() -> R + 'a,
) -> impl Future<Output = AsyncResult<R>> + Send + 'a
where
    R: 'a,
{
    on_fiber_result(move || Ok::<_, core::convert::Infallible>(func()))
}

struct LocalOnFiber<F> {
    inner: F,
    _not_send: PhantomData<*mut ()>,
}

impl<F> LocalOnFiber<F> {
    const fn new(inner: F) -> Self {
        Self { inner, _not_send: PhantomData }
    }
}

impl<F: Future + Unpin> Future for LocalOnFiber<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().inner).poll(cx)
    }
}

enum OnFiber<'a, R, E> {
    Running(FiberFuture<'a, Result<R, E>>),
    Error(Option<AsyncError>),
    Done,
}

impl<'a, R, E> OnFiber<'a, R, E> {
    #[cfg(test)]
    fn new(func: impl FnOnce() -> Result<R, E> + 'a) -> Self {
        Self::new_inner(None, func)
    }

    fn with_stack(stack: NonNull<FiberStack>, func: impl FnOnce() -> Result<R, E> + 'a) -> Self {
        Self::new_inner(Some(stack), func)
    }

    fn new_inner(
        stack: Option<NonNull<FiberStack>>,
        func: impl FnOnce() -> Result<R, E> + 'a,
    ) -> Self {
        match FiberFuture::new(stack, func) {
            Ok(fiber) => Self::Running(fiber),
            Err(error) => Self::Error(Some(error)),
        }
    }
}

impl<R, E> Future for OnFiber<'_, R, E> {
    type Output = AsyncResult<R, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this {
            Self::Running(fiber) => match Pin::new(fiber).poll(cx) {
                Poll::Ready(Ok(Ok(value))) => {
                    *this = Self::Done;
                    Poll::Ready(Ok(value))
                }
                Poll::Ready(Ok(Err(error))) => {
                    *this = Self::Done;
                    Poll::Ready(Err(AsyncError::Inner(error)))
                }
                Poll::Ready(Err(error)) => {
                    *this = Self::Done;
                    Poll::Ready(Err(error.with_inner_error()))
                }
                Poll::Pending => Poll::Pending,
            },
            Self::Error(error) => {
                let error = error.take().expect("async EVM fiber error already returned");
                Poll::Ready(Err(error.with_inner_error()))
            }
            Self::Done => panic!("async EVM fiber polled after completion"),
        }
    }
}

struct FiberFuture<'a, R> {
    fiber: Option<EvmFiber<R>>,
    stack: Option<NonNull<FiberStack>>,
    _marker: PhantomData<&'a ()>,
}

// SAFETY: The future may move between polls, but the coroutine stack itself is heap allocated and
// is only resumed through `poll` with a fresh task context. Values that can remain on the coroutine
// stack across suspension are required to be `Send` by the blocking boundary.
unsafe impl<R> Send for FiberFuture<'_, R> {}

impl<'a, R> FiberFuture<'a, R> {
    fn new(
        mut stack: Option<NonNull<FiberStack>>,
        func: impl FnOnce() -> R + 'a,
    ) -> AsyncResult<Self> {
        let fiber_stack = match &mut stack {
            Some(stack) => unsafe { stack.as_mut() }.take_or_new()?,
            None => DefaultStack::new(DEFAULT_STACK_SIZE).map_err(AsyncError::Io)?,
        };
        let body = move |suspend: &Yielder<Resume, Yield>, resume| {
            let future_cx = resume?;
            let previous = CURRENT.get();
            let mut current = CurrentFiber {
                suspend: NonNull::from(suspend),
                future_cx,
                previous,
                cancelled: false,
            };
            let current = NonNull::from(&mut current);
            CURRENT.set(Some(current));
            let _reset = ResetCurrentFiber(previous);
            Ok(func())
        };
        // SAFETY: The coroutine is stored inside `FiberFuture<'a, R>`, which is tied to the
        // borrowed state lifetime and dropped before those borrows can expire.
        let fiber = unsafe { Coroutine::with_stack_unchecked(fiber_stack, body) };
        Ok(Self { fiber: Some(fiber), stack, _marker: PhantomData })
    }

    fn recycle_stack(&mut self) {
        let Some(fiber) = self.fiber.take() else { return };
        debug_assert!(fiber.done());
        let stack = fiber.into_stack();
        if let Some(mut slot) = self.stack {
            unsafe { slot.as_mut() }.put(stack);
        }
    }
}

impl<R> Future for FiberFuture<'_, R> {
    type Output = AsyncResult<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let cx = NonNull::from(unsafe { change_context_lifetime(cx) });
        let fiber = this.fiber.as_mut().expect("async EVM fiber polled after completion");
        match fiber.resume(Ok(cx)) {
            CoroutineResult::Return(result) => {
                this.recycle_stack();
                Poll::Ready(result)
            }
            CoroutineResult::Yield(()) => Poll::Pending,
        }
    }
}

impl<R> Drop for FiberFuture<'_, R> {
    fn drop(&mut self) {
        let Some(fiber) = self.fiber.as_mut() else {
            return;
        };
        if fiber.done() {
            self.recycle_stack();
        } else if matches!(fiber.resume(Err(AsyncError::Cancelled)), CoroutineResult::Yield(())) {
            // SAFETY: Cancellation already gave the coroutine a chance to return normally. If it
            // yields again, the stack is no longer useful to this future.
            unsafe { fiber.force_reset() };
        } else {
            self.recycle_stack();
        }
    }
}

/// Polls `future` to completion from inside an async EVM fiber.
///
/// If `future` returns `Poll::Pending`, the current EVM fiber is suspended and the outer
/// async EVM future returns `Poll::Pending`. When the executor wakes and polls the outer future
/// again, the EVM fiber resumes and continues polling `future`.
///
/// # Errors
///
/// Returns [`AsyncError::NotOnFiber`] if called outside async EVM execution, or
/// [`AsyncError::Cancelled`] if the outer async EVM execution was dropped.
pub(crate) fn block_on_current<F: Future>(future: F) -> AsyncResult<F::Output> {
    let mut future = core::pin::pin!(future);
    loop {
        match with_current(|current| {
            if current.is_cancelled() {
                return Err(AsyncError::Cancelled);
            }
            let poll = future.as_mut().poll(current.context());
            if poll.is_pending() {
                current.suspend()?;
            }
            Ok(poll)
        })? {
            Poll::Ready(value) => return Ok(value),
            Poll::Pending => {}
        }
    }
}

fn block_on_current_result<F, T, E>(future: F) -> AsyncResult<T, E>
where
    F: Future<Output = Result<T, E>>,
{
    match block_on_current(future).map_err(AsyncError::with_inner_error)? {
        Ok(value) => Ok(value),
        Err(error) => Err(AsyncError::Inner(error)),
    }
}

fn with_current<R>(f: impl FnOnce(&mut CurrentFiber) -> AsyncResult<R>) -> AsyncResult<R> {
    let mut current = CURRENT.get().ok_or(AsyncError::NotOnFiber)?;
    f(unsafe { current.as_mut() })
}

unsafe fn change_context_lifetime<'a>(cx: &'a mut Context<'_>) -> &'a mut Context<'static> {
    unsafe { core::mem::transmute::<&'a mut Context<'_>, &'a mut Context<'static>>(cx) }
}

unsafe fn restore_context_lifetime<'a>(cx: &'a mut Context<'static>) -> &'a mut Context<'a> {
    unsafe { core::mem::transmute::<&'a mut Context<'static>, &'a mut Context<'a>>(cx) }
}

/// Asynchronous backing database implementation.
///
/// To take advantage of yielding host I/O, this must be wrapped in [`AsyncDb`] and used with
/// async EVM entrypoints such as [`crate::Evm::transact_async`]. Calling synchronous EVM
/// entrypoints with an [`AsyncDb`] fails because the adapter can only poll futures from inside an
/// async EVM fiber.
pub trait AsyncDatabase: NonStaticAny {
    /// Database error type.
    type Error: Error + Send + Sync + 'static;

    /// Whether a concrete database error indicates an internal failure rather than invalid input.
    fn is_fatal(_error: &Self::Error) -> bool {
        true
    }

    /// Loads account information.
    fn get_account(
        &mut self,
        address: Address,
    ) -> impl Future<Output = Result<Option<AccountInfo>, Self::Error>> + Send + '_;

    /// Loads bytecode by code hash.
    fn get_code_by_hash(
        &mut self,
        code_hash: B256,
    ) -> impl Future<Output = Result<Bytecode, Self::Error>> + Send + '_;

    /// Loads a persistent storage slot.
    fn get_storage(
        &mut self,
        address: Address,
        key: Word,
    ) -> impl Future<Output = Result<Word, Self::Error>> + Send + '_;

    /// Loads a historical block hash.
    fn get_block_hash(
        &mut self,
        number: Word,
    ) -> impl Future<Output = Result<B256, Self::Error>> + Send + '_;
}

/// Adapter that exposes an [`AsyncDatabase`] through the synchronous [`DynDatabase`] interface.
pub struct AsyncDb<D: AsyncDatabase> {
    db: D,
}

impl<D: AsyncDatabase> AsyncDb<D> {
    /// Creates a new async database adapter.
    #[inline]
    pub const fn new(db: D) -> Self {
        Self { db }
    }

    /// Returns the wrapped database.
    #[inline]
    pub const fn inner(&self) -> &D {
        &self.db
    }

    /// Returns the wrapped database mutably.
    #[inline]
    pub const fn inner_mut(&mut self) -> &mut D {
        &mut self.db
    }

    /// Consumes the adapter and returns the wrapped database.
    #[inline]
    pub fn into_inner(self) -> D {
        self.db
    }

    #[inline]
    fn database_result<T>(&mut self, result: AsyncResult<T, D::Error>) -> DbResult<T> {
        result.map_err(|error| match error {
            AsyncError::Inner(error) => {
                let fatal = D::is_fatal(&error);
                DatabaseError::new(error, fatal)
            }
            error => DatabaseError::new(error, true),
        })
    }
}

impl<D: AsyncDatabase> DynDatabase for AsyncDb<D> {
    #[inline]
    fn get_account(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        let result = {
            let Self { db, .. } = self;
            block_on_current_result(db.get_account(*address))
        };
        self.database_result(result)
    }

    #[inline]
    fn get_code_by_hash(&mut self, code_hash: &B256) -> DbResult<Bytecode> {
        let result = {
            let Self { db, .. } = self;
            block_on_current_result(db.get_code_by_hash(*code_hash))
        };
        self.database_result(result)
    }

    #[inline]
    fn get_storage(&mut self, address: &Address, key: &Word) -> DbResult<Word> {
        let result = {
            let Self { db, .. } = self;
            block_on_current_result(db.get_storage(*address, *key))
        };
        self.database_result(result)
    }

    #[inline]
    fn get_block_hash(&mut self, number: &Word) -> DbResult<B256> {
        let result = {
            let Self { db, .. } = self;
            block_on_current_result(db.get_block_hash(*number))
        };
        self.database_result(result)
    }
}

impl<D: AsyncDatabase + fmt::Debug> fmt::Debug for AsyncDb<D> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncDb").field("db", &self.db).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmTypes, Evm, ExecutionConfig, Inspector, PrecompileError, Precompiles, SpecId,
        Version,
        bytecode::Bytecode,
        constants::CALL_DEPTH_LIMIT,
        env::BlockEnvExt,
        evm::{AccountInfo, Database, Db, DynDatabase, InMemoryDB, PrecompileProvider, SystemTx},
        interpreter::{GasTracker, InstrStop, Interpreter, Message, MessageResult, Word, op},
        precompile::PrecompileOutput,
        precompiles::PrecompileId,
        registry::{HandlerError, TxRegistry, handler},
        test_utils::{handle_test_tx, test_tx},
    };
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_primitives::{Address, B256, Bytes, TxKind, bytes};
    use core::{
        assert_matches, convert::Infallible, fmt, future::Future, pin::Pin, ptr::NonNull,
        task::Poll,
    };
    use corosensei::stack::Stack;
    use std::{
        error::Error,
        rc::Rc,
        task::{Context, Waker},
    };

    #[test]
    fn block_on_requires_fiber() {
        assert_matches!(block_on_current(core::future::ready(())), Err(AsyncError::NotOnFiber));
    }

    #[test]
    fn fiber_suspends_and_resumes_pending_future() {
        let mut state = 1;
        let mut future = core::pin::pin!(on_fiber(|| {
            state += block_on_current(PendingOnce { pending: true }).unwrap();
            state
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert_matches!(future.as_mut().poll(&mut cx), Poll::Pending);
        assert_matches!(future.as_mut().poll(&mut cx), Poll::Ready(Ok(3)));
    }

    #[test]
    fn current_tls_is_cleared_while_fiber_is_pending() {
        let mut future = core::pin::pin!(on_fiber(|| {
            let _ = block_on_current(PendingForever);
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert_matches!(future.as_mut().poll(&mut cx), Poll::Pending);
        assert_matches!(block_on_current(core::future::ready(())), Err(AsyncError::NotOnFiber));
    }

    #[test]
    fn fiber_reuses_stack_slot() {
        let mut stack = super::FiberStack::default();
        let stack_ptr = NonNull::from(&mut stack);

        let first = poll_ready(unsafe {
            super::on_fiber_result_with_stack(stack_ptr, || Ok::<_, Infallible>(1))
        })
        .unwrap();
        let first_base = stack.stack.as_ref().unwrap().base();
        let second = poll_ready(unsafe {
            super::on_fiber_result_with_stack(stack_ptr, || Ok::<_, Infallible>(2))
        })
        .unwrap();
        let second_base = stack.stack.as_ref().unwrap().base();

        assert_eq!(first, 1);
        assert_eq!(second, 2);
        assert_eq!(first_base, second_base);
    }

    #[test]
    fn async_database_adapts_to_dyn_database() {
        let mut db = AsyncDb::new(TestDb);
        let address = Address::ZERO;
        let key = Word::from(7);
        let mut future = core::pin::pin!(on_fiber(|| {
            DynDatabase::get_storage(&mut db, &address, &key).unwrap()
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert_matches!(
            future.as_mut().poll(&mut cx),
            Poll::Ready(Ok(value)) if value == Word::from(9),
        );
    }

    #[test]
    fn async_database_suspends_until_ready() {
        let mut db = AsyncDb::new(PendingDb { pending: true });
        let address = Address::ZERO;
        let key = Word::from(7);
        let mut future = core::pin::pin!(on_fiber(|| {
            DynDatabase::get_storage(&mut db, &address, &key).unwrap()
        }));
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        assert_matches!(future.as_mut().poll(&mut cx), Poll::Pending);
        assert_matches!(
            future.as_mut().poll(&mut cx),
            Poll::Ready(Ok(value)) if value == Word::from(9),
        );
    }

    #[test]
    fn async_database_returns_owned_database_error() {
        let mut db = AsyncDb::new(FailingDb);
        let address = Address::ZERO;
        let key = Word::from(7);
        let code = on_fiber(|| DynDatabase::get_storage(&mut db, &address, &key).unwrap_err());
        let code = poll_ready(code).unwrap();

        assert_eq!(code.to_string(), "storage read failed");
        assert!(code.is_fatal());
        assert!(code.downcast_ref::<TestError>().is_some());
    }

    #[test]
    fn async_database_inner_futures_are_send() {
        let mut db = TestDb;

        drop(assert_send(db.get_account(Address::ZERO)));
        drop(assert_send(db.get_code_by_hash(B256::ZERO)));
        drop(assert_send(db.get_storage(Address::ZERO, Word::ZERO)));
        drop(assert_send(db.get_block_hash(Word::ZERO)));
    }

    #[test]
    fn dispatches_transaction_async_by_typed_2718_type() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_test_tx(req)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = test_tx(41);

        let result = poll_ready(evm.transact_async(&tx)).unwrap();

        assert_eq!(result.result().tx_gas_used(), 42);
    }

    #[test]
    fn transaction_async_send_future_is_send_with_send_erased_fields() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_test_tx(req)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        let tx = test_tx(41);

        let result = poll_ready(assert_send(evm.transact_async_send(&tx))).unwrap();

        assert_eq!(result.result().tx_gas_used(), 42);
    }

    #[test]
    fn transaction_async_send_future_is_send_after_type_check() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_test_tx(req)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(SendInspector);
        evm.evm_is_send_with_inspector::<InMemoryDB, Precompiles<BaseEvmTypes>, SendInspector>();
        let tx = test_tx(41);

        let result = poll_ready(assert_send(evm.transact_async_send(&tx))).unwrap();

        assert_eq!(result.result().tx_gas_used(), 42);
    }

    #[test]
    #[should_panic = "async EVM execution requires EVM erased fields to be verified as Send with Evm::evm_is_send"]
    fn transaction_async_send_panics_with_non_send_erased_fields() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = test_tx(41);

        drop(evm.transact_async_send(&tx));
    }

    #[test]
    #[should_panic = "async EVM execution requires EVM erased fields to be verified as Send with Evm::evm_is_send"]
    fn transaction_async_send_panics_after_non_send_setter() {
        let marker = Rc::new(());
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_test_tx(req)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        evm.set_inspector(NonSendInspector { marker });
        let tx = test_tx(41);

        drop(evm.transact_async_send(&tx));
    }

    #[test]
    #[should_panic = "async EVM execution requires EVM erased fields to be verified as Send with Evm::evm_is_send"]
    fn transaction_async_send_panics_after_state_mut_replaces_database() {
        let marker = Rc::new(());
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        evm.state_mut().set_initial(Db::new(NonSendDb { marker }));
        let tx = test_tx(41);

        drop(evm.transact_async_send(&tx));
    }

    #[test]
    fn transaction_async_send_survives_state_mut_access() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_test_tx(req)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        evm.state_mut();
        evm.overlay_db_mut();
        let tx = test_tx(41);

        let result = poll_ready(assert_send(evm.transact_async_send(&tx))).unwrap();

        assert_eq!(result.result().tx_gas_used(), 42);
    }

    #[test]
    fn transaction_async_accepts_non_send_erased_fields() {
        let marker = Rc::new(());
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            crate::ethereum::TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_test_tx(req)),
        );
        let database = Db::new(NonSendDb { marker: Rc::clone(&marker) });
        let precompiles = NonSendPrecompiles { marker: Rc::clone(&marker) };
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            database,
            precompiles,
        );
        evm.set_inspector(NonSendInspector { marker });
        let tx = test_tx(41);

        let result = poll_ready(evm.transact_async(&tx)).unwrap();

        assert_eq!(result.result().tx_gas_used(), 42);
    }

    #[test]
    fn evm_accepts_non_send_erased_fields() {
        let marker = Rc::new(());
        let database = Db::new(NonSendDb { marker: Rc::clone(&marker) });
        let precompiles = NonSendPrecompiles { marker: Rc::clone(&marker) };
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            database,
            precompiles,
        );

        evm.set_inspector(NonSendInspector { marker });

        let address = Address::ZERO;
        let key = Word::from(7);
        let value = evm.database_mut().get_storage(&address, &key).unwrap();

        assert_eq!(value, Word::from(9));
    }

    #[test]
    fn transaction_async_flattens_handler_error() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = test_tx(41);

        let result = poll_ready(evm.transact_async(&tx));

        assert_matches!(
            result,
            Err(AsyncError::Inner(HandlerError::UnsupportedTransactionType(TEST_TX_TYPE)))
        );
    }

    #[test]
    fn synchronous_database_requires_fiber() {
        let mut db = AsyncDb::new(TestDb);
        let address = Address::ZERO;
        let key = Word::from(7);
        let code = DynDatabase::get_storage(&mut db, &address, &key).unwrap_err();

        assert_eq!(code.to_string(), "async host operation requires EVM async fiber execution");
        assert!(code.is_fatal());
    }

    #[test]
    fn system_call_async_to_missing_code_is_noop() {
        let contract = Address::repeat_byte(0x42);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );

        let result = poll_ready(evm.system_call_async(SystemTx::new(contract, Bytes::new())))
            .unwrap()
            .discard();

        assert!(result.is_success());
        assert_eq!(result.tx_gas_used(), 0);
    }

    #[test]
    fn system_call_async_send_future_is_send() {
        let contract = Address::repeat_byte(0x42);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();

        let result = poll_ready(assert_send(
            evm.system_call_async_send(SystemTx::new(contract, Bytes::new())),
        ))
        .unwrap()
        .discard();

        assert!(result.is_success());
        assert_eq!(result.tx_gas_used(), 0);
    }

    #[test]
    #[should_panic = "async EVM execution requires EVM erased fields to be verified as Send with Evm::evm_is_send"]
    fn system_call_async_send_panics_after_overlay_db_mut_replaces_database() {
        let marker = Rc::new(());
        let contract = Address::repeat_byte(0x42);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<InMemoryDB, Precompiles<BaseEvmTypes>>();
        evm.overlay_db_mut().db = Box::new(Db::new(NonSendDb { marker }));

        drop(evm.system_call_async_send(SystemTx::new(contract, Bytes::new())));
    }

    #[test]
    fn system_call_async_succeeds_after_database_error() {
        let contract = Address::repeat_byte(0x42);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            crate::ethereum::ethereum_tx_registry(SpecId::OSAKA),
            Db::new(FailOnceAccountDb { fail_next_account: true }),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = Recovered::new_unchecked(
            crate::ethereum::TxEnvelope::Legacy(TxLegacy {
                gas_limit: 100_000,
                ..TxLegacy::default()
            }),
            Address::ZERO,
        );

        assert_matches!(evm.transact(&tx), Err(HandlerError::Database(_)));

        let result = poll_ready(evm.system_call_async(SystemTx::new(contract, Bytes::new())))
            .unwrap()
            .discard();

        assert!(result.is_success());
    }

    #[test]
    fn dropping_transact_async_discards_nonce_after_cancelled_db_future() {
        let caller = Address::ZERO;
        let contract = Address::repeat_byte(0x42);
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0, op::SLOAD, op::STOP]));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            crate::ethereum::ethereum_tx_registry(SpecId::OSAKA),
            AsyncDb::new(CancellingDb { contract, code }),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.evm_is_send::<AsyncDb<CancellingDb>, Precompiles<BaseEvmTypes>>();
        let tx = Recovered::new_unchecked(
            crate::ethereum::TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(contract),
                gas_limit: 100_000,
                ..TxLegacy::default()
            }),
            caller,
        );
        {
            let mut future = core::pin::pin!(evm.transact_async(&tx));
            let waker = Waker::noop();
            let mut cx = Context::from_waker(waker);

            assert_matches!(future.as_mut().poll(&mut cx), Poll::Pending);
        }

        let nonce = evm.read_account_info(&caller).unwrap().map_or(0, |info| info.nonce);

        assert_eq!(nonce, 0);
    }

    #[test]
    fn dropping_fiber_cancels_blocked_future() {
        let mut saw_cancel = false;
        {
            let mut future = core::pin::pin!(on_fiber(|| {
                saw_cancel = matches!(block_on_current(PendingForever), Err(AsyncError::Cancelled));
            }));
            let waker = Waker::noop();
            let mut cx = Context::from_waker(waker);

            assert_matches!(future.as_mut().poll(&mut cx), Poll::Pending);
        }
        assert!(saw_cancel);
    }

    #[rstest::rstest]
    #[case(0)]
    #[case(7)]
    #[case(u16::MAX - CALL_DEPTH_LIMIT - 1)]
    fn transact_async_reaches_call_depth_limit(#[case] initial_call_depth: u16) {
        #[derive(Default)]
        struct DepthInspector {
            calls: Vec<u16>,
            writes: Vec<u16>,
            too_deep: usize,
        }

        impl Inspector<BaseEvmTypes> for DepthInspector {
            fn call(
                &mut self,
                _: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                self.calls.push(message.depth);
                None
            }

            fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                if interp.opcode() == op::SSTORE {
                    self.writes.push(interp.message().depth);
                }
            }

            fn call_end(
                &mut self,
                _: &mut Interpreter<'_, '_, BaseEvmTypes>,
                _: &Message<BaseEvmTypes>,
                result: &mut MessageResult<BaseEvmTypes>,
            ) {
                self.too_deep += usize::from(result.stop == InstrStop::CallTooDeep);
            }
        }

        let caller = Address::repeat_byte(0xaa);
        let target = Address::repeat_byte(0x11);
        // SLOAD 0, add 1, SSTORE 0, then CALL self with all but 1024 gas remaining.
        let code =
            Bytecode::new_legacy(bytes!("60005460010160005560006000600060006000306104005a03f100"));
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &caller,
            AccountInfo::default().with_balance(Word::from(10).pow(Word::from(18))),
        );
        database.insert_account_info(
            &target,
            AccountInfo { nonce: 1, ..AccountInfo::default().with_code(code) },
        );
        let mut version = Version::new(SpecId::FRONTIER);
        version.initial_call_depth = initial_call_depth;
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::FRONTIER, version),
            SpecId::FRONTIER,
            BlockEnvExt::default(),
            crate::ethereum::ethereum_tx_registry(SpecId::FRONTIER),
            database,
            Precompiles::base(SpecId::FRONTIER),
        );
        evm.set_inspector(DepthInspector::default());
        let tx = Recovered::new_unchecked(
            crate::ethereum::TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(target),
                gas_limit: 7_000_000,
                ..TxLegacy::default()
            }),
            caller,
        );

        let result = poll_ready(evm.transact_async(&tx)).unwrap().commit();

        assert!(result.is_success());
        assert_eq!(result.tx_gas_used(), 5_291_175);
        assert_eq!(
            evm.state_mut().storage_slot_untracked(&target, &Word::ZERO).unwrap(),
            Word::from(1025)
        );
        let inspector = evm.clear_inspector_as::<DepthInspector>().unwrap();
        assert_eq!(
            inspector.writes,
            (initial_call_depth..=initial_call_depth + 1024).collect::<Vec<_>>()
        );
        assert_eq!(
            inspector.calls,
            (initial_call_depth..=initial_call_depth + 1025).collect::<Vec<_>>()
        );
        assert_eq!(inspector.too_deep, 1);
    }

    const TEST_TX_TYPE: u8 = 0x00;

    fn poll_ready<F: Future>(future: F) -> F::Output {
        let mut future = core::pin::pin!(future);
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);

        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("future unexpectedly pending"),
        }
    }

    fn assert_send<F: Future + Send>(future: F) -> F {
        future
    }

    struct NonSendDb {
        marker: Rc<()>,
    }

    impl Database for NonSendDb {
        type Error = Infallible;

        fn get_account(
            &mut self,
            _address: &Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            let _ = Rc::strong_count(&self.marker);
            Ok(None)
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
            let _ = Rc::strong_count(&self.marker);
            Ok(Word::from(9))
        }

        fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    struct FailOnceAccountDb {
        fail_next_account: bool,
    }

    impl Database for FailOnceAccountDb {
        type Error = TestError;

        fn get_account(
            &mut self,
            _address: &Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            if self.fail_next_account {
                self.fail_next_account = false;
                return Err(TestError);
            }
            Ok(None)
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: &Address, _key: &Word) -> Result<Word, Self::Error> {
            Ok(Word::ZERO)
        }

        fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    struct NonSendPrecompiles {
        marker: Rc<()>,
    }

    impl PrecompileProvider<BaseEvmTypes> for NonSendPrecompiles {
        fn precompile_ids(&self) -> Vec<(Address, PrecompileId)> {
            Vec::new()
        }

        fn contains(&self, _address: &Address) -> bool {
            let _ = Rc::strong_count(&self.marker);
            false
        }

        fn move_precompiles(
            &mut self,
            moves: &[(Address, Address)],
        ) -> Result<(), crate::precompiles::MovePrecompileError> {
            <crate::evm::precompile::NoPrecompiles as PrecompileProvider<BaseEvmTypes>>::move_precompiles(
                &mut Default::default(),
                moves,
            )
        }

        fn execute(
            &mut self,
            _evm: &mut Evm<'_, BaseEvmTypes>,
            _message: &Message<BaseEvmTypes>,
            _gas: &mut GasTracker,
        ) -> Option<Result<PrecompileOutput, PrecompileError>> {
            None
        }
    }

    struct NonSendInspector {
        marker: Rc<()>,
    }

    impl crate::evm::Inspector<BaseEvmTypes> for NonSendInspector {
        fn step(&mut self, _interp: &mut crate::interpreter::Interpreter<'_, '_, BaseEvmTypes>) {
            let _ = Rc::strong_count(&self.marker);
        }
    }

    struct SendInspector;

    impl crate::evm::Inspector<BaseEvmTypes> for SendInspector {}

    struct PendingOnce {
        pending: bool,
    }

    impl Future for PendingOnce {
        type Output = i32;

        fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.pending {
                self.pending = false;
                Poll::Pending
            } else {
                Poll::Ready(2)
            }
        }
    }

    struct PendingForever;

    impl Future for PendingForever {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }

    struct TestDb;

    impl AsyncDatabase for TestDb {
        type Error = Infallible;

        async fn get_account(
            &mut self,
            _address: Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            Ok(None)
        }

        async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        async fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> Result<Word, Self::Error> {
            Ok(Word::from(9))
        }

        async fn get_block_hash(&mut self, _number: Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    struct PendingDb {
        pending: bool,
    }

    impl AsyncDatabase for PendingDb {
        type Error = Infallible;

        async fn get_account(
            &mut self,
            _address: Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            Ok(None)
        }

        async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        async fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> Result<Word, Self::Error> {
            PendingStorage { pending: &mut self.pending }.await;
            Ok(Word::from(9))
        }

        async fn get_block_hash(&mut self, _number: Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    struct CancellingDb {
        contract: Address,
        code: Bytecode,
    }

    impl AsyncDatabase for CancellingDb {
        type Error = Infallible;

        async fn get_account(
            &mut self,
            address: Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            if address == self.contract {
                return Ok(Some(crate::evm::AccountInfo::default().with_code(self.code.clone())));
            }
            Ok(Some(crate::evm::AccountInfo::default()))
        }

        async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(self.code.clone())
        }

        async fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> Result<Word, Self::Error> {
            PendingForever.await;
            Ok(Word::ZERO)
        }

        async fn get_block_hash(&mut self, _number: Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    struct PendingStorage<'a> {
        pending: &'a mut bool,
    }

    impl Future for PendingStorage<'_> {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
            if *self.pending {
                *self.pending = false;
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        }
    }

    struct FailingDb;

    impl AsyncDatabase for FailingDb {
        type Error = TestError;

        async fn get_account(
            &mut self,
            _address: Address,
        ) -> Result<Option<crate::evm::AccountInfo>, Self::Error> {
            Ok(None)
        }

        async fn get_code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        async fn get_storage(
            &mut self,
            _address: Address,
            _key: Word,
        ) -> Result<Word, Self::Error> {
            Err(TestError)
        }

        async fn get_block_hash(&mut self, _number: Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct TestError;

    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("storage read failed")
        }
    }

    impl Error for TestError {}
}
