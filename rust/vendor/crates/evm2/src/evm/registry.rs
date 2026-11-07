//! Type-erased transaction handler registry with typed handlers.
//!
//! Handlers are written against concrete transaction types. The registry stores
//! them behind an object-safe boundary and dispatches by transaction type byte.
//!
//! The registry is generic over an [`EvmTypesHost`] family and handler output, so
//! it does not force a particular transaction or receipt representation onto
//! the rest of the crate.

use crate::{AnyError, DatabaseError, EvmTypesHost, ExecutionError};
use alloc::sync::Arc;
use alloy_consensus::transaction::Recovered;
use alloy_primitives::{U256, map::HashMap};
use core::{error::Error, fmt, marker::PhantomData};
use thiserror::Error;

/// Convenience result type used by the registry and handlers.
pub type HandlerResult<T> = core::result::Result<T, HandlerError>;

/// Registry, transaction validation, and transaction handler errors.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HandlerError {
    /// Database error propagated as a transaction handler failure.
    #[error("database error: {0}")]
    Database(#[source] DatabaseError),
    /// Unrecoverable execution error.
    #[error("fatal error: {0}")]
    Fatal(#[source] AnyError),
    /// Typed error supplied by a custom transaction handler.
    #[error(transparent)]
    External(AnyError),
    /// No handler is registered for the transaction type byte.
    #[error("unsupported transaction type 0x{0:02x}")]
    UnsupportedTransactionType(u8),
    /// A registered handler's extractor did not match the provided envelope.
    #[error("envelope did not contain expected transaction type 0x{expected:02x}")]
    WrongTransactionType {
        /// Expected transaction type byte.
        expected: u8,
    },
    /// Sender account does not have the expected nonce.
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce {
        /// Expected nonce.
        expected: u64,
        /// Transaction nonce.
        got: u64,
    },
    /// Transaction chain ID does not match the active chain.
    #[error("invalid chain id: expected {expected}, got {got}")]
    InvalidChainId {
        /// Active chain ID.
        expected: u64,
        /// Transaction chain ID.
        got: u64,
    },
    /// Transaction chain ID is required.
    #[error("missing chain id")]
    MissingChainId,
    /// Transaction gas limit is lower than intrinsic gas.
    #[error("intrinsic gas too low: required {required}, got {got}")]
    IntrinsicGasTooLow {
        /// Required intrinsic gas.
        required: u64,
        /// Transaction gas limit.
        got: u64,
    },
    /// Transaction gas limit is lower than the calldata floor gas (EIP-7623).
    #[error("gas limit below floor gas: required {required}, got {got}")]
    GasLimitBelowFloor {
        /// Required floor gas.
        required: u64,
        /// Transaction gas limit.
        got: u64,
    },
    /// Sender cannot pay value plus maximum gas cost.
    #[error("lack of funds ({balance}) for max fee ({fee})")]
    InsufficientFunds {
        /// Required upfront cost: value plus maximum gas cost.
        fee: U256,
        /// Balance of the sender.
        balance: U256,
    },
    /// Sender account has deployed code.
    #[error("caller has code")]
    RejectCallerWithCode,
    /// Transaction nonce cannot be incremented.
    #[error("nonce overflow in transaction")]
    NonceOverflow,
    /// Transaction gas limit exceeds the block gas limit.
    #[error("transaction gas limit {gas_limit} exceeds block gas limit {block_gas_limit}")]
    GasLimitMoreThanBlock {
        /// Transaction gas limit.
        gas_limit: u64,
        /// Block gas limit.
        block_gas_limit: U256,
    },
    /// Transaction gas limit exceeds the active per-transaction gas cap.
    #[error("transaction gas limit {gas_limit} exceeds cap {cap}")]
    TxGasLimitGreaterThanCap {
        /// Transaction gas limit.
        gas_limit: u64,
        /// Active transaction gas limit cap.
        cap: u64,
    },
    /// Intrinsic execution gas, or the calldata floor gas if larger, exceeds the active execution
    /// gas cap (EIP-8037).
    ///
    /// Unlike [`TxGasLimitGreaterThanCap`](Self::TxGasLimitGreaterThanCap), the transaction's own
    /// gas limit is not what exceeds a cap: it may exceed the execution cap, but the gas it must
    /// spend before execution cannot.
    #[error("intrinsic gas exceeds cap: required {required}, cap {cap}")]
    IntrinsicGasGreaterThanCap {
        /// Required execution gas: the larger of the intrinsic and floor gas.
        required: u64,
        /// Active execution gas cap.
        cap: u64,
    },
    /// Create transaction initcode exceeds the active size limit.
    #[error("create initcode size limit exceeded: limit {limit}, got {got}")]
    CreateInitCodeSizeLimit {
        /// Maximum initcode size.
        limit: usize,
        /// Transaction initcode size.
        got: usize,
    },
    /// Fee cap is lower than the block base fee.
    #[error("fee cap less than base fee: max_fee_per_gas {max_fee_per_gas}, base_fee {base_fee}")]
    FeeCapLessThanBaseFee {
        /// Maximum fee per gas.
        max_fee_per_gas: U256,
        /// Block base fee.
        base_fee: U256,
    },
    /// EIP-7702 authorization list is empty.
    #[error("EIP-7702 authorization list is empty")]
    EmptyAuthorizationList,
    /// EIP-4844 blob fee cap is lower than the block blob base fee.
    #[error(
        "blob fee cap less than blob base fee: max_fee_per_blob_gas {max_fee_per_blob_gas}, blob_base_fee {blob_base_fee}"
    )]
    BlobFeeCapLessThanBlobBaseFee {
        /// Maximum fee per blob gas.
        max_fee_per_blob_gas: U256,
        /// Block blob base fee.
        blob_base_fee: U256,
    },
    /// EIP-4844 blob transaction contains no blob hashes.
    #[error("empty blobs")]
    EmptyBlobs,
    /// EIP-4844 blob transaction contains too many blob hashes.
    #[error("too many blobs: have {have}, max {max}")]
    TooManyBlobs {
        /// Blob count in the transaction.
        have: usize,
        /// Maximum allowed blob count.
        max: usize,
    },
    /// EIP-4844 blob transaction contains an unsupported versioned hash.
    #[error("blob version not supported")]
    BlobVersionNotSupported,
    /// Priority fee is greater than max fee.
    #[error("priority fee greater than max fee")]
    PriorityFeeGreaterThanMaxFee,
}

impl From<DatabaseError> for HandlerError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}
impl From<ExecutionError> for HandlerError {
    fn from(error: ExecutionError) -> Self {
        match error {
            ExecutionError::Database(error) => Self::Database(error),
            ExecutionError::Fatal(error) => Self::Fatal(error),
        }
    }
}

impl HandlerError {
    /// Wraps a typed custom transaction handler error.
    pub fn external(error: impl Error + Send + Sync + 'static) -> Self {
        Self::External(AnyError::new(error))
    }

    /// Returns the typed custom error when it has type `E`.
    pub fn external_ref<E: Error + 'static>(&self) -> Option<&E> {
        match self {
            Self::External(error) => error.downcast_ref(),
            _ => None,
        }
    }
}

/// Request passed to a typed transaction handler.
#[derive(Debug)]
pub struct TxRequest<'a, 'host, T: EvmTypesHost, Tx> {
    /// Full transaction envelope passed to the registry.
    pub envelope: &'a T::Tx,
    /// Concrete transaction extracted from the envelope.
    pub tx: Recovered<&'a Tx>,
    /// Mutable host used by this handler.
    pub host: &'a mut T::Host<'host>,
    #[doc(hidden)] // Not public API. Please use an existing constructor.
    pub _non_exhaustive: (),
}

/// A typed transaction handler with shared validation and execution rules.
///
/// `Tx` remains concrete even though the registry itself is type-erased. Use [`handler`]
/// to implement both entry points with preparation and execution closures.
pub trait TxHandler<T: EvmTypesHost, Tx, Output> {
    /// Validates the transaction without executing its user calls.
    ///
    /// Validation may apply pre-execution writes. The caller owns discarding these writes
    /// and any other transaction-local state on both success and error.
    fn validate(&self, req: TxRequest<'_, '_, T, Tx>) -> HandlerResult<()>;

    /// Validates and executes the transaction.
    fn execute(&self, req: TxRequest<'_, '_, T, Tx>) -> HandlerResult<Output>;
}

/// Builds a handler from shared preparation and execution closures.
///
/// Validation runs `prepare` and drops its result. Execution runs `prepare` exactly once,
/// then passes its result and the request to `execute`. Preparation results stay concrete
/// inside the adapter and are never boxed or exposed through the erased registry.
///
/// The prepared value must not borrow the request or host. It may own a context guard;
/// that guard is dropped after validation or when the execution closure releases it.
pub fn handler<T, Tx, Output, Prepared, P, E>(
    prepare: P,
    execute: E,
) -> impl TxHandler<T, Tx, Output>
where
    T: EvmTypesHost,
    P: Fn(&mut TxRequest<'_, '_, T, Tx>) -> HandlerResult<Prepared>,
    E: Fn(TxRequest<'_, '_, T, Tx>, Prepared) -> HandlerResult<Output>,
{
    FnHandler { prepare, execute, _prepared: PhantomData }
}

struct FnHandler<P, E, Prepared> {
    prepare: P,
    execute: E,
    _prepared: PhantomData<fn() -> Prepared>,
}

impl<T, Tx, Output, Prepared, P, E> TxHandler<T, Tx, Output> for FnHandler<P, E, Prepared>
where
    T: EvmTypesHost,
    P: Fn(&mut TxRequest<'_, '_, T, Tx>) -> HandlerResult<Prepared>,
    E: Fn(TxRequest<'_, '_, T, Tx>, Prepared) -> HandlerResult<Output>,
{
    fn validate(&self, mut req: TxRequest<'_, '_, T, Tx>) -> HandlerResult<()> {
        (self.prepare)(&mut req).map(|_| ())
    }

    fn execute(&self, mut req: TxRequest<'_, '_, T, Tx>) -> HandlerResult<Output> {
        let prepared = (self.prepare)(&mut req)?;
        (self.execute)(req, prepared)
    }
}

/// An erased transaction handler returned by [`TxRegistry`].
#[derive(Clone)]
pub struct AnyTxHandler<T: EvmTypesHost, Output> {
    inner: Arc<dyn ErasedTxHandler<T, Output>>,
}

impl<T: EvmTypesHost, Output> fmt::Debug for AnyTxHandler<T, Output> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnyTxHandler").finish_non_exhaustive()
    }
}

impl<T: EvmTypesHost, Output> AnyTxHandler<T, Output> {
    /// Executes the erased handler against an envelope and host.
    pub fn execute<'host>(
        &self,
        env: Recovered<&T::Tx>,
        host: &mut T::Host<'host>,
    ) -> HandlerResult<Output> {
        self.inner.execute(env, host)
    }

    /// Validates an envelope using the registered handler without executing user calls.
    ///
    /// This does not discard pre-execution writes or transaction-local state. The caller
    /// owns cleanup; [`crate::Evm::validate_tx`] provides validation with state cleanup.
    pub fn validate<'host>(
        &self,
        env: Recovered<&T::Tx>,
        host: &mut T::Host<'host>,
    ) -> HandlerResult<()> {
        self.inner.validate(env, host)
    }
}

trait ErasedTxHandler<T: EvmTypesHost, Output>: Send + Sync {
    fn execute<'host>(
        &self,
        env: Recovered<&T::Tx>,
        host: &mut T::Host<'host>,
    ) -> HandlerResult<Output>;

    fn validate<'host>(
        &self,
        env: Recovered<&T::Tx>,
        host: &mut T::Host<'host>,
    ) -> HandlerResult<()>;
}

struct HandlerAdapter<Tx, H, F> {
    type_id: u8,
    handler: H,
    extract: F,
    _tx: PhantomData<fn() -> Tx>,
}

impl<Tx, H, F> HandlerAdapter<Tx, H, F> {
    const fn new(type_id: u8, extract: F, handler: H) -> Self {
        Self { type_id, handler, extract, _tx: PhantomData }
    }
}

impl<T, Tx, Output, H, F> ErasedTxHandler<T, Output> for HandlerAdapter<Tx, H, F>
where
    T: EvmTypesHost,
    H: TxHandler<T, Tx, Output> + Send + Sync,
    F: for<'a> Fn(&'a T::Tx) -> Option<&'a Tx> + Send + Sync,
{
    fn execute<'host>(
        &self,
        env: Recovered<&T::Tx>,
        host: &mut T::Host<'host>,
    ) -> HandlerResult<Output> {
        let (envelope, signer) = env.into_parts();
        let tx = (self.extract)(envelope)
            .ok_or(HandlerError::WrongTransactionType { expected: self.type_id })?;
        self.handler.execute(TxRequest {
            envelope,
            tx: Recovered::new_unchecked(tx, signer),
            host,
            _non_exhaustive: (),
        })
    }

    fn validate<'host>(
        &self,
        env: Recovered<&T::Tx>,
        host: &mut T::Host<'host>,
    ) -> HandlerResult<()> {
        let (envelope, signer) = env.into_parts();
        let tx = (self.extract)(envelope)
            .ok_or(HandlerError::WrongTransactionType { expected: self.type_id })?;
        self.handler.validate(TxRequest {
            envelope,
            tx: Recovered::new_unchecked(tx, signer),
            host,
            _non_exhaustive: (),
        })
    }
}

/// A type-erased transaction handler registry keyed by transaction type byte.
///
/// Cloning is cheap and clones share handler instances. Registering on a clone copies the table
/// first, so it never affects the original.
pub struct TxRegistry<T: EvmTypesHost, Output = ()> {
    handlers: Arc<HashMap<u8, Arc<dyn ErasedTxHandler<T, Output>>>>,
}

impl<T: EvmTypesHost, Output> fmt::Debug for TxRegistry<T, Output> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TxRegistry").field("len", &self.handlers.len()).finish_non_exhaustive()
    }
}

impl<T: EvmTypesHost, Output> Clone for TxRegistry<T, Output> {
    fn clone(&self) -> Self {
        Self { handlers: Arc::clone(&self.handlers) }
    }
}

impl<T: EvmTypesHost, Output> Default for TxRegistry<T, Output> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: EvmTypesHost, Output> TxRegistry<T, Output> {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self { handlers: Arc::default() }
    }

    /// Registers a typed handler for a transaction type byte.
    ///
    /// `extract` projects `Tx` out of the envelope. The handler remains typed
    /// as `TxHandler<T, Tx, Output>`; only this registry boundary is erased.
    pub fn register<Tx, H, F>(&mut self, type_id: u8, extract: F, handler: H) -> &mut Self
    where
        Tx: 'static,
        H: TxHandler<T, Tx, Output> + Send + Sync + 'static,
        F: for<'a> Fn(&'a T::Tx) -> Option<&'a Tx> + Send + Sync + 'static,
    {
        Arc::make_mut(&mut self.handlers)
            .insert(type_id, Arc::new(HandlerAdapter::new(type_id, extract, handler)));
        self
    }

    /// Adds a typed handler and returns the registry.
    #[must_use]
    pub fn with_handler<Tx, H, F>(mut self, type_id: u8, extract: F, handler: H) -> Self
    where
        Tx: 'static,
        H: TxHandler<T, Tx, Output> + Send + Sync + 'static,
        F: for<'a> Fn(&'a T::Tx) -> Option<&'a Tx> + Send + Sync + 'static,
    {
        self.register(type_id, extract, handler);
        self
    }

    /// Returns true if a handler is registered for `type_id`.
    pub fn contains(&self, type_id: u8) -> bool {
        self.handlers.contains_key(&type_id)
    }

    /// Returns the erased handler registered for `type_id`, if any.
    pub fn get_by_type(&self, type_id: u8) -> Option<AnyTxHandler<T, Output>> {
        self.handlers.get(&type_id).map(|inner| AnyTxHandler { inner: Arc::clone(inner) })
    }

    /// Returns the erased handler registered for `type_id`.
    pub fn try_get_by_type(&self, type_id: u8) -> HandlerResult<AnyTxHandler<T, Output>> {
        self.get_by_type(type_id).ok_or(HandlerError::UnsupportedTransactionType(type_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BaseEvmConfigSelector, EvmTypesHost, ExecutionConfig, SpecId,
        env::{BlockEnv, BlockEnvExt, TxEnv},
        evm::{AccountLoad, SLoad, SStore, SelfDestructResult},
        interpreter::{Host, Message, MessageResult, Word},
    };
    use alloc::{borrow::Cow, rc::Rc, string::ToString, vec::Vec};
    use alloy_primitives::{Address, B256};
    use core::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, thiserror::Error)]
    #[error("typed handler error")]
    struct TestHandlerError;

    #[test]
    fn preserves_typed_external_errors() {
        let error = HandlerError::external(TestHandlerError);
        assert!(error.external_ref::<TestHandlerError>().is_some());
    }

    #[derive(Clone, Debug)]
    struct TransferTx {
        amount: u64,
    }

    #[derive(Clone, Debug)]
    struct CreateTx {
        initcode: Vec<u8>,
    }

    #[derive(Clone, Debug)]
    enum Envelope {
        Transfer(TransferTx),
        Create(CreateTx),
    }

    struct TestTypes;

    impl EvmTypesHost for TestTypes {
        type ConfigSelector = BaseEvmConfigSelector;
        type SpecId = SpecId;
        type Tx = Envelope;
        type EvmExt = ();
        type MessageExt = ();
        type MessageResultExt = ();
        type TxEnvExt = ();
        type TxResultExt = ();
        type BlockEnvExt = ();
        type Host<'a> = TestHost;
    }

    struct TestHost {
        block: BlockEnv<TestTypes>,
    }

    impl Host<TestTypes> for TestHost {
        fn spec_id(&self) -> SpecId {
            SpecId::default()
        }

        fn execution_config(&self) -> &ExecutionConfig<TestTypes> {
            unimplemented!()
        }

        fn block_env(&mut self) -> &BlockEnv<TestTypes> {
            &self.block
        }

        fn tx_env(&mut self) -> &TxEnv<TestTypes> {
            unimplemented!()
        }

        fn load_account(
            &mut self,
            _address: &Address,
            _load_code: bool,
            _skip_cold_load: bool,
        ) -> Result<AccountLoad, crate::HostError> {
            unimplemented!()
        }

        fn block_hash(&mut self, _number: &Word) -> Result<B256, DatabaseError> {
            unimplemented!()
        }

        fn sload(
            &mut self,
            _address: &Address,
            _key: &Word,
            _skip_cold_load: bool,
        ) -> Result<SLoad, crate::HostError> {
            unimplemented!()
        }

        fn sstore(
            &mut self,
            _address: &Address,
            _key: &Word,
            _value: &Word,
            _skip_cold_load: bool,
        ) -> Result<SStore, crate::HostError> {
            unimplemented!()
        }

        fn tload(&mut self, _address: &Address, _key: &Word) -> Word {
            unimplemented!()
        }

        fn tstore(&mut self, _address: &Address, _key: &Word, _value: &Word) {
            unimplemented!()
        }

        fn log(&mut self, _address: &Address, _topics: Cow<'_, [B256]>, _data: Cow<'_, [u8]>) {
            unimplemented!()
        }

        fn execute_message(
            &mut self,
            _message: &mut Message<TestTypes>,
        ) -> Result<MessageResult<TestTypes>, crate::ExecutionError> {
            unimplemented!()
        }

        fn selfdestruct(
            &mut self,
            _contract: &Address,
            _target: &Address,
            _skip_cold_load: bool,
        ) -> Result<SelfDestructResult, crate::HostError> {
            unimplemented!()
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Receipt {
        success: bool,
        cumulative_gas_used: u64,
    }

    fn transfer(env: &Envelope) -> Option<&TransferTx> {
        match env {
            Envelope::Transfer(tx) => Some(tx),
            Envelope::Create(_) => None,
        }
    }

    fn create(env: &Envelope) -> Option<&CreateTx> {
        match env {
            Envelope::Create(tx) => Some(tx),
            Envelope::Transfer(_) => None,
        }
    }

    fn receipt(cumulative_gas_used: u64) -> Receipt {
        Receipt { success: true, cumulative_gas_used }
    }

    fn handle_transfer(req: TxRequest<'_, '_, TestTypes, TransferTx>) -> HandlerResult<Receipt> {
        assert!(matches!(req.envelope, Envelope::Transfer(_)));
        let gas_used = 21_000 + req.tx.amount;
        Ok(receipt(gas_used))
    }

    fn handle_create(req: TxRequest<'_, '_, TestTypes, CreateTx>) -> HandlerResult<Receipt> {
        let gas_used = 53_000 + req.tx.initcode.len() as u64;
        Ok(receipt(gas_used))
    }

    fn call_registered(
        registry: &TxRegistry<TestTypes, Receipt>,
        type_id: u8,
        env: &Envelope,
    ) -> HandlerResult<Receipt> {
        registry.try_get_by_type(type_id)?.execute(
            Recovered::new_unchecked(env, Address::ZERO),
            &mut TestHost { block: BlockEnvExt::default() },
        )
    }

    #[test]
    fn dispatches_to_typed_handlers_from_erased_registry() {
        let mut registry = TxRegistry::<TestTypes, Receipt>::new();
        registry.register(0x01, transfer, handler(|_| Ok(()), |req, ()| handle_transfer(req)));
        registry.register(0x02, create, handler(|_| Ok(()), |req, ()| handle_create(req)));

        let transfer_receipt =
            call_registered(&registry, 0x01, &Envelope::Transfer(TransferTx { amount: 7 }))
                .expect("transfer handler is registered");
        assert_eq!(transfer_receipt, receipt(21_007));

        let create_receipt =
            call_registered(&registry, 0x02, &Envelope::Create(CreateTx { initcode: Vec::new() }))
                .expect("create handler is registered");
        assert_eq!(create_receipt, receipt(53_000));
    }

    #[test]
    fn reports_unsupported_and_mismatched_types() {
        let mut registry = TxRegistry::<TestTypes, Receipt>::new();
        registry.register(0x01, transfer, handler(|_| Ok(()), |req, ()| handle_transfer(req)));

        assert_eq!(
            call_registered(&registry, 0xff, &Envelope::Transfer(TransferTx { amount: 7 })),
            Err(HandlerError::UnsupportedTransactionType(0xff))
        );
        assert_eq!(
            call_registered(&registry, 0x01, &Envelope::Create(CreateTx { initcode: Vec::new() })),
            Err(HandlerError::WrongTransactionType { expected: 0x01 })
        );
    }

    #[test]
    fn validation_and_execution_share_preparation() {
        let prepared_count = Arc::new(AtomicUsize::new(0));
        let executed_count = Arc::new(AtomicUsize::new(0));
        let mut registry = TxRegistry::<TestTypes, Receipt>::new();
        registry.register(
            0x01,
            transfer,
            handler(
                {
                    let prepared_count = prepared_count.clone();
                    move |req: &mut TxRequest<'_, '_, TestTypes, TransferTx>| {
                        prepared_count.fetch_add(1, Ordering::Relaxed);
                        if req.tx.amount == 1 {
                            return Err(HandlerError::NonceOverflow);
                        }
                        // Prepared values need not be Send even though the registry is Send + Sync.
                        Ok(Rc::new(req.tx.amount))
                    }
                },
                {
                    let executed_count = executed_count.clone();
                    move |_, prepared| {
                        executed_count.fetch_add(1, Ordering::Relaxed);
                        if *prepared == 2 {
                            return Err(HandlerError::Fatal("execution failed".into()));
                        }
                        Ok(receipt(*prepared))
                    }
                },
            ),
        );
        let handler = registry.try_get_by_type(0x01).unwrap();
        let mut host = TestHost { block: BlockEnvExt::default() };
        for amount in 0..3 {
            let tx =
                Recovered::new_unchecked(Envelope::Transfer(TransferTx { amount }), Address::ZERO);
            let validation = handler.validate(tx.as_recovered_ref(), &mut host);
            let execution = handler.execute(tx.as_recovered_ref(), &mut host);
            match amount {
                0 => {
                    assert_eq!(validation, Ok(()));
                    assert_eq!(execution, Ok(receipt(0)));
                    assert_eq!(prepared_count.load(Ordering::Relaxed), 2);
                    assert_eq!(executed_count.load(Ordering::Relaxed), 1);
                }
                1 => {
                    assert_eq!(validation, Err(HandlerError::NonceOverflow));
                    assert_eq!(execution, Err(HandlerError::NonceOverflow));
                }
                2 => {
                    assert_eq!(validation, Ok(()));
                    let Err(HandlerError::Fatal(error)) = execution else {
                        panic!("expected fatal execution error, got {execution:?}");
                    };
                    assert_eq!(error.to_string(), "execution failed");
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(prepared_count.load(Ordering::Relaxed), 6);
        assert_eq!(executed_count.load(Ordering::Relaxed), 2);

        let wrong = Recovered::new_unchecked(
            Envelope::Create(CreateTx { initcode: Vec::new() }),
            Address::ZERO,
        );
        assert_eq!(
            handler.validate(wrong.as_recovered_ref(), &mut host),
            Err(HandlerError::WrongTransactionType { expected: 0x01 })
        );
        assert_eq!(prepared_count.load(Ordering::Relaxed), 6);
    }

    /// Mirrors a handler wrapper that forwards the borrowed envelope to an inner handler.
    struct Forwarding(AnyTxHandler<TestTypes, Receipt>);

    impl TxHandler<TestTypes, TransferTx, Receipt> for Forwarding {
        fn validate(&self, req: TxRequest<'_, '_, TestTypes, TransferTx>) -> HandlerResult<()> {
            self.0.validate(Recovered::new_unchecked(req.envelope, req.tx.signer()), req.host)
        }

        fn execute(&self, req: TxRequest<'_, '_, TestTypes, TransferTx>) -> HandlerResult<Receipt> {
            self.0.execute(Recovered::new_unchecked(req.envelope, req.tx.signer()), req.host)
        }
    }

    #[test]
    fn wrapper_forwards_borrowed_envelope() {
        let mut inner = TxRegistry::<TestTypes, Receipt>::new();
        inner.register(
            0x01,
            transfer,
            handler(
                |_| Ok(()),
                |req: TxRequest<'_, '_, TestTypes, TransferTx>, ()| {
                    assert_eq!(req.tx.signer(), Address::with_last_byte(1));
                    handle_transfer(req)
                },
            ),
        );
        let mut outer = TxRegistry::<TestTypes, Receipt>::new();
        outer.register(0x01, transfer, Forwarding(inner.try_get_by_type(0x01).unwrap()));

        let tx = Recovered::new_unchecked(
            Envelope::Transfer(TransferTx { amount: 3 }),
            Address::with_last_byte(1),
        );
        let handler = outer.try_get_by_type(0x01).unwrap();
        let mut host = TestHost { block: BlockEnvExt::default() };
        assert_eq!(handler.validate(tx.as_recovered_ref(), &mut host), Ok(()));
        assert_eq!(handler.execute(tx.as_recovered_ref(), &mut host), Ok(receipt(21_003)));
    }

    #[test]
    fn cloned_registries_are_independent() {
        let mut original = TxRegistry::<TestTypes, Receipt>::new();
        original.register(0x01, transfer, handler(|_| Ok(()), |req, ()| handle_transfer(req)));

        let mut cloned = original.clone();
        assert!(Arc::ptr_eq(&original.handlers, &cloned.handlers));
        cloned.register(0x02, create, handler(|_| Ok(()), |req, ()| handle_create(req)));
        assert!(cloned.contains(0x01));
        assert!(cloned.contains(0x02));
        assert!(original.contains(0x01));
        assert!(!original.contains(0x02));

        original.register(0x03, transfer, handler(|_| Ok(()), |req, ()| handle_transfer(req)));
        assert!(original.contains(0x03));
        assert!(!cloned.contains(0x03));
    }
}
