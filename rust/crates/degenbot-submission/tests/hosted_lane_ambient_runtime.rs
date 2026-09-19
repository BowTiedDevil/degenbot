#![expect(clippy::expect_used, reason = "test assertions fail loudly")]

//! A hosted lane must see the same multi-thread runtime ambience the
//! standalone `backrun_sidecar` gets from `#[tokio::main]`.
//!
//! The live two-lane run failed every `BlockSimHandle::build` with "no ambient
//! multi-threaded tokio runtime" because `StrategyHost::start_driving` booted
//! the !Send lane future on a dedicated current-thread runtime; the standalone
//! sidecar polls the same future directly on a multi-thread runtime. `revm`'s
//! `WrapDatabaseAsync::new` (the layer `BlockSimHandle::build_inner` stacks
//! over `AlloyDB`) returns `None` unless the current handle is multi-threaded,
//! and its reads block on that handle through `block_in_place`.
//!
//! This test boots a driver through the real host edge and asserts both halves
//! of the callsite contract inside the lane's own future: construction returns
//! `Some`, and a read completes (the `block_in_place` path) without panicking.
//! No chain is needed.

use std::future::{ready, Future};
use std::sync::Arc;

use degenbot_bot::bot_core::RouteRegistry;
use degenbot_bot::nonce_authority::{NonceAuthority, StrategyId};
use degenbot_bot::sidecar_paths::V2ConnectorIndex;
use degenbot_bot::strategy_host::{DriverExit, DriverSpawnFactory, FacetStatus, StrategyHost};
use degenbot_eventhub::Hub;
use revm::database_interface::async_db::DatabaseAsyncRef;
use revm::database_interface::{DatabaseRef, WrapDatabaseAsync};
use revm::primitives::{Address, StorageKey, StorageValue, B256};
use revm::state::AccountInfo;

/// A ready `DatabaseAsyncRef` so the wrapped read completes synchronously
/// without a node; `Infallible` already implements `DBErrorMarker`.
struct ReadyDb;

impl DatabaseAsyncRef for ReadyDb {
    type Error = core::convert::Infallible;

    fn basic_async_ref(
        &self,
        _address: Address,
    ) -> impl Future<Output = Result<Option<AccountInfo>, Self::Error>> + Send {
        ready(Ok(None))
    }

    fn code_by_hash_async_ref(
        &self,
        _code_hash: B256,
    ) -> impl Future<Output = Result<revm::bytecode::Bytecode, Self::Error>> + Send {
        ready(Ok(revm::bytecode::Bytecode::default()))
    }

    fn storage_async_ref(
        &self,
        _address: Address,
        _index: StorageKey,
    ) -> impl Future<Output = Result<StorageValue, Self::Error>> + Send {
        ready(Ok(StorageValue::ZERO))
    }

    fn block_hash_async_ref(
        &self,
        _number: u64,
    ) -> impl Future<Output = Result<B256, Self::Error>> + Send {
        ready(Ok(B256::ZERO))
    }
}

#[tokio::test]
async fn hosted_lane_builds_wrap_database_async() {
    let mut host = StrategyHost::new(
        Arc::new(Hub::new()),
        Arc::new(RouteRegistry::new(V2ConnectorIndex::default())),
        Arc::new(NonceAuthority::new(7)),
    );
    let id = StrategyId::new("backrun");
    host.register(id.clone(), FacetStatus::Configured)
        .expect("register");

    let (contract_tx, contract_rx) = tokio::sync::oneshot::channel::<bool>();
    let spawn: DriverSpawnFactory = Box::new(move |_lane| {
        Box::pin(async move {
            // The exact `BlockSimHandle::build_inner` construction contract.
            let built = WrapDatabaseAsync::new(ReadyDb);
            // The layer-read contract: `WrapDatabaseAsync`'s `DatabaseRef`
            // impl blocks on the captured handle, which must be reachable via
            // `block_in_place` from the lane's runtime context.
            let read_ok = built
                .as_ref()
                .is_some_and(|db| DatabaseRef::basic_ref(db, Address::ZERO).is_ok());
            contract_tx.send(read_ok).expect("lane contract receiver");
            DriverExit::Stopped
        })
    });
    host.attach_spawn(&id, spawn).expect("attach spawn");
    host.enable(&id).expect("enable");

    let tasks = host.start_driving().expect("start driving");
    let task = tasks.into_iter().next().expect("one driver task");
    assert_eq!(task.wait().await, DriverExit::Stopped);
    assert!(
        contract_rx
            .await
            .expect("lane reported the callsite verdict"),
        "hosted lane must expose a multi-thread runtime to WrapDatabaseAsync"
    );
}
