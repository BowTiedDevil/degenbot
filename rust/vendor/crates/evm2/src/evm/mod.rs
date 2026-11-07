//! EVM execution host.
//!
//! This module exposes the [`Evm`] dispatcher, transaction result types, database adapters,
//! state-change streaming traits, and block-state accumulator used by the host.
//!
//! ## State output and transaction lifecycle
//!
//! Transaction execution separates execution output from state materialization. [`Evm::transact`]
//! validates and executes a transaction through the registered handler, finalizes transaction-level
//! effects, and returns an [`ExecutedTx`] handle. The handle owns no copied write-set; it keeps the
//! transaction's post-finalization writes in reusable scratch state until the caller chooses how to
//! resolve them.
//!
//! Resolve an [`ExecutedTx`] with one of these methods:
//!
//! - [`ExecutedTx::commit`] accepts the transaction into the accepted overlay;
//! - [`ExecutedTx::commit_to`] accepts it and records the same writes in a
//!   [`BlockStateAccumulator`];
//! - [`ExecutedTx::commit_with`] streams writes to a [`StateChangeSink`] and then accepts them;
//! - [`ExecutedTx::discard`] drops the writes and returns only the result;
//! - [`ExecutedTx::discard_with`] streams writes to a [`StateChangeSink`] and then drops them;
//! - [`ExecutedTx::detach`] moves the pending transaction overlay out as an owned
//!   [`TxResultWithState`] without accepting the writes.
//!
//! [`ExecutedTx::account_changes`] borrows the writes account by account before resolving.
//!
//! Dropping an unresolved [`ExecutedTx`] is equivalent to [`ExecutedTx::discard`], so transaction
//! scratch cannot leak into later execution.
//!
//! ## State layers
//!
//! The host state is split into three layers:
//!
//! 1. **Accepted overlay**: transaction-boundary state accepted by prior commits. It shadows the
//!    wrapped database and is visible to later transactions executed by the same [`Evm`].
//! 2. **Transaction scratch**: writes, warm-access state, transient storage, journal entries,
//!    touched and selfdestructed account flags, and logs for the currently executing transaction.
//!    It is cleared after `commit`, `commit_to`, `commit_with`, `discard`, `discard_with`, or
//!    `detach` while retaining capacity where possible.
//! 3. **Block accumulator**: optional block-level state output. It coalesces committed transaction
//!    writes and keeps block-boundary originals.
//!
//! The accepted overlay is for execution correctness between transactions. The block accumulator
//! is for final block output.
//!
//! ## Outcomes, logs, and detached state
//!
//! [`TxResult`] is the cheap result-only shape: status, gas used, output, stop reason, logs,
//! host error code, and extension data. Logs live in [`TxResult`] because logs are
//! execution output, not database state.
//!
//! [`PendingState`] is the owned transaction overlay, moved out by [`ExecutedTx::detach`]. Normal
//! serial block execution can build receipts from [`TxResult`] and stream state directly into a
//! [`BlockStateAccumulator`] without detaching a per-transaction [`PendingState`].
//!
//! ## Source and sink API
//!
//! [`StateChangeSource`] and [`StateChangeSink`] provide borrowed state-change streaming. Sources
//! include transaction scratch, [`PendingState`], and [`BlockStateAccumulator`]. Sinks include
//! [`BlockStateAccumulator`], [`CacheDB`], [`Tee`], and custom consumers such as trie updaters,
//! witnesses, execution caches, or test recorders.
//!
//! The common hot path can therefore stream the same transaction writes into multiple consumers
//! without cloning or materializing the write-set first.
//!
//! ## Error and status behavior
//!
//! - Successful execution returns an [`ExecutedTx`] whose result has `status = true`.
//! - EVM revert/halt can still return an [`ExecutedTx`]. The result records the failed
//!   status/stop/output, while transaction-level effects remain resolvable if finalization
//!   completed.
//! - Invalid transactions and handler errors return a handler error and clear transaction scratch;
//!   there is no [`ExecutedTx`] to resolve.
//! - Host errors during execution/finalization are recorded as compact error codes. The owning
//!   component can recover the full error from that code.
//!
//! ## Common flows
//!
//! ```text
//! eth_call / simulation: transact -> discard
//! serial block:          transact -> commit
//! block output:          transact -> commit_to -> BlockStateAccumulator
//! traced simulation:     transact -> discard_with -> Sink
//! parallel worker:       transact -> detach -> send owned TxResultWithState
//! ```
//!
//! Result-only execution:
//!
//! ```rust,ignore
//! let executed = evm.transact(&tx)?;
//! let outcome = executed.discard();
//! ```
//!
//! Serial block execution with coalesced block output:
//!
//! ```rust,ignore
//! let mut block_state = BlockStateAccumulator::new();
//!
//! for tx in block.transactions() {
//!     let executed = evm.transact(tx)?;
//!     receipt_builder.observe(executed.result());
//!     let outcome = executed.commit_to(&mut block_state);
//!     receipts.push(receipt_builder.finish(outcome));
//! }
//!
//! let storage_deltas = block_state.storage_sorted();
//! ```
//!
//! Detached materialized output:
//!
//! ```rust,ignore
//! let result: TxResultWithState<_> = evm.transact(&tx)?.detach();
//! ```

use self::{
    inspector::{Inspector, boxed_inspector},
    precompile::{PrecompileOutput, PrecompileProvider, boxed_precompile_provider},
};
use crate::{
    DatabaseError, EvmConfigSelector, EvmTypes, EvmTypesHost, ExecutionConfig, ExecutionError,
    HostError, LoadError, PrecompileError, PrecompileHalt, SpecId,
    bytecode::Bytecode,
    constants::EIP7708_TRANSFER_TOPIC,
    env::{BlockEnv, TxEnv},
    interpreter::{
        Gas, GasTracker, Host, InstrStop, Interpreter, InterpreterPool, Message, MessageKind,
        MessageResult, MessageResultExt, Word, create_preaccess_failure,
    },
    registry::{HandlerResult, TxRegistry},
    trustme,
    version::{EvmFeatures, GasId},
};
use alloc::{borrow::Cow, boxed::Box, sync::Arc, vec, vec::Vec};
use alloy_consensus::transaction::Recovered;
use alloy_eips::eip2718::Typed2718;
use alloy_primitives::{Address, B256, Bytes, Log};
use core::ptr::NonNull;
#[cfg(feature = "async")]
use core::{any::TypeId, future::Future};
use derive_where::derive_where;

#[cfg(feature = "async")]
pub mod r#async;
pub mod config;
pub mod env;
pub mod handler;
pub mod inspector;
pub mod precompile;
pub mod registry;
mod system;
pub use system::{
    BEACON_ROOTS_ADDRESS, BUILDER_DEPOSIT_REQUEST_ADDRESS, BUILDER_EXIT_REQUEST_ADDRESS,
    CONSOLIDATION_REQUEST_ADDRESS, HISTORY_STORAGE_ADDRESS, SYSTEM_ADDRESS, SYSTEM_CALL_GAS_LIMIT,
    SYSTEM_MAX_SSTORES_PER_CALL, SystemTx, WITHDRAWAL_REQUEST_ADDRESS,
};

mod any;
pub use any::NonStaticAny;

pub mod bal;
pub use bal::{
    AccountBal, AccountInfoBal, Bal, BalAccountLookup, BalChange, BalChanges, BalCodeChange,
    BalContext, BalError, BlockAccessIndex, StorageBal,
};

mod db;
use db::boxed_dyn_database;
pub use db::{
    AccountStorageCache, Cache, CacheDB, Database, DatabaseRef, Db, DbRef, DbResult, DbStats,
    DbStatsCounts, DynDatabase, DynDatabaseRef, EmptyDB, InMemoryDB,
};

mod tx;
pub use tx::{ExecutedTx, TxResult, TxResultExt, TxResultWithState};

mod state;
#[cfg(feature = "account-ext")]
pub use state::AccountExtension;
pub use state::{
    AccountChangeRef, AccountChanges, AccountFlags, AccountHandle, AccountInfo,
    BlockStateAccumulator, JournalEntry, NoopChangeSink, PendingState, State, StateChangeSink,
    StateChangeSource, StateCheckpoint, StateInner, StateSnapshot, StorageChange, StorageHandle,
    StorageOverlay, StorageSlot, StorageSlotHandle, Tee, Tracked, TxStateChanges,
};

pub(crate) mod prewarm_set;
pub use prewarm_set::PrewarmSet;

/// Optional external interpreter runner.
///
/// Returning `Some(stop)` means the runner executed the frame. Returning `None` makes the EVM run
/// the regular interpreter for the same frame.
pub trait InterpreterRunner<T: EvmTypesHost>: core::fmt::Debug + Send + Sync + 'static {
    /// Attempts to execute `interpreter` with an external backend.
    ///
    /// Record execution errors with [`Interpreter::fail`]; the EVM extracts them after this
    /// method returns.
    ///
    /// The EVM calls this only while [`Self::supports`] holds for the active execution config;
    /// other callers must check it first.
    fn run<'frame, 'host>(
        &self,
        interpreter: &mut Interpreter<'frame, 'host, T>,
        host: &mut T::Host<'host>,
    ) -> Option<InstrStop>;

    /// Returns whether this runner can execute frames under `config`.
    ///
    /// The EVM calls this when the runner or the execution config changes, not per frame, and
    /// skips the runner while it returns `false`.
    fn supports(&self, _config: &ExecutionConfig<T>) -> bool {
        true
    }
}

/// EVM host and transaction dispatcher.
#[derive_where(Debug)]
pub struct Evm<'a, T: EvmTypesHost> {
    #[derive_where(skip)]
    spec_id: T::SpecId,
    #[derive_where(skip)]
    execution_config: ExecutionConfig<T>,
    features: EvmFeatures,
    pub(crate) block: BlockEnv<T>,
    tx_env: TxEnv<T>,
    registry: TxRegistry<T, TxResult<T>>,
    #[derive_where(skip)]
    pub(crate) state: State<'a>,
    #[derive_where(skip)]
    ext: T::EvmExt,
    #[derive_where(skip)]
    precompiles: Box<dyn PrecompileProvider<T> + 'a>,
    #[derive_where(skip)]
    interpreter_pool: InterpreterPool<T>,
    #[derive_where(skip)]
    inspector: Option<InspectorSlot<'a, T>>,
    inspect_system_calls: bool,
    #[derive_where(skip)]
    interpreter_runner: Option<Arc<dyn InterpreterRunner<T>>>,
    /// Whether `interpreter_runner` is set and supports `execution_config`.
    ///
    /// Refreshed whenever either changes.
    #[derive_where(skip)]
    runner_supported: bool,
    /// The currently running interpreter frame, if any.
    ///
    /// This is passed to the inspector call and create hooks as the parent frame.
    #[derive_where(skip)]
    current_frame: Option<NonNull<Interpreter<'static, 'static, T>>>,
    /// The checkpoint the currently running interpreter frame rolls back to if it fails.
    #[derive_where(skip)]
    frame_checkpoint: Option<StateCheckpoint>,
    #[derive_where(skip)]
    running: bool,
    #[cfg(feature = "async")]
    #[derive_where(skip)]
    async_stack: r#async::FiberStack,
    /// The erased value types last verified as `Send` by [`Evm::evm_is_send`] or
    /// [`Evm::evm_is_send_with_inspector`].
    #[cfg(feature = "async")]
    #[derive_where(skip)]
    send_types: Option<ErasedTypeIds>,
}

impl<'a, T: EvmTypes> Evm<'a, T> {
    /// Creates an EVM for `spec_id` with the provided transaction registry, database, and
    /// precompile provider.
    #[inline]
    pub fn new(
        spec_id: T::SpecId,
        block: BlockEnv<T>,
        registry: TxRegistry<T, TxResult<T>>,
        database: impl DynDatabase + 'a,
        precompiles: impl PrecompileProvider<T> + 'a,
    ) -> Self
    where
        T::EvmExt: Default,
    {
        Self::new_with_ext(spec_id, block, registry, database, precompiles, T::EvmExt::default())
    }

    /// Creates an EVM with explicit instance-specific extension state.
    #[inline]
    pub fn new_with_ext(
        spec_id: T::SpecId,
        block: BlockEnv<T>,
        registry: TxRegistry<T, TxResult<T>>,
        database: impl DynDatabase + 'a,
        precompiles: impl PrecompileProvider<T> + 'a,
        ext: T::EvmExt,
    ) -> Self {
        Self::new_with_execution_config_and_ext(
            <T::ConfigSelector as EvmConfigSelector<T>>::execution_config(spec_id),
            spec_id,
            block,
            registry,
            database,
            precompiles,
            ext,
        )
    }

    /// Creates an EVM with the provided transaction registry, database, and precompile provider.
    #[inline]
    pub fn new_with_execution_config(
        execution_config: ExecutionConfig<T>,
        spec_id: T::SpecId,
        block: BlockEnv<T>,
        registry: TxRegistry<T, TxResult<T>>,
        database: impl DynDatabase + 'a,
        precompiles: impl PrecompileProvider<T> + 'a,
    ) -> Self
    where
        T::EvmExt: Default,
    {
        Self::new_with_execution_config_and_ext(
            execution_config,
            spec_id,
            block,
            registry,
            database,
            precompiles,
            T::EvmExt::default(),
        )
    }

    /// Creates an EVM with an execution config and explicit instance-specific extension state.
    #[inline]
    pub fn new_with_execution_config_and_ext(
        execution_config: ExecutionConfig<T>,
        spec_id: T::SpecId,
        block: BlockEnv<T>,
        registry: TxRegistry<T, TxResult<T>>,
        database: impl DynDatabase + 'a,
        precompiles: impl PrecompileProvider<T> + 'a,
        ext: T::EvmExt,
    ) -> Self {
        Self::new_mono(
            execution_config,
            spec_id,
            block,
            registry,
            boxed_dyn_database(database),
            boxed_precompile_provider(precompiles),
            ext,
        )
    }

    #[inline]
    fn new_mono(
        execution_config: ExecutionConfig<T>,
        spec_id: T::SpecId,
        block: BlockEnv<T>,
        registry: TxRegistry<T, TxResult<T>>,
        database: Box<dyn DynDatabase + 'a>,
        precompiles: Box<dyn PrecompileProvider<T> + 'a>,
        ext: T::EvmExt,
    ) -> Self {
        assert_eq!(
            spec_id.into(),
            execution_config.base_spec_id(),
            "execution config spec mismatch"
        );
        Self {
            spec_id,
            features: execution_config.version().features,
            execution_config,
            block,
            tx_env: TxEnv::<T>::default(),
            registry,
            state: State::new_mono(database),
            ext,
            precompiles,
            interpreter_pool: InterpreterPool::new(),
            inspector: None,
            inspect_system_calls: false,
            interpreter_runner: None,
            runner_supported: false,
            current_frame: None,
            frame_checkpoint: None,
            running: false,
            #[cfg(feature = "async")]
            async_stack: r#async::FiberStack::default(),
            #[cfg(feature = "async")]
            send_types: None,
        }
    }

    #[inline]
    fn contains_precompile(&self, message: &Message<T>) -> bool {
        // Native dispatch takes precedence over code stored at the original callee.
        self.precompiles.contains(&message.call_target)
    }

    #[inline]
    fn execute_precompile(
        &mut self,
        message: &Message<T>,
        gas: &mut GasTracker,
    ) -> Result<PrecompileOutput, PrecompileError> {
        let guard = self.enter_execution();
        let precompiles = guard.evm.precompiles.as_mut() as *mut dyn PrecompileProvider<T>;
        let evm_ptr = guard.evm as *mut Self;
        // SAFETY: Precompile execution may need access to both the provider and the host EVM.
        // The provider is not moved or replaced during this call, and `execute` is expected to
        // preserve `Evm` invariants while using the host reference.
        unsafe {
            (&mut *precompiles)
                .execute(&mut *evm_ptr, message, gas)
                .expect("precompile was checked before execution")
        }
    }

    #[inline]
    fn assert_precompiles_mutable(&self) {
        assert!(!self.running, "precompile provider cannot be modified during EVM execution");
    }

    #[inline]
    fn assert_precompiles_downcast_mutable(&self) {
        self.assert_precompiles_mutable();
    }

    #[inline]
    fn assert_inspector_mutable(&self) {
        assert!(!self.running, "inspector cannot be modified during EVM execution");
    }

    #[inline]
    fn assert_interpreter_runner_mutable(&self) {
        assert!(!self.running, "interpreter runner cannot be modified during EVM execution");
    }

    #[inline]
    fn assert_execution_config_mutable(&self) {
        assert!(!self.running, "execution config cannot be modified during EVM execution");
    }

    #[inline]
    fn refresh_runner_support(&mut self) {
        self.runner_supported = self
            .interpreter_runner
            .as_ref()
            .is_some_and(|runner| runner.supports(&self.execution_config));
    }

    #[inline]
    const fn enter_execution(&mut self) -> ExecutionGuard<'_, 'a, T> {
        let was_running = self.running;
        self.running = true;
        ExecutionGuard { evm: self, was_running }
    }

    /// Returns the transaction handler registry.
    #[inline]
    pub const fn registry(&self) -> &TxRegistry<T, TxResult<T>> {
        &self.registry
    }

    /// Replaces the EVM instance-specific extension state, preserving the rest of the EVM.
    ///
    /// The extension type is fixed by [`EvmTypesHost::EvmExt`].
    #[inline]
    pub fn with_ext(self, ext: T::EvmExt) -> Self {
        self.map_ext(|_| ext)
    }

    /// Transforms the EVM instance-specific extension state, preserving the rest of the EVM.
    ///
    /// The extension type is fixed by [`EvmTypesHost::EvmExt`].
    #[inline]
    pub fn map_ext(self, f: impl FnOnce(T::EvmExt) -> T::EvmExt) -> Self {
        Self { ext: f(self.ext), ..self }
    }

    /// Returns the EVM instance-specific extension state.
    #[inline]
    pub const fn ext(&self) -> &T::EvmExt {
        &self.ext
    }

    /// Returns the EVM instance-specific extension state mutably.
    #[inline]
    pub const fn ext_mut(&mut self) -> &mut T::EvmExt {
        &mut self.ext
    }

    /// Returns the active block environment.
    #[inline]
    pub const fn block(&self) -> &BlockEnv<T> {
        &self.block
    }

    /// Replaces the active block environment, including during execution.
    ///
    /// This changes only the block fields visible to execution. The [`State`] object is kept in
    /// place: its accepted account and storage overlay, backing database, cached block hashes,
    /// attached BAL, BAL builder, and current BAL index all remain unchanged. Transaction
    /// scratch is also left untouched.
    ///
    /// A live replacement is intentional context mutation for testing and simulation hosts. The
    /// current frame and nested frames observe the new environment on subsequent host reads, so
    /// callers can implement controls such as changing the block number or timestamp during a
    /// call. This does not change the active execution specification or its gas rules; use
    /// [`Self::set_execution_config`] only at a frame boundary for a fork transition.
    ///
    /// For normal block execution, call this at a segment boundary so a transaction observes one
    /// coherent block context. The caller is responsible for the resulting semantics when it is
    /// called from an inspector or precompile.
    #[inline]
    pub const fn set_block(&mut self, block: BlockEnv<T>) {
        self.block = block;
    }

    /// Returns the active transaction environment.
    ///
    /// Transaction handlers and system calls replace it before executing their initial frame, and
    /// it keeps the last transaction's values afterwards.
    #[inline]
    pub const fn tx_env(&self) -> &TxEnv<T> {
        &self.tx_env
    }

    /// Returns the active transaction environment mutably, including during execution.
    ///
    /// The current frame and nested frames observe changes on subsequent reads, such as `ORIGIN`
    /// or `GASPRICE`. Validation, fees, and nonces do not read it, so an inspector can override,
    /// for example, the origin of a running transaction from its top-level call or create hook
    /// without affecting how the transaction is charged. Handlers overwrite it when the next
    /// transaction starts executing.
    #[inline]
    pub const fn tx_env_mut(&mut self) -> &mut TxEnv<T> {
        &mut self.tx_env
    }

    /// Replaces the active execution specification and its fork-dependent dispatch state.
    ///
    /// `execution_config` must have the same base specification as `spec_id`. The transaction
    /// registry and precompile provider are supplied together with the new config so a hardfork
    /// transition cannot silently retain handlers or precompiles from the previous fork.
    ///
    /// The existing [`State`] is preserved in place. In particular, the accepted account and
    /// storage overlay, backing database, cached block hashes, attached BAL, BAL builder, and
    /// current BAL index are not recreated or cleared. The derived feature set is refreshed from
    /// `execution_config.version()`.
    ///
    /// # Panics
    ///
    /// Panics if an interpreter frame is currently executing or if `spec_id` does not match
    /// `execution_config`.
    #[inline]
    pub fn set_execution_config(
        &mut self,
        execution_config: ExecutionConfig<T>,
        spec_id: T::SpecId,
        registry: TxRegistry<T, TxResult<T>>,
        precompiles: impl PrecompileProvider<T> + 'a,
    ) {
        self.assert_execution_config_mutable();
        assert_eq!(
            spec_id.into(),
            execution_config.base_spec_id(),
            "execution config spec mismatch"
        );
        let precompiles = boxed_precompile_provider(precompiles);
        self.replace_execution_config(execution_config, spec_id, registry, precompiles);
    }

    /// Replaces the block environment and execution specification as one segment-boundary update.
    ///
    /// This is equivalent to [`Self::set_block`] and [`Self::set_execution_config`] performed as
    /// one atomic update. The state, database/cache, block-hash cache, and BAL context are kept in
    /// place; only the supplied block/config/registry/precompile fields are replaced.
    ///
    /// # Panics
    ///
    /// Panics if an interpreter frame is currently executing or if `spec_id` does not match
    /// `execution_config`.
    #[inline]
    pub fn set_block_and_execution_config(
        &mut self,
        block: BlockEnv<T>,
        execution_config: ExecutionConfig<T>,
        spec_id: T::SpecId,
        registry: TxRegistry<T, TxResult<T>>,
        precompiles: impl PrecompileProvider<T> + 'a,
    ) {
        self.assert_execution_config_mutable();
        assert_eq!(
            spec_id.into(),
            execution_config.base_spec_id(),
            "execution config spec mismatch"
        );
        let precompiles = boxed_precompile_provider(precompiles);
        self.set_block(block);
        self.replace_execution_config(execution_config, spec_id, registry, precompiles);
    }

    /// Replaces the block environment and execution configuration within the active
    /// specification.
    ///
    /// Unlike [`Self::set_block_and_execution_config`], this keeps the transaction registry and
    /// precompile provider, which depend only on the specification, so provider state such as a
    /// precompile result cache survives. Use it at a segment boundary that stays on one fork but
    /// changes runtime values in the [`Version`](crate::Version), such as blob or gas caps. Only
    /// the base specification is checked; the caller must also keep the selector-specific one.
    ///
    /// # Panics
    ///
    /// Panics if an interpreter frame is currently executing or if `execution_config` has a
    /// different base specification than the active one.
    #[inline]
    pub fn set_block_and_execution_config_within_spec(
        &mut self,
        block: BlockEnv<T>,
        execution_config: ExecutionConfig<T>,
    ) {
        self.assert_execution_config_mutable();
        assert_eq!(
            self.spec_id(),
            execution_config.base_spec_id(),
            "execution config spec mismatch"
        );
        self.set_block(block);
        self.features = execution_config.version().features;
        self.execution_config = execution_config;
        self.refresh_runner_support();
    }

    #[inline]
    fn replace_execution_config(
        &mut self,
        execution_config: ExecutionConfig<T>,
        spec_id: T::SpecId,
        registry: TxRegistry<T, TxResult<T>>,
        precompiles: Box<dyn PrecompileProvider<T> + 'a>,
    ) {
        self.spec_id = spec_id;
        self.features = execution_config.version().features;
        self.execution_config = execution_config;
        // Refresh before the old registry and precompiles drop, so an unwinding destructor
        // cannot leave a stale verdict.
        self.refresh_runner_support();
        self.registry = registry;
        self.precompiles = precompiles;
    }

    /// Returns the accepted-state overlay database.
    ///
    /// This cache contains state changes committed through transaction lifecycle methods or
    /// [`Self::commit_source`]. The wrapped backing database is available through
    /// [`Self::database`].
    #[inline]
    pub fn overlay_db(&self) -> &CacheDB<Box<dyn DynDatabase + 'a>> {
        self.state.overlay_db()
    }

    /// Returns the accepted-state overlay database mutably.
    ///
    /// This is useful when an external state-change source should be streamed into the accepted
    /// overlay with a [`Tee`] or another [`StateChangeSink`]. The wrapped backing database is
    /// available through [`Self::database_mut`].
    #[inline]
    pub fn overlay_db_mut(&mut self) -> &mut CacheDB<Box<dyn DynDatabase + 'a>> {
        self.state.overlay_db_mut()
    }

    /// Returns the backing database.
    #[inline]
    pub fn database(&self) -> &(dyn DynDatabase + 'a) {
        self.state.initial()
    }

    /// Returns the backing database mutably.
    #[inline]
    pub fn database_mut(&mut self) -> &mut (dyn DynDatabase + 'a) {
        self.state.initial_mut()
    }

    /// Returns account information visible through the accepted state overlay.
    #[inline]
    pub fn read_account_info(&mut self, address: &Address) -> DbResult<Option<AccountInfo>> {
        self.state.account_info_untracked(address)
    }

    /// Applies borrowed changes to the accepted state overlay.
    #[inline]
    pub fn commit_source<S: StateChangeSource>(&mut self, source: &S) {
        self.state.commit_source(source);
    }

    /// Replaces the backing database.
    #[inline]
    pub fn set_database(&mut self, database: impl DynDatabase + 'a) {
        self.state.set_initial(database);
    }

    #[cfg(feature = "async")]
    #[inline]
    fn async_stack(&mut self) -> NonNull<r#async::FiberStack> {
        NonNull::from(&mut self.async_stack)
    }

    #[cfg(feature = "async")]
    #[inline]
    fn assert_erased_send(&self) {
        assert!(
            self.send_types == Some(self.erased_type_ids()),
            "async EVM execution requires EVM erased fields to be verified as Send with \
             Evm::evm_is_send"
        );
        assert!(
            !matches!(self.inspector, Some(InspectorSlot::Borrowed(_))),
            "async EVM execution cannot run with a borrowed inspector"
        );
    }

    #[cfg(feature = "async")]
    #[inline]
    fn erased_type_ids(&self) -> ErasedTypeIds {
        ErasedTypeIds {
            database: self.database().type_id(),
            precompiles: self.precompiles().type_id(),
            inspector: self.inspector().map(|inspector| inspector.type_id()),
        }
    }

    /// Marks this EVM as thread-sendable after checking the current erased field types.
    ///
    /// This requires no active inspector. Use [`Self::evm_is_send_with_inspector`] when an
    /// inspector is installed.
    ///
    /// Async execution re-checks that the erased types are still the verified ones, so replacing
    /// one with a different type makes it panic until the new types are verified.
    #[cfg(feature = "async")]
    #[inline]
    pub fn evm_is_send<D, P>(&mut self) -> &mut Self
    where
        D: DynDatabase + Send + 'static,
        P: PrecompileProvider<T> + Send + 'static,
        'a: 'static,
    {
        self.assert_database_type::<D>();
        self.assert_precompiles_type::<P>();
        assert!(self.inspector.is_none(), "inspector type mismatch");
        self.send_types = Some(self.erased_type_ids());
        self
    }

    /// Marks this EVM as thread-sendable after checking the current erased field types.
    #[cfg(feature = "async")]
    #[inline]
    pub fn evm_is_send_with_inspector<D, P, I>(&mut self) -> &mut Self
    where
        D: DynDatabase + Send + 'static,
        P: PrecompileProvider<T> + Send + 'static,
        I: Inspector<T> + Send + 'static,
        'a: 'static,
    {
        self.assert_database_type::<D>();
        self.assert_precompiles_type::<P>();
        self.assert_inspector_type::<I>();
        self.send_types = Some(self.erased_type_ids());
        self
    }

    #[cfg(feature = "async")]
    #[inline]
    fn assert_database_type<D: DynDatabase + 'static>(&self)
    where
        'a: 'static,
    {
        assert_eq!(self.database().type_id(), typeid::of::<D>(), "database type mismatch");
    }

    #[cfg(feature = "async")]
    #[inline]
    fn assert_precompiles_type<P: PrecompileProvider<T> + 'static>(&self)
    where
        'a: 'static,
    {
        assert_eq!(
            self.precompiles().type_id(),
            typeid::of::<P>(),
            "precompile provider type mismatch"
        );
    }

    #[cfg(feature = "async")]
    #[inline]
    fn assert_inspector_type<I: Inspector<T> + 'static>(&self)
    where
        'a: 'static,
    {
        let inspector = self.inspector().expect("inspector type mismatch");
        assert_eq!(inspector.type_id(), typeid::of::<I>(), "inspector type mismatch");
    }

    /// Returns the backing database as `D` if it has that concrete type.
    #[inline]
    pub fn database_as<D: DynDatabase + 'static>(&self) -> Option<&D>
    where
        'a: 'static,
    {
        self.database().downcast_ref()
    }

    /// Returns the backing database mutably as `D` if it has that concrete type.
    #[inline]
    pub fn database_as_mut<D: DynDatabase + 'static>(&mut self) -> Option<&mut D>
    where
        'a: 'static,
    {
        self.database_mut().downcast_mut()
    }

    /// Consumes the EVM and returns the current backing database, dropping all execution state.
    ///
    /// This is the consuming counterpart to [`Self::database`]. Unlike
    /// [`Self::database_as_mut`] it needs neither a `'static` bound nor a downcast, so it also
    /// works for databases that borrow. The in-memory layers — accepted state, the journal, and
    /// transaction scratch — are never written through to the database, so nothing the EVM
    /// executed reaches it. The database itself is returned as-is and may differ from the one
    /// passed to the constructor: it can be replaced through [`Self::set_database`], mutated
    /// through [`Self::database_mut`], or changed by its own reads.
    #[inline]
    pub fn into_db(self) -> Box<dyn DynDatabase + 'a> {
        self.state.into_db()
    }

    /// Returns the checkpoint the currently running interpreter frame rolls back to if it fails,
    /// or `None` outside a frame.
    ///
    /// For a create frame, the checkpoint follows the creator's nonce bump and precedes the
    /// account creation and endowment transfer.
    #[inline]
    pub const fn frame_checkpoint(&self) -> Option<&StateCheckpoint> {
        self.frame_checkpoint.as_ref()
    }

    /// Returns the mutable EVM state.
    #[inline]
    pub const fn state(&self) -> &State<'a> {
        &self.state
    }

    /// Returns the mutable EVM state.
    #[inline]
    pub const fn state_mut(&mut self) -> &mut State<'a> {
        &mut self.state
    }

    /// Returns logs emitted by the current in-flight transaction.
    #[inline]
    pub fn logs(&self) -> &[Log] {
        self.state.logs()
    }

    /// Returns the logs emitted by the current in-flight transaction mutably.
    #[inline]
    pub const fn logs_mut(&mut self) -> &mut Vec<Log> {
        self.state.logs_mut()
    }

    /// Returns the precompile provider.
    #[inline]
    pub fn precompiles(&self) -> &(dyn PrecompileProvider<T> + 'a) {
        self.precompiles.as_ref()
    }

    /// Warms every precompile address in the prewarm set.
    ///
    /// Feeds the precompile addresses directly into the prewarm set. This relies on disjoint
    /// borrows of the `precompiles` and `state` fields, which a caller holding only `&mut Evm`
    /// cannot express through [`Self::precompiles`] (it borrows all of `self`).
    #[inline]
    pub fn warm_precompiles(&mut self) {
        self.precompiles.warm_addresses(self.state.prewarm_set_mut());
    }

    /// Returns the precompile provider mutably.
    #[inline]
    pub fn precompiles_mut(&mut self) -> &mut (dyn PrecompileProvider<T> + 'a) {
        self.assert_precompiles_mutable();
        self.precompiles.as_mut()
    }

    /// Replaces the precompile provider.
    #[inline]
    pub fn set_precompiles(&mut self, precompiles: impl PrecompileProvider<T> + 'a) {
        self.set_boxed_precompiles(boxed_precompile_provider(precompiles));
    }

    /// Replaces the precompile provider with an already boxed one.
    #[inline]
    pub fn set_boxed_precompiles(&mut self, precompiles: Box<dyn PrecompileProvider<T> + 'a>) {
        self.assert_precompiles_mutable();
        self.precompiles = precompiles;
    }

    /// Returns the precompile provider as `P` if it has that concrete type.
    #[inline]
    pub fn precompiles_as<P: PrecompileProvider<T> + 'static>(&self) -> Option<&P>
    where
        'a: 'static,
    {
        self.precompiles().downcast_ref()
    }

    /// Returns the precompile provider mutably as `P` if it has that concrete type.
    #[inline]
    pub fn precompiles_as_mut<P: PrecompileProvider<T> + 'static>(&mut self) -> Option<&mut P>
    where
        'a: 'static,
    {
        self.assert_precompiles_downcast_mutable();
        self.precompiles.as_mut().downcast_mut()
    }

    /// Returns the active execution inspector.
    ///
    /// A borrowed inspector installed by [`Self::transact_with_inspector`] is not returned.
    #[inline]
    pub fn inspector(&self) -> Option<&(dyn Inspector<T> + 'a)> {
        match &self.inspector {
            Some(InspectorSlot::Owned(inspector)) => Some(&**inspector),
            Some(InspectorSlot::Borrowed(_)) | None => None,
        }
    }

    /// Returns the active execution inspector mutably.
    ///
    /// A borrowed inspector installed by [`Self::transact_with_inspector`] is not returned.
    #[inline]
    pub fn inspector_mut(&mut self) -> Option<&mut (dyn Inspector<T> + 'a)> {
        self.assert_inspector_mutable();
        match &mut self.inspector {
            Some(InspectorSlot::Owned(inspector)) => Some(&mut **inspector),
            Some(InspectorSlot::Borrowed(_)) | None => None,
        }
    }

    #[inline]
    fn active_inspector(&mut self) -> Option<&mut (dyn Inspector<T> + 'a)> {
        self.inspector.as_mut().map(InspectorSlot::as_dyn_mut)
    }

    #[inline]
    fn inspect_log(&mut self, log: &Log) {
        let guard = self.enter_execution();
        if let Some(inspector) = guard.evm.active_inspector() {
            // SAFETY: The inspector is stored in `self`; the execution guard prevents inspector
            // replacement while the hook is running.
            let inspector = unsafe { trustme::decouple_lt_mut(inspector) };
            inspector.log(log, guard.evm);
        }
    }

    /// Returns whether emitted logs are observed: recorded in the transaction result
    /// ([`EvmFeatures::LOG_COLLECTION`]) or passed to an inspector.
    ///
    /// Precompiles and handlers that emit logs should check this before building them.
    #[inline]
    pub const fn logs_observed(&self) -> bool {
        self.feature(EvmFeatures::LOG_COLLECTION) || self.inspector.is_some()
    }

    /// Notifies the inspector and records an emitted log if [`EvmFeatures::LOG_COLLECTION`] is
    /// enabled.
    #[inline]
    pub fn log(&mut self, log: Log) {
        self.inspect_log(&log);
        if self.feature(EvmFeatures::LOG_COLLECTION) {
            self.state.log(log);
        }
    }

    #[inline]
    fn finish_executed_tx(
        &mut self,
        mut result: TxResult<T>,
    ) -> HandlerResult<ExecutedTx<'_, 'a, T>> {
        if let Err(error) = self.state.finalize_transaction(self.execution_config.version()) {
            self.state.clear_transaction_state();
            return Err(error.into());
        }
        result.logs = self.state.take_logs();
        Ok(ExecutedTx::from_result(self, result, true))
    }

    #[inline(never)]
    fn log_eip7708_transfer(&mut self, from: &Address, to: &Address, value: &Word) {
        if self.feature(EvmFeatures::EIP7708)
            && self.logs_observed()
            && let Some(log) = eip7708_transfer_log(from, to, value)
        {
            self.log(log);
        }
    }

    /// Sets the active execution inspector.
    #[inline]
    pub fn set_inspector<I: Inspector<T> + 'a>(&mut self, inspector: I) {
        self.set_boxed_inspector(boxed_inspector(inspector));
    }

    /// Sets the active boxed execution inspector.
    #[inline]
    pub fn set_boxed_inspector(&mut self, inspector: Box<dyn Inspector<T> + 'a>) {
        self.assert_inspector_mutable();
        self.inspector = Some(InspectorSlot::Owned(inspector));
    }

    /// Removes the active execution inspector.
    #[inline]
    pub fn clear_inspector(&mut self) -> Option<Box<dyn Inspector<T> + 'a>> {
        self.assert_inspector_mutable();
        match self.inspector.take()? {
            InspectorSlot::Owned(inspector) => Some(inspector),
            InspectorSlot::Borrowed(_) => unreachable!("borrowed inspector outlived its call"),
        }
    }

    /// Removes the active execution inspector if it has type `I`.
    #[inline]
    pub fn clear_inspector_as<I: Inspector<T> + 'static>(&mut self) -> Option<Box<I>>
    where
        'a: 'static,
    {
        self.assert_inspector_mutable();
        let slot = self.inspector.take_if(|slot| match slot {
            InspectorSlot::Owned(inspector) => inspector.is::<I>(),
            InspectorSlot::Borrowed(_) => false,
        })?;
        let InspectorSlot::Owned(i) = slot else { unreachable!("borrowed slot was filtered out") };
        Some(unsafe { Box::from_raw(Box::into_raw(i).cast::<I>()) })
    }

    /// Returns whether [`Self::system_call`] runs with the active inspector installed.
    #[inline]
    pub const fn inspects_system_calls(&self) -> bool {
        self.inspect_system_calls
    }

    /// Sets whether [`Self::system_call`] runs with the active inspector installed.
    ///
    /// Defaults to `false`: system calls are not inspected.
    #[inline]
    pub const fn set_inspect_system_calls(&mut self, inspect: bool) {
        self.inspect_system_calls = inspect;
    }

    /// Sets the optional external interpreter runner.
    #[inline]
    pub fn set_interpreter_runner<R: InterpreterRunner<T>>(&mut self, runner: R) {
        self.set_shared_interpreter_runner(Arc::new(runner));
    }

    /// Sets the optional shared external interpreter runner.
    #[inline]
    pub fn set_shared_interpreter_runner(&mut self, runner: Arc<dyn InterpreterRunner<T>>) {
        self.assert_interpreter_runner_mutable();
        self.interpreter_runner = Some(runner);
        self.refresh_runner_support();
    }

    /// Removes the optional external interpreter runner.
    #[inline]
    pub fn clear_interpreter_runner(&mut self) -> Option<Arc<dyn InterpreterRunner<T>>> {
        self.assert_interpreter_runner_mutable();
        let runner = self.interpreter_runner.take();
        self.refresh_runner_support();
        runner
    }

    /// Returns the active EVM version.
    #[inline]
    pub const fn version(&self) -> &crate::Version {
        self.execution_config.version()
    }

    /// Returns `true` if the active EVM feature set contains `feature`.
    #[inline]
    pub const fn feature(&self, feature: EvmFeatures) -> bool {
        self.features.contains(feature)
    }

    /// Returns the active base specification ID.
    #[inline]
    pub fn spec_id(&self) -> SpecId {
        self.spec_id.into()
    }

    /// Returns the selector-specific runtime specification ID.
    #[inline]
    pub const fn config_spec_id(&self) -> T::SpecId {
        self.spec_id
    }
}

/// The active inspector: owned by the EVM, or borrowed for one [`Evm::transact_with_inspector`]
/// call.
enum InspectorSlot<'a, T: EvmTypesHost> {
    Owned(Box<dyn Inspector<T> + 'a>),
    /// Lifetime-erased pointer to the inspector borrowed by [`ScopedInspector`], which removes it
    /// before the borrow ends.
    Borrowed(NonNull<dyn Inspector<T>>),
}

impl<'a, T: EvmTypesHost> InspectorSlot<'a, T> {
    #[inline]
    fn as_dyn_mut(&mut self) -> &mut (dyn Inspector<T> + 'a) {
        match self {
            Self::Owned(inspector) => &mut **inspector,
            // SAFETY: A borrowed pointer is only installed by `ScopedInspector`, which holds the
            // inspector's `&mut` borrow and removes the pointer before that borrow ends, so the
            // pointee is live and not otherwise borrowed for as long as the slot holds it.
            Self::Borrowed(inspector) => unsafe { inspector.as_mut() },
        }
    }
}

/// Installs a borrowed inspector in an [`Evm`] and restores the previous slot on drop.
struct ScopedInspector<'scope, 'a, T: EvmTypesHost> {
    evm: &'scope mut Evm<'a, T>,
    previous: Option<InspectorSlot<'a, T>>,
    completed: bool,
}

impl<'scope, 'a, T: EvmTypesHost> ScopedInspector<'scope, 'a, T> {
    #[inline]
    fn install(evm: &'scope mut Evm<'a, T>, inspector: &mut (dyn Inspector<T> + '_)) -> Self {
        // SAFETY: Only the object lifetime bound is erased. The slot cannot be replaced or read
        // out while the inspector is installed: `ScopedInspector` holds `&mut Evm`, so user code
        // only touches the EVM from handlers and hooks, which run with `running` set, where
        // every inspector setter and `inspector_mut` panic, while `inspector` and the other
        // readers skip a borrowed slot. `Drop` removes the pointer before this call returns or
        // unwinds, which ends the `inspector` borrow.
        let inspector = unsafe {
            core::mem::transmute::<NonNull<dyn Inspector<T> + '_>, NonNull<dyn Inspector<T>>>(
                NonNull::from(inspector),
            )
        };
        let previous = evm.inspector.replace(InspectorSlot::Borrowed(inspector));
        Self { evm, previous, completed: false }
    }
}

impl<T: EvmTypesHost> Drop for ScopedInspector<'_, '_, T> {
    #[inline]
    fn drop(&mut self) {
        self.evm.inspector = self.previous.take();
        // A panic unwinding out of a frame skips its cleanup, which would leave this pointing at
        // a dropped interpreter.
        self.evm.current_frame = None;
        if !self.completed {
            self.evm.state.clear_transaction_state();
        }
    }
}

struct ExecutionGuard<'guard, 'evm, T: EvmTypesHost> {
    evm: &'guard mut Evm<'evm, T>,
    was_running: bool,
}

impl<'guard, 'evm, T: EvmTypesHost> Drop for ExecutionGuard<'guard, 'evm, T> {
    #[inline]
    fn drop(&mut self) {
        self.evm.running = self.was_running;
    }
}

/// The concrete types of the erased database, precompile provider, and inspector.
#[cfg(feature = "async")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ErasedTypeIds {
    database: TypeId,
    precompiles: TypeId,
    inspector: Option<TypeId>,
}

#[cfg(feature = "async")]
struct SendEvmRef<'a, 'evm, T: EvmTypesHost> {
    evm: &'a mut Evm<'evm, T>,
}

#[cfg(feature = "async")]
// SAFETY: `SendEvmRef` is only constructed by async entrypoints for an `Evm<'static, T>` whose
// erased field types `Evm::evm_is_send` verified as `Send`. The `'static` bound keeps a verified
// type from being replaced by a shorter-lived instance of itself, which `TypeId` cannot tell apart.
unsafe impl<T> Send for SendEvmRef<'_, '_, T>
where
    T: EvmTypesHost,
    T::SpecId: Send,
    T::Tx: Send,
    T::EvmExt: Send,
    T::MessageExt: Send,
    T::MessageResultExt: Send,
    T::TxEnvExt: Send,
    T::TxResultExt: Send,
    T::BlockEnvExt: Send,
{
}

impl<'a, T: EvmTypes<Tx: Typed2718>> Evm<'a, T> {
    /// Dispatches the transaction to its handler and returns an executed transaction handle.
    ///
    /// The returned [`ExecutedTx`] keeps post-finalization writes in the transaction scratch layer.
    /// Callers must resolve it with [`ExecutedTx::commit`], [`ExecutedTx::commit_to`],
    /// [`ExecutedTx::commit_with`], [`ExecutedTx::discard`], [`ExecutedTx::discard_with`], or
    /// [`ExecutedTx::detach`] before
    /// another transaction can be executed. Dropping the handle is equivalent to
    /// [`ExecutedTx::discard`].
    ///
    /// Transaction handlers cannot modify the inspector, precompile provider, interpreter runner,
    /// or execution configuration while the transaction is being executed.
    pub fn transact(&mut self, tx: &Recovered<T::Tx>) -> HandlerResult<ExecutedTx<'_, 'a, T>> {
        let result = self.execute_tx(tx)?;
        self.finish_executed_tx(result)
    }

    /// Dispatches the transaction like [`Self::transact`], inspecting it with a borrowed
    /// inspector.
    ///
    /// The borrowed inspector replaces the installed inspector, if any, for this call only. The
    /// installed inspector is restored before this method returns, including when it unwinds, so
    /// the borrow of `inspector` ends with the call even while the returned [`ExecutedTx`] is
    /// alive. During the call, [`Self::inspector`] and [`Self::inspector_mut`] return `None`, and
    /// the inspector setters panic like they do during any execution. If the call unwinds, the
    /// transaction state is discarded. System calls are not inspected.
    pub fn transact_with_inspector(
        &mut self,
        tx: &Recovered<T::Tx>,
        inspector: &mut (dyn Inspector<T> + '_),
    ) -> HandlerResult<ExecutedTx<'_, 'a, T>> {
        self.assert_inspector_mutable();
        let mut scope = ScopedInspector::install(self, inspector);
        let result = scope.evm.execute_tx(tx);
        scope.completed = true;
        drop(scope);
        self.finish_executed_tx(result?)
    }

    #[inline]
    fn execute_tx(&mut self, tx: &Recovered<T::Tx>) -> HandlerResult<TxResult<T>> {
        let handler = self.registry.try_get_by_type(tx.ty())?;
        let guard = self.enter_execution();
        let result = handler.execute(tx.as_recovered_ref(), guard.evm);
        drop(guard);
        if result.is_err() {
            self.state.clear_transaction_state();
        }
        result
    }

    /// Executes a transaction for its outcome and discards its state changes.
    ///
    /// This is the cheapest convenience entrypoint for `eth_call`-style simulations: execution
    /// output and logs are returned, but transaction writes are not accepted and the transaction
    /// overlay is not detached.
    pub fn call_tx(&mut self, tx: &Recovered<T::Tx>) -> HandlerResult<TxResult<T>> {
        self.transact(tx).map(ExecutedTx::discard)
    }

    /// Validates a transaction through its registered handler without executing user calls.
    ///
    /// The handler may apply pre-execution writes such as nonce increments and fee deductions.
    /// Transaction state is discarded on both success and error, while accepted state and cached
    /// database reads are retained. Custom EVM extension state remains the handler's
    /// responsibility.
    ///
    /// Uses the active validation rules; callers must configure any pool-specific relaxations.
    pub fn validate_tx(&mut self, tx: &Recovered<T::Tx>) -> HandlerResult<()> {
        let result = self
            .registry
            .try_get_by_type(tx.ty())
            .and_then(|handler| handler.validate(tx.as_recovered_ref(), self));
        self.state.clear_transaction_state();
        result
    }

    /// Dispatches the transaction to the handler registered for its EIP-2718 type byte on an async
    /// fiber.
    ///
    /// This must be used with an async database adapter such as
    /// [`evm::async::AsyncDb`](crate::evm::async::AsyncDb) to take
    /// advantage of yielding database I/O. With a synchronous database this is mostly equivalent to
    /// running the synchronous transaction on a fiber.
    ///
    /// This returns a local future and does not require the erased database, precompile provider,
    /// or optional inspector to be `Send`. Use [`Self::transact_async_send`] when the returned
    /// future must be `Send`.
    #[cfg(feature = "async")]
    pub fn transact_async<'fut>(
        &'fut mut self,
        tx: &'fut Recovered<T::Tx>,
    ) -> impl Future<Output = r#async::AsyncResult<ExecutedTx<'fut, 'a, T>, registry::HandlerError>> + 'fut
    where
        T::Tx: Sync,
    {
        let stack = self.async_stack();
        // SAFETY: The returned future owns the exclusive `&mut self` borrow, so nothing else can
        // access the EVM stack slot until that future is dropped.
        unsafe { r#async::on_local_fiber_result_with_stack(stack, move || self.transact(tx)) }
    }

    /// Dispatches the transaction to the handler registered for its EIP-2718 type byte on an async
    /// fiber and returns a `Send` future.
    ///
    /// Before calling it, the current erased database, precompile provider, and optional inspector
    /// must be verified with [`Self::evm_is_send`] or [`Self::evm_is_send_with_inspector`].
    #[cfg(feature = "async")]
    pub fn transact_async_send<'fut>(
        &'fut mut self,
        tx: &'fut Recovered<T::Tx>,
    ) -> impl Future<Output = r#async::AsyncResult<ExecutedTx<'fut, 'a, T>, registry::HandlerError>>
    + Send
    + 'fut
    where
        T::Tx: Sync,
        'a: 'static,
    {
        self.assert_erased_send();
        let stack = self.async_stack();
        let evm = SendEvmRef { evm: self };
        // SAFETY: The returned future owns the exclusive `&mut self` borrow, so nothing else can
        // access the EVM stack slot until that future is dropped. The send marker checked above
        // requires all erased EVM fields to have been verified by `Evm::evm_is_send`.
        unsafe {
            r#async::on_fiber_result_with_stack(stack, move || {
                let SendEvmRef { evm } = evm;
                evm.transact(tx)
            })
        }
    }

    /// Dispatches each transaction to its registered EIP-2718 handler and commits it.
    ///
    /// Use [`Self::transact`] directly when the caller wants to choose between commit, discard,
    /// detach, and accumulator/sink commits for each transaction.
    pub fn transact_iter<'txs, I>(
        &'txs mut self,
        txs: I,
    ) -> impl Iterator<Item = HandlerResult<TxResult<T>>> + 'txs
    where
        I: IntoIterator<Item = &'txs Recovered<T::Tx>>,
        I::IntoIter: 'txs,
        T::Tx: 'txs,
        Self: 'txs,
    {
        txs.into_iter().map(move |tx| self.transact(tx).map(ExecutedTx::commit))
    }
}

impl<'a, T: EvmTypes> Evm<'a, T> {
    /// The input of a call message may borrow the caller's memory (see
    /// [`Interpreter::call_input`]), so this must not keep or clone out a borrowed input.
    #[inline]
    fn execute_message_impl(
        &mut self,
        message: &mut Message<T>,
    ) -> Result<MessageResult<T>, ExecutionError> {
        let mut result = match message.kind {
            MessageKind::Create | MessageKind::Create2 => self.execute_create_message(message),
            MessageKind::Call
            | MessageKind::CallCode
            | MessageKind::DelegateCall
            | MessageKind::StaticCall => self.execute_call_message(message),
        }?;
        // Settle the returning frame's gas for its stop reason at this single exit,
        // rather than in each result builder, so every consumer (parent
        // `merge_child_gas`, top-level accounting, inspectors) reads the settled gas.
        result.gas.settle_gas(result.stop);
        Ok(result)
    }

    /// Fires the inspector call/create hooks around message execution.
    ///
    /// This is invoked for every message when an inspector is installed; hook overrides skip
    /// execution entirely, including the call depth check.
    ///
    /// A `halted` result stands in for the execution: the hooks fire around it, and a hook
    /// override does not replace it.
    #[inline(never)]
    fn execute_message_inspected<'frame>(
        &mut self,
        message: &'frame mut Message<T>,
        halted: Option<MessageResult<T>>,
    ) -> Result<MessageResult<T>, ExecutionError> {
        let guard = self.enter_execution();
        let Some(inspector) = guard.evm.active_inspector() else {
            return match halted {
                Some(result) => Ok(result),
                None => guard.evm.execute_message_impl(message),
            };
        };
        // SAFETY: The inspector is stored in `self`; the execution guard prevents inspector
        // replacement while the hooks are running.
        let inspector = unsafe { trustme::decouple_lt_mut(inspector) };
        // The hooks can change the caller's memory and keep the message, so the input must not
        // borrow that memory.
        message.input.make_owned();

        // `destination` already holds the create's contract address (set when the message was
        // constructed), so the create hook observes it directly.
        let is_create = message.kind.is_create();

        // Keeps the top-level frame's bytecode alive when a hook replaces `message.code`.
        let top_code;
        let mut top_frame: Option<Box<Interpreter<'frame, 'a, T>>> = None;
        let frame = match guard.evm.current_frame {
            // SAFETY: The parent frame is suspended on this call stack for the duration of the
            // message execution.
            Some(mut frame) => unsafe {
                core::mem::transmute::<
                    &mut Interpreter<'static, 'static, T>,
                    &mut Interpreter<'frame, 'a, T>,
                >(frame.as_mut())
            },
            None => {
                // SAFETY: The message outlives the frame, which is returned to the pool below.
                let frame_message = unsafe { trustme::decouple_lt(&*message) };
                let frame = top_frame.insert(guard.evm.interpreter_pool.pop(frame_message));
                top_code = message.code.clone();
                // SAFETY: `top_code` owns a clone of the code, so the frame's pointers into it
                // stay valid even if a hook replaces `message.code`. It is declared before
                // `top_frame` and dropped after the frame is returned to the pool, which resets
                // its bytecode borrow.
                frame.set_bytecode(unsafe { trustme::decouple_lt(&top_code) });
                frame.prepare_run(guard.evm);
                frame
            }
        };
        // SAFETY: The frame outlives the hook invocations below.
        let frame = unsafe { trustme::decouple_lt_mut(frame) };

        let inspected = if is_create {
            inspector.create(frame, message)
        } else {
            inspector.call(frame, message)
        };

        let mut result = if let Some(error) = frame.take_error() {
            Err(error)
        } else if let Some(halted) = halted {
            Ok(halted)
        } else {
            inspected.map(Ok).unwrap_or_else(|| guard.evm.execute_message_impl(message))
        };

        if let Ok(result) = &mut result {
            if is_create {
                inspector.create_end(frame, message, result);
            } else {
                inspector.call_end(frame, message, result);
            }
        }

        if let Some(error) = frame.take_error() {
            result = Err(error);
        }

        if let Some(frame) = top_frame {
            let _ = guard.evm.interpreter_pool.push(frame);
        }

        result
    }

    #[inline(never)]
    fn execute_create_message(
        &mut self,
        message: &mut Message<T>,
    ) -> Result<MessageResult<T>, ExecutionError> {
        if let Err(error) = self.prepare_create_message(message) {
            return match error {
                HostError::Halt(stop) => {
                    Ok(Self::error_message_result(stop, message.gas_limit, message.reservoir))
                }
                HostError::Execution(error) => Err(error),
            };
        }
        let checkpoint = self.state.checkpoint();
        if let Err(error) = self.create_message_account(message) {
            self.state.rollback(checkpoint, self.features);
            return match error {
                HostError::Halt(stop) => {
                    Ok(Self::error_message_result(stop, message.gas_limit, message.reservoir))
                }
                HostError::Execution(error) => Err(error),
            };
        }
        // Empty initcode deploys an empty contract without executing the bytecode's padding STOP.
        let (stop, gas, output) = if message.code.is_empty() {
            (
                InstrStop::Stop,
                Gas::new_with_execution_gas_and_reservoir(message.gas_limit, message.reservoir),
                Bytes::new(),
            )
        } else {
            let input = core::mem::take(&mut message.input);
            let result = self.run_frame(message, checkpoint.clone());
            message.input = input;
            let stop = result?;
            let interp = self.interpreter_pool.last_mut().unwrap();
            (stop, interp.gas(), Bytes::copy_from_slice(interp.output()))
        };
        self.finish_create_message_run(checkpoint, &message.destination, stop, gas, output)
    }

    #[inline(never)]
    fn prepare_create_message(&mut self, message: &mut Message<T>) -> Result<(), HostError> {
        let nested = message.depth > self.version().initial_call_depth;
        // A too-deep create fails on depth alone, without reading the creator.
        let info =
            if message.depth <= self.version().max_call_depth() && (message.value > 0 || nested) {
                self.state.account_info_untracked(&message.caller)?
            } else {
                None
            };
        let (balance, nonce) = info.as_ref().map_or((Word::ZERO, 0), |i| (i.balance, i.nonce));
        if let Some(stop) =
            create_preaccess_failure(balance, nonce, message.value, message.depth, self.version())
        {
            return Err(stop.into());
        }

        // `destination` already holds the contract address (derived when the message was
        // constructed); warm it before running the initcode.
        self.state.account(&message.destination)?.warm();

        if nested && let Err(code) = self.state.account(&message.caller).map(|mut a| a.bump_nonce())
        {
            return Err(code.into());
        }

        Ok(())
    }

    #[inline(never)]
    fn create_message_account(&mut self, message: &Message<T>) -> Result<(), HostError> {
        self.state.create_account(
            &message.caller,
            &message.destination,
            &message.value,
            self.features,
        )??;

        self.log_eip7708_transfer(&message.caller, &message.destination, &message.value);
        Ok(())
    }

    #[inline(never)]
    fn finish_create_message_run(
        &mut self,
        checkpoint: StateCheckpoint,
        address: &Address,
        stop: InstrStop,
        mut gas: Gas,
        mut output: Bytes,
    ) -> Result<MessageResult<T>, ExecutionError> {
        if stop.is_success() {
            if let Err(stop) = self.validate_create_output(&mut gas, &mut output) {
                self.state.rollback(checkpoint, self.features);
                return Ok(MessageResultExt {
                    stop,
                    gas: *gas.tracker(),
                    output: Bytes::new(),
                    created_address: None,
                    ext: T::MessageResultExt::default(),
                    _non_exhaustive: (),
                });
            }

            if let Err(code) = self
                .state
                .account(address)
                .map(|mut a| a.set_code_slow(Bytecode::new_legacy(output.clone())))
            {
                self.state.rollback(checkpoint, self.features);
                return Err(code.into());
            }
        } else {
            self.state.rollback(checkpoint, self.features);
        }

        Ok(MessageResultExt {
            stop,
            gas: *gas.tracker(),
            output,
            created_address: stop.is_success().then_some(*address),
            ext: T::MessageResultExt::default(),
            _non_exhaustive: (),
        })
    }

    fn validate_create_output(&self, gas: &mut Gas, output: &mut Bytes) -> Result<(), InstrStop> {
        if self.feature(EvmFeatures::CODE_SIZE_CHECK) && output.len() > self.version().max_code_size
        {
            return Err(InstrStop::CreateContractSizeLimit);
        }
        if self.feature(EvmFeatures::EIP3541) && output.first().is_some_and(|byte| *byte == 0xef) {
            return Err(InstrStop::CreateContractStartingWithEF);
        }

        let code_deposit_gas = output
            .len()
            .saturating_mul(self.version().gas_params.get(GasId::CodeDepositCost) as usize);
        let code_deposit_gas = u64::try_from(code_deposit_gas).unwrap_or(u64::MAX);
        if gas.remaining() < code_deposit_gas {
            if self.feature(EvmFeatures::EIP2) {
                // EIP-2 makes code-deposit OOG fail contract creation; Frontier instead
                // creates the account with empty code.
                return Err(InstrStop::OutOfGas);
            }
            *output = Bytes::new();
            return Ok(());
        }
        gas.spend(code_deposit_gas)?;

        // EIP-8037: hashing the deployed bytecode to compute its code_hash costs
        // execution keccak word gas, and depositing the code costs state gas. The
        // state-gas charge must be the last spend before the journal commit so
        // that any 0→x→0 reservoir refills earlier in the frame are not disturbed.
        if self.feature(EvmFeatures::EIP8037) {
            let params = &self.version().gas_params;
            gas.spend(params.keccak256_word_cost(output.len()))?;
            let code_deposit_state_gas = params.code_deposit_state_gas(output.len());
            if code_deposit_state_gas > 0 {
                gas.spend_state(code_deposit_state_gas)?;
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn execute_call_message(
        &mut self,
        message: &mut Message<T>,
    ) -> Result<MessageResult<T>, ExecutionError> {
        if message.depth > self.version().max_call_depth() {
            return Ok(Self::error_message_result(
                InstrStop::CallTooDeep,
                message.gas_limit,
                message.reservoir,
            ));
        }
        let checkpoint = self.state.checkpoint();
        let transfers_balance = matches!(
            message.kind,
            MessageKind::Call | MessageKind::CallCode | MessageKind::StaticCall
        );
        if transfers_balance {
            if !self.state.transfer(&message.caller, &message.destination, &message.value)? {
                return Ok(Self::error_message_result(
                    InstrStop::OutOfFunds,
                    message.gas_limit,
                    message.reservoir,
                ));
            }
            self.log_eip7708_transfer(&message.caller, &message.destination, &message.value);
        }
        if self.contains_precompile(message) {
            return self.execute_call_precompile(checkpoint, message);
        }
        // Empty accounts complete the call without executing the bytecode's padding STOP.
        if message.code.is_empty() {
            return Ok(MessageResultExt {
                stop: InstrStop::Stop,
                gas: GasTracker::new_with_execution_gas_and_reservoir(
                    message.gas_limit,
                    message.reservoir,
                ),
                ..MessageResultExt::default()
            });
        }
        let stop = self.run_frame(message, checkpoint.clone())?;
        Ok(self.finish_call_message_run(checkpoint, stop))
    }

    #[inline(never)]
    fn execute_call_precompile(
        &mut self,
        checkpoint: StateCheckpoint,
        message: &Message<T>,
    ) -> Result<MessageResult<T>, ExecutionError> {
        let mut gas =
            GasTracker::new_with_execution_gas_and_reservoir(message.gas_limit, message.reservoir);
        let execution = self.execute_precompile(message, &mut gas);
        let (stop, output) = match execution {
            Ok(_) | Err(PrecompileError::Revert(_)) if gas.remaining() > message.gas_limit => {
                (InstrStop::PrecompileOOG, Bytes::new())
            }
            Ok(output) => (InstrStop::Return, output.into_bytes()),
            Err(PrecompileError::Revert(output)) => (InstrStop::Revert, output),
            Err(PrecompileError::Halt(PrecompileHalt::OutOfGas)) => {
                (InstrStop::PrecompileOOG, Bytes::new())
            }
            Err(PrecompileError::Halt(_)) => (InstrStop::PrecompileError, Bytes::new()),
            Err(PrecompileError::Database(error)) => {
                self.state.rollback(checkpoint, self.features);
                return Err(error.into());
            }
            Err(PrecompileError::Fatal(error)) => {
                self.state.rollback(checkpoint, self.features);
                return Err(ExecutionError::Fatal(error));
            }
        };
        if !stop.is_success() {
            self.state.rollback(checkpoint, self.features);
        }
        Ok(MessageResultExt {
            stop,
            gas,
            output,
            created_address: None,
            ext: T::MessageResultExt::default(),
            _non_exhaustive: (),
        })
    }

    #[inline(never)]
    fn finish_call_message_run(
        &mut self,
        checkpoint: StateCheckpoint,
        stop: InstrStop,
    ) -> MessageResult<T> {
        let interp = self.interpreter_pool.last_mut().unwrap();
        let child_gas = interp.gas();
        let output = Bytes::copy_from_slice(interp.output());
        if !stop.is_success() {
            self.state.rollback(checkpoint, self.features);
        }

        MessageResultExt {
            stop,
            gas: *child_gas.tracker(),
            output,
            created_address: None,
            ext: T::MessageResultExt::default(),
            _non_exhaustive: (),
        }
    }

    #[inline]
    fn error_message_result(
        stop: InstrStop,
        gas_remaining: u64,
        reservoir: u64,
    ) -> MessageResult<T> {
        MessageResultExt {
            stop,
            gas: GasTracker::new_with_execution_gas_and_reservoir(gas_remaining, reservoir),
            ..MessageResultExt::default()
        }
    }

    /// Runs `message`'s interpreter frame, which rolls back to `checkpoint` if it fails.
    #[inline]
    fn run_frame(
        &mut self,
        message: &Message<T>,
        checkpoint: StateCheckpoint,
    ) -> Result<InstrStop, ExecutionError> {
        let previous = self.frame_checkpoint.replace(checkpoint);
        let stop = self.run_interpreter(message);
        self.frame_checkpoint = previous;
        stop
    }

    #[inline(never)]
    fn run_interpreter<'frame>(
        &mut self,
        message: &'frame Message<T>,
    ) -> Result<InstrStop, ExecutionError> {
        let guard = self.enter_execution();
        let mut interp: Box<Interpreter<'frame, 'a, T>> = guard.evm.interpreter_pool.pop(message);
        let interp_ref = interp.as_mut();
        interp_ref.lend_memory();
        guard.evm.inspect_initialize_interp(interp_ref);
        let inspector = guard.evm.active_inspector().map(|inspector| {
            // SAFETY: The inspector is stored in `self` and remains alive for the duration of the
            // interpreter run.
            unsafe { trustme::decouple_lt_mut(inspector) }
        });
        let prev_frame = guard
            .evm
            .current_frame
            .replace(NonNull::from(&mut *interp_ref).cast::<Interpreter<'static, 'static, T>>());
        let runner =
            guard.evm.interpreter_runner.as_ref().filter(|_| guard.evm.runner_supported).cloned();
        let stop = if let Some(inspector) = inspector {
            interp_ref.run_inspect(guard.evm, inspector)
        } else if let Some(runner) = runner
            && let Some(stop) = runner.run(interp_ref, guard.evm)
        {
            interp_ref.finish_run(stop)
        } else {
            interp_ref.run(guard.evm)
        };
        guard.evm.current_frame = prev_frame;
        guard.evm.interpreter_pool.push(interp);
        stop
    }

    fn inspect_initialize_interp(&mut self, interp: &mut Interpreter<'_, 'a, T>) {
        if let Some(inspector) = self.active_inspector() {
            // SAFETY: The inspector is stored in `self` and remains alive for the duration of the
            // hook.
            let inspector = unsafe { trustme::decouple_lt_mut(inspector) };
            // The host and spec are normally wired up by the interpreter run; set them up early so
            // that the hook can access them.
            interp.prepare_run(self);
            inspector.initialize_interp(interp);
        }
    }

    /// Reports a message that halts without executing, such as a transaction's first frame whose
    /// runtime gas phase ran out of gas.
    ///
    /// With an inspector installed, the call or create hook fires and the matching end hook
    /// receives `result`, as for an executed frame; the hook cannot replace `result`. `result`
    /// must already have its gas settled for its stop reason.
    pub fn execute_halted_message(
        &mut self,
        message: &mut Message<T>,
        result: MessageResult<T>,
    ) -> Result<MessageResult<T>, ExecutionError> {
        if self.inspector.is_some() {
            return self.execute_message_inspected(message, Some(result));
        }
        Ok(result)
    }
}

impl<'a, T: EvmTypes> Host<T> for Evm<'a, T> {
    fn spec_id(&self) -> SpecId {
        self.spec_id()
    }

    fn execution_config(&self) -> &ExecutionConfig<T> {
        &self.execution_config
    }

    fn block_env(&mut self) -> &BlockEnv<T> {
        &self.block
    }

    fn tx_env(&mut self) -> &TxEnv<T> {
        &self.tx_env
    }

    fn load_account(
        &mut self,
        address: &Address,
        load_code: bool,
        skip_cold_load: bool,
    ) -> Result<AccountLoad, HostError> {
        let mut account = match self.state.account_with_skip(address, skip_cold_load) {
            Ok(account) => account,
            Err(LoadError::ColdLoadSkipped) => return Err(InstrStop::OutOfGas.into()),
            Err(code) => return Err(code.into()),
        };
        let is_cold = account.warm();

        let is_empty_for_new_account_gas = account.is_empty_for_new_account_gas(self.features);
        let (balance, nonce, code_hash, is_empty) = match account.get() {
            Some(info) => (info.balance, info.nonce, info.code_hash, info.is_empty()),
            None => (Word::ZERO, 0, B256::ZERO, true),
        };

        // load code
        let code = if load_code { account.load_code()? } else { Bytecode::default() };
        Ok(AccountLoad {
            balance,
            nonce,
            code_hash,
            code,
            is_empty,
            is_empty_for_new_account_gas,
            is_cold,
            _non_exhaustive: (),
        })
    }

    fn block_hash(&mut self, number: &Word) -> Result<B256, DatabaseError> {
        self.state.block_hash(number)
    }

    fn sload(
        &mut self,
        address: &Address,
        key: &Word,
        skip_cold_load: bool,
    ) -> Result<SLoad, HostError> {
        let eip2929 = self.feature(EvmFeatures::EIP2929);
        let storage = self.state.storage(address)?;
        let loaded = if eip2929 {
            storage.into_slot_with_skip_and_warm(*key, skip_cold_load)
        } else {
            storage.into_slot_with_skip(*key, skip_cold_load).map(|slot| (slot, false))
        };
        let (slot, is_cold) = match loaded {
            Ok(loaded) => loaded,
            // SLOAD's out-of-gas is the cold-access charge itself, so the slot was never accessed
            // and is not recorded in the block access list (unlike SSTORE, which first pays the
            // warm-read cost, accessing the slot, before the cold/dynamic charge can run out).
            Err(LoadError::ColdLoadSkipped) => return Err(InstrStop::OutOfGas.into()),
            Err(code) => return Err(code.into()),
        };
        let value = slot.current();
        Ok(SLoad { value, is_cold, _non_exhaustive: () })
    }

    fn sstore(
        &mut self,
        address: &Address,
        key: &Word,
        value: &Word,
        skip_cold_load: bool,
    ) -> Result<SStore, HostError> {
        let eip2929 = self.feature(EvmFeatures::EIP2929);
        // EIP-8037: SSTORE must cover the slot's access cost before the
        // implicit storage read. When the cold access is unaffordable the read is skipped, so the
        // slot stays out of the EIP-7928 block access list (the warm-read cost has already been
        // paid by the instruction, so an affordable warm slot is still read on OOG).
        let storage = self.state.storage(address)?;
        let loaded = if eip2929 {
            storage.into_slot_with_skip_and_warm(*key, skip_cold_load)
        } else {
            storage.into_slot_with_skip(*key, skip_cold_load).map(|slot| (slot, false))
        };
        let (mut slot, is_cold) = match loaded {
            Ok(loaded) => loaded,
            Err(LoadError::ColdLoadSkipped) => return Err(InstrStop::OutOfGas.into()),
            Err(code) => return Err(code.into()),
        };

        let (original_value, present_value) = slot.write(*value);
        Ok(SStore {
            original_value,
            present_value,
            new_value: *value,
            is_cold,
            _non_exhaustive: (),
        })
    }

    fn tload(&mut self, address: &Address, key: &Word) -> Word {
        self.state.tload(address, key)
    }

    fn tstore(&mut self, address: &Address, key: &Word, value: &Word) {
        self.state.tstore(address, key, value);
    }

    #[inline]
    fn log(&mut self, address: &Address, topics: Cow<'_, [B256]>, data: Cow<'_, [u8]>) {
        if self.logs_observed() {
            self.log(Log::new_unchecked(*address, topics.into_owned(), data.into_owned().into()));
        }
    }

    #[inline]
    fn execute_message(
        &mut self,
        message: &mut Message<T>,
    ) -> Result<MessageResult<T>, ExecutionError> {
        let checkpoint = self.state.checkpoint();
        let result = if self.inspector.is_some() {
            self.execute_message_inspected(message, None)
        } else {
            self.execute_message_impl(message)
        };
        if result.is_err() {
            self.state.rollback(checkpoint, self.features);
        }
        result
    }

    fn selfdestruct(
        &mut self,
        contract: &Address,
        target: &Address,
        skip_cold_load: bool,
    ) -> Result<SelfDestructResult, HostError> {
        let features = self.features;
        let (is_cold, target_is_empty_for_new_account_gas) =
            match self.state.account_with_skip(target, skip_cold_load) {
                Ok(mut account) => (account.warm(), account.is_empty_for_new_account_gas(features)),
                Err(LoadError::ColdLoadSkipped) => return Err(InstrStop::OutOfGas.into()),
                Err(code) => return Err(code.into()),
            };
        let (previously_destroyed, balance, should_destroy) = {
            let account = self.state.account(contract)?;
            let should_destroy = !features.contains(EvmFeatures::EIP6780) || account.is_created();
            (account.is_destructed(), account.balance(), should_destroy)
        };

        if contract != target {
            let transferred = self.state.transfer(contract, target, &balance)?;
            if transferred {
                self.log_eip7708_transfer(contract, target, &balance);
            }
        } else if should_destroy && !balance.is_zero() && !self.feature(EvmFeatures::EIP8246) {
            // Pre-EIP-8246: SELFDESTRUCT to self burns the contract's balance. EIP-8246 removes
            // this burn, leaving the balance untouched; finalization resets the account
            // to balance-only.
            let delta = Word::ZERO.wrapping_sub(balance);
            self.state.account(contract)?.add_balance(delta);
        }
        if should_destroy {
            self.state.account(contract)?.mark_destructed();
        }
        Ok(SelfDestructResult {
            had_value: !balance.is_zero(),
            value: balance,
            target_is_empty: target_is_empty_for_new_account_gas,
            is_cold,
            previously_destroyed,
            _non_exhaustive: (),
        })
    }
}

/// Loaded account information.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AccountLoad {
    /// Account balance.
    pub balance: Word,
    /// Account nonce.
    pub nonce: u64,
    /// Account code hash.
    pub code_hash: B256,
    /// Account bytecode.
    pub code: Bytecode,
    /// Whether the account is empty.
    pub is_empty: bool,
    /// Whether the account is empty for new-account gas: absent or empty under EIP-161, absent
    /// and untouched before it.
    pub is_empty_for_new_account_gas: bool,
    /// Whether the account access was cold.
    pub is_cold: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// Result of an `SLOAD` host operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SLoad {
    /// Storage slot value.
    pub value: Word,
    /// Whether the storage slot access was cold.
    pub is_cold: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// Result of an `SSTORE` host operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SStore {
    /// Storage value at the start of the transaction.
    pub original_value: Word,
    /// Storage value immediately before this `SSTORE`.
    pub present_value: Word,
    /// Storage value written by this `SSTORE`.
    pub new_value: Word,
    /// Whether the storage slot access was cold.
    pub is_cold: bool,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

impl SStore {
    /// Returns whether this `SSTORE` leaves the slot unchanged (`new == present`).
    #[inline]
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.new_value == self.present_value
    }

    /// Returns whether the slot is clean (`original == present`).
    #[inline]
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.original_value == self.present_value
    }

    /// Returns whether this `SSTORE` restores the slot to its original value (`new == original`).
    #[inline]
    #[must_use]
    pub fn resets_original(&self) -> bool {
        self.original_value == self.new_value
    }

    /// Returns whether the original value is zero.
    #[inline]
    #[must_use]
    pub fn original_is_zero(&self) -> bool {
        self.original_value.is_zero()
    }

    /// Returns whether the present value is zero.
    #[inline]
    #[must_use]
    pub fn present_is_zero(&self) -> bool {
        self.present_value.is_zero()
    }

    /// Returns whether the new value is zero.
    #[inline]
    #[must_use]
    pub fn new_is_zero(&self) -> bool {
        self.new_value.is_zero()
    }
}

/// Result of a `SELFDESTRUCT` host operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SelfDestructResult {
    /// Whether the destroyed account had non-zero value.
    pub had_value: bool,
    /// Balance transferred or cleared by the destruction.
    pub value: Word,
    /// Whether the beneficiary is empty/non-existent for new-account gas checks.
    pub target_is_empty: bool,
    /// Whether the beneficiary access was cold.
    pub is_cold: bool,
    /// Whether this account was already destroyed in this transaction.
    pub previously_destroyed: bool,

    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

fn eip7708_transfer_log(from: &Address, to: &Address, value: &Word) -> Option<Log> {
    if value.is_zero() || from == to {
        return None;
    }
    Some(Log::new_unchecked(
        SYSTEM_ADDRESS,
        vec![EIP7708_TRANSFER_TOPIC, from.into_word(), to.into_word()],
        Bytes::from(value.to_be_bytes::<32>()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfigSelector, BaseEvmTypes, NoopInspector, Precompiles, SpecId, Version,
        bytecode::Bytecode,
        env::BlockEnvExt,
        ethereum::{TxEnvelope, ethereum_tx_registry},
        interpreter::{GasTracker, Interpreter, Message, MessageExt, MessageKind, op},
        precompiles::{Precompile, PrecompileError, PrecompileId, PrecompileMap},
        registry::{HandlerError, TxRequest, handler},
        test_utils::{handle_test_tx, legacy_bytecode, push_address, test_tx},
        utils::address_to_word,
    };
    use alloc::{borrow::Cow, string::ToString, sync::Arc, vec, vec::Vec};
    use alloy_consensus::{TxLegacy, transaction::Recovered};
    use alloy_eip7928::{BlockAccessList, StorageChange};
    use alloy_primitives::{Address, Bytes, Log, LogData, TxKind, U256};
    use core::{
        error::Error,
        fmt,
        sync::atomic::{AtomicUsize, Ordering},
    };

    const TEST_TX_TYPE: u8 = 0x00;
    const TEST_PRECOMPILE: Address = Address::with_last_byte(0x42);
    const INNER_TEST_PRECOMPILE: Address = Address::with_last_byte(0x43);

    struct ExtensionTestTypes;

    impl EvmTypesHost for ExtensionTestTypes {
        type ConfigSelector = BaseEvmConfigSelector;
        type SpecId = SpecId;
        type Tx = ();
        type EvmExt = u64;
        type MessageExt = ();
        type MessageResultExt = ();
        type TxEnvExt = ();
        type TxResultExt = ();
        type BlockEnvExt = ();
        type Host<'a> = Evm<'a, Self>;
    }

    #[test]
    fn stores_typed_evm_extension_state() {
        let mut evm = Evm::<ExtensionTestTypes>::new_with_ext(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompile::NoPrecompiles::default(),
            41,
        );

        assert_eq!(*evm.ext(), 41);
        *evm.ext_mut() += 1;
        assert_eq!(*evm.ext(), 42);
    }

    #[derive(Debug)]
    struct TestInterpreterRunner {
        stop: Option<InstrStop>,
        calls: Arc<AtomicUsize>,
    }

    impl InterpreterRunner<BaseEvmTypes> for TestInterpreterRunner {
        fn run<'frame, 'host>(
            &self,
            _interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
            _host: &mut Evm<'host, BaseEvmTypes>,
        ) -> Option<InstrStop> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.stop
        }
    }

    fn run_with_test_interpreter_runner(stop: Option<InstrStop>, bytecode: &[u8]) -> InstrStop {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_interpreter_runner(TestInterpreterRunner { stop, calls: Arc::clone(&calls) });
        let message = MessageExt {
            gas_limit: 30_000,
            code: Bytecode::new_legacy(Bytes::copy_from_slice(bytecode)),
            ..Default::default()
        };

        let stop = evm.run_interpreter(&message).unwrap();

        assert_eq!(calls.load(Ordering::Relaxed), 1);
        stop
    }

    const LIFECYCLE_ACCOUNT: Address = Address::with_last_byte(0x7a);
    const LIFECYCLE_STORAGE_KEY: Word = Word::from_limbs([1, 0, 0, 0]);

    fn handle_lifecycle_tx(
        req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>,
    ) -> HandlerResult<TxResult> {
        let value = Word::from(req.tx.nonce);
        req.host.state.storage_slot(&LIFECYCLE_ACCOUNT, LIFECYCLE_STORAGE_KEY)?.write(value);
        req.host.state.log(Log::new_unchecked(LIFECYCLE_ACCOUNT, vec![], Bytes::new()));
        Ok(TxResultExt { status: true, total_gas_spent: req.tx.nonce, ..TxResultExt::default() })
    }

    fn handle_read_only_tx(
        req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>,
    ) -> HandlerResult<TxResult> {
        let account = req.host.state.account(&LIFECYCLE_ACCOUNT)?;
        assert_eq!(account.balance(), Word::ONE);
        drop(account);

        let slot = req.host.state.storage_slot(&LIFECYCLE_ACCOUNT, LIFECYCLE_STORAGE_KEY)?;
        assert_eq!(slot.current(), Word::ONE);

        Ok(TxResultExt { status: true, ..TxResultExt::default() })
    }

    fn empty_precompiles() -> Precompiles<BaseEvmTypes> {
        Precompiles::new(Cow::Owned(PrecompileMap::new()))
    }

    fn test_precompile(
        address: Address,
        f: crate::precompiles::PrecompileFn<BaseEvmTypes>,
    ) -> Precompile<BaseEvmTypes> {
        Precompile::new(address, PrecompileId::custom("test"), f)
    }

    fn precompiles_with(
        precompiles: impl IntoIterator<Item = Precompile<BaseEvmTypes>>,
    ) -> Precompiles<BaseEvmTypes> {
        let mut map = PrecompileMap::new();
        for precompile in precompiles {
            map.insert(precompile);
        }
        Precompiles::new(Cow::Owned(map))
    }

    fn precompile_message(address: Address) -> Message {
        MessageExt {
            kind: MessageKind::Call,
            depth: 0,
            gas_limit: 30_000,
            reservoir: 0,
            destination: address,
            call_target: address,
            caller: Address::ZERO,
            input: Default::default(),
            value: U256::ZERO,
            code: Bytecode::default(),
            code_address: address,
            caller_is_static: false,
            salt: B256::ZERO,
            ext: (),
            _non_exhaustive: (),
        }
    }

    #[test]
    fn precompile_logs_are_inspected() {
        #[derive(Default)]
        struct LogInspector(Vec<Log>);

        impl Inspector<BaseEvmTypes> for LogInspector {
            fn log(&mut self, log: &Log, _host: &mut Evm<'_, BaseEvmTypes>) {
                self.0.push(log.clone());
            }
        }

        let precompiles = precompiles_with([test_precompile(TEST_PRECOMPILE, |evm, _, _| {
            evm.log(Log::new_unchecked(
                TEST_PRECOMPILE,
                Vec::new(),
                Bytes::from_static(b"precompile"),
            ));
            Ok(PrecompileOutput::new(Bytes::new()))
        })]);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );
        evm.set_inspector(LogInspector::default());
        let mut message = precompile_message(TEST_PRECOMPILE);

        let result = Host::execute_message(&mut evm, &mut message).unwrap();

        assert_eq!(result.stop, InstrStop::Return);
        let inspector = evm.clear_inspector_as::<LogInspector>().unwrap();
        assert_eq!(inspector.0.len(), 1);
        assert_eq!(inspector.0, evm.logs());
    }

    #[test]
    fn nested_precompile_logs_are_inspected_once() {
        #[derive(Default)]
        struct LogInspector(Vec<Log>);

        impl Inspector<BaseEvmTypes> for LogInspector {
            fn log(&mut self, log: &Log, _host: &mut Evm<'_, BaseEvmTypes>) {
                self.0.push(log.clone());
            }
        }

        let precompiles = precompiles_with([
            test_precompile(TEST_PRECOMPILE, |evm, message, _| {
                let mut child = precompile_message(INNER_TEST_PRECOMPILE);
                child.depth = message.depth + 1;
                child.input = message.input.clone();
                Host::execute_message(evm, &mut child)?;
                evm.log(Log { address: TEST_PRECOMPILE, data: LogData::default() });
                Ok(PrecompileOutput::new(Bytes::new()))
            }),
            test_precompile(INNER_TEST_PRECOMPILE, |evm, message, _| {
                let log = Log { address: INNER_TEST_PRECOMPILE, data: LogData::default() };
                Host::log(evm, &INNER_TEST_PRECOMPILE, Cow::Borrowed(&[]), Cow::Borrowed(&[]));
                evm.log(log);
                if message.input.is_empty() {
                    Ok(PrecompileOutput::new(Bytes::new()))
                } else {
                    Err(PrecompileError::Revert(Bytes::new()))
                }
            }),
        ]);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );
        for revert in [false, true] {
            evm.state.clear_transaction_state();
            evm.set_inspector(LogInspector::default());
            let mut message = precompile_message(TEST_PRECOMPILE);
            if revert {
                message.input = Bytes::from_static(b"revert").into();
            }
            let result = Host::execute_message(&mut evm, &mut message).unwrap();

            assert_eq!(result.stop, InstrStop::Return);
            let inspector = evm.clear_inspector_as::<LogInspector>().unwrap();
            assert_eq!(
                inspector.0.iter().map(|log| log.address).collect::<Vec<_>>(),
                [INNER_TEST_PRECOMPILE, INNER_TEST_PRECOMPILE, TEST_PRECOMPILE,]
            );
            assert_eq!(evm.logs().len(), if revert { 1 } else { 3 });
        }
    }

    #[test]
    fn precompile_cannot_return_more_gas_than_its_limit() {
        fn overcharge_and_return(
            _: &mut Evm<'_, BaseEvmTypes>,
            _: &Message,
            gas: &mut GasTracker,
        ) -> crate::precompiles::PrecompileResult {
            let _ = gas.spend(gas.limit() + 1);
            Ok(PrecompileOutput::new(Bytes::new()))
        }

        fn overcharge_and_revert(
            _: &mut Evm<'_, BaseEvmTypes>,
            _: &Message,
            gas: &mut GasTracker,
        ) -> crate::precompiles::PrecompileResult {
            let _ = gas.spend(gas.limit() + 1);
            Err(PrecompileError::Revert(Bytes::new()))
        }

        let precompiles = precompiles_with([
            test_precompile(TEST_PRECOMPILE, overcharge_and_return),
            test_precompile(INNER_TEST_PRECOMPILE, overcharge_and_revert),
        ]);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );

        for address in [TEST_PRECOMPILE, INNER_TEST_PRECOMPILE] {
            let mut message = precompile_message(address);
            let result = Host::execute_message(&mut evm, &mut message).unwrap();

            assert_eq!(result.stop, InstrStop::PrecompileOOG);
            assert_eq!(result.gas.remaining(), 0);
        }
    }

    fn lifecycle_evm() -> Evm<'static, BaseEvmTypes> {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_lifecycle_tx(req)),
        );
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &LIFECYCLE_ACCOUNT,
            AccountInfo::default().with_balance(Word::ONE),
        );
        database.insert_account_storage(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY, &Word::ONE);
        Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            database,
            Precompiles::base(SpecId::OSAKA),
        )
    }

    fn read_only_evm() -> Evm<'static, BaseEvmTypes> {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_read_only_tx(req)),
        );
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &LIFECYCLE_ACCOUNT,
            AccountInfo::default().with_balance(Word::ONE),
        );
        database.insert_account_storage(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY, &Word::ONE);
        Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            database,
            Precompiles::base(SpecId::OSAKA),
        )
    }

    #[test]
    fn owned_database_error_survives_nested_precompile_calls() {
        let precompiles = precompiles_with([
            test_precompile(TEST_PRECOMPILE, |evm, _, _| {
                let mut child = precompile_message(INNER_TEST_PRECOMPILE);
                child.depth = 1;
                Host::execute_message(evm, &mut child)?;
                panic!("execution must abort on a database error");
            }),
            test_precompile(INNER_TEST_PRECOMPILE, |_, _, _| {
                Err(DatabaseError::new(
                    BalError::AccountNotFound { address: LIFECYCLE_ACCOUNT },
                    false,
                )
                .into())
            }),
        ]);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );
        let result = Host::execute_message(&mut evm, &mut precompile_message(TEST_PRECOMPILE));
        let ExecutionError::Database(error) = result.unwrap_err() else {
            panic!("expected database error")
        };
        assert!(!error.is_fatal());
        assert_eq!(
            error.downcast_ref::<BalError>(),
            Some(&BalError::AccountNotFound { address: LIFECYCLE_ACCOUNT })
        );
    }

    #[test]
    fn skipped_cold_loads_do_not_read_database() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            DbStats::new(InMemoryDB::default()),
            Precompiles::base(SpecId::OSAKA),
        );
        let address = LIFECYCLE_ACCOUNT;
        assert!(matches!(
            Host::load_account(&mut evm, &address, true, true),
            Err(HostError::Halt(InstrStop::OutOfGas))
        ));
        let stats = evm.database_as::<DbStats<InMemoryDB>>().unwrap().counts();
        assert_eq!(stats.get_account, 0);
        assert_eq!(stats.get_code_by_hash, 0);

        // SLOAD runs against the executing account, which frame setup has already loaded.
        evm.state.account(&address).unwrap();
        assert!(matches!(
            Host::sload(&mut evm, &address, &Word::ZERO, true),
            Err(HostError::Halt(InstrStop::OutOfGas))
        ));
        let stats = evm.database_as::<DbStats<InMemoryDB>>().unwrap().counts();
        assert_eq!(stats.get_account, 1);
        assert_eq!(stats.get_storage, 0);
        assert_eq!(stats.get_code_by_hash, 0);
    }

    #[test]
    fn fatal_custom_precompile_aborts_parent_call() {
        const FATAL_PRECOMPILE_ADDRESS: Address = Address::with_last_byte(0x43);

        #[derive(Debug)]
        struct TestPrecompileError;

        impl fmt::Display for TestPrecompileError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("test precompile error")
            }
        }

        impl Error for TestPrecompileError {}

        let contract = Address::repeat_byte(0xbb);
        let mut code = vec![op::PUSH1, 0, op::PUSH1, 0, op::PUSH1, 0, op::PUSH1, 0, op::PUSH1, 0];
        push_address(&mut code, &FATAL_PRECOMPILE_ADDRESS);
        code.extend([
            op::PUSH2,
            0x75,
            0x30,
            op::CALL,
            op::PUSH1,
            1,
            op::PUSH1,
            1,
            op::SSTORE,
            op::STOP,
        ]);
        let bytecode = Bytecode::new_legacy(Bytes::from(code));
        let mut precompiles = Precompiles::base(SpecId::OSAKA);
        precompiles.as_map_mut().insert(Precompile::new(
            FATAL_PRECOMPILE_ADDRESS,
            PrecompileId::custom("fatal-test"),
            |_, _, _| Err(PrecompileError::fatal(TestPrecompileError)),
        ));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );
        let mut message = MessageExt {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 200_000,
            code: bytecode,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message);

        let ExecutionError::Fatal(error) = result.unwrap_err() else {
            panic!("expected fatal precompile error")
        };
        assert!(error.downcast_ref::<TestPrecompileError>().is_some());
        evm.state.finalize_transaction_(Version::base(SpecId::OSAKA));
        let pending = evm.state.take_pending_state();
        assert!(
            !pending
                .accounts
                .get(&contract)
                .is_some_and(|account| account.storage.slots.contains_key(&Word::ONE))
        );
    }

    #[test]
    fn precompile_oog_halt_remains_recoverable_by_parent_call() {
        const OOG_PRECOMPILE_ADDRESS: Address = Address::with_last_byte(0x44);

        let contract = Address::repeat_byte(0xbc);
        let mut code = vec![op::PUSH1, 0, op::PUSH1, 0, op::PUSH1, 0, op::PUSH1, 0, op::PUSH1, 0];
        push_address(&mut code, &OOG_PRECOMPILE_ADDRESS);
        code.extend([
            op::PUSH2,
            0x75,
            0x30,
            op::CALL,
            op::PUSH1,
            1,
            op::PUSH1,
            1,
            op::SSTORE,
            op::STOP,
        ]);
        let bytecode = Bytecode::new_legacy(Bytes::from(code));
        let mut precompiles = Precompiles::base(SpecId::OSAKA);
        precompiles.as_map_mut().insert(Precompile::new(
            OOG_PRECOMPILE_ADDRESS,
            PrecompileId::custom("oog-test"),
            |_, _, _| Err(PrecompileHalt::OutOfGas.into()),
        ));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );
        let mut message = MessageExt {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 200_000,
            code: bytecode,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message).unwrap();

        assert_eq!(result.stop, InstrStop::Stop);
        evm.state.finalize_transaction_(Version::base(SpecId::OSAKA));
        let pending = evm.state.take_pending_state();
        assert!(
            pending
                .accounts
                .get(&contract)
                .is_some_and(|account| account.storage.slots.contains_key(&Word::ONE))
        );
    }

    #[test]
    fn fatal_custom_precompile_error_is_owned_and_evm_can_be_reused() {
        const FATAL_PRECOMPILE_ADDRESS: Address = Address::with_last_byte(0x43);

        #[derive(Debug)]
        struct TestPrecompileError;

        impl fmt::Display for TestPrecompileError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("test precompile error")
            }
        }

        impl Error for TestPrecompileError {}

        let caller = Address::repeat_byte(0xaa);
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                gas_limit: 50_000,
                to: TxKind::Call(FATAL_PRECOMPILE_ADDRESS),
                ..TxLegacy::default()
            }),
            caller,
        );
        let mut precompiles = Precompiles::base(SpecId::OSAKA);
        precompiles.as_map_mut().insert(Precompile::new(
            FATAL_PRECOMPILE_ADDRESS,
            PrecompileId::custom("fatal-test"),
            |_, _, _| Err(PrecompileError::fatal(TestPrecompileError)),
        ));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            InMemoryDB::default(),
            precompiles,
        );

        let error = evm.call_tx(&tx).unwrap_err();
        let HandlerError::Fatal(error) = error else { panic!("expected fatal precompile error") };
        assert!(error.downcast_ref::<TestPrecompileError>().is_some());
        assert_eq!(error.to_string(), "test precompile error");
        evm.set_precompiles(Precompiles::base(SpecId::OSAKA));
        let next = evm.transact(&tx).unwrap().discard();
        assert!(next.is_success());
        assert!(error.downcast_ref::<TestPrecompileError>().is_some());
    }

    #[derive(Clone, Copy)]
    enum PrecompileAccess {
        Mut,
        AsMut,
        Set,
    }

    fn run_precompile_access(access: PrecompileAccess) {
        let precompiles = precompiles_with([test_precompile(
            TEST_PRECOMPILE,
            match access {
                PrecompileAccess::Mut => |evm, _, _| {
                    let _ = evm.precompiles_mut();
                    Ok(PrecompileOutput::new(Bytes::new()))
                },
                PrecompileAccess::AsMut => |evm, _, _| {
                    evm.assert_precompiles_downcast_mutable();
                    Ok(PrecompileOutput::new(Bytes::new()))
                },
                PrecompileAccess::Set => |evm, _, _| {
                    evm.set_precompiles(empty_precompiles());
                    Ok(PrecompileOutput::new(Bytes::new()))
                },
            },
        )]);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );
        let message = precompile_message(TEST_PRECOMPILE);
        let _ = evm.execute_precompile(&message, &mut GasTracker::new(30_000));
    }

    #[test]
    fn replacing_block_preserves_state_and_bal_context() {
        let mut evm = lifecycle_evm();
        let state = core::ptr::addr_of!(evm.state);
        let account = Address::with_last_byte(0x51);
        let storage_key = Word::from(0x52);
        let block_number = Word::from(0x53);
        let block_hash = B256::with_last_byte(0x54);

        evm.overlay_db_mut()
            .insert_account_info(&account, AccountInfo::default().with_balance(Word::from(55)));
        evm.overlay_db_mut().insert_account_storage(&account, &storage_key, &Word::from(56));
        evm.overlay_db_mut().insert_block_hash(&block_number, &block_hash);

        let bal = Arc::new(Bal::new());
        evm.state.set_bal(Arc::clone(&bal));
        evm.state.enable_bal_builder();
        evm.state.set_bal_index(BlockAccessIndex::new(57));

        let block = BlockEnvExt { number: Word::from(58), ..BlockEnvExt::default() };
        evm.set_block(block);

        assert!(core::ptr::eq(state, core::ptr::addr_of!(evm.state)));
        assert_eq!(evm.block(), &block);
        assert_eq!(evm.overlay_db().account_info(&account).unwrap().balance, Word::from(55));
        assert_eq!(
            evm.overlay_db()
                .cache
                .storage
                .get(&account)
                .and_then(|storage| storage.slots.get(&storage_key)),
            Some(&Word::from(56))
        );
        assert_eq!(evm.overlay_db().cache.block_hashes.get(&block_number), Some(&block_hash));
        assert!(Arc::ptr_eq(evm.state.bal().unwrap(), &bal));
        assert!(evm.state.bal_builder().is_some());
        assert_eq!(evm.state.bal_index(), BlockAccessIndex::new(57));
    }

    #[test]
    fn replacing_execution_config_preserves_state_and_rebuilds_fork_state() {
        let mut evm = lifecycle_evm();
        let state = core::ptr::addr_of!(evm.state);
        let account = Address::with_last_byte(0x61);
        let storage_key = Word::from(0x62);
        let block_number = Word::from(0x63);
        let block_hash = B256::with_last_byte(0x64);
        evm.overlay_db_mut()
            .insert_account_info(&account, AccountInfo::default().with_balance(Word::from(65)));
        evm.overlay_db_mut().insert_account_storage(&account, &storage_key, &Word::from(66));
        evm.overlay_db_mut().insert_block_hash(&block_number, &block_hash);

        let bal = Arc::new(Bal::new());
        evm.state.set_bal(Arc::clone(&bal));
        evm.state.enable_bal_builder();
        evm.state.set_bal_index(BlockAccessIndex::new(67));

        let config = ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::PRAGUE);
        evm.set_execution_config(
            config.clone(),
            SpecId::PRAGUE,
            crate::ethereum::ethereum_tx_registry(SpecId::PRAGUE),
            Precompiles::base(SpecId::PRAGUE),
        );

        assert!(core::ptr::eq(state, core::ptr::addr_of!(evm.state)));
        assert_eq!(evm.spec_id(), SpecId::PRAGUE);
        assert_eq!(evm.config_spec_id(), SpecId::PRAGUE);
        assert_eq!(evm.version().features, config.version().features);
        assert!(evm.feature(EvmFeatures::EIP7702));
        assert!(evm.registry().contains(4));
        assert!(
            evm.precompiles_as::<Precompiles>()
                .unwrap()
                .as_map()
                .contains(&Address::with_last_byte(0x0b))
        );
        assert_eq!(evm.overlay_db().account_info(&account).unwrap().balance, Word::from(65));
        assert_eq!(evm.overlay_db().cache.block_hashes.get(&block_number), Some(&block_hash));
        assert!(Arc::ptr_eq(evm.state.bal().unwrap(), &bal));
        assert!(evm.state.bal_builder().is_some());
        assert_eq!(evm.state.bal_index(), BlockAccessIndex::new(67));
    }

    #[test]
    fn replacing_execution_config_within_spec_keeps_fork_state() {
        let mut evm = lifecycle_evm();
        evm.set_precompiles(precompiles_with([test_precompile(TEST_PRECOMPILE, |_, _, _| {
            Ok(PrecompileOutput::new(Bytes::new()))
        })]));
        let block = BlockEnvExt { number: Word::from(0x71), ..BlockEnvExt::default() };
        let mut version = Version::new(SpecId::OSAKA);
        version.max_blobs_per_tx = 21;

        evm.set_block_and_execution_config_within_spec(
            block,
            ExecutionConfig::for_spec_and_version(SpecId::OSAKA, version),
        );

        assert_eq!(evm.block(), &block);
        assert_eq!(evm.version().max_blobs_per_tx, 21);
        assert_eq!(evm.config_spec_id(), SpecId::OSAKA);
        assert!(evm.registry().contains(TEST_TX_TYPE));
        assert!(evm.precompiles().contains(&TEST_PRECOMPILE));
    }

    #[test]
    #[should_panic(expected = "execution config spec mismatch")]
    fn replacing_execution_config_within_spec_rejects_fork_change() {
        let mut evm = lifecycle_evm();
        evm.set_block_and_execution_config_within_spec(
            BlockEnvExt::default(),
            ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::PRAGUE),
        );
    }

    #[test]
    fn immutable_precompile_access_is_allowed_during_execution() {
        let precompiles = precompiles_with([test_precompile(TEST_PRECOMPILE, |evm, _, _| {
            let _ = evm.precompiles();
            Ok(PrecompileOutput::new(Bytes::new()))
        })]);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );
        let message = precompile_message(TEST_PRECOMPILE);

        evm.execute_precompile(&message, &mut GasTracker::new(30_000)).unwrap();
    }

    #[test]
    fn set_boxed_precompiles_keeps_provider_type() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );

        evm.set_boxed_precompiles(Box::new(precompiles_with([])));

        assert!(evm.precompiles_as::<Precompiles>().unwrap().as_map().is_empty());
    }

    #[test]
    #[should_panic(expected = "precompile provider cannot be modified during EVM execution")]
    fn precompiles_mut_panics_during_execution() {
        run_precompile_access(PrecompileAccess::Mut);
    }

    #[test]
    #[should_panic(expected = "precompile provider cannot be modified during EVM execution")]
    fn precompiles_as_mut_panics_during_execution() {
        run_precompile_access(PrecompileAccess::AsMut);
    }

    #[test]
    #[should_panic(expected = "precompile provider cannot be modified during EVM execution")]
    fn set_precompiles_panics_during_execution() {
        run_precompile_access(PrecompileAccess::Set);
    }

    #[derive(Clone, Copy)]
    enum InspectorAccess {
        Mut,
        Set,
        SetBoxed,
        Clear,
    }

    fn run_inspector_access(access: InspectorAccess) {
        struct AccessingInspector {
            access: InspectorAccess,
        }

        impl Inspector<BaseEvmTypes> for AccessingInspector {
            fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let evm = interp.host();
                match self.access {
                    InspectorAccess::Mut => {
                        let _ = evm.inspector_mut();
                    }
                    InspectorAccess::Set => evm.set_inspector(NoopInspector::default()),
                    InspectorAccess::SetBoxed => {
                        evm.set_boxed_inspector(Box::<NoopInspector>::default());
                    }
                    InspectorAccess::Clear => {
                        let _ = evm.clear_inspector();
                    }
                }
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(AccessingInspector { access });
        let message = MessageExt {
            code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            ..MessageExt::default()
        };
        let _ = evm.run_interpreter(&message);
    }

    #[test]
    fn immutable_inspector_access_is_allowed_during_execution() {
        struct ReadingInspector {}

        impl Inspector<BaseEvmTypes> for ReadingInspector {
            fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let _ = interp.host().inspector();
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(ReadingInspector {});
        let message = MessageExt {
            code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            ..MessageExt::default()
        };

        let _ = evm.run_interpreter(&message);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn inspector_mut_panics_during_execution() {
        run_inspector_access(InspectorAccess::Mut);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn set_inspector_panics_during_execution() {
        run_inspector_access(InspectorAccess::Set);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn set_boxed_inspector_panics_during_execution() {
        run_inspector_access(InspectorAccess::SetBoxed);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn clear_inspector_panics_during_execution() {
        run_inspector_access(InspectorAccess::Clear);
    }

    #[test]
    fn set_block_during_execution_updates_live_context() {
        struct BlockReplacingInspector;

        impl Inspector<BaseEvmTypes> for BlockReplacingInspector {
            fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let host = interp.host();
                host.set_block(BlockEnvExt {
                    number: Word::from(123),
                    timestamp: Word::from(124),
                    ..BlockEnvExt::default()
                });
                assert_eq!(host.block().number, Word::from(123));
                assert_eq!(host.block().timestamp, Word::from(124));
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(BlockReplacingInspector);
        let message = MessageExt {
            code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            ..MessageExt::default()
        };
        let _ = evm.run_interpreter(&message);
        assert_eq!(evm.block().number, Word::from(123));
        assert_eq!(evm.block().timestamp, Word::from(124));
    }

    #[test]
    fn tx_env_mut_during_transaction_overrides_origin() {
        struct OriginInspector(Address);

        impl Inspector<BaseEvmTypes> for OriginInspector {
            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                message: &mut Message<BaseEvmTypes>,
            ) -> Option<MessageResult<BaseEvmTypes>> {
                if message.depth == 0 {
                    interp.host().tx_env_mut().origin = self.0;
                }
                None
            }
        }

        let caller = Address::repeat_byte(0xaa);
        let origin = Address::repeat_byte(0xbb);
        let target = Address::repeat_byte(0x11);
        let inner = Address::repeat_byte(0x22);
        // SSTORE(0, ORIGIN), then CALL(inner).
        let mut code = vec![op::ORIGIN, op::PUSH0, op::SSTORE];
        code.extend([op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH20]);
        code.extend(inner.0);
        code.extend([op::GAS, op::CALL, op::STOP]);
        // SSTORE(1, ORIGIN)
        let inner_code = vec![op::ORIGIN, op::PUSH1, 1, op::SSTORE, op::STOP];
        let mut database = InMemoryDB::default();
        for (address, code) in [(target, code), (inner, inner_code)] {
            database.insert_account_info(
                &address,
                AccountInfo::default().with_nonce(1).with_code(legacy_bytecode(code)),
            );
        }
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            database,
            Precompiles::base(SpecId::CANCUN),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(target),
                gas_limit: 500_000,
                ..Default::default()
            }),
            caller,
        );
        let executed = evm.transact_with_inspector(&tx, &mut OriginInspector(origin)).unwrap();
        assert!(executed.commit().is_success());

        let origin_word = address_to_word(&origin);
        assert_eq!(evm.state.storage_slot_untracked(&target, &Word::ZERO).unwrap(), origin_word);
        assert_eq!(evm.state.storage_slot_untracked(&inner, &Word::ONE).unwrap(), origin_word);
        assert_eq!(evm.read_account_info(&caller).unwrap().unwrap().nonce, 1);
    }

    #[test]
    #[should_panic(expected = "execution config cannot be modified during EVM execution")]
    fn set_execution_config_panics_during_execution() {
        struct ConfigReplacingInspector;

        impl Inspector<BaseEvmTypes> for ConfigReplacingInspector {
            fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
                let host = interp.host();
                host.set_execution_config(
                    ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::PRAGUE),
                    SpecId::PRAGUE,
                    crate::ethereum::ethereum_tx_registry(SpecId::PRAGUE),
                    Precompiles::base(SpecId::PRAGUE),
                );
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(ConfigReplacingInspector);
        let message = MessageExt {
            code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            ..MessageExt::default()
        };
        let _ = evm.run_interpreter(&message);
    }

    #[derive(Clone, Copy)]
    enum TopLevelInspectorHook {
        Log,
        Call,
        Create,
    }

    fn run_top_level_inspector_clear(hook: TopLevelInspectorHook) {
        struct ClearingInspector;

        impl Inspector<BaseEvmTypes> for ClearingInspector {
            fn log(&mut self, _log: &Log, host: &mut Evm<'_, BaseEvmTypes>) {
                let _ = host.clear_inspector();
            }

            fn call(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                _message: &mut Message,
            ) -> Option<MessageResult> {
                let _ = interp.host().clear_inspector();
                None
            }

            fn create(
                &mut self,
                interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
                _message: &mut Message,
            ) -> Option<MessageResult> {
                let _ = interp.host().clear_inspector();
                None
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        evm.set_inspector(ClearingInspector);

        match hook {
            TopLevelInspectorHook::Log => {
                Host::log(&mut evm, &Address::ZERO, Cow::Borrowed(&[]), Cow::Borrowed(&[]))
            }
            TopLevelInspectorHook::Call => {
                let mut message = MessageExt { kind: MessageKind::Call, ..Default::default() };
                let _ = Host::execute_message(&mut evm, &mut message);
            }
            TopLevelInspectorHook::Create => {
                let mut message = MessageExt { kind: MessageKind::Create, ..Default::default() };
                let _ = Host::execute_message(&mut evm, &mut message);
            }
        }
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn top_level_log_inspector_clear_panics_during_execution() {
        run_top_level_inspector_clear(TopLevelInspectorHook::Log);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn top_level_call_inspector_clear_panics_during_execution() {
        run_top_level_inspector_clear(TopLevelInspectorHook::Call);
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn top_level_create_inspector_clear_panics_during_execution() {
        run_top_level_inspector_clear(TopLevelInspectorHook::Create);
    }

    #[test]
    fn passes_evm_to_precompile_provider() {
        let address = TEST_PRECOMPILE;
        let block = BlockEnvExt { number: U256::from(17), ..BlockEnvExt::default() };
        let precompiles = precompiles_with([test_precompile(address, |evm, message, _| {
            assert_eq!(message.kind, MessageKind::Call);
            assert_eq!(message.depth, 67);
            assert_eq!(message.destination, TEST_PRECOMPILE);
            assert_eq!(message.caller, Address::with_last_byte(0x7a));
            assert_eq!(&*message.input, b"message input");
            assert_eq!(message.value, U256::from(99));
            assert_eq!(message.code_address, TEST_PRECOMPILE);
            Ok(PrecompileOutput::new(Bytes::from(evm.block.number.to_be_bytes::<32>())))
        })]);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            block,
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );
        let message = MessageExt {
            kind: MessageKind::Call,
            depth: 67,
            gas_limit: 30_000,
            reservoir: 0,
            destination: address,
            call_target: address,
            caller: Address::with_last_byte(0x7a),
            input: Bytes::from_static(b"message input").into(),
            value: U256::from(99),
            code: Bytecode::default(),
            code_address: address,
            caller_is_static: false,
            salt: B256::ZERO,
            ext: (),
            _non_exhaustive: (),
        };
        let output = evm
            .execute_precompile(&message, &mut GasTracker::new(30_000))
            .expect("precompile succeeds");

        assert_eq!(U256::from_be_slice(output.bytes()), U256::from(17));
    }

    #[test]
    fn precompile_can_call_another_precompile() {
        let precompiles = precompiles_with([
            test_precompile(TEST_PRECOMPILE, |evm, _, gas| {
                let message = precompile_message(INNER_TEST_PRECOMPILE);
                evm.execute_precompile(&message, gas)
            }),
            test_precompile(INNER_TEST_PRECOMPILE, |_, _, _| {
                Ok(PrecompileOutput::new(Bytes::from_static(b"inner")))
            }),
        ]);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            precompiles,
        );
        let message = precompile_message(TEST_PRECOMPILE);

        let output = evm
            .execute_precompile(&message, &mut GasTracker::new(30_000))
            .expect("precompile succeeds");

        assert_eq!(output.bytes(), b"inner");
    }

    #[test]
    fn dispatches_transaction_by_typed_2718_type() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
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

        assert_eq!(evm.transact(&tx).map(|executed| executed.discard().tx_gas_used()), Ok(42));
    }

    #[test]
    fn dispatches_transaction_without_evm_config() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_test_tx(req)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::OSAKA),
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = test_tx(41);

        assert_eq!(evm.transact(&tx).map(|executed| executed.discard().tx_gas_used()), Ok(42));
    }

    #[test]
    fn interpreter_runner_can_execute_frame() {
        assert_eq!(
            run_with_test_interpreter_runner(Some(InstrStop::Return), &[op::INVALID]),
            InstrStop::Return
        );
    }

    #[test]
    fn interpreter_runner_can_fallback_to_interpreter() {
        assert_eq!(run_with_test_interpreter_runner(None, &[op::STOP]), InstrStop::Stop);
    }

    #[derive(Debug)]
    struct SpecBoundRunner {
        spec_id: SpecId,
        initial_call_depth: u16,
        calls: Arc<AtomicUsize>,
    }

    impl InterpreterRunner<BaseEvmTypes> for SpecBoundRunner {
        fn run<'frame, 'host>(
            &self,
            _interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
            _host: &mut Evm<'host, BaseEvmTypes>,
        ) -> Option<InstrStop> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Some(InstrStop::Return)
        }

        fn supports(&self, config: &ExecutionConfig<BaseEvmTypes>) -> bool {
            config.base_spec_id() == self.spec_id
                && config.version().initial_call_depth == self.initial_call_depth
        }
    }

    #[test]
    fn interpreter_runner_support_follows_runner_and_execution_config() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let message = MessageExt {
            gas_limit: 30_000,
            code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
            ..Default::default()
        };
        let run = |evm: &mut Evm<'_, BaseEvmTypes>| {
            let stop = evm.run_interpreter(&message).unwrap();
            (stop, calls.load(Ordering::Relaxed))
        };
        let set_spec = |evm: &mut Evm<'_, BaseEvmTypes>, spec_id: SpecId| {
            evm.set_execution_config(
                ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(spec_id),
                spec_id,
                TxRegistry::new(),
                Precompiles::base(spec_id),
            );
        };

        evm.set_shared_interpreter_runner(Arc::new(SpecBoundRunner {
            spec_id: SpecId::PRAGUE,
            initial_call_depth: 0,
            calls: Arc::clone(&calls),
        }));
        assert_eq!(run(&mut evm), (InstrStop::Stop, 0));

        set_spec(&mut evm, SpecId::PRAGUE);
        assert_eq!(run(&mut evm), (InstrStop::Return, 1));

        set_spec(&mut evm, SpecId::OSAKA);
        assert_eq!(run(&mut evm), (InstrStop::Stop, 1));

        set_spec(&mut evm, SpecId::PRAGUE);
        assert_eq!(run(&mut evm), (InstrStop::Return, 2));

        let mut version = Version::new(SpecId::PRAGUE);
        version.initial_call_depth = 1;
        evm.set_block_and_execution_config_within_spec(
            BlockEnvExt::default(),
            ExecutionConfig::for_spec_and_version(SpecId::PRAGUE, version),
        );
        assert_eq!(run(&mut evm), (InstrStop::Stop, 2));

        evm.set_block_and_execution_config_within_spec(
            BlockEnvExt::default(),
            ExecutionConfig::for_base_spec::<BaseEvmConfigSelector>(SpecId::PRAGUE),
        );
        assert_eq!(run(&mut evm), (InstrStop::Return, 3));

        let runner = evm.clear_interpreter_runner().unwrap();
        assert_eq!(run(&mut evm), (InstrStop::Stop, 3));

        evm.set_shared_interpreter_runner(runner);
        assert_eq!(run(&mut evm), (InstrStop::Return, 4));
    }

    #[test]
    fn dispatches_transaction_with_dynamic_version() {
        fn handle_test_tx_version(
            req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>,
        ) -> HandlerResult<TxResult> {
            Ok(TxResultExt {
                status: true,
                total_gas_spent: req.host.version().tx_gas_limit_cap,
                ..TxResultExt::default()
            })
        }

        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_test_tx_version(req)),
        );
        let mut version = crate::Version::new(SpecId::OSAKA);
        version.tx_gas_limit_cap = 42;
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(SpecId::OSAKA, version),
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let tx = test_tx(0);

        assert_eq!(evm.transact(&tx).map(|executed| executed.discard().tx_gas_used()), Ok(42));
    }

    #[test]
    fn dispatches_transaction_iter() {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_test_tx(req)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let txs = [test_tx(1), test_tx(2)];
        let gas_used = evm
            .transact_iter(&txs)
            .map(|result| result.map(|result| result.tx_gas_used()))
            .collect::<HandlerResult<Vec<_>>>();

        assert_eq!(gas_used, Ok(vec![2, 3]));
    }

    #[test]
    fn fatal_handler_error_discards_transaction_state() {
        fn fail(req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>) -> HandlerResult<TxResult> {
            let _ = handle_lifecycle_tx(req)?;
            Err(HandlerError::Fatal("handler failed".into()))
        }

        let mut evm = lifecycle_evm();
        evm.registry.register(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| fail(req)),
        );
        let error = evm.call_tx(&test_tx(7)).unwrap_err();
        let HandlerError::Fatal(error) = error else { panic!("expected fatal handler error") };
        assert_eq!(error.to_string(), "handler failed");
        assert!(evm.logs().is_empty());
        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::ONE
        );
        evm.registry.register(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(|_| Ok(()), |req, ()| handle_lifecycle_tx(req)),
        );
        assert!(evm.transact(&test_tx(9)).unwrap().commit().is_success());
    }

    #[test]
    fn executed_transaction_discard_drops_state_but_keeps_outcome_logs() {
        let mut evm = lifecycle_evm();
        let outcome =
            evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").discard();

        assert_eq!(outcome.tx_gas_used(), 7);
        assert_eq!(outcome.logs.len(), 1);
        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::ONE
        );
    }

    #[test]
    fn executed_transaction_discard_with_streams_without_committing() {
        let mut evm = lifecycle_evm();
        let mut sink = BlockStateAccumulator::new();

        let outcome = evm
            .transact(&test_tx(7))
            .expect("lifecycle transaction should execute")
            .discard_with(&mut sink)
            .expect("block accumulator is infallible");

        assert_eq!(outcome.tx_gas_used(), 7);
        assert_eq!(outcome.logs.len(), 1);
        let storage = sink.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].1.original, Word::ONE);
        assert_eq!(storage[0].1.current, Word::from(7));
        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::ONE
        );
    }

    #[test]
    fn executed_transaction_discard_with_streams_bal_reads() {
        let mut evm = read_only_evm();
        let mut sink = InMemoryDB::default();
        sink.bal_context.enable_bal_builder();

        let _ = evm
            .transact(&test_tx(0))
            .expect("read-only transaction should execute")
            .discard_with(&mut sink)
            .expect("cache sink is infallible");

        let bal = sink.bal_context.take_bal_builder().expect("builder was enabled");
        let account = bal.accounts.get(&LIFECYCLE_ACCOUNT).expect("read account is in the bal");
        assert!(account.account_info.balance.is_empty());
        assert!(account.account_info.nonce.is_empty());
        assert!(
            account
                .storage
                .storage
                .get(&LIFECYCLE_STORAGE_KEY)
                .expect("read slot is in the bal")
                .is_empty()
        );
    }

    #[test]
    fn executed_transaction_detach_moves_pending_state_without_committing() {
        let mut evm = lifecycle_evm();
        let result =
            evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").detach();

        assert_eq!(result.result.logs.len(), 1);
        let overlay = &result
            .pending_state
            .accounts
            .get(&LIFECYCLE_ACCOUNT)
            .expect("storage change should be present")
            .storage;
        let slot =
            overlay.slots.get(&LIFECYCLE_STORAGE_KEY).expect("storage slot should be present");
        assert_eq!(slot.value.original, Word::ONE);
        assert_eq!(slot.value.current, Word::from(7));
        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::ONE
        );
    }

    #[test]
    fn executed_transaction_detach_matches_commit_fold() {
        // Commit with the builder enabled: the reference BAL fold.
        let mut evm = lifecycle_evm();
        evm.state.enable_bal_builder();
        evm.state.set_bal_index(BlockAccessIndex::new(1));
        let _ = evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").commit();
        let committed_bal = evm.state.take_bal_builder().expect("builder was enabled");

        // The same execution detached as a pending state.
        let mut evm = lifecycle_evm();
        let TxResultWithState { pending_state: pending, .. } =
            evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").detach();

        // Folding the pending state into a fresh BAL matches the commit-built BAL.
        let mut rebuilt = Bal::new();
        rebuilt.commit(BlockAccessIndex::new(1), pending);
        assert_eq!(rebuilt, committed_bal);
        // Detaching accepted nothing into this EVM's overlay.
        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::ONE
        );
    }

    #[test]
    fn executed_transaction_commit_updates_accepted_overlay() {
        let mut evm = lifecycle_evm();
        let outcome =
            evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").commit();

        assert_eq!(outcome.logs.len(), 1);
        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::from(7)
        );

        let _ = evm.transact(&test_tx(9)).expect("lifecycle transaction should execute").commit();
        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::from(9)
        );
    }

    #[test]
    fn executed_transaction_commit_to_accumulates_block_state() {
        let mut evm = lifecycle_evm();
        let mut block_state = BlockStateAccumulator::new();

        let _ = evm
            .transact(&test_tx(7))
            .expect("lifecycle transaction should execute")
            .commit_to(&mut block_state);
        let _ = evm
            .transact(&test_tx(9))
            .expect("lifecycle transaction should execute")
            .commit_to(&mut block_state);

        let storage = block_state.storage_sorted();
        assert_eq!(storage.len(), 1);
        assert_eq!(storage[0].1.original, Word::ONE);
        assert_eq!(storage[0].1.current, Word::from(9));
        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::from(9)
        );
    }

    #[test]
    fn committed_transactions_build_block_access_list_on_cache_db() {
        let mut evm = lifecycle_evm();
        evm.state.enable_bal_builder();
        evm.state.reset_bal_index();

        // Transaction 0 maps to block access index 1: it writes 7 to the lifecycle slot (was 1).
        evm.state.bump_bal_index();
        assert_eq!(evm.state.bal_index(), BlockAccessIndex::new(1));
        let _ = evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").commit();

        // Transaction 1 maps to index 2: it overwrites the slot with 9.
        evm.state.bump_bal_index();
        let _ = evm.transact(&test_tx(9)).expect("lifecycle transaction should execute").commit();

        let bal = evm.state.bal_builder().expect("bal construction is enabled");
        let account =
            bal.accounts.get(&LIFECYCLE_ACCOUNT).expect("lifecycle account is in the bal");
        let slot = account
            .storage
            .storage
            .get(&LIFECYCLE_STORAGE_KEY)
            .expect("lifecycle slot is in the bal");
        assert_eq!(
            slot.changes,
            vec![
                StorageChange::new(BlockAccessIndex::new(1), Word::from(7)),
                StorageChange::new(BlockAccessIndex::new(2), Word::from(9)),
            ]
        );
        // The account's info never changed, so it is recorded as reads (no info writes).
        assert!(account.account_info.balance.is_empty());
        assert!(account.account_info.nonce.is_empty());

        // Taking the BAL yields a canonical EIP-7928 list and resets the index.
        let alloy = BlockAccessList::from(evm.state.take_bal_builder().expect("bal is present"));
        assert_eq!(alloy.len(), 1);
        assert_eq!(alloy[0].address(), LIFECYCLE_ACCOUNT);
        assert_eq!(alloy[0].storage_changes.len(), 1);
        assert_eq!(alloy[0].storage_changes[0].slot, LIFECYCLE_STORAGE_KEY);
        assert_eq!(alloy[0].storage_changes[0].len(), 2);
        assert_eq!(evm.state.bal_index(), BlockAccessIndex::PRE_EXECUTION);
        assert!(evm.state.bal_builder().is_none());
    }

    #[test]
    fn same_index_commits_that_restore_a_slot_record_a_read() {
        let mut evm = lifecycle_evm();
        evm.state.enable_bal_builder();
        evm.state.set_bal_index(BlockAccessIndex::new(1));

        // Two commits at one index write 1 -> 7 -> 1.
        let _ = evm.transact(&test_tx(7)).expect("lifecycle transaction should execute").commit();
        let _ = evm.transact(&test_tx(1)).expect("lifecycle transaction should execute").commit();

        let bal = evm.state.take_bal_builder().expect("builder was enabled");
        let slot = &bal.accounts[&LIFECYCLE_ACCOUNT].storage.storage[&LIFECYCLE_STORAGE_KEY];
        assert!(slot.is_empty());
    }

    #[test]
    fn executed_transaction_commit_with_tee_fans_out_changes() {
        let mut evm = lifecycle_evm();
        let mut left = BlockStateAccumulator::new();
        let mut right = BlockStateAccumulator::new();
        let mut tee = Tee::new(&mut left, &mut right);

        let _ = evm
            .transact(&test_tx(7))
            .expect("lifecycle transaction should execute")
            .commit_with(&mut tee)
            .expect("block accumulators are infallible");

        assert_eq!(left.storage_sorted()[0].1.current, Word::from(7));
        assert_eq!(right.storage_sorted()[0].1.current, Word::from(7));
    }

    #[test]
    fn executed_transaction_account_changes_borrow_without_committing() {
        let mut evm = lifecycle_evm();
        let executed = evm.transact(&test_tx(7)).expect("lifecycle transaction should execute");

        let changes = executed
            .account_changes()
            .find(|changes| changes.address == LIFECYCLE_ACCOUNT)
            .expect("lifecycle account was loaded");
        assert!(changes.is_changed());
        let slots = changes.storage.changed_slots().collect::<Vec<_>>();
        assert_eq!(slots.len(), 1);
        assert_eq!(*slots[0].0, LIFECYCLE_STORAGE_KEY);
        assert_eq!(slots[0].1.original, Word::ONE);
        assert_eq!(slots[0].1.current, Word::from(7));

        let _ = executed.commit();
        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::from(7)
        );
    }

    #[test]
    fn dropped_executed_transaction_discards_state() {
        let mut evm = lifecycle_evm();
        drop(evm.transact(&test_tx(7)).expect("lifecycle transaction should execute"));

        assert_eq!(
            evm.state.storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY).unwrap(),
            Word::ONE
        );
    }

    #[test]
    fn host_executes_message() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let contract = Address::repeat_byte(0x11);
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::ADDRESS, op::STOP]));
        let mut message = MessageExt {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 50_000,
            code: bytecode,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message).unwrap();
        assert!(result.is_success());
    }

    #[test]
    fn interpreter_returns_owned_database_error() {
        #[derive(Debug)]
        struct FailingDbError;

        impl fmt::Display for FailingDbError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("storage read failed")
            }
        }

        impl Error for FailingDbError {}

        #[derive(Debug, Default)]
        struct FailingStorageDb;

        impl Database for FailingStorageDb {
            type Error = FailingDbError;

            fn get_account(
                &mut self,
                _address: &Address,
            ) -> Result<Option<AccountInfo>, Self::Error> {
                Ok(Some(AccountInfo::default()))
            }

            fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
                Ok(Bytecode::default())
            }

            fn get_storage(
                &mut self,
                _address: &Address,
                _key: &Word,
            ) -> Result<Word, Self::Error> {
                Err(FailingDbError)
            }

            fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
                Ok(B256::ZERO)
            }
        }

        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            Db::new(FailingStorageDb),
            Precompiles::base(SpecId::OSAKA),
        );
        let contract = Address::repeat_byte(0x11);
        let bytecode = Bytecode::new_legacy(Bytes::from_static(&[op::PUSH0, op::SLOAD]));
        let mut message = MessageExt {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 50_000,
            code: bytecode,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message);

        let ExecutionError::Database(error) = result.unwrap_err() else {
            panic!("expected database error")
        };
        assert!(error.is_fatal());
        assert!(error.downcast_ref::<FailingDbError>().is_some());
        assert_eq!(error.to_string(), "storage read failed");
        message.code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        assert_eq!(Host::execute_message(&mut evm, &mut message).unwrap().stop, InstrStop::Stop);
    }

    #[test]
    fn cold_storage_oog_rolls_back_warmth() {
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        );
        let contract = Address::repeat_byte(0x11);
        let key = Word::ZERO;
        let bytecode =
            Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 0, op::SLOAD, op::STOP]));
        let mut message = MessageExt {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 500,
            code: bytecode,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message).unwrap();

        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert!(!evm.state.storage(&contract).unwrap().is_warm(&key));
    }

    #[test]
    fn unaffordable_cold_target_selfdestruct_does_not_load_target_account() {
        #[derive(Clone, Debug)]
        struct SelfdestructTargetLoadDb {
            target: Address,
            target_reads: Arc<AtomicUsize>,
        }

        #[derive(Clone, Debug, PartialEq, Eq)]
        struct SelfdestructTargetLoadError;

        impl fmt::Display for SelfdestructTargetLoadError {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("selfdestruct target account was loaded")
            }
        }

        impl Error for SelfdestructTargetLoadError {}

        impl Database for SelfdestructTargetLoadDb {
            type Error = SelfdestructTargetLoadError;

            fn get_account(
                &mut self,
                address: &Address,
            ) -> Result<Option<AccountInfo>, Self::Error> {
                if *address == self.target {
                    self.target_reads.fetch_add(1, Ordering::SeqCst);
                    return Err(SelfdestructTargetLoadError);
                }
                Ok(Some(AccountInfo::default().with_balance(Word::ONE)))
            }

            fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
                Ok(Bytecode::default())
            }

            fn get_storage(
                &mut self,
                _address: &Address,
                _key: &Word,
            ) -> Result<Word, Self::Error> {
                Ok(Word::ZERO)
            }

            fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
                Ok(B256::ZERO)
            }
        }

        fn selfdestruct_to_code(target: &Address) -> Bytecode {
            let mut code = Vec::new();
            push_address(&mut code, target);
            code.push(op::SELFDESTRUCT);
            legacy_bytecode(code)
        }

        let contract = Address::repeat_byte(0xbb);
        let target = Address::repeat_byte(0xcc);
        let target_reads = Arc::new(AtomicUsize::new(0));
        let database = SelfdestructTargetLoadDb { target, target_reads: Arc::clone(&target_reads) };
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::BERLIN,
            BlockEnvExt::default(),
            TxRegistry::new(),
            Db::new(database),
            Precompiles::base(SpecId::BERLIN),
        );
        let mut message = MessageExt {
            kind: MessageKind::Call,
            destination: contract,
            code_address: contract,
            gas_limit: 6_000,
            code: selfdestruct_to_code(&target),
            ..MessageExt::default()
        };
        let result = Host::execute_message(&mut evm, &mut message).unwrap();

        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert_eq!(target_reads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn frontier_code_deposit_oog_creates_empty_contract() {
        let caller = Address::repeat_byte(0x11);
        let created = Address::repeat_byte(0x22);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_balance(Word::ONE));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::FRONTIER,
            BlockEnvExt::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::FRONTIER),
        );
        let code =
            Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 1, op::PUSH1, 0, op::RETURN]));
        let mut message = MessageExt {
            kind: MessageKind::Create,
            destination: created,
            caller,
            gas_limit: 50,
            code,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message).unwrap();
        assert!(result.is_success());

        evm.state.finalize_transaction_(Version::base(SpecId::FRONTIER));
        let pending = evm.state.take_pending_state();
        let account = pending.account_info(&created).unwrap();
        assert!(account.is_empty_code_hash());
    }

    #[test]
    fn frontier_materialized_empty_account_is_created() {
        let target = Address::repeat_byte(0x22);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::FRONTIER,
            BlockEnvExt::default(),
            TxRegistry::new(),
            InMemoryDB::default(),
            Precompiles::base(SpecId::FRONTIER),
        );
        evm.state.account(&target).unwrap().touch();

        evm.state.finalize_transaction_(Version::base(SpecId::FRONTIER));
        let pending = evm.state.take_pending_state();

        assert!(pending.accounts.get(&target).unwrap().is_created());
    }

    #[test]
    fn homestead_code_deposit_oog_fails_create() {
        let caller = Address::repeat_byte(0x11);
        let created = Address::repeat_byte(0x22);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_balance(Word::ONE));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::HOMESTEAD,
            BlockEnvExt::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::HOMESTEAD),
        );
        let code =
            Bytecode::new_legacy(Bytes::from_static(&[op::PUSH1, 1, op::PUSH1, 0, op::RETURN]));
        let mut message = MessageExt {
            kind: MessageKind::Create,
            destination: created,
            caller,
            gas_limit: 50,
            code,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message).unwrap();
        assert_eq!(result.stop, InstrStop::OutOfGas);
        assert!(result.output.is_empty());

        evm.state.finalize_transaction_(Version::base(SpecId::HOMESTEAD));
        let pending = evm.state.take_pending_state();
        assert!(pending.accounts.get(&created).is_none_or(|entry| entry.present.is_none()));
    }

    #[test]
    fn create_validation_failure_clears_output() {
        let caller = Address::repeat_byte(0x11);
        let created = Address::repeat_byte(0x22);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_balance(Word::ONE));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::LONDON,
            BlockEnvExt::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::LONDON),
        );
        let code = Bytecode::new_legacy(Bytes::from_static(&[
            op::PUSH1,
            0xef,
            op::PUSH1,
            0,
            op::MSTORE8,
            op::PUSH1,
            1,
            op::PUSH1,
            0,
            op::RETURN,
        ]));
        let mut message = MessageExt {
            kind: MessageKind::Create,
            destination: created,
            caller,
            gas_limit: 50_000,
            code,
            ..MessageExt::default()
        };
        let result = Host::execute_message(&mut evm, &mut message).unwrap();

        assert_eq!(result.stop, InstrStop::CreateContractStartingWithEF);
        assert!(result.output.is_empty());
    }

    #[test]
    fn create_size_limit_failure_clears_output() {
        let caller = Address::repeat_byte(0x11);
        let created = Address::repeat_byte(0x22);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_balance(Word::ONE));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::SPURIOUS_DRAGON,
            BlockEnvExt::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::SPURIOUS_DRAGON),
        );
        let code = Bytecode::new_legacy(Bytes::from_static(&[
            op::PUSH2,
            0x60,
            0x01,
            op::PUSH1,
            0,
            op::RETURN,
        ]));
        let mut message = MessageExt {
            kind: MessageKind::Create,
            destination: created,
            caller,
            gas_limit: 100_000,
            code,
            ..MessageExt::default()
        };
        let result = Host::execute_message(&mut evm, &mut message).unwrap();

        assert_eq!(result.stop, InstrStop::CreateContractSizeLimit);
        assert!(result.output.is_empty());
    }

    #[test]
    fn staticcall_touches_empty_existing_destination() {
        let target = Address::repeat_byte(0x11);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&target, AccountInfo::default());
        database.insert_account_storage(&target, &Word::ZERO, &Word::ONE);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::SPURIOUS_DRAGON,
            BlockEnvExt::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::SPURIOUS_DRAGON),
        );
        let mut message = MessageExt {
            kind: MessageKind::StaticCall,
            destination: target,
            code_address: target,
            gas_limit: 50_000,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message).unwrap();
        assert!(result.is_success());

        evm.state.finalize_transaction_(Version::base(SpecId::SPURIOUS_DRAGON));
        let pending = evm.state.take_pending_state();
        let account = pending.accounts.get(&target).expect("empty destination should be deleted");
        assert!(account.original.is_some());
        assert_eq!(account.present, None);
        assert!(pending.accounts.get(&target).is_some_and(|account| account.storage.wiped));
    }

    #[test]
    fn delegatecall_does_not_touch_empty_code_address() {
        let destination = Address::repeat_byte(0x11);
        let code_address = Address::repeat_byte(0x22);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&destination, AccountInfo::default().with_balance(Word::ONE));
        database.insert_account_info(&code_address, AccountInfo::default());
        database.insert_account_storage(&code_address, &Word::ZERO, &Word::ONE);
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::SPURIOUS_DRAGON,
            BlockEnvExt::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::SPURIOUS_DRAGON),
        );
        let mut message = MessageExt {
            kind: MessageKind::DelegateCall,
            destination,
            code_address,
            gas_limit: 50_000,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message).unwrap();
        assert!(result.is_success());

        evm.state.finalize_transaction_(Version::base(SpecId::SPURIOUS_DRAGON));
        let pending = evm.state.take_pending_state();
        assert!(pending.accounts.get(&code_address).is_none_or(|entry| !entry.is_changed()));
    }

    #[test]
    fn account_info_with_code_sets_hash() {
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let info = AccountInfo::default().with_code(code.clone());

        assert_eq!(info.code_hash, code.hash_slow());
    }

    #[test]
    fn transfer_moves_value() {
        let from = Address::repeat_byte(0x01);
        let to = Address::repeat_byte(0x02);
        let mut state = State::new(InMemoryDB::default());
        state.account(&from).unwrap().add_balance(U256::from(10));

        assert!(state.transfer(&from, &to, &U256::from(7)).unwrap());
        assert_eq!(
            state
                .account_info_untracked(&from)
                .expect("sender account should exist")
                .unwrap()
                .balance,
            U256::from(3)
        );
        assert_eq!(
            state
                .account_info_untracked(&to)
                .expect("recipient account should exist")
                .unwrap()
                .balance,
            U256::from(7)
        );
    }

    #[test]
    fn amsterdam_call_value_emits_eip7708_log() {
        let caller = Address::repeat_byte(0x01);
        let target = Address::repeat_byte(0x02);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_balance(U256::from(10)));
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::AMSTERDAM,
            BlockEnvExt::default(),
            TxRegistry::new(),
            database,
            Precompiles::base(SpecId::AMSTERDAM),
        );
        let mut message = MessageExt {
            kind: MessageKind::Call,
            destination: target,
            caller,
            value: U256::from(7),
            // Covers the EIP-2780 depth-0 `new_account_state_gas` charge for the
            // value transfer to the empty target account.
            gas_limit: 300_000,
            ..MessageExt::default()
        };

        let result = Host::execute_message(&mut evm, &mut message).unwrap();
        assert!(result.is_success());

        let version = *evm.version();
        evm.state.finalize_transaction_(&version);
        let logs = evm.state.take_logs();
        assert_eq!(logs.len(), 1);
        let log = &logs[0];
        assert_eq!(log.address, SYSTEM_ADDRESS);
        assert_eq!(log.topics(), &[EIP7708_TRANSFER_TOPIC, caller.into_word(), target.into_word()]);
        assert_eq!(log.data.data, Bytes::from(B256::with_last_byte(7)));
    }

    #[test]
    fn eip8246_selfdestruct_preserves_balance_at_finalization() {
        let contract = Address::repeat_byte(0x11);
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &contract,
            AccountInfo::default().with_balance(U256::from(5)).with_code(code),
        );
        database.insert_account_storage(&contract, &Word::ZERO, &Word::from(9));
        let mut state = State::new(database);

        state.account(&contract).unwrap().mark_destructed();
        state.finalize_transaction_(Version::base(SpecId::AMSTERDAM));

        // EIP-8246: the balance is preserved and the account becomes balance-only (nonce 0, no
        // code).
        let info = state
            .account_info_untracked(&contract)
            .unwrap()
            .expect("balance-only account should remain");
        assert_eq!(info.balance, U256::from(5));
        assert_eq!(info.nonce, 0);
        assert!(info.is_empty_code_hash());

        let pending = state.take_pending_state();
        assert!(pending.accounts[&contract].flags.contains(AccountFlags::DESTROYED));
    }

    #[test]
    fn commit_transaction_clears_detached_destroyed_flag() {
        let contract = Address::repeat_byte(0x44);
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(&contract, AccountInfo::default().with_code(code));
        let mut state = State::new(database);

        state.account(&contract).unwrap().mark_destructed();
        state.finalize_transaction_(Version::base(SpecId::AMSTERDAM));
        let pending = state.take_pending_state();

        state.set_pending_state(pending);
        state.commit_transaction();

        assert!(!state.account(&contract).unwrap().is_destructed());
    }

    #[test]
    fn eip8246_selfdestruct_zero_balance_is_deleted() {
        let contract = Address::repeat_byte(0x22);
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(&contract, AccountInfo::default().with_code(code));
        let mut state = State::new(database);

        state.account(&contract).unwrap().mark_destructed();
        state.finalize_transaction_(Version::base(SpecId::AMSTERDAM));

        // A zero-balance balance-only account is empty and deleted by EIP-161.
        assert!(state.account_info_untracked(&contract).unwrap().is_none());
    }

    #[test]
    fn pre_eip8246_selfdestruct_burns_balance_by_deleting_account() {
        let contract = Address::repeat_byte(0x33);
        let code = Bytecode::new_legacy(Bytes::from_static(&[op::STOP]));
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &contract,
            AccountInfo::default().with_balance(U256::from(5)).with_code(code),
        );
        let mut state = State::new(database);

        state.account(&contract).unwrap().mark_destructed();
        state.finalize_transaction_(Version::base(SpecId::PRAGUE));

        // Before EIP-8246 the self-destructed account (and its balance) is deleted at finalization.
        assert!(state.account_info_untracked(&contract).unwrap().is_none());
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FailingInspectorHook {
        Initialize,
        Call,
        CallEnd,
        Create,
        CreateEnd,
        Step,
        StepEnd,
    }

    struct FailingInspector {
        hook: FailingInspectorHook,
        depth: u16,
        error: Option<ExecutionError>,
        override_outcome: bool,
        steps: Arc<AtomicUsize>,
    }

    impl FailingInspector {
        fn fail_hook(
            &mut self,
            hook: FailingInspectorHook,
            depth: u16,
            interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
        ) -> bool {
            if self.hook == hook
                && self.depth == depth
                && let Some(error) = self.error.take()
            {
                let stop = interp.fail(error);
                if matches!(hook, FailingInspectorHook::Step | FailingInspectorHook::StepEnd) {
                    interp.set_stop(stop);
                }
                return true;
            }
            false
        }
    }

    impl Inspector<BaseEvmTypes> for FailingInspector {
        fn initialize_interp(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.fail_hook(FailingInspectorHook::Initialize, interp.message().depth, interp);
        }

        fn call(
            &mut self,
            interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message,
        ) -> Option<MessageResult> {
            (self.fail_hook(FailingInspectorHook::Call, message.depth, interp)
                && self.override_outcome)
                .then(MessageResultExt::default)
        }

        fn call_end(
            &mut self,
            interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &Message,
            _result: &mut MessageResult,
        ) {
            self.fail_hook(FailingInspectorHook::CallEnd, message.depth, interp);
        }

        fn create(
            &mut self,
            interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &mut Message,
        ) -> Option<MessageResult> {
            (self.fail_hook(FailingInspectorHook::Create, message.depth, interp)
                && self.override_outcome)
                .then(MessageResultExt::default)
        }

        fn create_end(
            &mut self,
            interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            message: &Message,
            _result: &mut MessageResult,
        ) {
            self.fail_hook(FailingInspectorHook::CreateEnd, message.depth, interp);
        }

        fn step(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.steps.fetch_add(1, Ordering::Relaxed);
            self.fail_hook(FailingInspectorHook::Step, interp.message().depth, interp);
        }

        fn step_end(&mut self, interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            self.fail_hook(FailingInspectorHook::StepEnd, interp.message().depth, interp);
        }
    }

    #[rstest::rstest]
    #[case::initialize(FailingInspectorHook::Initialize, false)]
    #[case::call(FailingInspectorHook::Call, false)]
    #[case::call_override(FailingInspectorHook::Call, true)]
    #[case::call_end(FailingInspectorHook::CallEnd, false)]
    #[case::create(FailingInspectorHook::Create, false)]
    #[case::create_override(FailingInspectorHook::Create, true)]
    #[case::create_end(FailingInspectorHook::CreateEnd, false)]
    #[case::step(FailingInspectorHook::Step, false)]
    #[case::step_end(FailingInspectorHook::StepEnd, false)]
    fn inspector_error_is_owned_and_evm_can_be_reused(
        #[case] hook: FailingInspectorHook,
        #[case] override_outcome: bool,
        #[values(false, true)] nested: bool,
    ) {
        let caller = Address::repeat_byte(0xaa);
        let parent = Address::repeat_byte(0xbb);
        let child = Address::repeat_byte(0xcc);
        let create = matches!(hook, FailingInspectorHook::Create | FailingInspectorHook::CreateEnd);
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &child,
            AccountInfo::default().with_code(legacy_bytecode([op::STOP])),
        );
        let to = if nested {
            let mut code = if create { vec![op::PUSH0; 3] } else { vec![op::PUSH0; 5] };
            if create {
                code.push(op::CREATE);
            } else {
                push_address(&mut code, &child);
                code.extend([op::GAS, op::CALL]);
            }
            code.extend([op::POP, op::STOP]);
            database.insert_account_info(
                &parent,
                AccountInfo::default().with_code(legacy_bytecode(code)),
            );
            TxKind::Call(parent)
        } else if create {
            TxKind::Create
        } else {
            TxKind::Call(child)
        };
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy { gas_limit: 1_000_000, to, ..TxLegacy::default() }),
            caller,
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::OSAKA),
            database,
            Precompiles::base(SpecId::OSAKA),
        );
        let error = ExecutionError::Fatal("inspector failed".into());
        let steps = Arc::new(AtomicUsize::new(0));
        evm.set_inspector(FailingInspector {
            hook,
            depth: u16::from(nested),
            error: Some(error.clone()),
            override_outcome,
            steps: steps.clone(),
        });

        assert_eq!(evm.call_tx(&tx).unwrap_err(), HandlerError::from(error));
        if !nested
            && matches!(
                hook,
                FailingInspectorHook::Initialize
                    | FailingInspectorHook::Call
                    | FailingInspectorHook::Create
            )
        {
            assert_eq!(steps.load(Ordering::Relaxed), 0);
        }
        assert!(evm.call_tx(&tx).unwrap().is_success());
    }

    #[test]
    fn validate_tx_discards_preparation_writes_on_success_and_error() {
        let mut evm = lifecycle_evm();
        let errors = [
            HandlerError::NonceOverflow,
            HandlerError::Database(crate::DatabaseError::new(fmt::Error, true)),
            HandlerError::Database(crate::DatabaseError::new(fmt::Error, false)),
            HandlerError::Fatal("preparation failed".into()),
        ];
        let expected = errors.clone();
        evm.registry.register(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(
                move |req: &mut TxRequest<'_, '_, BaseEvmTypes, TxLegacy>| {
                    req.host
                        .state
                        .storage_slot(&LIFECYCLE_ACCOUNT, LIFECYCLE_STORAGE_KEY)?
                        .write(Word::from(99));
                    req.host.state.account(&LIFECYCLE_ACCOUNT)?.bump_nonce();
                    req.host.state.log(Log::new_unchecked(LIFECYCLE_ACCOUNT, vec![], Bytes::new()));
                    if let Some(error) = errors.get(req.tx.nonce as usize) {
                        return Err(error.clone());
                    }
                    Ok(())
                },
                |_, ()| panic!("validation must not execute user calls"),
            ),
        );

        for nonce in [4, 0, 1, 2, 3, 4] {
            assert_eq!(
                evm.validate_tx(&test_tx(nonce)),
                expected.get(nonce as usize).cloned().map_or(Ok(()), Err)
            );
            assert!(evm.logs().is_empty());
            assert_eq!(evm.state.account(&LIFECYCLE_ACCOUNT).unwrap().nonce(), 0);
            assert_eq!(
                evm.state
                    .storage_slot_untracked(&LIFECYCLE_ACCOUNT, &LIFECYCLE_STORAGE_KEY)
                    .unwrap(),
                Word::ONE
            );
        }
    }

    #[rstest::rstest]
    #[case(InspectorAccess::Mut)]
    #[case(InspectorAccess::Set)]
    #[case(InspectorAccess::SetBoxed)]
    #[case(InspectorAccess::Clear)]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn inspector_mutation_panics_during_transact(#[case] access: InspectorAccess) {
        let mut evm = lifecycle_evm();
        evm.set_inspector(NoopInspector::default());
        evm.registry.register(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(
                |_| Ok(()),
                move |req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>, ()| {
                    match access {
                        InspectorAccess::Mut => {
                            let _ = req.host.inspector_mut();
                        }
                        InspectorAccess::Set => req.host.set_inspector(NoopInspector::default()),
                        InspectorAccess::SetBoxed => {
                            req.host.set_boxed_inspector(Box::<NoopInspector>::default());
                        }
                        InspectorAccess::Clear => {
                            let _ = req.host.clear_inspector();
                        }
                    }
                    Ok(TxResultExt::default())
                },
            ),
        );

        let _ = evm.transact(&test_tx(0));
    }

    #[cfg(feature = "std")]
    #[test]
    fn transact_restores_inspector_access_after_success_error_and_panic() {
        let mut evm = lifecycle_evm();
        evm.registry.register(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(
                |_| Ok(()),
                |req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>, ()| match req.tx.nonce {
                    0 => Ok(TxResultExt::default()),
                    1 => Err(HandlerError::NonceOverflow),
                    _ => panic!("transaction handler panicked"),
                },
            ),
        );

        for nonce in 0..3 {
            evm.set_inspector(NoopInspector::default());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                evm.call_tx(&test_tx(nonce))
            }));
            match nonce {
                0 => assert!(result.unwrap().is_ok()),
                1 => assert_eq!(result.unwrap().unwrap_err(), HandlerError::NonceOverflow),
                _ => assert!(result.is_err()),
            }
            assert!(evm.clear_inspector_as::<NoopInspector>().is_some());
            evm.set_precompiles(empty_precompiles());
        }
    }

    #[rstest::rstest]
    #[case(PrecompileAccess::Mut)]
    #[case(PrecompileAccess::Set)]
    #[should_panic(expected = "precompile provider cannot be modified during EVM execution")]
    fn precompiles_cannot_be_modified_during_transact(#[case] access: PrecompileAccess) {
        let mut evm = lifecycle_evm();
        evm.registry.register(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(
                |_| Ok(()),
                move |req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>, ()| {
                    match access {
                        PrecompileAccess::Mut => {
                            let _ = req.host.precompiles_mut();
                        }
                        PrecompileAccess::Set => req.host.set_precompiles(empty_precompiles()),
                        PrecompileAccess::AsMut => unreachable!(),
                    }
                    Ok(TxResultExt::default())
                },
            ),
        );

        let _ = evm.transact(&test_tx(0));
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn nested_transact_preserves_inspector_guard() {
        let mut evm = lifecycle_evm();
        evm.registry.register(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(
                |_| Ok(()),
                |req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>, ()| {
                    if req.tx.nonce == 0 {
                        let _ = req.host.call_tx(&test_tx(1))?;
                        req.host.set_inspector(NoopInspector::default());
                    }
                    Ok(TxResultExt::default())
                },
            ),
        );

        let _ = evm.transact(&test_tx(0));
    }

    #[rstest::rstest]
    #[case(SpecId::CANCUN, 0, 3)]
    #[case(SpecId::CANCUN, 7, 3)]
    #[case(SpecId::CANCUN, 7, u64::MAX - 1)]
    #[case(SpecId::AMSTERDAM, 7, u64::MAX - 1)]
    fn nested_root_create_bumps_transaction_sender_nonce_once(
        #[case] spec: SpecId,
        #[case] initial_call_depth: u16,
        #[case] nonce: u64,
    ) {
        let caller = Address::repeat_byte(0xaa);
        let created = caller.create(nonce);
        let mut database = InMemoryDB::default();
        database.insert_account_info(&caller, AccountInfo::default().with_nonce(nonce));
        let mut version = Version::new(spec);
        version.initial_call_depth = initial_call_depth;
        let mut evm = Evm::<BaseEvmTypes>::new_with_execution_config(
            ExecutionConfig::for_spec_and_version(spec, version),
            spec,
            BlockEnvExt::default(),
            ethereum_tx_registry(spec),
            database,
            Precompiles::base(spec),
        );
        // The nested transaction root also creates a child, which must still bump its creator.
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                nonce,
                to: TxKind::Create,
                gas_limit: 1_000_000,
                input: Bytes::from_static(&[op::PUSH0, op::PUSH0, op::PUSH0, op::CREATE, op::STOP]),
                ..Default::default()
            }),
            caller,
        );
        let executed = evm.transact(&tx).unwrap();
        assert!(executed.result().is_success());
        let pending = executed.detach().pending_state;
        assert_eq!(pending.account_info(&caller).unwrap().nonce, nonce + 1);
        assert_eq!(pending.account_info(&created).unwrap().nonce, 2);
        assert_eq!(pending.account_info(&created.create(1)).unwrap().nonce, 1);
    }

    #[test]
    fn transaction_touches_survive_output_but_not_child_revert_or_next_transaction() {
        let caller = Address::repeat_byte(0xaa);
        let target = Address::repeat_byte(0x11);
        let read = Address::repeat_byte(0x22);
        let child = Address::repeat_byte(0x33);
        let leaf = Address::repeat_byte(0x44);
        let mut code = vec![op::PUSH20];
        code.extend_from_slice(read.as_slice());
        code.extend([op::BALANCE, op::POP]);
        let call = |code: &mut Vec<u8>, address: Address| {
            code.extend([op::PUSH0; 5]);
            code.push(op::PUSH20);
            code.extend_from_slice(address.as_slice());
            code.extend([op::GAS, op::CALL, op::POP]);
        };
        call(&mut code, child);
        code.push(op::STOP);
        let mut child_code = Vec::new();
        call(&mut child_code, leaf);
        child_code.extend([op::PUSH0, op::PUSH0, op::REVERT]);
        let mut read_code = vec![op::PUSH20];
        read_code.extend_from_slice(target.as_slice());
        read_code.extend([op::BALANCE, op::POP, op::STOP]);
        let mut database = InMemoryDB::default();
        for (address, code) in
            [(target, code), (child, child_code), (read, read_code), (leaf, vec![])]
        {
            database.insert_account_info(
                &address,
                AccountInfo::default().with_nonce(1).with_code(legacy_bytecode(code)),
            );
        }
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            database,
            Precompiles::base(SpecId::CANCUN),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(target),
                gas_limit: 500_000,
                ..Default::default()
            }),
            caller,
        );
        let executed = evm.transact(&tx).unwrap();
        assert!(executed.result().is_success());
        for (address, touched) in [(target, true), (read, false), (child, false), (leaf, false)] {
            let changes = executed.account_changes().find(|c| c.address == address).unwrap();
            assert_eq!(changes.touched, touched, "{address}");
            assert!(!changes.is_changed());
        }
        let pending = executed.detach().pending_state;
        for (address, touched) in [(target, true), (read, false), (child, false), (leaf, false)] {
            assert_eq!(
                pending.account_changes().find(|c| c.address == address).unwrap().touched,
                touched
            );
        }
        evm.state.set_pending_state(pending);
        evm.state.commit_transaction();
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                nonce: 1,
                to: TxKind::Call(read),
                gas_limit: 100_000,
                ..Default::default()
            }),
            caller,
        );
        let next = evm.transact(&tx).unwrap();
        assert!(next.result().is_success());
        assert!(!next.account_changes().find(|c| c.address == target).unwrap().touched);
    }

    #[test]
    fn changes_inspect_transaction_state_before_commit_or_discard() {
        let caller = Address::repeat_byte(0xaa);
        let target = Address::repeat_byte(0x11);
        // SSTORE(0, 1)
        let code = vec![op::PUSH1, 1, op::PUSH0, op::SSTORE, op::STOP];
        let mut database = InMemoryDB::default();
        database.insert_account_info(
            &target,
            AccountInfo::default().with_nonce(1).with_code(legacy_bytecode(code)),
        );
        let mut evm = Evm::<BaseEvmTypes>::new(
            SpecId::CANCUN,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::CANCUN),
            database,
            Precompiles::base(SpecId::CANCUN),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                to: TxKind::Call(target),
                gas_limit: 100_000,
                ..Default::default()
            }),
            caller,
        );

        let executed = evm.transact(&tx).unwrap();
        assert!(executed.result().is_success());
        let changes = executed.changes();
        assert!(!changes.is_empty());
        assert_eq!(changes.account_info(&caller).unwrap().nonce, 1);
        let slot = changes.account_changes_for(&target).unwrap().storage.slots[&Word::ZERO].value;
        assert_eq!(slot.current, Word::ONE);
        let mut in_place = executed.account_changes().map(|c| c.address).collect::<Vec<_>>();
        in_place.sort();
        let _ = executed.discard();

        // Discarding after inspection leaves the accepted state unchanged, so the same transaction
        // executes again, and its detached state reports the same accounts.
        let executed = evm.transact(&tx).unwrap();
        assert!(executed.result().is_success());
        let pending = executed.detach().pending_state;
        let mut detached = pending.account_changes().map(|c| c.address).collect::<Vec<_>>();
        detached.sort();
        assert_eq!(in_place, detached);
        assert_eq!(pending.changes().account_info(&caller).unwrap().nonce, 1);
    }

    #[derive(Default)]
    struct CallCounter {
        calls: usize,
        panic_on_call: bool,
        panic_on_step: bool,
        probe_host: bool,
        saw_inspector: bool,
    }

    impl Inspector<BaseEvmTypes> for CallCounter {
        fn call(
            &mut self,
            interp: &mut Interpreter<'_, '_, BaseEvmTypes>,
            _message: &mut Message,
        ) -> Option<MessageResult<BaseEvmTypes>> {
            self.calls += 1;
            self.saw_inspector |= self.probe_host && interp.host().inspector().is_some();
            assert!(!self.panic_on_call, "inspector panicked");
            None
        }

        fn step(&mut self, _interp: &mut Interpreter<'_, '_, BaseEvmTypes>) {
            assert!(!self.panic_on_step, "inspector panicked");
        }
    }

    fn scoped_inspector_evm() -> Evm<'static, BaseEvmTypes> {
        let registry = TxRegistry::new().with_handler(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(
                |_| Ok(()),
                |req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>, ()| {
                    let mut message = MessageExt {
                        code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
                        ..MessageExt::default()
                    };
                    Host::execute_message(req.host, &mut message)?;
                    Ok(TxResultExt::default())
                },
            ),
        );
        Evm::<BaseEvmTypes>::new(
            SpecId::OSAKA,
            BlockEnvExt::default(),
            registry,
            InMemoryDB::default(),
            Precompiles::base(SpecId::OSAKA),
        )
    }

    #[test]
    fn transact_with_inspector_reuses_borrowed_inspector() {
        let mut evm = scoped_inspector_evm();
        let mut inspector = CallCounter::default();

        let _ = evm.transact_with_inspector(&test_tx(0), &mut inspector).unwrap().discard();
        assert_eq!(inspector.calls, 1);
        let _ = evm.transact_with_inspector(&test_tx(0), &mut inspector).unwrap().discard();
        assert_eq!(inspector.calls, 2);
        assert!(evm.inspector().is_none());
    }

    #[test]
    fn transact_with_inspector_hides_borrowed_inspector() {
        let mut evm = scoped_inspector_evm();
        let mut borrowed = CallCounter { probe_host: true, ..CallCounter::default() };

        let _ = evm.transact_with_inspector(&test_tx(0), &mut borrowed).unwrap().discard();
        assert_eq!(borrowed.calls, 1);
        assert!(!borrowed.saw_inspector);
        assert!(evm.inspector().is_none() && evm.inspector_mut().is_none());
        assert!(evm.clear_inspector().is_none());

        evm.set_inspector(CallCounter::default());
        let _ = evm.transact_with_inspector(&test_tx(0), &mut borrowed).unwrap().discard();
        assert_eq!(borrowed.calls, 2);
        assert!(!borrowed.saw_inspector);
        let previous = evm.inspector().unwrap().downcast_ref::<CallCounter>().unwrap();
        assert_eq!(previous.calls, 0);
        let _ = evm.call_tx(&test_tx(0)).unwrap();
        let previous = evm.clear_inspector_as::<CallCounter>().unwrap();
        assert_eq!(previous.calls, 1);
        assert_eq!(borrowed.calls, 2);
        assert!(evm.clear_inspector().is_none());
    }

    #[cfg(feature = "std")]
    #[test]
    fn transact_with_inspector_restores_previous_inspector_after_panic() {
        let mut evm = scoped_inspector_evm();
        evm.set_inspector(CallCounter::default());
        let mut panicking = CallCounter { panic_on_call: true, ..CallCounter::default() };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = evm.transact_with_inspector(&test_tx(0), &mut panicking);
        }));

        assert!(result.is_err());
        assert_eq!(panicking.calls, 1);
        let _ = evm.call_tx(&test_tx(0)).unwrap();
        let previous = evm.clear_inspector_as::<CallCounter>().unwrap();
        assert_eq!(previous.calls, 1);
        assert_eq!(panicking.calls, 1);
    }

    #[cfg(feature = "std")]
    #[test]
    fn transact_with_inspector_clears_borrowed_inspector_after_panic() {
        let mut evm = scoped_inspector_evm();
        let mut panicking = CallCounter { panic_on_call: true, ..CallCounter::default() };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = evm.transact_with_inspector(&test_tx(0), &mut panicking);
        }));

        assert!(result.is_err());
        assert!(evm.inspector().is_none());
        let _ = evm.call_tx(&test_tx(0)).unwrap();
        assert_eq!(panicking.calls, 1);
    }

    #[cfg(feature = "std")]
    #[test]
    fn transact_with_inspector_recovers_from_panic_inside_frame() {
        let mut evm = scoped_inspector_evm();
        let mut panicking = CallCounter { panic_on_step: true, ..CallCounter::default() };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = evm.transact_with_inspector(&test_tx(0), &mut panicking);
        }));
        assert!(result.is_err());
        assert!(evm.current_frame.is_none());

        let mut inspector = CallCounter::default();
        let _ = evm.transact_with_inspector(&test_tx(0), &mut inspector).unwrap().discard();
        assert_eq!(inspector.calls, 1);
    }

    #[cfg(feature = "std")]
    #[test]
    fn transact_with_inspector_discards_transaction_state_after_panic() {
        let mut evm = scoped_inspector_evm();
        evm.registry.register(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(
                |_| Ok(()),
                |req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>, ()| {
                    req.host.log(Log { address: TEST_PRECOMPILE, data: LogData::default() });
                    assert!(!req.host.logs().is_empty());
                    let mut message = MessageExt {
                        code: Bytecode::new_legacy(Bytes::from_static(&[op::STOP])),
                        ..MessageExt::default()
                    };
                    Host::execute_message(req.host, &mut message)?;
                    Ok(TxResultExt::default())
                },
            ),
        );
        let mut panicking = CallCounter { panic_on_step: true, ..CallCounter::default() };

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = evm.transact_with_inspector(&test_tx(0), &mut panicking);
        }));

        assert!(result.is_err());
        assert!(evm.logs().is_empty());
    }

    #[test]
    #[should_panic(expected = "inspector cannot be modified during EVM execution")]
    fn nested_transact_with_inspector_panics() {
        let mut evm = scoped_inspector_evm();
        evm.registry.register(
            TEST_TX_TYPE,
            TxEnvelope::as_legacy,
            handler(
                |_| Ok(()),
                |req: TxRequest<'_, '_, BaseEvmTypes, TxLegacy>, ()| {
                    let _ = req
                        .host
                        .transact_with_inspector(&test_tx(1), &mut CallCounter::default())?;
                    Ok(TxResultExt::default())
                },
            ),
        );

        let _ = evm.transact_with_inspector(&test_tx(0), &mut CallCounter::default());
    }
}
